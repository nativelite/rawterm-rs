//! rawterm — raw terminal mode and key/resize events, on the Rust standard
//! library alone. Zero dependencies, including no `libc` or `windows` crates:
//! the OS boundary is this crate's own small, audited `extern` blocks
//! (termios/`poll` on Unix, the Win32 console API on Windows).
//!
//! Two layers, deliberately separated:
//!
//! * [`Decoder`] is **pure bytes → events** and works anywhere: feed it the
//!   byte stream a terminal in raw+VT-input mode produces (chunks may split
//!   an escape sequence or a UTF-8 character anywhere) and it yields
//!   [`Event`]s — keys with modifiers, and bracketed paste.
//! * [`Terminal`] is the **thin OS edge**: enter raw mode (saving prior
//!   state), read input with a timeout, query the size, and restore the
//!   terminal on drop — including on panic, since restoration runs in `Drop`.
//!
//! ```
//! use rawterm::{Decoder, Event, Key};
//!
//! let mut d = Decoder::new();
//! let events = d.feed(b"a\x1b[1;5C");
//! assert_eq!(events[0], Event::key(Key::Char('a')));
//! assert_eq!(events[1], Event::key(Key::Right).ctrl()); // Ctrl+Right
//! ```
//!
//! Rendering is not here — build output bytes with the `ansi` crate (or by
//! hand) and write them to stdout yourself. PTYs are not here either; that
//! is the `pty` crate's concern.

mod decode;
#[cfg(unix)]
#[path = "sys_unix.rs"]
mod sys;
#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;

pub use decode::Decoder;

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

/// A decoded input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    /// Text inserted via bracketed paste (`ESC[200~` ... `ESC[201~`).
    Paste(String),
    /// The terminal was resized (detected by [`Terminal::read_event`]).
    Resize {
        rows: u16,
        cols: u16,
    },
}

impl Event {
    /// Convenience constructor: a key with no modifiers.
    pub fn key(key: Key) -> Event {
        Event::Key(KeyEvent {
            key,
            mods: Mods::default(),
        })
    }

    /// Convenience: the same event with the ctrl modifier set (keys only).
    pub fn ctrl(self) -> Event {
        match self {
            Event::Key(mut k) => {
                k.mods.ctrl = true;
                Event::Key(k)
            }
            other => other,
        }
    }

    /// Convenience: the same event with the alt modifier set (keys only).
    pub fn alt(self) -> Event {
        match self {
            Event::Key(mut k) => {
                k.mods.alt = true;
                Event::Key(k)
            }
            other => other,
        }
    }

    /// Convenience: the same event with the shift modifier set (keys only).
    pub fn shift(self) -> Event {
        match self {
            Event::Key(mut k) => {
                k.mods.shift = true;
                Event::Key(k)
            }
            other => other,
        }
    }
}

/// A key press with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

/// The key itself. `Char` carries the character as typed (already shifted:
/// `A` is `Char('A')` with no shift modifier reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

/// Modifier state reported alongside a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// A terminal in raw mode. Constructing one saves the current terminal
/// state and switches stdin to raw input (and, on Windows, stdout to VT
/// processing); dropping it restores everything, panic included.
pub struct Terminal {
    sys: sys::Sys,
    dec: Decoder,
    queue: VecDeque<Event>,
    last_size: (u16, u16),
}

/// How long to wait for the continuation of a lone `ESC` byte before
/// reporting it as the Esc key.
const ESC_GRACE: Duration = Duration::from_millis(15);

/// Poll granularity while waiting for input, so resizes are noticed even
/// with no keystrokes.
const POLL_TICK: Duration = Duration::from_millis(60);

impl Terminal {
    /// Enter raw mode on the process's terminal. Fails with an error (never
    /// panics, never half-configures) when stdin/stdout is not a terminal.
    pub fn raw() -> io::Result<Terminal> {
        let sys = sys::Sys::enter()?;
        let last_size = sys.size()?;
        Ok(Terminal {
            sys,
            dec: Decoder::new(),
            queue: VecDeque::new(),
            last_size,
        })
    }

    /// Current size as `(rows, cols)`.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        self.sys.size()
    }

    /// Toggle mouse capture. **Off by default** so the user keeps native
    /// drag-to-select / copy. Turning it `on` makes clicks arrive as SGR mouse
    /// sequences in [`read_bytes`] (at the cost of plain selection, which then
    /// needs the terminal's Shift-drag override) — a host enables it only when
    /// it consumes clicks (e.g. click-to-focus a pane). Idempotent.
    pub fn set_mouse(&mut self, on: bool) -> io::Result<()> {
        self.sys.set_mouse(on)
    }

    /// Read whatever raw input bytes arrive within `timeout`, undecoded.
    /// Empty result means the wait timed out.
    ///
    /// This is the passthrough primitive: a terminal-hosting program (a
    /// multiplexer) forwards these bytes to a child PTY verbatim, with
    /// zero decode/re-encode loss. Don't mix with [`read_event`] on the
    /// same terminal — whichever call runs consumes the bytes.
    pub fn read_bytes(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        self.sys.read_timeout(timeout)
    }

    /// Wait up to `timeout` (forever if `None`) for the next event. Returns
    /// `Ok(None)` on timeout. Resizes are detected by polling the size at a
    /// small interval while waiting.
    pub fn read_event(&mut self, timeout: Option<Duration>) -> io::Result<Option<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            let size = self.sys.size()?;
            if size != self.last_size {
                self.last_size = size;
                return Ok(Some(Event::Resize {
                    rows: size.0,
                    cols: size.1,
                }));
            }
            let wait = match deadline {
                None => POLL_TICK,
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Ok(None);
                    }
                    (d - now).min(POLL_TICK)
                }
            };
            let bytes = self.sys.read_timeout(wait)?;
            if bytes.is_empty() {
                continue;
            }
            self.queue.extend(self.dec.feed(&bytes));
            // A pending lone ESC is ambiguous (key vs. sequence start): give
            // the continuation a short grace period, then resolve it.
            if self.queue.is_empty() && self.dec.has_pending() {
                let more = self.sys.read_timeout(ESC_GRACE)?;
                if more.is_empty() {
                    self.queue.extend(self.dec.flush());
                } else {
                    self.queue.extend(self.dec.feed(&more));
                }
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.sys.restore();
    }
}
