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

pub(super) struct Clipboard {
    enabled: bool,
    emit: super::Emit,
    /// What the last local copy offered, keyed by remote format id.
    offered: Vec<(u16, Vec<u8>)>,
    /// The format id this client asked for and has not received yet.
    pending: Option<u16>,
    /// Whether the server answered the capability exchange.
    ready: bool,
    /// When the last announcement went out, so a channel that was not ready yet
    /// is retried instead of staying silent.
    announced: Option<std::time::Instant>,
}

impl Clipboard {
    pub(super) fn new(enabled: bool, emit: super::Emit) -> Clipboard {
        Clipboard {
            enabled,
            emit,
            offered: Vec::new(),
            pending: None,
            ready: false,
            announced: None,
        }
    }

    /// Called once the static channel exists: capabilities and monitor ready are
    /// the two messages a server waits for before sending its own format list.
    pub(super) fn announce(&mut self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(id) = ch.clipboard else {
            return Ok(());
        };
        // CB_CLIP_CAPS: one general capability set, version 2, no long names.
        let caps = [1u16, 0, 1, 0, 0, 0, 0, 0];
        self.send(ch, out, id, CB_CLIP_CAPS, 0, &words(&caps))?;
        self.send(ch, out, id, CB_MONITOR_READY, CB_RESPONSE_OK, &[])?;
        self.announced = Some(std::time::Instant::now());
        Ok(())
    }

    /// Re-announces while the server has not answered: the channel exists before
    /// the server's clipboard service is attached, and a first announcement can
    /// fall into that gap.
    pub(super) fn poll(&mut self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        if !self.enabled || self.ready {
            return Ok(());
        }
        if let Some(at) = self.announced {
            if at.elapsed() < std::time::Duration::from_secs(2) {
                return Ok(());
            }
        }
        self.announce(ch, out)
    }

    /// Offers the local clipboard. An image is only offered as a DIB: the PNG form
    /// is a local convenience the wire format does not carry.
    pub(super) fn offer(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        entries: &[ClipboardEntry],
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
        let stored = formats(entries)?;
        let list: Vec<u16> = stored.iter().map(|(id, _)| *id).collect();
        self.offered = stored;
        self.send(ch, out, id, CB_FORMAT_LIST, 0, &words(&list))
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
                let chosen = preferred(data)?;
                if let Some(format) = chosen {
                    self.pending = Some(format);
                    self.send(
                        ch,
                        out,
                        id,
                        CB_FORMAT_DATA_REQUEST,
                        0,
                        &u32::from(format).to_le_bytes(),
                    )?;
                }
                self.send(ch, out, id, CB_FORMAT_LIST_RESPONSE, CB_RESPONSE_OK, &[])?;
            }
            CB_FORMAT_DATA_REQUEST => {
                let mut request = Packet(data);
                let format = u16::try_from(request.le32()?).unwrap_or(0);
                let found = self
                    .offered
                    .iter()
                    .find(|(id, _)| *id == format)
                    .map(|(_, bytes)| bytes.clone());
                match found {
                    Some(bytes) => {
                        self.send(ch, out, id, CB_FORMAT_DATA_RESPONSE, CB_RESPONSE_OK, &bytes)?
                    }
                    None => {
                        self.send(ch, out, id, CB_FORMAT_DATA_RESPONSE, CB_RESPONSE_FAIL, &[])?
                    }
                }
            }
            CB_FORMAT_DATA_RESPONSE => {
                if flags & CB_RESPONSE_OK == 0 {
                    self.pending = None;
                    return Ok(());
                }
                let mut cut_short = false;
                let entry = match self.pending.take() {
                    Some(CF_UNICODETEXT) => {
                        let text = from_utf16(data)?;
                        let cut = cut_text(&text, MAX_CLIPBOARD_TEXT_BYTES);
                        cut_short = cut.len() < text.len();
                        ClipboardEntry::text(cut)
                    }
                    Some(CF_DIB) | Some(CF_DIBV5) => {
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
            CB_CLIP_CAPS => {
                // The server's capabilities close the exchange; answering here
                // also covers a server that only speaks after its own start-up.
                self.ready = true;
                self.announce(ch, out)?;
            }
            CB_MONITOR_READY | CB_FORMAT_LIST_RESPONSE => {}
            _ => {}
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
        let mut message = Vec::with_capacity(data.len() + 8);
        message.extend_from_slice(&kind.to_le_bytes());
        message.extend_from_slice(&flags.to_le_bytes());
        message.extend_from_slice(&(data.len() as u32).to_le_bytes());
        message.extend_from_slice(data);
        ch.send(out, id, &message)
    }
}

/// The formats one local copy offers and the bytes each carries, in the order the
/// format list announces them: text as CF_UNICODETEXT, an image as CF_DIB.
fn formats(entries: &[ClipboardEntry]) -> io::Result<Vec<(u16, Vec<u8>)>> {
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
                stored.push((CF_DIB, entry.bytes.clone()));
            }
            ClipboardKind::Png => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "an RDP clipboard carries images as a DIB",
                ));
            }
        }
    }
    Ok(stored)
}

/// Test hook: the formats a local copy offers, through the same conversion the
/// channel uses. Errors are reported as an empty list.
pub(crate) fn probe_offer(entries: &[ClipboardEntry]) -> Vec<(u16, Vec<u8>)> {
    formats(entries).unwrap_or_default()
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
        user: 1005,
        global: 1003,
        clipboard: Some(1004),
        drive: None,
    };
    let mut out = Vec::new();
    if let Ok(stored) = formats(offered) {
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
    let mut p = Packet(data);
    let mut seen = Vec::new();
    while !p.0.is_empty() {
        let raw = p.le16()?;
        let format = if raw & 0x8000 != 0 {
            // A registered format name follows the 4-byte id; skip it.
            p.take(2)?;
            let length = usize::from(p.le16()?);
            p.take(length.saturating_mul(2))?;
            raw
        } else {
            raw
        };
        seen.push(format);
    }
    Ok([CF_UNICODETEXT, CF_DIBV5, CF_DIB]
        .into_iter()
        .find(|wanted| seen.contains(wanted)))
}

/// UTF-16LE, CRLF line ends and a terminating NUL, as CF_UNICODETEXT defines it.
fn to_utf16(text: &str) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut units: Vec<u16> = normalised.encode_utf16().collect();
    units.push(0);
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// Reads up to the first NUL; an odd trailing byte is ignored.
fn from_utf16(data: &[u8]) -> io::Result<String> {
    let mut units = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks_exact(2) {
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
