//! Pure bytes → [`Event`] decoding for a terminal in raw + VT-input mode.
//!
//! No I/O here: [`Decoder::feed`] takes whatever bytes arrived (chunks may
//! split an escape sequence or a UTF-8 character anywhere) and returns the
//! events completed so far; unfinished input is buffered. A lone `ESC` is
//! ambiguous — the Esc key and the start of a sequence look identical — so
//! it is *not* resolved by `feed`; the caller decides when no continuation
//! is coming and calls [`Decoder::flush`] (the `Terminal` does this after a
//! short grace period).

use crate::{Event, Key, KeyEvent, Mods};

/// Longest escape sequence we will buffer before dropping it as malformed.
const MAX_SEQ: usize = 64;

/// Cap on bracketed-paste payload, against a stream that never terminates
/// the paste.
const MAX_PASTE: usize = 1 << 20;

/// The incremental input decoder. Create one per input stream.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    paste: Option<Vec<u8>>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed newly-arrived bytes; returns the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        self.drain(false, &mut out);
        out
    }

    /// True if undecoded bytes are buffered (e.g. a lone `ESC` waiting to
    /// learn whether it starts a sequence).
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Resolve buffered input on the assumption that no continuation is
    /// coming: a pending `ESC` becomes the Esc key, and an unterminated
    /// paste is delivered as-is.
    pub fn flush(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        self.drain(true, &mut out);
        out
    }

    fn drain(&mut self, flush: bool, out: &mut Vec<Event>) {
        loop {
            if let Some(paste) = self.paste.as_mut() {
                // Consume until ESC[201~, everything before it is payload.
                match find_paste_end(&self.buf) {
                    Some((end, after)) => {
                        paste.extend_from_slice(&self.buf[..end]);
                        self.buf.drain(..after);
                        let data = self.paste.take().unwrap();
                        out.push(Event::Paste(lossy(&data)));
                        continue;
                    }
                    None => {
                        // Keep any suffix that could begin the terminator.
                        let keep = partial_terminator_len(&self.buf);
                        let take = self.buf.len() - keep;
                        paste.extend_from_slice(&self.buf[..take]);
                        self.buf.drain(..take);
                        if flush || paste.len() > MAX_PASTE {
                            paste.extend_from_slice(&self.buf);
                            self.buf.clear();
                            let data = self.paste.take().unwrap();
                            out.push(Event::Paste(lossy(&data)));
                        }
                        return;
                    }
                }
            }
            match decode_one(&self.buf, flush) {
                Step::Incomplete => return,
                Step::Consume(n, ev) => {
                    self.buf.drain(..n);
                    if let Some(ev) = ev {
                        out.push(ev);
                    }
                }
                Step::PasteStart(n) => {
                    self.buf.drain(..n);
                    self.paste = Some(Vec::new());
                }
            }
            if self.buf.is_empty() {
                return;
            }
        }
    }
}

enum Step {
    /// Need more bytes.
    Incomplete,
    /// Consume `n` bytes, optionally emitting an event.
    Consume(usize, Option<Event>),
    /// Consume `n` bytes and enter bracketed-paste mode.
    PasteStart(usize),
}

fn decode_one(b: &[u8], flush: bool) -> Step {
    let Some(&first) = b.first() else {
        return Step::Incomplete;
    };
    if first != 0x1B {
        return decode_plain(b);
    }
    if b.len() == 1 {
        return if flush {
            Step::Consume(1, Some(Event::key(Key::Esc)))
        } else {
            Step::Incomplete
        };
    }
    match b[1] {
        0x1B => Step::Consume(1, Some(Event::key(Key::Esc))),
        b'[' => decode_csi(b, flush),
        b'O' => {
            if b.len() < 3 {
                return if flush {
                    Step::Consume(1, Some(Event::key(Key::Esc)))
                } else {
                    Step::Incomplete
                };
            }
            let key = match b[2] {
                b'A' => Some(Key::Up),
                b'B' => Some(Key::Down),
                b'C' => Some(Key::Right),
                b'D' => Some(Key::Left),
                b'H' => Some(Key::Home),
                b'F' => Some(Key::End),
                f @ b'P'..=b'S' => Some(Key::F(f - b'P' + 1)),
                _ => None,
            };
            Step::Consume(3, key.map(Event::key))
        }
        _ => {
            // Alt + whatever the rest decodes to (a char or a control).
            match decode_plain(&b[1..]) {
                Step::Incomplete => {
                    if flush {
                        Step::Consume(1, Some(Event::key(Key::Esc)))
                    } else {
                        Step::Incomplete
                    }
                }
                Step::Consume(n, ev) => Step::Consume(1 + n, ev.map(Event::alt)),
                Step::PasteStart(_) => unreachable!("plain bytes never start a paste"),
            }
        }
    }
}

