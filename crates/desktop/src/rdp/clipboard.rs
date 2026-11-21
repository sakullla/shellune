//! The CLIPRDR channel: format lists, data requests and the text conversion.
//!
//! Only formats this client can actually carry are advertised. Text crosses the
//! wire as CF_UNICODETEXT (UTF-16LE, CRLF line ends, NUL terminated); a server that
//! lists an image format first is still asked for text when text is all it has.
use super::mcs::Channels;
use super::wire::{Packet, invalid};
use crate::{
    ClipboardEntry, ClipboardKind, Event, MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_TEXT_BYTES,
};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

const CB_MONITOR_READY: u16 = 0x0001;
const CB_FORMAT_LIST: u16 = 0x0002;
const CB_FORMAT_LIST_RESPONSE: u16 = 0x0003;
const CB_FORMAT_DATA_REQUEST: u16 = 0x0004;
const CB_FORMAT_DATA_RESPONSE: u16 = 0x0005;
const CB_CLIP_CAPS: u16 = 0x0007;

const CB_RESPONSE_OK: u16 = 0x0001;
const CB_RESPONSE_FAIL: u16 = 0x0002;

const CF_UNICODETEXT: u16 = 13;
const CF_DIB: u16 = 8;
const CF_DIBV5: u16 = 17;

#[derive(Default)]
enum RemoteRequest {
    #[default]
    Idle,
    Waiting(u16),
    // CLIPRDR responses do not identify their format. Drain the previous
    // request before asking for the most recently advertised clipboard.
    Superseded(Option<u16>),
}

pub(super) struct Clipboard {
    enabled: bool,
    emit: super::Emit,
    /// What the last local copy offered, keyed by remote format id.
    offered: Vec<(u16, Vec<u8>)>,
    /// The format id this client asked for and has not received yet.
    pending: RemoteRequest,
    /// The server's Monitor Ready enables local format announcements.
    ready: bool,
    incoming: Vec<u8>,
    expected: Option<usize>,
    /// Whether the server has refused this client's formats since the last time it
    /// offered any of its own.
    refused: bool,
}

impl Clipboard {
    pub(super) fn new(enabled: bool, emit: super::Emit) -> Clipboard {
        Clipboard {
            enabled,
            emit,
            offered: Vec::new(),
            pending: RemoteRequest::Idle,
            ready: false,
            incoming: Vec::new(),
            expected: None,
            refused: false,
        }
    }

    fn send_formats(&self, ch: &Channels, out: &mut impl Write, id: u16) -> io::Result<()> {
        // We negotiate short names. A format ID is four bytes followed by a
        // 32-byte name, including for standard formats whose name is empty.
        let mut list = Vec::with_capacity(self.offered.len() * 36);
        for (format, _) in &self.offered {
            list.extend_from_slice(&u32::from(*format).to_le_bytes());
            list.extend_from_slice(&[0; 32]);
        }
        self.send(ch, out, id, CB_FORMAT_LIST, 0, &list)
    }

