//! Translate a Windows console `MOUSE_EVENT_RECORD` into an SGR mouse escape
//! sequence (`ESC [ < Cb ; Cx ; Cy` then `M` for press / `m` for release).
//!
//! This is the testable core of Windows mouse support: a pure function over a
//! plain record plus a caller-held "previous button state", so it can be unit-
//! tested without a live console (CI cannot inject real mouse events).
//!
//! ## What it covers
//! - Left / middle / right button **press** and **release**, derived from the
//!   transition in `dwButtonState` against the previous state.
//! - **Drag** (motion with a button held): `MOUSE_MOVED` with buttons down →
//!   the button's code + 32, reported as a press (`M`).
//! - **Wheel** up / down (`MOUSE_WHEELED`), from the sign of the high word of
//!   `dwButtonState`.
//! - Modifier folding from `dwControlKeyState` (shift/alt/ctrl).
//!
//! ## What it omits (by design, for v1)
//! - Plain motion with **no** button held is dropped (returns `None`); most
//!   apps that want it enable "any-motion" mode, which is a consumer concern.
//! - Horizontal wheel and X1/X2 buttons are not encoded.
//! - Only single-button transitions are tracked; a truly simultaneous
//!   multi-button change reports one button per record as state settles.

/// The `MOUSE_EVENT_RECORD` body (the 16 bytes after the `INPUT_RECORD`
/// header). Field order and types match the Win32 struct exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseRecord {
    /// `dwMousePosition`: a `COORD` (0-based cell column `x`, row `y`).
    pub position: Coord,
    /// `dwButtonState`: low word is the button bitmask; for a wheel event the
    /// high word is a signed rotation amount.
    pub button_state: u32,
    /// `dwControlKeyState`: keyboard modifier flags active at the event.
    pub control_key_state: u32,
    /// `dwEventFlags`: `MOUSE_MOVED` / `MOUSE_WHEELED` / etc.
    pub event_flags: u32,
}

/// A `COORD`: two 16-bit signed cell coordinates (0-based).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Coord {
    pub x: i16,
    pub y: i16,
}

// dwEventFlags
const MOUSE_MOVED: u32 = 0x0001;
const MOUSE_WHEELED: u32 = 0x0004;

// dwButtonState low-word button bits.
const FROM_LEFT_1ST_BUTTON: u32 = 0x0001; // left
const RIGHTMOST_BUTTON: u32 = 0x0002; // right
const FROM_LEFT_2ND_BUTTON: u32 = 0x0004; // middle
const BUTTON_MASK: u32 = FROM_LEFT_1ST_BUTTON | RIGHTMOST_BUTTON | FROM_LEFT_2ND_BUTTON;

// dwControlKeyState modifier bits we fold into the SGR button code.
const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_ALT_PRESSED: u32 = 0x0002;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const SHIFT_PRESSED: u32 = 0x0010;

// SGR modifier additions.
const SGR_SHIFT: u32 = 4;
const SGR_ALT: u32 = 8;
const SGR_CTRL: u32 = 16;

/// Encode one mouse record as an SGR sequence, updating `prev_buttons` to this
/// record's button state. Returns `None` for events we do not surface (plain
/// motion with no button held).
///
/// `prev_buttons` is the caller-held low-word button bitmask from the previous
/// mouse record; comparing it to this record's mask is how a press is told
/// apart from a release (a click is a state *transition*).
pub fn encode_mouse(rec: &MouseRecord, prev_buttons: &mut u32) -> Option<Vec<u8>> {
    let buttons = rec.button_state & BUTTON_MASK;
    let prev = *prev_buttons;
    let mods = sgr_mods(rec.control_key_state);

    // 1-based cell coordinates (SGR is 1-based; the console COORD is 0-based).
    let cx = (rec.position.x as i32 + 1).max(1) as u32;
    let cy = (rec.position.y as i32 + 1).max(1) as u32;

    // Wheel: independent of button-state transitions.
    if rec.event_flags & MOUSE_WHEELED != 0 {
        // High word of dwButtonState is a signed rotation: positive = up.
        let delta = (rec.button_state >> 16) as i16;
        let cb = if delta >= 0 { 64 } else { 65 } + mods;
        return Some(sgr(cb, cx, cy, true));
    }

    if rec.event_flags & MOUSE_MOVED != 0 {
        // Drag: motion with a button held → button code + 32, press-style.
        // Plain motion with no button is not surfaced.
        let btn = lowest_button_code(buttons)?;
        *prev_buttons = buttons;
        return Some(sgr(btn + 32 + mods, cx, cy, true));
    }

    // A press adds bits, a release clears them. Encode the changed button.
    let pressed = buttons & !prev; // bits newly set → press
    let released = prev & !buttons; // bits newly cleared → release
    *prev_buttons = buttons;

    if let Some(btn) = lowest_button_code(pressed) {
        Some(sgr(btn + mods, cx, cy, true))
    } else {
        let btn = lowest_button_code(released)?;
        Some(sgr(btn + mods, cx, cy, false))
    }
}

