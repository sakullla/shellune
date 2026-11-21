//! The two clipboard forms RFB has: classic cut text, which is Latin-1 by
//! definition, and the ExtendedClipboard pseudo-encoding, which carries UTF-8 in
//! a zlib stream.
//!
//! Images are not forwarded in either direction: RFB has no image format.
use crate::control::Emit;
use crate::{
    ClipboardEntry, ClipboardKind, Event, MAX_CLIPBOARD_TEXT_BYTES, Notice, cut_clipboard_text,
};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use std::io::{Read, Write};

/// The pseudo-encoding a client lists to ask for the extended clipboard.
pub(crate) const EXTENDED_CLIPBOARD: i32 = 0xC0A1_E5CEu32 as i32;

pub(crate) const EXT_TEXT: u32 = 1;
pub(crate) const EXT_CAPS: u32 = 1 << 24;
pub(crate) const EXT_REQUEST: u32 = 1 << 25;
pub(crate) const EXT_PEEK: u32 = 1 << 26;
pub(crate) const EXT_NOTIFY: u32 = 1 << 27;
pub(crate) const EXT_PROVIDE: u32 = 1 << 28;

/// ISO 8859-1 as UTF-8. Every byte is one code point, so this never fails.
pub(crate) fn latin1_to_utf8(bytes: &[u8]) -> Vec<u8> {
    let mut text = String::with_capacity(bytes.len());
    for byte in bytes {
        text.push(char::from(*byte));
    }
    text.into_bytes()
}

/// UTF-8 as ISO 8859-1, or None when a character does not fit. A server that
/// speaks only classic cut text must not receive mojibake.
pub(crate) fn utf8_to_latin1(text: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(text).ok()?;
    let mut out = Vec::with_capacity(text.len());
    for character in text.chars() {
        let value = u32::from(character);
        if value > 0xFF {
            return None;
        }
        out.push(value as u8);
    }
    Some(out)
}

/// One remote text, already cut to the crate's bound when it was too long.
pub(crate) struct RemoteText {
    pub(crate) utf8: Vec<u8>,
    pub(crate) cut: bool,
}

/// Decodes classic cut text and cuts it at the last whole character within the
/// bound, reporting that it did.
pub(crate) fn classic(text: &[u8]) -> RemoteText {
    let mut decoded = bound(latin1_to_utf8(
        &text[..text.len().min(MAX_CLIPBOARD_TEXT_BYTES)],
    ));
    decoded.cut |= text.len() > MAX_CLIPBOARD_TEXT_BYTES;
    decoded
}

fn bound(mut utf8: Vec<u8>) -> RemoteText {
    if utf8.len() <= MAX_CLIPBOARD_TEXT_BYTES {
        return RemoteText { utf8, cut: false };
    }
    let len = cut_clipboard_text(&utf8, MAX_CLIPBOARD_TEXT_BYTES);
    utf8.truncate(len);
    RemoteText { utf8, cut: true }
}

/// Hands one remote text to the consumer, with the notice a cut earns.
pub(crate) fn deliver(emit: &Emit, text: RemoteText) {
    if text.utf8.is_empty() {
        return;
    }
    emit(Event::Clipboard {
        entries: vec![ClipboardEntry {
            kind: ClipboardKind::Text,
            bytes: text.utf8,
        }],
    });
    if text.cut {
        emit(Event::Notice {
            notice: Notice::ClipboardTooLarge(ClipboardKind::Text),
        });
    }
}

/// What one extended clipboard message held.
pub(crate) enum Extended {
    /// The server announced its capabilities; the text bit says it can carry text.
    Caps {
        flags: u32,
        text_limit: u32,
    },
    Notify(bool),
    Request,
    Peek,
    /// The server provided a text.
    Text(RemoteText),
    /// A message with an unsupported action or format.
    Other,
}