    pub(super) fn receive(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        body: &[u8],
    ) -> io::Result<()> {
        match self.chunk(ch, out, body) {
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                self.expected = None;
                self.pending = RemoteRequest::Idle;
                self.incoming.clear();
                if !self.refused {
                    self.refused = true;
                    (self.emit)(Event::Notice {
                        notice: crate::Notice::ClipboardRefused,
                    });
                }
                Ok(())
            }
            result => result,
        }
    }

    fn chunk(&mut self, ch: &Channels, out: &mut impl Write, body: &[u8]) -> io::Result<()> {
        let mut p = Packet(body);
        let total = p.le32()? as usize;
        let flags = p.le32()?;
        if flags & 1 != 0 {
            self.incoming.clear();
            self.expected = Some(total);
            if total > MAX_CLIPBOARD_IMAGE_BYTES + 8 {
                self.expected = None;
                if let RemoteRequest::Superseded(next) = std::mem::take(&mut self.pending)
                    && let Some(id) = ch.clipboard
                {
                    self.request(ch, out, id, next)?;
                }
                (self.emit)(Event::Notice {
                    notice: crate::Notice::ClipboardTooLarge(ClipboardKind::Dib),
                });
            }
        }
        if self.expected != Some(total) {
            return Ok(());
        }
        if self.incoming.len() + p.0.len() > total {
            return Err(invalid("invalid clipboard chunk length"));
        }
        self.incoming.extend_from_slice(p.0);
        if flags & 2 != 0 {
            self.expected = None;
            let message = std::mem::take(&mut self.incoming);
            if message.len() != total {
                return Err(invalid("incomplete clipboard message"));
            }
            self.on_data(ch, out, &message)?;
        }
        Ok(())
    }

    /// Offers the local clipboard. An image is only offered as a DIB: the PNG form
    /// is a local convenience the wire format does not carry.
    pub(super) fn offer(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        entries: Vec<ClipboardEntry>,
    ) -> io::Result<()> {
        let Some(id) = ch.clipboard else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the clipboard channel is not open",
            ));
        };
        if !self.enabled {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "clipboard sharing is off",
            ));
        }
        // Text over the bound is cut, and the caller is told it was.
        let cut = entries.iter().any(|entry| {
            entry.kind == ClipboardKind::Text && entry.bytes.len() > MAX_CLIPBOARD_TEXT_BYTES
        });
        let empty = entries.is_empty();
        let stored = formats(entries)?;
        if stored.is_empty() && !empty {
            return Ok(());
        }
        self.offered = stored;
        if self.ready {
            self.send_formats(ch, out, id)?;
        }
        if cut {
            (self.emit)(Event::Notice {
                notice: crate::Notice::ClipboardTooLarge(ClipboardKind::Text),
            });
        }
        Ok(())
    }

    pub(super) fn on_data(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        body: &[u8],
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(id) = ch.clipboard else {
            return Ok(());
        };
        let mut p = Packet(body);
        let kind = p.le16()?;
        let flags = p.le16()?;
        let length =
            usize::try_from(p.le32()?).map_err(|_| invalid("clipboard message too long"))?;
        let data = p.take(length)?;
        match kind {
            CB_FORMAT_LIST => {
                if self.refused {
                    self.refused = false;
                    (self.emit)(Event::Notice {
                        notice: crate::Notice::ClipboardReady,
                    });
                }
                let chosen = preferred(data)?;
                self.send(ch, out, id, CB_FORMAT_LIST_RESPONSE, CB_RESPONSE_OK, &[])?;
                if matches!(self.pending, RemoteRequest::Idle) {
                    self.request(ch, out, id, chosen)?;
                } else {
                    self.pending = RemoteRequest::Superseded(chosen);
                }
            }
            CB_FORMAT_DATA_REQUEST => {
                let mut request = Packet(data);
                let format = u16::try_from(request.le32()?).unwrap_or(0);
                let found = self
                    .offered
                    .iter()
                    .find(|(id, _)| *id == format)
                    .map(|(_, bytes)| bytes.as_slice());
                match found {
                    Some(bytes) => {
                        self.send(ch, out, id, CB_FORMAT_DATA_RESPONSE, CB_RESPONSE_OK, bytes)?
                    }
                    None => {
                        self.send(ch, out, id, CB_FORMAT_DATA_RESPONSE, CB_RESPONSE_FAIL, &[])?
                    }
                }
            }
            CB_FORMAT_DATA_RESPONSE => {
                let format = match std::mem::take(&mut self.pending) {
                    RemoteRequest::Superseded(next) => return self.request(ch, out, id, next),
                    RemoteRequest::Waiting(format) if flags & CB_RESPONSE_OK != 0 => format,
                    _ => return Ok(()),
                };
                let mut cut_short = false;
                let entry = match format {
                    CF_UNICODETEXT => {
                        let text = from_utf16(data)?;
                        let cut = cut_text(&text, MAX_CLIPBOARD_TEXT_BYTES);
                        cut_short = cut.len() < text.len();
                        ClipboardEntry::text(cut)
                    }
                    CF_DIB | CF_DIBV5 => {
                        if data.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                            (self.emit)(Event::Notice {
                                notice: crate::Notice::ClipboardTooLarge(ClipboardKind::Dib),
                            });
                            return Ok(());
                        }
                        ClipboardEntry {
                            kind: ClipboardKind::Dib,
                            bytes: data.to_vec(),
                        }
                    }
                    _ => return Ok(()),
                };
                (self.emit)(Event::Clipboard {
                    entries: vec![entry],
                });
                if cut_short {
                    (self.emit)(Event::Notice {
                        notice: crate::Notice::ClipboardTooLarge(ClipboardKind::Text),
                    });
                }
            }
            CB_CLIP_CAPS => {} // No optional capabilities are requested.
            CB_MONITOR_READY => {
                self.ready = true;
                // One general capability set, length 12, version 2, no flags.
                self.send(
                    ch,
                    out,
                    id,
                    CB_CLIP_CAPS,
                    0,
                    &words(&[1, 0, 1, 12, 2, 0, 0, 0]),
                )?;
                self.send_formats(ch, out, id)?;
            }
            CB_FORMAT_LIST_RESPONSE if flags & CB_RESPONSE_OK == 0 && !self.refused => {
                self.refused = true;
                (self.emit)(Event::Notice {
                    notice: crate::Notice::ClipboardRefused,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn request(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        id: u16,
        format: Option<u16>,
    ) -> io::Result<()> {
        if let Some(format) = format {
            self.send(
                ch,
                out,
                id,
                CB_FORMAT_DATA_REQUEST,
                0,
                &u32::from(format).to_le_bytes(),
            )?;
            self.pending = RemoteRequest::Waiting(format);
        }
        Ok(())
    }

    fn send(
        &self,
        ch: &Channels,
        out: &mut impl Write,
        id: u16,
        kind: u16,
        flags: u16,
        data: &[u8],
    ) -> io::Result<()> {
        let mut header = [0u8; 8];
        header[..2].copy_from_slice(&kind.to_le_bytes());
        header[2..4].copy_from_slice(&flags.to_le_bytes());
        header[4..].copy_from_slice(&(data.len() as u32).to_le_bytes());
        ch.send_channel(out, id, &header, data)
    }
}

/// The formats one local copy offers and the bytes each carries, in the order the
/// format list announces them: text as CF_UNICODETEXT, an image as CF_DIB.
fn formats(entries: Vec<ClipboardEntry>) -> io::Result<Vec<(u16, Vec<u8>)>> {
    let mut stored = Vec::new();
    for entry in entries {
        match entry.kind {
            ClipboardKind::Text => {
                let text = std::str::from_utf8(&entry.bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "clipboard text is not UTF-8")
                })?;
                let cut = cut_text(text, MAX_CLIPBOARD_TEXT_BYTES);
                stored.push((CF_UNICODETEXT, to_utf16(cut)));
            }
            ClipboardKind::Dib => {
                if entry.bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "clipboard image is too large",
                    ));
                }
                stored.push((CF_DIB, entry.bytes));
            }
            ClipboardKind::Png => continue,
        }
    }
    Ok(stored)
}

