//! The Windows edge: our own `extern "system"` declarations against
//! kernel32 — no `windows`/`winapi` crates. Raw input is achieved with
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` (keys arrive as VT escape sequences,
//! which the shared [`Decoder`](crate::Decoder) understands) and output
//! gains `ENABLE_VIRTUAL_TERMINAL_PROCESSING` so ANSI bytes render.
//!
//! Mouse input is enabled too (`ENABLE_MOUSE_INPUT`, quick-edit cleared) and
//! the console's binary `MOUSE_EVENT` records are translated to SGR mouse
//! escape sequences ([`encode_mouse`]) so they ride the same byte stream as
//! keys — a passthrough multiplexer forwards clicks exactly like keystrokes.

use std::ffi::c_void;
use std::io;
use std::time::Duration;

#[path = "mouse.rs"]
mod mouse;
use mouse::{encode_mouse, MouseRecord};

type Handle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
struct ScreenBufferInfo {
    size: Coord,
    cursor: Coord,
    attributes: u16,
    window: SmallRect,
    max_window: Coord,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> Handle;
    fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
    fn GetConsoleScreenBufferInfo(handle: Handle, info: *mut ScreenBufferInfo) -> i32;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn GetNumberOfConsoleInputEvents(handle: Handle, count: *mut u32) -> i32;
    fn ReadConsoleInputW(
        handle: Handle,
        buffer: *mut InputRecord,
        length: u32,
        read: *mut u32,
    ) -> i32;
}

/// The `KEY_EVENT_RECORD` body (everything after the `event_type` + pad
/// header). 16 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyRecord {
    key_down: i32,
    repeat_count: u16,
    virtual_key_code: u16,
    virtual_scan_code: u16,
    unicode_char: u16,
    control_key_state: u32,
}

/// The `INPUT_RECORD.Event` union, restricted to the two variants we read.
/// Both bodies fit the same 16 bytes that follow the record header; the
/// active variant is chosen by `InputRecord::event_type`.
#[repr(C)]
#[derive(Clone, Copy)]
union EventBody {
    key: KeyRecord,
    mouse: MouseRecord,
}

/// `INPUT_RECORD`: a 4-byte header (`EventType` + alignment pad) followed by
/// the tagged [`EventBody`] union. We read only `KEY_EVENT` and `MOUSE_EVENT`
/// variants; other records are ignored via `event_type`.
#[repr(C)]
#[derive(Clone, Copy)]
struct InputRecord {
    event_type: u16,
    _pad: u16,
    event: EventBody,
}

const KEY_EVENT: u16 = 0x0001;
const MOUSE_EVENT: u16 = 0x0002;

// The union overlay is only sound if both bodies match the real Win32
// `INPUT_RECORD.Event` size (16 bytes). Pin it at compile time so a stray
// field edit can never silently misread the console buffer.
const _: () = assert!(std::mem::size_of::<KeyRecord>() == 16);
const _: () = assert!(std::mem::size_of::<MouseRecord>() == 16);
const _: () = assert!(std::mem::size_of::<InputRecord>() == 20);

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const INVALID_HANDLE: Handle = -1isize as Handle;

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_MOUSE_INPUT: u32 = 0x0010;
const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

const WAIT_OBJECT_0: u32 = 0;

pub struct Sys {
    stdin: Handle,
    stdout: Handle,
    saved_in: u32,
    saved_out: u32,
    /// A high surrogate held until its low half arrives in the next read.
    pending_surrogate: Option<u16>,
    /// Previous console mouse-button bitmask, so a press vs. release can be
    /// derived from the state transition between consecutive mouse records.
    prev_buttons: u32,
}

impl Sys {
    pub fn enter() -> io::Result<Sys> {
        let stdin = std_handle(STD_INPUT_HANDLE)?;
        let stdout = std_handle(STD_OUTPUT_HANDLE)?;
        let saved_in = console_mode(stdin)?;
        let saved_out = console_mode(stdout)?;
        // Enable mouse input and clear quick-edit (which would otherwise steal
        // clicks for text selection). ENABLE_EXTENDED_FLAGS must accompany a
        // quick-edit change for the console to honor it.
        let raw_in = (saved_in
            & !(ENABLE_ECHO_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE))
            | ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_EXTENDED_FLAGS;
        let raw_out = saved_out | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        set_mode(stdin, raw_in)?;
        if let Err(e) = set_mode(stdout, raw_out) {
            let _ = set_mode(stdin, saved_in); // never leave it half-configured
            return Err(e);
        }
        Ok(Sys {
            stdin,
            stdout,
            saved_in,
            saved_out,
            pending_surrogate: None,
            prev_buttons: 0,
        })
    }

    pub fn restore(&mut self) {
        unsafe {
            SetConsoleMode(self.stdin, self.saved_in);
            SetConsoleMode(self.stdout, self.saved_out);
        }
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        let mut info = ScreenBufferInfo {
            size: Coord { x: 0, y: 0 },
            cursor: Coord { x: 0, y: 0 },
            attributes: 0,
            window: SmallRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            max_window: Coord { x: 0, y: 0 },
        };
        if unsafe { GetConsoleScreenBufferInfo(self.stdout, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let rows = (info.window.bottom - info.window.top + 1).max(0) as u16;
        let cols = (info.window.right - info.window.left + 1).max(0) as u16;
        Ok((rows, cols))
    }

    /// Read whatever input is available within `timeout`, as bytes. Key
    /// records become their UTF-8 characters and mouse records become SGR
    /// mouse escape sequences ([`encode_mouse`]), interleaved in stream
    /// order. Empty result means the wait timed out (or only events we do
    /// not surface — focus, buffer-size, plain mouse motion — arrived; they
    /// are consumed so they can never wedge the wait loop).
    pub fn read_timeout(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        if unsafe { WaitForSingleObject(self.stdin, millis) } != WAIT_OBJECT_0 {
            return Ok(Vec::new());
        }
        // The wait signals for ANY input record. Blocking `ReadConsoleW`
        // here would hang on a focus/mouse event (ConPTY emits focus
        // records at startup), so read the records themselves and keep
        // only key-down characters — VT input sequences arrive as those.
        let mut pending: u32 = 0;
        if unsafe { GetNumberOfConsoleInputEvents(self.stdin, &mut pending) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if pending == 0 {
            return Ok(Vec::new());
        }
        let mut records = [unsafe { std::mem::zeroed::<InputRecord>() }; 128];
        let want = records.len().min(pending as usize) as u32;
        let mut read: u32 = 0;
        if unsafe { ReadConsoleInputW(self.stdin, records.as_mut_ptr(), want, &mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out: Vec<u8> = Vec::new();
        // UTF-16 units from key records, buffered so surrogate pairs stay
        // whole across records; flushed to `out` (as UTF-8) in stream order
        // whenever a mouse record has to be emitted between keystrokes.
        // A high surrogate held from the previous read is re-attached by the
        // first `flush_units` (it drains `self.pending_surrogate`).
        let mut units: Vec<u16> = Vec::new();
        for rec in &records[..read as usize] {
            match rec.event_type {
                // SAFETY: event_type == KEY_EVENT, so the union's `key` body
                // is the active variant per the Win32 INPUT_RECORD contract.
                KEY_EVENT => {
                    let key = unsafe { rec.event.key };
                    if key.key_down != 0 && key.unicode_char != 0 {
                        for _ in 0..key.repeat_count.max(1) {
                            units.push(key.unicode_char);
                        }
                    }
                }
                // SAFETY: event_type == MOUSE_EVENT, so the union's `mouse`
                // body is the active variant per the Win32 INPUT_RECORD
                // contract.
                MOUSE_EVENT => {
                    let mouse = unsafe { rec.event.mouse };
                    if let Some(seq) = encode_mouse(&mouse, &mut self.prev_buttons) {
                        flush_units(&mut units, &mut out, &mut self.pending_surrogate);
                        out.extend_from_slice(&seq);
                    }
                }
                _ => {}
            }
        }
        flush_units(&mut units, &mut out, &mut self.pending_surrogate);
        Ok(out)
    }
}

/// Convert buffered UTF-16 `units` to UTF-8, appending to `out`. A trailing
/// lone high surrogate is not emitted; it is stashed in `pending` to be
/// prepended to the next batch (a surrogate pair can span two key records —
/// or, if a mouse record forced a flush between the halves, two flushes).
fn flush_units(units: &mut Vec<u16>, out: &mut Vec<u8>, pending: &mut Option<u16>) {
    // Re-attach a surrogate held from a prior flush so its low half (if it
    // arrives now) still pairs correctly.
    if let Some(hi) = pending.take() {
        units.insert(0, hi);
    }
    if let Some(&last) = units.last() {
        if (0xD800..0xDC00).contains(&last) {
            *pending = Some(last);
            units.pop();
        }
    }
    out.reserve(units.len() * 3);
    for ch in char::decode_utf16(units.drain(..)) {
        let ch = ch.unwrap_or('\u{FFFD}');
        let mut b = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
    }
}

fn std_handle(which: u32) -> io::Result<Handle> {
    let h = unsafe { GetStdHandle(which) };
    if h.is_null() || h == INVALID_HANDLE {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no console handle (not a terminal?)",
        ));
    }
    Ok(h)
}

fn console_mode(h: Handle) -> io::Result<u32> {
    let mut mode = 0u32;
    if unsafe { GetConsoleMode(h, &mut mode) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle is not a console (redirected?)",
        ));
    }
    Ok(mode)
}

fn set_mode(h: Handle, mode: u32) -> io::Result<()> {
    if unsafe { SetConsoleMode(h, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
