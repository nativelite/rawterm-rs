# rawterm-rs
**Raw terminal mode and key/resize events**, built entirely on the Rust
standard library. **Zero dependencies**, including no `libc` or `windows`
crates: the OS boundary is this crate's own small, audited `extern` blocks.

This is the input edge of a TUI, and nothing else: switch the terminal to
raw mode, decode what the user types (arrows, modifiers, paste), notice
resizes, and put everything back exactly as found: on drop, panic included.

## Two layers, deliberately separated

**`Decoder`: pure bytes → events.** No I/O; fully testable anywhere. Feed
it the byte stream a raw + VT-input terminal produces, in chunks split at
*any* boundary (mid-escape, mid-UTF-8), and it yields events:

```rust
use rawterm::{Decoder, Event, Key};

let mut d = Decoder::new();
let events = d.feed(b"a\x1b[1;5C");
assert_eq!(events[0], Event::key(Key::Char('a')));
assert_eq!(events[1], Event::key(Key::Right).ctrl()); // Ctrl+Right
```

Coverage: printable + UTF-8 chars, control keys (Ctrl+letter), arrows,
Home/End/PgUp/PgDn/Ins/Del, F1–F12 (both SS3 and CSI encodings), xterm
modifier parameters (shift/alt/ctrl), Alt+key via ESC prefix, and bracketed
paste (`ESC[200~ ... ESC[201~`) with payloads that survive splits even
inside the terminator. A lone `ESC` is genuinely ambiguous, so `feed` never
guesses: the caller resolves it with `flush()` after a grace period
(`Terminal` does this for you).

**`Terminal`: the thin OS edge.**

```rust,no_run
use std::time::Duration;

let mut term = rawterm::Terminal::raw()?;        // saves state, goes raw
let (rows, cols) = term.size()?;
while let Some(event) = term.read_event(Some(Duration::from_secs(1)))? {
    // Key(..), Paste(..), or Resize { rows, cols }
}
// dropping `term` restores the terminal, panic included
# std::io::Result::Ok(())
```

Per platform, in our own FFI:

- **Windows** (kernel32): `SetConsoleMode` with
  `ENABLE_VIRTUAL_TERMINAL_INPUT` (keys arrive as the same VT sequences the
  decoder already understands), plus `ENABLE_MOUSE_INPUT` (quick-edit cleared)
  for clicks, and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout so ANSI output
  renders. Input records are read (`ReadConsoleInputW`): key records become
  UTF-16 text (surrogate pairs handled across read boundaries) and mouse
  records are translated to SGR, interleaved in stream order.
- **Unix** (the platform's always-present C library): classic termios raw
  mode, `poll` + `read` for timeouts (no signal handlers), and
  `ioctl(TIOCGWINSZ)` for size. The suite passes on Linux (run it locally);
  macOS carries the standard BSD constants and compiles, but isn't exercised yet.

Resizes are detected by polling the size at a short interval while waiting
for input; no SIGWINCH handler, so the crate installs nothing global.

## Mouse

Raw mode captures mouse input and surfaces it as **SGR mouse escape sequences**
(`ESC[<Cb;Cx;Cy` then `M` press / `m` release) inline in the byte stream
`read_bytes` returns, so a passthrough multiplexer forwards clicks exactly like
keystrokes, with no extra plumbing. Covered: left/middle/right press and
release, drag (motion with a button held), wheel up/down, and shift/alt/ctrl
modifiers. Plain motion with no button held is dropped.

On **Windows** this means enabling `ENABLE_MOUSE_INPUT` and clearing
`ENABLE_QUICK_EDIT_MODE` (which would otherwise consume clicks for text
selection), then translating the console's binary `MOUSE_EVENT` records to SGR.
On **Unix** the terminal already delivers SGR mouse as bytes once the app
enables mouse mode, so the reader just passes them through, no translation.

> **Gating is the consumer's job.** rawterm always *emits* mouse SGR once raw
> mode is entered; it does not track whether a hosted app asked for mouse
> reporting. A multiplexer that forwards these bytes to a child should gate them
> on the pane's own mouse mode (`ESC[?1000h` / `ESC[?1006h` on, `…l` off) so an
> app that never requested mouse doesn't receive stray click bytes. That
> tracking lives in the consumer (e.g. atrium), not here.

## What's deliberately out of scope

- **Rendering:** build output with the `ansi` crate; write it yourself.
- **PTYs / child processes:** the `pty` crate's concern.
- **Mouse-mode gating and focus events:** the consumer's concern (see above).
- **A main loop:** `read_event(timeout)` is the primitive; own your loop.

## Correctness

The decoder carries the suite discipline: a fixture table over every key
class is verified fed one-shot **and** byte-at-a-time (identical results
required), lone-ESC resolution is pinned to `flush`, partial UTF-8 is held
across feeds, and bracketed paste is tested with the terminator split mid-
sequence. When stdin/stdout is not a terminal, `Terminal::raw()` fails with
a clear error and configures nothing. All three OS modules compile-check
(Windows and Linux exercised; macOS compile-only so far).

## Development

```bash
python dev.py check   # zero-dependency guard + cargo test (the pre-push gate)
python dev.py test    # cargo test
python dev.py fmt     # cargo fmt --check
python dev.py guard   # zero-dependency guard
```

`dev.py` is a stdlib-only runner, so `python dev.py check` is the same
one-command local gate used across every nativelite package. The guard fails
if `Cargo.toml` declares any dependency: runtime, build, or dev.