/// Test hook: the formats a local copy offers, through the same conversion the
/// channel uses. Errors are reported as an empty list.
pub(crate) fn probe_offer(entries: &[ClipboardEntry]) -> Vec<(u16, Vec<u8>)> {
    formats(entries.to_vec()).unwrap_or_default()
}

/// Test hook: runs the channel's offer path over `entries` and returns what it
/// sent and the events it reported.
pub(crate) fn probe_offer_events(entries: &[ClipboardEntry]) -> (Vec<u8>, Vec<Event>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let emit: super::Emit = Arc::new(move |event| {
        if let Ok(mut slot) = sink.lock() {
            slot.push(event);
        }
    });
    let mut clipboard = Clipboard::new(true, emit);
    let channels = Channels {
        dynamic: None,
        user: 1005,
        global: 1003,
        clipboard: Some(1004),
        drive: None,
    };
    let mut out = Vec::new();
    clipboard.ready = true;
    let _ = clipboard.offer(&channels, &mut out, entries.to_vec());
    let events = events.lock().map(|slot| slot.clone()).unwrap_or_default();
    (out, events)
}

/// Test hook: runs the channel's receive path over canned messages, after offering
/// `offered`. Returns the events it emitted and the bytes it replied with.
pub(crate) fn probe_receive(
    offered: &[ClipboardEntry],
    messages: &[(u16, u16, Vec<u8>)],
) -> (Vec<Event>, Vec<u8>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let emit: super::Emit = Arc::new(move |event| {
        if let Ok(mut slot) = sink.lock() {
            slot.push(event);
        }
    });
    let mut clipboard = Clipboard::new(true, emit);
    // The IDs a server would have assigned: the MCS user is 1001 + the attach
    // offset, so sending through them never underflows.
    let channels = Channels {
        dynamic: None,
        user: 1005,
        global: 1003,
        clipboard: Some(1004),
        drive: None,
    };
    let mut out = Vec::new();
    if let Ok(stored) = formats(offered.to_vec()) {
        clipboard.offered = stored;
    }
    for (kind, flags, data) in messages {
        let mut body = Vec::new();
        body.extend_from_slice(&kind.to_le_bytes());
        body.extend_from_slice(&flags.to_le_bytes());
        body.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
        body.extend_from_slice(data);
        let _ = clipboard.on_data(&channels, &mut out, &body);
    }
    let events = events.lock().map(|slot| slot.clone()).unwrap_or_default();
    (events, out)
}

