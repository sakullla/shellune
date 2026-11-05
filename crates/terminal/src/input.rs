//! Encoding of keys and text into the bytes a terminal program reads (R8).
//!
//! A terminal is not a text field: most keys are not characters but escape
//! sequences, and which sequence is correct depends on modes the running program
//! switched on (application cursor keys, bracketed paste). The rules live here,
//! away from GPUI, so they can be checked against the sequences xterm defines
//! without opening a window.
//!
//! One decision is worth stating: a plain printable key returns `None` from
//! [`encode_key`]. The character itself arrives through the text-input path, which is
//! also the path an input method uses. Sending it here as well would type every
//! letter twice, and would put committed IME text in front of the composition.
//! Keys that the text-input path does not cover — arrows, function keys,
//! Control combinations, Escape, Enter — are encoded here.

use alacritty_terminal::term::TermMode;

/// Modifier keys held during a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub control: bool,
    /// Alt (Option on macOS).
    pub alt: bool,
    /// The platform key: Command on macOS, Windows/Super elsewhere.
    pub platform: bool,
}

impl KeyModifiers {
    /// No modifier held.
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        alt: false,
        platform: false,
    };

    /// Control only.
    #[must_use]
    pub fn control() -> Self {
        Self {
            control: true,
            ..Self::NONE
        }
    }

    /// Shift only.
    #[must_use]
    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    /// Control and shift, the terminal's own clipboard shortcuts.
    #[must_use]
    pub fn control_shift() -> Self {
        Self {
            control: true,
            shift: true,
            ..Self::NONE
        }
    }

    /// Whether the platform key is held, which the shell must never see.
    #[must_use]
    pub fn has_platform(self) -> bool {
        self.platform
    }

    /// The xterm modifier parameter: 1 plus the modifier bits.
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift)
            + 2 * u8::from(self.alt)
            + 4 * u8::from(self.control)
            + 8 * u8::from(self.platform)
    }

    /// The prefix a modifier adds in front of a byte, for the control table.
    fn escape_prefix(self) -> Vec<u8> {
        if self.alt || self.platform {
            vec![0x1b]
        } else {
            Vec::new()
        }
    }
}

/// A key the encoder understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable character key.
    Character(char),
    /// Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Escape.
    Escape,
    /// Space, which is only encoded when a modifier makes it a control code.
    Space,
    /// Delete (forward).
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Function key, 1 to 12.
    Function(u8),
}

impl Key {
    /// The key a platform keystroke name describes, if this module handles it.
    ///
    /// Names follow the GPUI/keymap spelling, so a view can pass the keystroke
    /// through without its own table.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let single = {
            let mut characters = name.chars();
            match (characters.next(), characters.next()) {
                (Some(character), None) => Some(character),
                _ => None,
            }
        };
        match name {
            "enter" => Some(Self::Enter),
            "tab" => Some(Self::Tab),
            "backspace" => Some(Self::Backspace),
            "escape" => Some(Self::Escape),
            "space" => Some(Self::Space),
            "delete" => Some(Self::Delete),
            "insert" => Some(Self::Insert),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "pageup" | "page_up" => Some(Self::PageUp),
            "pagedown" | "page_down" => Some(Self::PageDown),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => {
                if let Some(rest) = name.strip_prefix('f')
                    && let Ok(number) = rest.parse::<u8>()
                    && (1..=12).contains(&number)
                {
                    return Some(Self::Function(number));
                }
                single.map(Self::Character)
            }
        }
    }
}

