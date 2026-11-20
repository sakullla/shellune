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
    bound(latin1_to_utf8(text))
}

fn bound(utf8: Vec<u8>) -> RemoteText {
    if utf8.len() <= MAX_CLIPBOARD_TEXT_BYTES {
        return RemoteText { utf8, cut: false };
    }
    let len = cut_clipboard_text(&utf8, MAX_CLIPBOARD_TEXT_BYTES);
    RemoteText {
        utf8: utf8[..len].to_vec(),
        cut: true,
    }
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
    Caps { text: bool },
    /// The server provided a text.
    Text(RemoteText),
    /// A message this client does not act on (a request or a peek).
    Other,
}

/// Parses the payload of an extended clipboard cut text: the flags word followed
/// by what the action carries.
pub(crate) fn parse_extended(payload: &[u8]) -> Option<Extended> {
    let flags = u32::from_be_bytes(payload.get(..4)?.try_into().ok()?);
    if flags & EXT_CAPS != 0 {
        return Some(Extended::Caps {
            text: flags & EXT_TEXT != 0,
        });
    }
    if flags & EXT_PROVIDE == 0 {
        return Some(Extended::Other);
    }
    // The text is a zlib stream. Servers that flush it with Z_SYNC_FLUSH and no
    // final block (TigerVNC does) never end it properly, so a decoder error after
    // the bytes arrived is the end of the stream, not a failure.
    let mut plain = Vec::new();
    let mut decoder = ZlibDecoder::new(&payload[4..]);
    let mut chunk = [0u8; 4096];
    loop {
        match decoder.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => plain.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }
    let declared = u32::from_be_bytes(plain.get(..4)?.try_into().ok()?) as usize;
    let mut body = plain.get(4..4 + declared)?.to_vec();
    while body.last() == Some(&0) {
        body.pop();
    }
    let utf8 = String::from_utf8_lossy(&body).into_owned().into_bytes();
    Some(Extended::Text(bound(utf8)))
}

/// The message a client sends to say it copied `text`: an empty Notify followed by
/// a compressed Provide, which is the form every extended clipboard peer reads.
pub(crate) fn provide(text: &str) -> (Vec<u8>, Vec<u8>) {
    let notify = (EXT_NOTIFY | EXT_TEXT).to_be_bytes().to_vec();
    let mut plain = ((text.len() + 1) as u32).to_be_bytes().to_vec();
    plain.extend_from_slice(text.as_bytes());
    plain.push(0);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // Writing into a Vec does not fail; a compression failure would be a bug.
    let _ = encoder.write_all(&plain);
    let compressed = encoder.finish().unwrap_or_default();
    let mut payload = (EXT_PROVIDE | EXT_TEXT).to_be_bytes().to_vec();
    payload.extend_from_slice(&compressed);
    (notify, payload)
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
        let (notify, provide) = provide("远端 ✓ text");
        assert_eq!(
            u32::from_be_bytes(notify[..4].try_into().unwrap()),
            EXT_NOTIFY | EXT_TEXT
        );
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
        let Some(Extended::Caps { text }) = parse_extended(&caps) else {
            panic!("capabilities");
        };
        assert!(text);
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