/// SGR base button code for the lowest set button bit, or `None` if no button.
fn lowest_button_code(buttons: u32) -> Option<u32> {
    if buttons & FROM_LEFT_1ST_BUTTON != 0 {
        Some(0) // left
    } else if buttons & FROM_LEFT_2ND_BUTTON != 0 {
        Some(1) // middle
    } else if buttons & RIGHTMOST_BUTTON != 0 {
        Some(2) // right
    } else {
        None
    }
}

fn sgr_mods(control_key_state: u32) -> u32 {
    let mut m = 0;
    if control_key_state & SHIFT_PRESSED != 0 {
        m += SGR_SHIFT;
    }
    if control_key_state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        m += SGR_ALT;
    }
    if control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        m += SGR_CTRL;
    }
    m
}

/// Build `ESC [ < Cb ; Cx ; Cy (M|m)` as ASCII bytes.
fn sgr(cb: u32, cx: u32, cy: u32, press: bool) -> Vec<u8> {
    let final_byte = if press { 'M' } else { 'm' };
    format!("\x1b[<{cb};{cx};{cy}{final_byte}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(x: i16, y: i16, buttons: u32, flags: u32, ctrl: u32) -> MouseRecord {
        MouseRecord {
            position: Coord { x, y },
            button_state: buttons,
            control_key_state: ctrl,
            event_flags: flags,
        }
    }

    #[test]
    fn left_click_press_and_release() {
        let mut prev = 0;
        // Press left at (0,0) → Cb=0, Cx=1, Cy=1, 'M'.
        let down = rec(0, 0, FROM_LEFT_1ST_BUTTON, 0, 0);
        assert_eq!(encode_mouse(&down, &mut prev).unwrap(), b"\x1b[<0;1;1M");
        // Release (button now up) → Cb=0, 'm'.
        let up = rec(0, 0, 0, 0, 0);
        assert_eq!(encode_mouse(&up, &mut prev).unwrap(), b"\x1b[<0;1;1m");
    }

    #[test]
    fn right_and_middle_press() {
        let mut prev = 0;
        let right = rec(4, 2, RIGHTMOST_BUTTON, 0, 0);
        // Cx = 4+1 = 5, Cy = 2+1 = 3, Cb = 2.
        assert_eq!(encode_mouse(&right, &mut prev).unwrap(), b"\x1b[<2;5;3M");

        let mut prev = 0;
        let middle = rec(0, 0, FROM_LEFT_2ND_BUTTON, 0, 0);
        assert_eq!(encode_mouse(&middle, &mut prev).unwrap(), b"\x1b[<1;1;1M");
    }

    #[test]
    fn wheel_up_and_down() {
        let mut prev = 0;
        // High word positive → up → Cb=64, 'M'.
        let up = rec(0, 0, 1u32 << 16, MOUSE_WHEELED, 0);
        assert_eq!(encode_mouse(&up, &mut prev).unwrap(), b"\x1b[<64;1;1M");
        // High word negative (0xFFFF as i16 = -1) → down → Cb=65, 'M'.
        let down = rec(0, 0, 0xFFFFu32 << 16, MOUSE_WHEELED, 0);
        assert_eq!(encode_mouse(&down, &mut prev).unwrap(), b"\x1b[<65;1;1M");
    }

    #[test]
    fn motion_with_button_is_a_drag() {
        let mut prev = FROM_LEFT_1ST_BUTTON; // left already down
        let drag = rec(1, 1, FROM_LEFT_1ST_BUTTON, MOUSE_MOVED, 0);
        // Left drag → base 0 + 32 = 32, press-style 'M', Cx=Cy=2.
        assert_eq!(encode_mouse(&drag, &mut prev).unwrap(), b"\x1b[<32;2;2M");
    }

    #[test]
    fn plain_motion_no_button_is_dropped() {
        let mut prev = 0;
        let moved = rec(3, 3, 0, MOUSE_MOVED, 0);
        assert_eq!(encode_mouse(&moved, &mut prev), None);
    }

    #[test]
    fn modifiers_fold_into_button_code() {
        let mut prev = 0;
        // Ctrl+Shift left press → Cb = 0 + 16 + 4 = 20.
        let ctrl_shift = rec(
            0,
            0,
            FROM_LEFT_1ST_BUTTON,
            0,
            SHIFT_PRESSED | LEFT_CTRL_PRESSED,
        );
        assert_eq!(
            encode_mouse(&ctrl_shift, &mut prev).unwrap(),
            b"\x1b[<20;1;1M"
        );
    }

    #[test]
    fn release_reports_the_button_that_went_up() {
        let mut prev = RIGHTMOST_BUTTON; // right was down
        let up = rec(0, 0, 0, 0, 0); // all up now
                                     // Right release → Cb=2, 'm'.
        assert_eq!(encode_mouse(&up, &mut prev).unwrap(), b"\x1b[<2;1;1m");
        assert_eq!(prev, 0);
    }
}