/// The bytes a key press sends.
///
/// `None` means the key produces no bytes here: a plain printable character, a
/// combination reserved for a shell shortcut, or a key the terminal does not map.
#[must_use]
pub fn encode_key(key: Key, modifiers: KeyModifiers, mode: TermMode) -> Option<Vec<u8>> {
    match key {
        Key::Character(character) => encode_character(character, modifiers),
        Key::Space => {
            if modifiers.control {
                Some(vec![0x00])
            } else {
                None
            }
        }
        Key::Enter => Some(with_prefix(b"\r".to_vec(), modifiers)),
        Key::Tab => {
            if modifiers.shift {
                Some(b"\x1b[Z".to_vec())
            } else if modifiers.alt {
                Some(b"\x1b\t".to_vec())
            } else {
                Some(b"\t".to_vec())
            }
        }
        Key::Backspace => Some(with_prefix(
            vec![if modifiers.control { 0x08 } else { 0x7f }],
            modifiers,
        )),
        Key::Escape => Some(vec![0x1b]),
        Key::Up => Some(cursor_key(b'A', modifiers, mode)),
        Key::Down => Some(cursor_key(b'B', modifiers, mode)),
        Key::Right => Some(cursor_key(b'C', modifiers, mode)),
        Key::Left => Some(cursor_key(b'D', modifiers, mode)),
        Key::Home => Some(home_end(b'H', modifiers, mode)),
        Key::End => Some(home_end(b'F', modifiers, mode)),
        Key::Insert => Some(tilde_key(2, modifiers)),
        Key::Delete => Some(tilde_key(3, modifiers)),
        Key::PageUp => Some(tilde_key(5, modifiers)),
        Key::PageDown => Some(tilde_key(6, modifiers)),
        Key::Function(number) => function_key(number, modifiers),
    }
}

/// Text typed through the text-input path, with line breaks the terminal expects.
///
/// A shell treats `\r` as the Return key and a bare `\n` as a literal control
/// character, so multi-line text is normalised before it is sent.
#[must_use]
pub fn encode_text(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                // A CRLF pair is one Return; a lone CR stays as it is.
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                bytes.push(b'\r');
            }
            '\n' => bytes.push(b'\r'),
            character => {
                let mut buffer = [0u8; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    bytes
}

/// Pasted text, wrapped when the running program asked for bracketed paste.
///
/// Bracketed paste is what stops an editor from treating a paste as typing: without
/// the markers, Vim starts inserting at every line, and a shell with auto-indent
/// mangles pasted code.
#[must_use]
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    let body = encode_text(text);
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        body
    }
}

fn encode_character(character: char, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    if modifiers.control {
        let control = control_code(character)?;
        let mut bytes = modifiers.escape_prefix();
        bytes.push(control);
        return Some(bytes);
    }
    if modifiers.alt || modifiers.platform {
        if modifiers.platform {
            return None;
        }
        let mut bytes = vec![0x1b];
        let mut buffer = [0u8; 4];
        bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        return Some(bytes);
    }
    // Plain text arrives through the text-input path.
    None
}