fn words(values: &[u16]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The first format this client can carry, in its own preference order.
fn preferred(data: &[u8]) -> io::Result<Option<u16>> {
    let (formats, rest) = data.as_chunks::<36>();
    if !rest.is_empty() {
        return Err(invalid("invalid short clipboard format list"));
    }
    Ok([CF_UNICODETEXT, CF_DIBV5, CF_DIB]
        .into_iter()
        .find(|wanted| {
            formats
                .iter()
                .any(|entry| entry[..4] == u32::from(*wanted).to_le_bytes())
        }))
}

/// CF_UNICODETEXT bytes for `text`: the conversion a session offers with.
pub(crate) fn cf_unicode_text(text: &str) -> Vec<u8> {
    to_utf16(text)
}

/// The UTF-8 text of CF_UNICODETEXT `bytes`; `None` when they are not valid UTF-16.
pub(crate) fn text_from_cf_unicode(bytes: &[u8]) -> Option<Vec<u8>> {
    from_utf16(bytes).ok().map(String::into_bytes)
}

/// UTF-16LE, CRLF line ends and a terminating NUL, as CF_UNICODETEXT defines it.
fn to_utf16(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    let mut previous = 0;
    for unit in text.encode_utf16() {
        if unit == 10 && previous != 13 {
            bytes.extend_from_slice(&13u16.to_le_bytes());
        }
        bytes.extend_from_slice(&unit.to_le_bytes());
        previous = unit;
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// Reads up to the first NUL; an odd trailing byte is ignored.
fn from_utf16(data: &[u8]) -> io::Result<String> {
    let mut units = Vec::with_capacity(data.len() / 2);
    for pair in data.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16(&units).map_err(|_| invalid("clipboard text is not valid UTF-16"))
}

/// Cuts UTF-8 text at the last whole character inside `limit` bytes.
fn cut_text(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
