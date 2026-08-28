//! The Unix edge: our own `extern "C"` declarations against the platform's
//! always-present C library — no `libc` crate. Raw mode is classic termios;
//! reads use `poll` + `read` so timeouts need no signals; size comes from
//! `ioctl(TIOCGWINSZ)`.
//!
//! Struct layouts and constants are per-OS (`linux` and `macos` differ) and
//! live in the `plat` module. Linux is CI-verified; macOS carries the
//! standard BSD values but is not yet exercised by CI.

use std::io;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod plat {
    pub type Flag = u32;
    pub const NCCS: usize = 32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: Flag,
        pub c_oflag: Flag,
        pub c_cflag: Flag,
        pub c_lflag: Flag,
        pub c_line: u8,
        pub c_cc: [u8; NCCS],
        pub c_ispeed: Flag,
        pub c_ospeed: Flag,
    }

    pub const IGNBRK: Flag = 0o0001;
    pub const BRKINT: Flag = 0o0002;
    pub const PARMRK: Flag = 0o0010;
    pub const ISTRIP: Flag = 0o0040;
    pub const INLCR: Flag = 0o0100;
    pub const IGNCR: Flag = 0o0200;
    pub const ICRNL: Flag = 0o0400;
    pub const IXON: Flag = 0o2000;
    pub const OPOST: Flag = 0o0001;
    pub const ECHO: Flag = 0o0010;
    pub const ECHONL: Flag = 0o0100;
    pub const ICANON: Flag = 0o0002;
    pub const ISIG: Flag = 0o0001;
    pub const IEXTEN: Flag = 0o100000;
    pub const CSIZE: Flag = 0o0060;
    pub const PARENB: Flag = 0o0400;
    pub const CS8: Flag = 0o0060;
    pub const VMIN: usize = 6;
    pub const VTIME: usize = 5;
    pub const TIOCGWINSZ: u64 = 0x5413;
    pub type Nfds = u64;
}

#[cfg(target_os = "macos")]
mod plat {
    pub type Flag = u64;
    pub const NCCS: usize = 20;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: Flag,
        pub c_oflag: Flag,
        pub c_cflag: Flag,
        pub c_lflag: Flag,
        pub c_cc: [u8; NCCS],
        pub c_ispeed: Flag,
        pub c_ospeed: Flag,
    }

    pub const IGNBRK: Flag = 0x0001;
    pub const BRKINT: Flag = 0x0002;
    pub const PARMRK: Flag = 0x0008;
    pub const ISTRIP: Flag = 0x0020;
    pub const INLCR: Flag = 0x0040;
    pub const IGNCR: Flag = 0x0080;
    pub const ICRNL: Flag = 0x0100;
    pub const IXON: Flag = 0x0200;
    pub const OPOST: Flag = 0x0001;
    pub const ECHO: Flag = 0x0008;
    pub const ECHONL: Flag = 0x0010;
    pub const ICANON: Flag = 0x0100;
    pub const ISIG: Flag = 0x0080;
    pub const IEXTEN: Flag = 0x0400;
    pub const CSIZE: Flag = 0x0300;
    pub const PARENB: Flag = 0x1000;
    pub const CS8: Flag = 0x0300;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
    pub const TIOCGWINSZ: u64 = 0x40087468;
    pub type Nfds = u32;
}

use plat::*;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x0001;
const TCSANOW: i32 = 0;
const STDIN_FD: i32 = 0;
const STDOUT_FD: i32 = 1;

extern "C" {
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, action: i32, termios: *const Termios) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn poll(fds: *mut PollFd, nfds: Nfds, timeout_ms: i32) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn isatty(fd: i32) -> i32;
}

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

pub struct Sys {
    saved: Termios,
}

impl Sys {
    pub fn enter() -> io::Result<Sys> {
        if unsafe { isatty(STDIN_FD) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stdin is not a terminal",
            ));
        }
        let mut saved = unsafe { std::mem::zeroed::<Termios>() };
        if unsafe { tcgetattr(STDIN_FD, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        raw.c_iflag &= !(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
        raw.c_oflag &= !OPOST;
        raw.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
        raw.c_cflag &= !(CSIZE | PARENB);
        raw.c_cflag |= CS8;
        raw.c_cc[VMIN] = 0; // reads return what is available;
        raw.c_cc[VTIME] = 0; // blocking/timeouts are poll()'s job
        if unsafe { tcsetattr(STDIN_FD, TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Sys { saved })
    }

    pub fn restore(&mut self) {
        unsafe {
            tcsetattr(STDIN_FD, TCSANOW, &self.saved);
        }
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        let mut ws = WinSize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { ioctl(STDOUT_FD, TIOCGWINSZ, &mut ws as *mut WinSize) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((ws.ws_row, ws.ws_col))
    }

    /// Read whatever input is available within `timeout`. Empty result
    /// means the wait timed out.
    pub fn read_timeout(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut fds = PollFd {
            fd: STDIN_FD,
            events: POLLIN,
            revents: 0,
        };
        let n = unsafe { poll(&mut fds, 1, millis) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new()); // EINTR: let the caller's loop retry
            }
            return Err(err);
        }
        if n == 0 || fds.revents & POLLIN == 0 {
            return Ok(Vec::new());
        }
        let mut buf = [0u8; 1024];
        let got = unsafe { read(STDIN_FD, buf.as_mut_ptr(), buf.len()) };
        if got < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf[..got as usize].to_vec())
    }
}