/// Decode a non-escape byte sequence: a control key or a UTF-8 character.
fn decode_plain(b: &[u8]) -> Step {
    let ev = |key| Step::Consume(1, Some(Event::key(key)));
    match b[0] {
        0x0D => ev(Key::Enter),
        0x09 => ev(Key::Tab),
        0x7F => ev(Key::Backspace),
        0x08 => Step::Consume(1, Some(Event::key(Key::Backspace).ctrl())),
        0x00 => Step::Consume(1, Some(Event::key(Key::Char(' ')).ctrl())),
        c @ 0x01..=0x1A => {
            let ch = (b'a' + c - 1) as char;
            Step::Consume(1, Some(Event::key(Key::Char(ch)).ctrl()))
        }
        c @ 0x1C..=0x1F => {
            let ch = (b'\\' + (c - 0x1C)) as char; // \ ] ^ _
            Step::Consume(1, Some(Event::key(Key::Char(ch)).ctrl()))
        }
        0x20..=0x7E => Step::Consume(1, Some(Event::key(Key::Char(b[0] as char)))),
        lead => {
            let want = match lead {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => return Step::Consume(1, Some(Event::key(Key::Char('\u{FFFD}')))),
            };
            if b.len() < want {
                return Step::Incomplete;
            }
            match std::str::from_utf8(&b[..want]) {
                Ok(s) => {
                    let ch = s.chars().next().unwrap();
                    Step::Consume(want, Some(Event::key(Key::Char(ch))))
                }
                Err(_) => Step::Consume(1, Some(Event::key(Key::Char('\u{FFFD}')))),
            }
        }
    }
}

fn decode_csi(b: &[u8], flush: bool) -> Step {
    // b starts with ESC [ ; find the final byte (0x40..=0x7E).
    let mut i = 2;
    while i < b.len() {
        match b[i] {
            0x40..=0x7E => break,
            _ if i - 2 >= MAX_SEQ => return Step::Consume(i, None), // runaway: drop
            _ => i += 1,
        }
    }
    if i >= b.len() {
        return if flush {
            Step::Consume(1, Some(Event::key(Key::Esc)))
        } else {
            Step::Incomplete
        };
    }
    let final_byte = b[i];
    let params: Vec<u16> = parse_params(&b[2..i]);
    let consumed = i + 1;
    let p0 = params.first().copied().unwrap_or(0);
    let mods = mods_from(params.get(1).copied().unwrap_or(0));
    let event = match final_byte {
        b'A' => Some(key_mods(Key::Up, mods)),
        b'B' => Some(key_mods(Key::Down, mods)),
        b'C' => Some(key_mods(Key::Right, mods)),
        b'D' => Some(key_mods(Key::Left, mods)),
        b'H' => Some(key_mods(Key::Home, mods)),
        b'F' => Some(key_mods(Key::End, mods)),
        b'Z' => Some(Event::key(Key::Tab).shift()),
        b'~' => {
            if p0 == 200 {
                return Step::PasteStart(consumed);
            }
            let key = match p0 {
                1 | 7 => Some(Key::Home),
                4 | 8 => Some(Key::End),
                2 => Some(Key::Insert),
                3 => Some(Key::Delete),
                5 => Some(Key::PageUp),
                6 => Some(Key::PageDown),
                11..=15 => Some(Key::F((p0 - 10) as u8)),
                17..=21 => Some(Key::F((p0 - 11) as u8)),
                23 | 24 => Some(Key::F((p0 - 12) as u8)),
                _ => None,
            };
            key.map(|k| key_mods(k, mods))
        }
        _ => None, // an unrecognized report/sequence: consume silently
    };
    Step::Consume(consumed, event)
}

fn key_mods(key: Key, mods: Mods) -> Event {
    Event::Key(KeyEvent { key, mods })
}

/// xterm modifier parameter: value minus 1 is a bitfield (1 = shift,
/// 2 = alt, 4 = ctrl); 0 or 1 means none.
fn mods_from(param: u16) -> Mods {
    if param < 2 {
        return Mods::default();
    }
    let bits = param - 1;
    Mods {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
    }
}

fn parse_params(bytes: &[u8]) -> Vec<u16> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split(|&b| b == b';')
        .take(8)
        .map(|seg| {
            let mut v: u32 = 0;
            for &b in seg {
                if !b.is_ascii_digit() {
                    break;
                }
                v = (v * 10 + (b - b'0') as u32).min(u16::MAX as u32);
            }
            v as u16
        })
        .collect()
}

const PASTE_END: &[u8] = b"\x1b[201~";

/// Find `ESC[201~` in `buf`; returns (payload end, bytes consumed through
/// the terminator).
fn find_paste_end(buf: &[u8]) -> Option<(usize, usize)> {
    buf.windows(PASTE_END.len())
        .position(|w| w == PASTE_END)
        .map(|i| (i, i + PASTE_END.len()))
}

/// Length of the longest buffer suffix that is a prefix of the paste
/// terminator (and so must not yet be treated as payload).
fn partial_terminator_len(buf: &[u8]) -> usize {
    let max = PASTE_END.len().min(buf.len());
    (1..=max)
        .rev()
        .find(|&n| buf[buf.len() - n..] == PASTE_END[..n])
        .unwrap_or(0)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
