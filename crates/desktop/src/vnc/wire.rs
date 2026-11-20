//! RFB messages as a byte stream: one parser that consumes only whole messages,
//! so a read that times out in the middle leaves the bytes in place.
use crate::MAX_FRAME_BYTES;
use std::io::{self, Read};

/// Reads exactly one buffer, so every protocol step either has its bytes or fails
/// with the error the transport reported.
pub(crate) fn read_exact(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<()> {
    reader.read_exact(buffer)
}

/// One rectangle of a framebuffer update, as far as this client decodes it.
pub(crate) enum Rect {
    Raw {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixels: Vec<u8>,
    },
    Copy {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        source_x: u16,
        source_y: u16,
    },
    Resize {
        width: u16,
        height: u16,
    },
}

/// What the server sent.
pub(crate) enum Message {
    Update(Vec<Rect>),
    Bell,
    /// Classic cut text: ISO 8859-1 bytes.
    CutText(Vec<u8>),
    /// Extended clipboard payload: the flags word and what the action carries.
    ExtendedCutText(Vec<u8>),
    /// A valid message this client has no use for (a colour map under true colour).
    Ignored,
}

/// Encodings this client requests and decodes. Anything else is a protocol error
/// here rather than a silently wrong picture.
const RAW: i32 = 0;
const COPY_RECT: i32 = 1;
const DESKTOP_SIZE: i32 = -223;
const LAST_RECT: i32 = -224;

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.data.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

enum Parsed {
    /// Not enough bytes yet: nothing was consumed.
    Incomplete,
    Complete(Message, usize),
}

/// Buffers bytes until one whole server message is available.
#[derive(Default)]
pub(crate) struct Incoming {
    pending: Vec<u8>,
}

impl Incoming {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// The next whole message, `Ok(None)` when more bytes are needed, or the reason
    /// the stream cannot be read as RFB.
    pub(crate) fn take(&mut self) -> Result<Option<Message>, String> {
        match parse(&self.pending)? {
            Parsed::Incomplete => Ok(None),
            Parsed::Complete(message, used) => {
                self.pending.drain(..used);
                Ok(Some(message))
            }
        }
    }
}

fn parse(data: &[u8]) -> Result<Parsed, String> {
    let Some(&kind) = data.first() else {
        return Ok(Parsed::Incomplete);
    };
    let mut cursor = Cursor::new(data);
    cursor.take(1);
    match kind {
        0 => framebuffer_update(&mut cursor),
        1 => colour_map(&mut cursor),
        2 => Ok(Parsed::Complete(Message::Bell, 1)),
        3 => cut_text(&mut cursor),
        other => Err(format!(
            "the server sent RFB message type {other}, which this client does not read"
        )),
    }
}

fn framebuffer_update(cursor: &mut Cursor<'_>) -> Result<Parsed, String> {
    // One padding byte, then the number of rectangles.
    if cursor.take(1).is_none() {
        return Ok(Parsed::Incomplete);
    }
    let Some(count) = cursor.u16() else {
        return Ok(Parsed::Incomplete);
    };
    let mut rects = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let (Some(x), Some(y), Some(width), Some(height), Some(encoding)) = (
            cursor.u16(),
            cursor.u16(),
            cursor.u16(),
            cursor.u16(),
            cursor.i32(),
        ) else {
            return Ok(Parsed::Incomplete);
        };
        match encoding {
            RAW => {
                let size = usize::from(width) * usize::from(height) * 4;
                if size > MAX_FRAME_BYTES {
                    return Err(format!(
                        "an update of {width}x{height} exceeds the {MAX_FRAME_BYTES} byte bound"
                    ));
                }
                let Some(pixels) = cursor.take(size) else {
                    return Ok(Parsed::Incomplete);
                };
                rects.push(Rect::Raw {
                    x,
                    y,
                    width,
                    height,
                    pixels: pixels.to_vec(),
                });
            }
            COPY_RECT => {
                let (Some(source_x), Some(source_y)) = (cursor.u16(), cursor.u16()) else {
                    return Ok(Parsed::Incomplete);
                };
                rects.push(Rect::Copy {
                    x,
                    y,
                    width,
                    height,
                    source_x,
                    source_y,
                });
            }
            DESKTOP_SIZE => rects.push(Rect::Resize { width, height }),
            LAST_RECT => break,
            other => {
                return Err(format!(
                    "the server used encoding {other}, which this client did not request"
                ));
            }
        }
    }
    Ok(Parsed::Complete(Message::Update(rects), cursor.at))
}

fn colour_map(cursor: &mut Cursor<'_>) -> Result<Parsed, String> {
    // A true-colour session never uses the map, but a server may still send it.
    if cursor.take(1).is_none() {
        return Ok(Parsed::Incomplete);
    }
    let Some(count) = cursor.u16() else {
        return Ok(Parsed::Incomplete);
    };
    if cursor.take(usize::from(count) * 6).is_none() {
        return Ok(Parsed::Incomplete);
    }
    Ok(Parsed::Complete(Message::Ignored, cursor.at))
}

fn cut_text(cursor: &mut Cursor<'_>) -> Result<Parsed, String> {
    if cursor.take(3).is_none() {
        return Ok(Parsed::Incomplete);
    }
    let Some(length) = cursor.i32() else {
        return Ok(Parsed::Incomplete);
    };
    // A negative length is the extended clipboard form; its bytes include the
    // four-byte flags word that the length covers.
    let size = length.unsigned_abs() as usize;
    if size > 1 << 24 {
        return Err("a cut text message exceeds the 16 MiB bound".to_owned());
    }
    let Some(payload) = cursor.take(size) else {
        return Ok(Parsed::Incomplete);
    };
    Ok(Parsed::Complete(
        if length < 0 {
            Message::ExtendedCutText(payload.to_vec())
        } else {
            Message::CutText(payload.to_vec())
        },
        cursor.at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_split_across_reads_is_joined_again() {
        let mut update = vec![0u8, 0];
        update.extend_from_slice(&1u16.to_be_bytes());
        update.extend_from_slice(&[0, 0, 0, 0, 0, 2, 0, 1]);
        update.extend_from_slice(&0i32.to_be_bytes());
        update.extend_from_slice(&[9, 8, 7, 6, 5, 4, 3, 2]);
        let mut incoming = Incoming::default();
        incoming.push(&update[..7]);
        assert!(incoming.take().expect("parsed").is_none());
        incoming.push(&update[7..]);
        let Some(Message::Update(rects)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        assert_eq!(rects.len(), 1);
        let Rect::Raw {
            width,
            height,
            pixels,
            ..
        } = &rects[0]
        else {
            panic!("raw");
        };
        assert_eq!((*width, *height), (2, 1));
        assert_eq!(pixels, &[9, 8, 7, 6, 5, 4, 3, 2]);
        assert!(matches!(incoming.take().expect("parsed"), None));
    }

    #[test]
    fn an_unrequested_encoding_is_reported_instead_of_drawn() {
        let mut update = vec![0u8, 0];
        update.extend_from_slice(&1u16.to_be_bytes());
        update.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
        update.extend_from_slice(&16i32.to_be_bytes());
        let mut incoming = Incoming::default();
        incoming.push(&update);
        assert!(incoming.take().is_err());
    }

    #[test]
    fn last_rect_ends_the_update_early() {
        let mut update = vec![0u8, 0];
        update.extend_from_slice(&2u16.to_be_bytes());
        update.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
        update.extend_from_slice(&(-224i32).to_be_bytes());
        let mut incoming = Incoming::default();
        incoming.push(&update);
        let Some(Message::Update(rects)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        assert!(rects.is_empty());
    }
}