/// The C0 code a Control combination produces.
fn control_code(character: char) -> Option<u8> {
    let upper = character.to_ascii_uppercase();
    match upper {
        'A'..='Z' => Some(upper as u8 - b'A' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn with_prefix(mut bytes: Vec<u8>, modifiers: KeyModifiers) -> Vec<u8> {
    if modifiers.alt || modifiers.platform {
        let mut prefixed = vec![0x1b];
        prefixed.append(&mut bytes);
        return prefixed;
    }
    bytes
}

fn cursor_key(final_byte: u8, modifiers: KeyModifiers, mode: TermMode) -> Vec<u8> {
    if modifiers.parameter() > 1 {
        let mut bytes = format!("\x1b[1;{}", modifiers.parameter()).into_bytes();
        bytes.push(final_byte);
        return bytes;
    }
    if mode.contains(TermMode::APP_CURSOR) {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn home_end(final_byte: u8, modifiers: KeyModifiers, mode: TermMode) -> Vec<u8> {
    if modifiers.parameter() > 1 {
        return format!("\x1b[1;{}{}", modifiers.parameter(), final_byte as char).into_bytes();
    }
    if mode.contains(TermMode::APP_CURSOR) {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn tilde_key(code: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if modifiers.parameter() > 1 {
        format!("\x1b[{};{}~", code, modifiers.parameter()).into_bytes()
    } else {
        format!("\x1b[{}~", code).into_bytes()
    }
}

fn function_key(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let modifier = modifiers.parameter();
    let code = match number {
        // The first four function keys use the SS3 form when unmodified.
        1..=4 => {
            let final_byte = b'P' + (number - 1);
            return Some(if modifier > 1 {
                let mut bytes = format!("\x1b[1;{}", modifier).into_bytes();
                bytes.push(final_byte);
                bytes
            } else {
                vec![0x1b, b'O', final_byte]
            });
        }
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(tilde_key(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(key: Key, modifiers: KeyModifiers) -> Vec<u8> {
        encode_key(key, modifiers, TermMode::empty()).expect("the key must encode")
    }

    #[test]
    fn control_letters_send_the_c0_code() {
        assert_eq!(
            encode(Key::Character('c'), KeyModifiers::control()),
            b"\x03"
        );
        assert_eq!(
            encode(Key::Character('C'), KeyModifiers::control()),
            b"\x03"
        );
        assert_eq!(
            encode(Key::Character('a'), KeyModifiers::control()),
            b"\x01"
        );
        assert_eq!(
            encode(Key::Character('z'), KeyModifiers::control()),
            b"\x1a"
        );
        // Ctrl+Space and Ctrl+@ are NUL, which many shells use for completion.
        assert_eq!(encode(Key::Space, KeyModifiers::control()), b"\x00");
        assert_eq!(
            encode(Key::Character('@'), KeyModifiers::control()),
            b"\x00"
        );
    }

    #[test]
    fn control_punctuation_follows_the_xterm_table() {
        assert_eq!(
            encode(Key::Character('['), KeyModifiers::control()),
            b"\x1b"
        );
        assert_eq!(
            encode(Key::Character('\\'), KeyModifiers::control()),
            b"\x1c"
        );
        assert_eq!(
            encode(Key::Character(']'), KeyModifiers::control()),
            b"\x1d"
        );
        assert_eq!(
            encode(Key::Character('^'), KeyModifiers::control()),
            b"\x1e"
        );
        assert_eq!(
            encode(Key::Character('_'), KeyModifiers::control()),
            b"\x1f"
        );
        assert_eq!(
            encode(Key::Character('?'), KeyModifiers::control()),
            b"\x7f"
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(encode(Key::Character('b'), alt), b"\x1bb");
        assert_eq!(encode(Key::Enter, alt), b"\x1b\r");
        assert_eq!(encode(Key::Backspace, alt), b"\x1b\x7f");
    }

    #[test]
    fn a_plain_character_is_left_to_the_text_input_path() {
        assert_eq!(
            encode_key(Key::Character('a'), KeyModifiers::NONE, TermMode::empty()),
            None
        );
        assert_eq!(
            encode_key(Key::Character('中'), KeyModifiers::NONE, TermMode::empty()),
            None
        );
    }

    #[test]
    fn the_platform_key_is_not_sent_to_the_shell() {
        let command = KeyModifiers {
            platform: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(
            encode_key(Key::Character('k'), command, TermMode::empty()),
            None
        );
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        assert_eq!(encode(Key::Up, KeyModifiers::NONE), b"\x1b[A");
        let application = TermMode::APP_CURSOR;
        assert_eq!(
            encode_key(Key::Up, KeyModifiers::NONE, application).expect("encodes"),
            b"\x1bOA"
        );
        // A modified arrow is always the CSI form with a modifier parameter.
        assert_eq!(
            encode_key(Key::Up, KeyModifiers::shift(), TermMode::APP_CURSOR).expect("encodes"),
            b"\x1b[1;2A"
        );
        assert_eq!(
            encode(Key::Left, KeyModifiers::control_shift()),
            b"\x1b[1;6D"
        );
        assert_eq!(encode(Key::Down, KeyModifiers::NONE), b"\x1b[B");
        assert_eq!(encode(Key::Right, KeyModifiers::NONE), b"\x1b[C");
    }

    #[test]
    fn navigation_keys_use_their_documented_sequences() {
        assert_eq!(encode(Key::Home, KeyModifiers::NONE), b"\x1b[H");
        assert_eq!(encode(Key::End, KeyModifiers::NONE), b"\x1b[F");
        assert_eq!(encode(Key::Insert, KeyModifiers::NONE), b"\x1b[2~");
        assert_eq!(encode(Key::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(encode(Key::PageUp, KeyModifiers::NONE), b"\x1b[5~");
        assert_eq!(encode(Key::PageDown, KeyModifiers::NONE), b"\x1b[6~");
        assert_eq!(encode(Key::PageUp, KeyModifiers::control()), b"\x1b[5;5~");
        assert_eq!(
            encode_key(Key::Home, KeyModifiers::NONE, TermMode::APP_CURSOR).expect("encodes"),
            b"\x1bOH"
        );
    }

    #[test]
    fn function_keys_cover_the_twelve_keys() {
        assert_eq!(encode(Key::Function(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(encode(Key::Function(4), KeyModifiers::NONE), b"\x1bOS");
        assert_eq!(encode(Key::Function(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(encode(Key::Function(10), KeyModifiers::NONE), b"\x1b[21~");
        assert_eq!(encode(Key::Function(12), KeyModifiers::NONE), b"\x1b[24~");
        assert_eq!(
            encode(Key::Function(1), KeyModifiers::shift()),
            b"\x1b[1;2P"
        );
        assert_eq!(
            encode_key(Key::Function(13), KeyModifiers::NONE, TermMode::empty()),
            None
        );
    }

    #[test]
    fn tab_and_return_carry_what_a_shell_expects() {
        assert_eq!(encode(Key::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(encode(Key::Tab, KeyModifiers::NONE), b"\t");
        assert_eq!(encode(Key::Tab, KeyModifiers::shift()), b"\x1b[Z");
        assert_eq!(encode(Key::Escape, KeyModifiers::NONE), b"\x1b");
        assert_eq!(encode(Key::Backspace, KeyModifiers::NONE), b"\x7f");
        assert_eq!(encode(Key::Backspace, KeyModifiers::control()), b"\x08");
    }

    #[test]
    fn typed_text_normalises_line_breaks() {
        assert_eq!(encode_text("ab"), b"ab");
        assert_eq!(encode_text("a\nb"), b"a\rb");
        assert_eq!(encode_text("a\r\nb"), b"a\rb");
        assert_eq!(encode_text("中文"), "中文".as_bytes());
    }

    #[test]
    fn paste_is_wrapped_only_when_the_program_asked_for_it() {
        assert_eq!(encode_paste("ls\n", TermMode::empty()), b"ls\r");
        assert_eq!(
            encode_paste("ls\n", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ls\r\x1b[201~"
        );
    }

    #[test]
    fn keystroke_names_map_to_the_same_keys() {
        assert_eq!(Key::from_name("enter"), Some(Key::Enter));
        assert_eq!(Key::from_name("pageup"), Some(Key::PageUp));
        assert_eq!(Key::from_name("page_up"), Some(Key::PageUp));
        assert_eq!(Key::from_name("f5"), Some(Key::Function(5)));
        assert_eq!(Key::from_name("f13"), None);
        assert_eq!(Key::from_name("a"), Some(Key::Character('a')));
        assert_eq!(Key::from_name("中"), Some(Key::Character('中')));
        assert_eq!(Key::from_name("shift"), None);
        assert_eq!(Key::from_name(""), None);
    }

    #[test]
    fn the_modifier_parameter_matches_the_xterm_encoding() {
        assert_eq!(KeyModifiers::NONE.parameter(), 1);
        assert_eq!(KeyModifiers::shift().parameter(), 2);
        assert_eq!(KeyModifiers::control().parameter(), 5);
        assert_eq!(
            KeyModifiers {
                alt: true,
                ..KeyModifiers::NONE
            }
            .parameter(),
            3
        );
        assert_eq!(KeyModifiers::control_shift().parameter(), 6);
    }
}