/// Parses the payload of an extended clipboard cut text: the flags word followed
/// by what the action carries.
pub(crate) fn parse_extended(payload: &[u8]) -> Option<Extended> {
    let flags = u32::from_be_bytes(payload.get(..4)?.try_into().ok()?);
    if flags & EXT_CAPS != 0 {
        let text_limit = if flags & EXT_TEXT != 0 {
            u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?)
        } else {
            0
        };
        return Some(Extended::Caps { flags, text_limit });
    }
    if flags & EXT_NOTIFY != 0 {
        return Some(Extended::Notify(flags & EXT_TEXT != 0));
    }
    if flags & EXT_REQUEST != 0 && flags & EXT_TEXT != 0 {
        return Some(Extended::Request);
    }
    if flags & EXT_PEEK != 0 {
        return Some(Extended::Peek);
    }
    if flags & EXT_PROVIDE == 0 {
        return Some(Extended::Other);
    }
    if flags & EXT_TEXT == 0 {
        return Some(Extended::Other);
    }
    // The first format is text. Read only its bounded prefix, including room
    // for the terminating NUL. A sync-flushed Provide need not end the zlib stream.
    let mut decoder = ZlibDecoder::new(&payload[4..]);
    let mut length = [0; 4];
    decoder.read_exact(&mut length).ok()?;
    let declared = u32::from_be_bytes(length) as usize;
    let mut bytes = vec![0; declared.min(MAX_CLIPBOARD_TEXT_BYTES + 1)];
    decoder.read_exact(&mut bytes).ok()?;
    let truncated = declared > bytes.len();
    if truncated
        && let Err(error) = std::str::from_utf8(&bytes)
        && error.error_len().is_none()
    {
        bytes.truncate(error.valid_up_to());
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    let mut text = bound(match std::str::from_utf8(&bytes) {
        Ok(_) => bytes,
        Err(_) => String::from_utf8_lossy(&bytes).into_owned().into_bytes(),
    });
    text.cut |= truncated;
    Some(Extended::Text(text))
}

/// Encode the text directly into the Provide PDU, including its terminating NUL.
pub(crate) fn provide(text: &[u8]) -> std::io::Result<Vec<u8>> {
    let payload = (EXT_PROVIDE | EXT_TEXT).to_be_bytes().to_vec();
    let mut encoder = ZlibEncoder::new(payload, Compression::default());
    encoder.write_all(&((text.len() + 1) as u32).to_be_bytes())?;
    encoder.write_all(text)?;
    encoder.write_all(&[0])?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_and_utf8_round_trip_exactly() {
        assert_eq!(latin1_to_utf8(b"caf\xe9 cr\xe8me"), "café crème".as_bytes());
        assert_eq!(
            utf8_to_latin1("café crème".as_bytes()).as_deref(),
            Some(&b"caf\xe9 cr\xe8me"[..])
        );
        assert_eq!(utf8_to_latin1("日本".as_bytes()), None);
        assert_eq!(utf8_to_latin1("€".as_bytes()), None);
        assert_eq!(
            utf8_to_latin1("ÿ".as_bytes()).as_deref(),
            Some(&[0xFFu8][..])
        );
    }

    #[test]
    fn a_provided_text_is_read_back_through_its_compression() {
        let provide = provide("远端 ✓ text".as_bytes()).unwrap();
        let Some(Extended::Text(text)) = parse_extended(&provide) else {
            panic!("a provided text");
        };
        assert_eq!(text.utf8, "远端 ✓ text".as_bytes());
        assert!(!text.cut);
    }

    #[test]
    fn capabilities_say_whether_text_is_carried() {
        let mut caps = (EXT_CAPS | EXT_TEXT).to_be_bytes().to_vec();
        caps.extend_from_slice(&(1u32 << 20).to_be_bytes());
        let Some(Extended::Caps { flags, text_limit }) = parse_extended(&caps) else {
            panic!("capabilities");
        };
        assert_ne!(flags & EXT_TEXT, 0);
        assert_eq!(text_limit, 1 << 20);
    }

    #[test]
    fn oversized_extended_text_is_cut_at_a_character_boundary() {
        let text = format!("{}界界", "x".repeat(MAX_CLIPBOARD_TEXT_BYTES - 1));
        let packet = provide(text.as_bytes()).unwrap();
        let Some(Extended::Text(decoded)) = parse_extended(&packet) else {
            panic!("text")
        };
        assert!(decoded.cut);
        assert_eq!(
            decoded.utf8,
            text.as_bytes()[..MAX_CLIPBOARD_TEXT_BYTES - 1]
        );
    }

    #[test]
    fn a_provided_text_flushed_without_a_trailer_is_still_read() {
        // TigerVNC's Provide for the text "tiger3-clip": the flags, then a zlib
        // stream that ends in a sync flush instead of a final block.
        let payload = [
            0x10u8, 0x00, 0x00, 0x01, 0x78, 0x9c, 0x62, 0x60, 0x60, 0xe0, 0x29, 0xc9, 0x4c, 0x4f,
            0x2d, 0x32, 0xd6, 0x4d, 0xce, 0xc9, 0x2c, 0x60, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff,
        ];
        let Some(Extended::Text(text)) = parse_extended(&payload) else {
            panic!("a provided text");
        };
        assert_eq!(text.utf8, b"tiger3-clip");
    }
}
