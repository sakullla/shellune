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
const HEXTILE: i32 = 5;
const DESKTOP_SIZE: i32 = -223;
const LAST_RECT: i32 = -224;

/// Hextile sub-encoding bits.
const HEXTILE_RAW: u8 = 1;
const HEXTILE_BACKGROUND: u8 = 2;
const HEXTILE_FOREGROUND: u8 = 4;
const HEXTILE_SUBRECTS: u8 = 8;
const HEXTILE_COLOURED: u8 = 16;

/// Four bytes per pixel: the format this client pins before any update is asked
/// for, so every encoding carries pixels in it.
const BYTES_PER_PIXEL: usize = 4;

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
            HEXTILE => {
                let Some(pixels) = hextile(cursor, width, height)? else {
                    return Ok(Parsed::Incomplete);
                };
                rects.push(Rect::Raw {
                    x,
                    y,
                    width,
                    height,
                    pixels,
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

/// Decodes one hextile rectangle into four-byte pixels.
///
/// Tiles are 16x16 (the last row and column are shorter). Each tile replaces its
/// area with the background it names or inherits, then paints its sub-rectangles.
/// A raw tile carries its own pixels and leaves the remembered colours alone.
fn hextile(cursor: &mut Cursor<'_>, width: u16, height: u16) -> Result<Option<Vec<u8>>, String> {
    let w = usize::from(width);
    let h = usize::from(height);
    let size = w
        .checked_mul(h)
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
        .unwrap_or(usize::MAX);
    if w == 0 || h == 0 || size > MAX_FRAME_BYTES {
        return Err(format!(
            "a hextile update of {width}x{height} exceeds the {MAX_FRAME_BYTES} byte bound"
        ));
    }
    let mut out = vec![0u8; size];
    let mut background = [0u8; BYTES_PER_PIXEL];
    let mut foreground = [0u8; BYTES_PER_PIXEL];
    for tile_y in (0..h).step_by(16) {
        for tile_x in (0..w).step_by(16) {
            let tile_w = (w - tile_x).min(16);
            let tile_h = (h - tile_y).min(16);
            let Some(byte) = cursor.take(1) else {
                return Ok(None);
            };
            let subencoding = byte[0];
            if subencoding & HEXTILE_RAW != 0 {
                let Some(bytes) = cursor.take(tile_w * tile_h * BYTES_PER_PIXEL) else {
                    return Ok(None);
                };
                for row in 0..tile_h {
                    let at = ((tile_y + row) * w + tile_x) * BYTES_PER_PIXEL;
                    let from = row * tile_w * BYTES_PER_PIXEL;
                    out[at..at + tile_w * BYTES_PER_PIXEL]
                        .copy_from_slice(&bytes[from..from + tile_w * BYTES_PER_PIXEL]);
                }
                continue;
            }
            if subencoding & HEXTILE_BACKGROUND != 0 {
                let Some(pixel) = cursor.take(BYTES_PER_PIXEL) else {
                    return Ok(None);
                };
                background.copy_from_slice(pixel);
            }
            if subencoding & HEXTILE_FOREGROUND != 0 {
                let Some(pixel) = cursor.take(BYTES_PER_PIXEL) else {
                    return Ok(None);
                };
                foreground.copy_from_slice(pixel);
            }
            paint(&mut out, w, tile_x, tile_y, tile_w, tile_h, &background);
            if subencoding & HEXTILE_SUBRECTS != 0 {
                let Some(count) = cursor.take(1) else {
                    return Ok(None);
                };
                let coloured = subencoding & HEXTILE_COLOURED != 0;
                for _ in 0..count[0] {
                    let colour = if coloured {
                        let Some(pixel) = cursor.take(BYTES_PER_PIXEL) else {
                            return Ok(None);
                        };
                        pixel
                    } else {
                        &foreground
                    };
                    let Some(geometry) = cursor.take(2) else {
                        return Ok(None);
                    };
                    let x = usize::from(geometry[0] >> 4);
                    let y = usize::from(geometry[0] & 0xF);
                    let wide = usize::from(geometry[1] >> 4) + 1;
                    let tall = usize::from(geometry[1] & 0xF) + 1;
                    if x + wide > tile_w || y + tall > tile_h {
                        return Err(format!(
                            "a hextile sub-rectangle {wide}x{tall} at {x},{y} is outside its {tile_w}x{tile_h} tile"
                        ));
                    }
                    paint(&mut out, w, tile_x + x, tile_y + y, wide, tall, colour);
                }
            }
        }
    }
    Ok(Some(out))
}

/// Fills a rectangle of the desktop buffer with one four-byte pixel.
fn paint(
    out: &mut [u8],
    desktop_width: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixel: &[u8],
) {
    for row in 0..height {
        let start = ((y + row) * desktop_width + x) * BYTES_PER_PIXEL;
        for column in 0..width {
            let at = start + column * BYTES_PER_PIXEL;
            out[at..at + BYTES_PER_PIXEL].copy_from_slice(pixel);
        }
    }
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

    fn hextile_message(width: u16, height: u16, body: &[u8]) -> Vec<u8> {
        let mut update = vec![0u8, 0];
        update.extend_from_slice(&1u16.to_be_bytes());
        update.extend_from_slice(&0u16.to_be_bytes());
        update.extend_from_slice(&0u16.to_be_bytes());
        update.extend_from_slice(&width.to_be_bytes());
        update.extend_from_slice(&height.to_be_bytes());
        update.extend_from_slice(&5i32.to_be_bytes());
        update.extend_from_slice(body);
        update
    }

    fn decoded_pixels(body: &[u8], width: u16, height: u16) -> Vec<u8> {
        let mut incoming = Incoming::default();
        incoming.push(&hextile_message(width, height, body));
        let Some(Message::Update(rects)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        let Rect::Raw { pixels, .. } = &rects[0] else {
            panic!("raw pixels");
        };
        pixels.clone()
    }

    #[test]
    fn a_hextile_tile_with_only_a_background_is_filled_with_it() {
        // Sub-encoding background, then the red pixel.
        let body = [2u8, 1, 2, 3, 4];
        assert_eq!(decoded_pixels(&body, 2, 1), [1, 2, 3, 4, 1, 2, 3, 4]);
    }

    #[test]
    fn a_hextile_sub_rectangle_keeps_its_own_colour() {
        // Background, foreground, one coloured sub-rectangle at 0,0 of 1x1.
        let body = [2 | 4 | 8 | 16, 1, 2, 3, 4, 5, 6, 7, 8, 1, 9, 9, 9, 9, 0, 0];
        assert_eq!(
            decoded_pixels(&body, 2, 2),
            [
                9, 9, 9, 9, 1, 2, 3, 4, //
                1, 2, 3, 4, 1, 2, 3, 4,
            ]
        );
    }

    #[test]
    fn a_second_tile_inherits_the_background_and_paints_with_the_foreground() {
        // One row of 17 pixels: a full 16x1 tile, then a 1x1 tile that names only
        // a foreground and a sub-rectangle.
        let mut body = vec![2u8, 1, 2, 3, 4];
        body.extend_from_slice(&[4 | 8, 5, 6, 7, 8, 1, 0, 0]);
        let pixels = decoded_pixels(&body, 17, 1);
        assert_eq!(&pixels[..4], &[1, 2, 3, 4]);
        assert_eq!(&pixels[16 * 4..], &[5, 6, 7, 8]);
    }

    #[test]
    fn a_raw_tile_is_copied_as_the_client_pixel_format() {
        // Sub-encoding raw, then four pixels for a 2x2 tile.
        let body = [
            1u8, 10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33, 40, 41, 42, 43,
        ];
        assert_eq!(
            decoded_pixels(&body, 2, 2),
            [
                10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33, 40, 41, 42, 43
            ]
        );
    }

    #[test]
    fn a_hextile_rectangle_split_by_a_read_is_joined_again() {
        let body = [2u8, 1, 2, 3, 4];
        let message = hextile_message(2, 1, &body);
        let mut incoming = Incoming::default();
        incoming.push(&message[..message.len() - 2]);
        assert!(incoming.take().expect("parsed").is_none());
        incoming.push(&message[message.len() - 2..]);
        let Some(Message::Update(rects)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn a_sub_rectangle_outside_its_tile_is_reported() {
        // A 2x2 sub-rectangle at 15,15 does not fit a tile.
        let body = [2 | 4 | 8, 1, 2, 3, 4, 5, 6, 7, 8, 1, 0xFE, 0x00];
        let mut incoming = Incoming::default();
        incoming.push(&hextile_message(4, 4, &body));
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
