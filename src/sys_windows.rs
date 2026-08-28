//! The Windows edge: our own `extern "system"` declarations against
//! kernel32 — no `windows`/`winapi` crates. Raw input is achieved with
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` (keys arrive as VT escape sequences,
//! which the shared [`Decoder`](crate::Decoder) understands) and output
//! gains `ENABLE_VIRTUAL_TERMINAL_PROCESSING` so ANSI bytes render.

use std::ffi::c_void;
use std::io;
use std::time::Duration;

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
    fn ReadConsoleW(
        handle: Handle,
        buffer: *mut u16,
        chars_to_read: u32,
        chars_read: *mut u32,
        input_control: *mut c_void,
    ) -> i32;
}

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const INVALID_HANDLE: Handle = -1isize as Handle;

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
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
}

impl Sys {
    pub fn enter() -> io::Result<Sys> {
        let stdin = std_handle(STD_INPUT_HANDLE)?;
        let stdout = std_handle(STD_OUTPUT_HANDLE)?;
        let saved_in = console_mode(stdin)?;
        let saved_out = console_mode(stdout)?;
        let raw_in = (saved_in & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
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

    /// Read whatever input is available within `timeout`, as UTF-8 bytes.
    /// Empty result means the wait timed out.
    pub fn read_timeout(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        if unsafe { WaitForSingleObject(self.stdin, millis) } != WAIT_OBJECT_0 {
            return Ok(Vec::new());
        }
        let mut buf = [0u16; 256];
        let mut read: u32 = 0;
        let ok = unsafe {
            ReadConsoleW(
                self.stdin,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut units: Vec<u16> = Vec::with_capacity(read as usize + 1);
        if let Some(hi) = self.pending_surrogate.take() {
            units.push(hi);
        }
        units.extend_from_slice(&buf[..read as usize]);
        // Hold a trailing lone high surrogate for the next read.
        if let Some(&last) = units.last() {
            if (0xD800..0xDC00).contains(&last) {
                self.pending_surrogate = Some(last);
                units.pop();
            }
        }
        let mut out = Vec::with_capacity(units.len() * 3);
        for ch in char::decode_utf16(units.into_iter()) {
            let ch = ch.unwrap_or('\u{FFFD}');
            let mut b = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
        }
        Ok(out)
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
