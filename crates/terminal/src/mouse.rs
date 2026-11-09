//! Mouse reporting: the bytes a program asked to receive for the pointer.
//!
//! A program turns reporting on with DECSET 1000 (presses and releases), 1002
//! (also motion while a button is held) or 1003 (all motion), and picks the
//! encoding with 1006 (SGR, unbounded coordinates) or 1005 (UTF-8). Without 1006
//! the classic X10 form is used, which cannot name a column or row past 223.
//!
//! Separately, a full-screen program that did not ask for the mouse still scrolls
//! with the wheel when the alternate-scroll mode (1007, on by default) is set: the
//! wheel becomes cursor-up and cursor-down, which is how `less` and `man` scroll.

use alacritty_terminal::term::TermMode;

/// Which button an event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held.
    None,
    WheelUp,
    WheelDown,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Motion; with a button held, a drag.
    Motion,
}

/// The modifier keys held during a mouse event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// Whether the program asked for mouse events at all.
#[must_use]
pub fn mouse_reporting(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// The bytes to send for a mouse event, or `None` when the program did not ask
/// for this kind of event or the position cannot be encoded.
///
/// `row` and `column` are zero-based cells of the visible grid.
#[must_use]
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    row: usize,
    column: usize,
    modifiers: MouseModifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !mouse_reporting(mode) {
        return None;
    }
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    match action {
        // The wheel only presses.
        MouseAction::Release if wheel => return None,
        MouseAction::Motion => {
            let reported = if button == MouseButton::None {
                mode.contains(TermMode::MOUSE_MOTION)
            } else {
                mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
            };
            if !reported || wheel {
                return None;
            }
        }
        _ => {}
    }

    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    let column = column + 1;
    let row = row + 1;

    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{column};{row}{end}").into_bytes());
    }

    // X10: a release does not say which button, and every value is offset by 32.
    if action == MouseAction::Release {
        code = (code & !0b11) | 3;
    }
    let mut bytes = b"\x1b[M".to_vec();
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    for value in [code, column as u32, row as u32] {
        let value = value + 32;
        if value < 0x80 {
            bytes.push(value as u8);
        } else if utf8 {
            let character = char::from_u32(value).filter(|_| value <= 0x7ff)?;
            let mut buffer = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        } else if value <= 0xff {
            bytes.push(value as u8);
        } else {
            return None;
        }
    }
    Some(bytes)
}

/// The keys the wheel sends to a full-screen program that did not ask for the
/// mouse, or `None` when the wheel should scroll the terminal's own history.
///
/// `lines` is positive for scrolling up.
#[must_use]
pub fn encode_alternate_scroll(lines: i32, mode: TermMode) -> Option<Vec<u8>> {
    if lines == 0
        || !mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
        || mouse_reporting(mode)
    {
        return None;
    }
    let application = mode.contains(TermMode::APP_CURSOR);
    let key: &[u8] = match (lines > 0, application) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    };
    Some(key.repeat(lines.unsigned_abs() as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(button: MouseButton, mode: TermMode) -> Option<Vec<u8>> {
        encode_mouse(
            button,
            MouseAction::Press,
            4,
            9,
            MouseModifiers::default(),
            mode,
        )
    }

    #[test]
    fn nothing_is_sent_unless_the_program_asked() {
        assert_eq!(press(MouseButton::Left, TermMode::empty()), None);
        assert_eq!(press(MouseButton::WheelUp, TermMode::SGR_MOUSE), None);
    }

    #[test]
    fn sgr_names_the_button_cell_and_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            press(MouseButton::Left, mode),
            Some(b"\x1b[<0;10;5M".to_vec())
        );
        let release = encode_mouse(
            MouseButton::Right,
            MouseAction::Release,
            0,
            0,
            MouseModifiers::default(),
            mode,
        );
        assert_eq!(release, Some(b"\x1b[<2;1;1m".to_vec()));
        assert_eq!(
            press(MouseButton::WheelDown, mode),
            Some(b"\x1b[<65;10;5M".to_vec())
        );
    }

    #[test]
    fn modifiers_and_drags_change_the_code() {
        let mode = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        let drag = encode_mouse(
            MouseButton::Left,
            MouseAction::Motion,
            1,
            1,
            MouseModifiers {
                control: true,
                ..MouseModifiers::default()
            },
            mode,
        );
        assert_eq!(drag, Some(b"\x1b[<48;2;2M".to_vec()));
    }

    #[test]
    fn motion_needs_the_mode_that_asks_for_it() {
        let hover = |mode| {
            encode_mouse(
                MouseButton::None,
                MouseAction::Motion,
                0,
                0,
                MouseModifiers::default(),
                mode,
            )
        };
        let drag = |mode| {
            encode_mouse(
                MouseButton::Left,
                MouseAction::Motion,
                0,
                0,
                MouseModifiers::default(),
                mode,
            )
        };
        let clicks = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(drag(clicks), None, "1000 does not report drags");
        assert_eq!(hover(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE), None);
        assert!(drag(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE).is_some());
        assert_eq!(
            hover(TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE),
            Some(b"\x1b[<35;1;1M".to_vec())
        );
    }

    #[test]
    fn x10_offsets_everything_and_cannot_reach_far_cells() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(press(MouseButton::Left, mode), Some(b"\x1b[M *%".to_vec()));
        let release = encode_mouse(
            MouseButton::Left,
            MouseAction::Release,
            0,
            0,
            MouseModifiers::default(),
            mode,
        );
        assert_eq!(release, Some(b"\x1b[M#!!".to_vec()));
        let far = encode_mouse(
            MouseButton::Left,
            MouseAction::Press,
            0,
            300,
            MouseModifiers::default(),
            mode,
        );
        assert_eq!(far, None);
        let utf8 = encode_mouse(
            MouseButton::Left,
            MouseAction::Press,
            0,
            300,
            MouseModifiers::default(),
            mode | TermMode::UTF8_MOUSE,
        )
        .expect("UTF-8 reaches further");
        assert_eq!(&utf8[..4], b"\x1b[M ");
        assert_eq!(std::str::from_utf8(&utf8[4..6]).ok(), Some("\u{14d}"));
    }

    #[test]
    fn the_wheel_scrolls_a_full_screen_program_with_the_arrows() {
        let full_screen = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            encode_alternate_scroll(2, full_screen),
            Some(b"\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(
            encode_alternate_scroll(-1, full_screen | TermMode::APP_CURSOR),
            Some(b"\x1bOB".to_vec())
        );
        assert_eq!(encode_alternate_scroll(1, TermMode::ALTERNATE_SCROLL), None);
        assert_eq!(
            encode_alternate_scroll(1, full_screen | TermMode::MOUSE_REPORT_CLICK),
            None
        );
    }
}
