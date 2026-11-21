//! Incremental RFB decoder. Pixel updates can span arbitrarily many socket reads.
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
    Rectangle(Rect),
    Pointer(crate::PointerShape),
    UpdateEnd,
    EndContinuous,
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
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
pub(super) const RICH_CURSOR: i32 = -239;

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

/// A pixel rectangle is published in bands. A slow full-screen update can
/// therefore paint before its last byte arrives, with bounded scratch storage.
const BAND_ROWS: usize = 64;

struct PixelRectangle {
    x: u16,
    y: u16,
    width: usize,
    height: usize,
    row: usize,
    hextile: Option<Tiles>,
}

#[derive(Default)]
struct Tiles {
    x: usize,
    y: usize,
    background: [u8; 4],
    foreground: [u8; 4],
    band: Vec<u8>,
}

impl PixelRectangle {
    fn next_band(&mut self, cursor: &mut Cursor<'_>) -> Result<Option<Rect>, String> {
        let rows = (self.height - self.row).min(BAND_ROWS);
        let size = rows * self.width * BYTES_PER_PIXEL;
        let pixels = if let Some(tiles) = self.hextile.as_mut() {
            tiles.band.resize(size, 0);
            while tiles.y < rows {
                let mut tile = Cursor::new(&cursor.data[cursor.at..]);
                if !tiles.decode(&mut tile, self.width, rows)? {
                    return Ok(None);
                }
                cursor.at += tile.at;
                tiles.x += 16;
                if tiles.x >= self.width {
                    tiles.x = 0;
                    tiles.y += 16;
                }
            }
            tiles.y = 0;
            std::mem::take(&mut tiles.band)
        } else {
            let Some(bytes) = cursor.take(size) else {
                return Ok(None);
            };
            bytes.to_vec()
        };
        let y = self.y + self.row as u16;
        self.row += rows;
        Ok(Some(Rect::Raw {
            x: self.x,
            y,
            width: self.width as u16,
            height: rows as u16,
            pixels,
        }))
    }
}

impl Tiles {
    /// Commit colours and pixels only once the complete tile is available.
    fn decode(
        &mut self,
        cursor: &mut Cursor<'_>,
        width: usize,
        rows: usize,
    ) -> Result<bool, String> {
        let tile_w = (width - self.x).min(16);
        let tile_h = (rows - self.y).min(16);
        let Some(&flags) = cursor.take(1).and_then(|s| s.first()) else {
            return Ok(false);
        };
        if flags & HEXTILE_RAW != 0 {
            let Some(raw) = cursor.take(tile_w * tile_h * 4) else {
                return Ok(false);
            };
            for (dy, row) in raw.chunks_exact(tile_w * 4).enumerate() {
                let at = ((self.y + dy) * width + self.x) * 4;
                self.band[at..at + row.len()].copy_from_slice(row);
            }
            return Ok(true);
        }
        let mut background = self.background;
        let mut foreground = self.foreground;
        for (flag, colour) in [
            (HEXTILE_BACKGROUND, &mut background),
            (HEXTILE_FOREGROUND, &mut foreground),
        ] {
            if flags & flag != 0 {
                let Some(value) = cursor.take(4) else {
                    return Ok(false);
                };
                colour.copy_from_slice(value);
            }
        }
        let coloured = flags & HEXTILE_COLOURED != 0;
        let entry = if coloured { 6 } else { 2 };
        let subrects = if flags & HEXTILE_SUBRECTS != 0 {
            let Some(count) = cursor.take(1) else {
                return Ok(false);
            };
            let Some(data) = cursor.take(usize::from(count[0]) * entry) else {
                return Ok(false);
            };
            data
        } else {
            &[]
        };
        self.background = background;
        self.foreground = foreground;
        paint(
            &mut self.band,
            width,
            self.x,
            self.y,
            tile_w,
            tile_h,
            &background,
        );
        for sub in subrects.chunks_exact(entry) {
            let (colour, geometry) = sub.split_at(entry - 2);
            let x = usize::from(geometry[0] >> 4);
            let y = usize::from(geometry[0] & 15);
            let w = usize::from(geometry[1] >> 4) + 1;
            let h = usize::from(geometry[1] & 15) + 1;
            if x + w > tile_w || y + h > tile_h {
                return Err(format!(
                    "a hextile sub-rectangle {w}x{h} at {x},{y} is outside its {tile_w}x{tile_h} tile"
                ));
            }
            paint(
                &mut self.band,
                width,
                self.x + x,
                self.y + y,
                w,
                h,
                if coloured { colour } else { &foreground },
            );
        }
        Ok(true)
    }
}

#[derive(Default)]
pub(crate) struct Incoming {
    pending: Vec<u8>,
    offset: usize,
    remaining: Option<u16>,
    pixels: Option<PixelRectangle>,
}

impl Incoming {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        if self.offset != 0 {
            self.pending.drain(..self.offset);
            self.offset = 0;
        }
        self.pending.extend_from_slice(bytes);
    }

    pub(crate) fn take(&mut self) -> Result<Option<Message>, String> {
        loop {
            // A large clipboard transfer should not reserve its peak buffer
            // for the rest of an idle desktop session.
            if self.offset == self.pending.len() && self.pending.capacity() > 1024 * 1024 {
                self.pending = Vec::new();
                self.offset = 0;
            }
            let data = &self.pending[self.offset..];
            let mut cursor = Cursor::new(data);
            if let Some(rect) = &mut self.pixels {
                let band = rect.next_band(&mut cursor)?;
                self.offset += cursor.at;
                if rect.row == rect.height {
                    self.pixels = None;
                }
                return Ok(band.map(Message::Rectangle));
            }
            if let Some(remaining) = self.remaining {
                if remaining == 0 {
                    self.remaining = None;
                    return Ok(Some(Message::UpdateEnd));
                }
                let (Some(x), Some(y), Some(width), Some(height), Some(encoding)) = (
                    cursor.u16(),
                    cursor.u16(),
                    cursor.u16(),
                    cursor.u16(),
                    cursor.i32(),
                ) else {
                    return Ok(None);
                };
                let rect = match encoding {
                    RAW | HEXTILE => {
                        let bytes = u64::from(width) * u64::from(height) * 4;
                        if bytes == 0
                            || bytes > MAX_FRAME_BYTES as u64
                            || x.checked_add(width).is_none()
                            || y.checked_add(height).is_none()
                        {
                            return Err(format!(
                                "invalid pixel rectangle of {width}x{height} at {x},{y}"
                            ));
                        }
                        self.pixels = Some(PixelRectangle {
                            x,
                            y,
                            width: usize::from(width),
                            height: usize::from(height),
                            row: 0,
                            hextile: (encoding == HEXTILE).then(Tiles::default),
                        });
                        None
                    }
                    COPY_RECT => {
                        let (Some(source_x), Some(source_y)) = (cursor.u16(), cursor.u16()) else {
                            return Ok(None);
                        };
                        Some(Rect::Copy {
                            x,
                            y,
                            width,
                            height,
                            source_x,
                            source_y,
                        })
                    }
                    RICH_CURSOR => {
                        // RFC 6143 7.8.1: pixel data followed by a one-bit mask,
                        // padded to a byte boundary on each row.
                        let w = usize::from(width);
                        let h = usize::from(height);
                        let shape = if w == 0 || h == 0 {
                            crate::PointerShape::Hidden
                        } else {
                            if w > 512 || h > 512 || x >= width || y >= height {
                                return Err(format!(
                                    "invalid cursor {width}x{height} at hotspot {x},{y}"
                                ));
                            }
                            let mask_stride = w.div_ceil(8);
                            let (Some(raw), Some(mask)) =
                                (cursor.take(w * h * 4), cursor.take(mask_stride * h))
                            else {
                                return Ok(None);
                            };
                            let mut pixels = raw.to_vec();
                            for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                                let col = i % w;
                                if mask[i / w * mask_stride + col / 8] & (0x80 >> (col % 8)) != 0 {
                                    pixel[3] = 255;
                                } else {
                                    pixel.fill(0);
                                }
                            }
                            crate::PointerShape::Bitmap {
                                width,
                                height,
                                hotspot: (x, y),
                                pixels: pixels.into(),
                            }
                        };
                        self.remaining = Some(remaining - 1);
                        self.offset += cursor.at;
                        return Ok(Some(Message::Pointer(shape)));
                    }
                    DESKTOP_SIZE => Some(Rect::Resize { width, height }),
                    LAST_RECT => {
                        self.remaining = None;
                        self.offset += cursor.at;
                        return Ok(Some(Message::UpdateEnd));
                    }
                    other => {
                        return Err(format!(
                            "the server used encoding {other}, which this client did not request"
                        ));
                    }
                };
                self.remaining = Some(remaining - 1);
                self.offset += cursor.at;
                if let Some(rect) = rect {
                    return Ok(Some(Message::Rectangle(rect)));
                }
                continue;
            }
            let Some(&kind) = data.first() else {
                return Ok(None);
            };
            cursor.take(1);
            let parsed = match kind {
                0 => {
                    cursor.take(1);
                    let Some(count) = cursor.u16() else {
                        return Ok(None);
                    };
                    self.remaining = Some(count);
                    self.offset += cursor.at;
                    continue;
                }
                1 => colour_map(&mut cursor)?,
                2 => Parsed::Complete(Message::Bell, 1),
                3 => cut_text(&mut cursor)?,
                150 => Parsed::Complete(Message::EndContinuous, 1),
                248 => fence(&mut cursor)?,
                other => {
                    return Err(format!(
                        "the server sent RFB message type {other}, which this client does not read"
                    ));
                }
            };
            return match parsed {
                Parsed::Incomplete => Ok(None),
                Parsed::Complete(message, used) => {
                    self.offset += used;
                    Ok(Some(message))
                }
            };
        }
    }
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
    let colour: [u8; BYTES_PER_PIXEL] = pixel.try_into().expect("four-byte wire pixel");
    for row in 0..height {
        let start = ((y + row) * desktop_width + x) * BYTES_PER_PIXEL;
        out[start..start + width * BYTES_PER_PIXEL]
            .as_chunks_mut::<BYTES_PER_PIXEL>()
            .0
            .fill(colour);
    }
}

fn colour_map(cursor: &mut Cursor<'_>) -> Result<Parsed, String> {
    // A true-colour session never uses the map, but a server may still send it.
    if cursor.take(3).is_none() {
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

fn fence(cursor: &mut Cursor<'_>) -> Result<Parsed, String> {
    let (Some(_), Some(flags), Some(length)) = (cursor.take(3), cursor.i32(), cursor.take(1))
    else {
        return Ok(Parsed::Incomplete);
    };
    if length[0] > 64 {
        return Err("a fence payload exceeds 64 bytes".into());
    }
    let Some(payload) = cursor.take(usize::from(length[0])) else {
        return Ok(Parsed::Incomplete);
    };
    Ok(Parsed::Complete(
        Message::Fence {
            flags: flags as u32,
            payload: payload.to_vec(),
        },
        cursor.at,
    ))
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
    fn continuous_update_negotiation_survives_fragmented_fences() {
        let message = [248, 0, 0, 0, 0x80, 0, 0, 1, 2, 0xaa, 0xbb, 150];
        let mut incoming = Incoming::default();
        for byte in &message[..10] {
            incoming.push(&[*byte]);
            assert!(incoming.take().unwrap().is_none());
        }
        incoming.push(&message[10..]);
        assert!(
            matches!(incoming.take().unwrap(), Some(Message::Fence { flags: 0x80000001, payload }) if payload == [0xaa,0xbb])
        );
        assert!(matches!(
            incoming.take().unwrap(),
            Some(Message::EndContinuous)
        ));
        incoming.push(&[248, 0, 0, 0, 0, 0, 0, 0, 65]);
        assert!(incoming.take().is_err());
    }

    #[test]
    fn rich_pointer_waits_for_its_row_padded_mask() {
        let mut data = vec![0, 0, 0, 1, 0, 8, 0, 1, 0, 9, 0, 2];
        data.extend_from_slice(&RICH_CURSOR.to_be_bytes());
        data.extend_from_slice(&[3; 9 * 2 * 4]);
        data.extend_from_slice(&[0, 0x80, 0x80, 0]);
        let mut incoming = Incoming::default();
        for byte in &data[..data.len() - 1] {
            incoming.push(&[*byte]);
            assert!(incoming.take().unwrap().is_none());
        }
        incoming.push(&[0]);
        let Some(Message::Pointer(crate::PointerShape::Bitmap {
            hotspot, pixels, ..
        })) = incoming.take().unwrap()
        else {
            panic!("expected a pointer");
        };
        assert_eq!(hotspot, (8, 1));
        for (i, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(
                pixel,
                if matches!(i, 8 | 9) {
                    &[3, 3, 3, 255]
                } else {
                    &[0; 4]
                }
            );
        }
        assert!(matches!(incoming.take().unwrap(), Some(Message::UpdateEnd)));
    }

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
        let Some(Message::Rectangle(rect)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        let Rect::Raw {
            width,
            height,
            pixels,
            ..
        } = &rect
        else {
            panic!("raw");
        };
        assert_eq!((*width, *height), (2, 1));
        assert_eq!(pixels, &[9, 8, 7, 6, 5, 4, 3, 2]);
        assert!(matches!(incoming.take().unwrap(), Some(Message::UpdateEnd)));
        assert!(incoming.take().expect("parsed").is_none());
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
        let Some(Message::Rectangle(rect)) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        let Rect::Raw { pixels, .. } = &rect else {
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
        let Some(Message::Rectangle(Rect::Raw { .. })) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
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
    fn fragmented_hextile_publishes_a_band_before_the_last_tile() {
        // Eight solid 16x16 tiles make up the first 32x64 band. The next
        // band inherits its background, then one subrect spans part of a tile.
        let mut body = vec![2, 11, 22, 33, 44];
        body.extend_from_slice(&[0; 7]);
        body.extend_from_slice(&[0, 4 | 8, 9, 8, 7, 6, 1, 0, 0]);
        let update = hextile_message(32, 65, &body);
        let mut incoming = Incoming::default();
        let mut bands = 0;
        for (i, byte) in update.iter().enumerate() {
            incoming.push(std::slice::from_ref(byte));
            if let Some(Message::Rectangle(Rect::Raw {
                y, height, pixels, ..
            })) = incoming.take().unwrap()
            {
                if bands == 0 {
                    assert!(i + 1 < update.len());
                    assert_eq!((y, height), (0, 64));
                    assert!(
                        pixels
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .all(|pixel| *pixel == [11, 22, 33, 44])
                    );
                } else {
                    assert_eq!((y, height), (64, 1));
                    assert_eq!(&pixels[..4], &[11, 22, 33, 44]);
                    assert_eq!(&pixels[64..68], &[9, 8, 7, 6]);
                }
                bands += 1;
            }
        }
        assert_eq!(bands, 2);
        assert!(matches!(incoming.take().unwrap(), Some(Message::UpdateEnd)));
        assert!(incoming.take().unwrap().is_none());
    }

    #[test]
    fn fragmented_raw_bands_and_nonzero_colour_map_stay_in_sync() {
        let mut stream = vec![1, 0, 0, 2, 0, 1, 0, 1, 0, 2, 0, 3];
        let mut update = hextile_message(1, 65, &[]);
        update[12..16].copy_from_slice(&RAW.to_be_bytes());
        stream.extend_from_slice(&update);
        let pixels: Vec<_> = (0u8..=64).flat_map(|i| [i; 4]).collect();
        stream.extend_from_slice(&pixels);
        stream.push(2); // A bell immediately follows the update.
        let mut incoming = Incoming::default();
        let mut painted = Vec::new();
        let mut events = Vec::new();
        for part in stream.chunks(3) {
            incoming.push(part);
            while let Some(message) = incoming.take().unwrap() {
                events.push(match message {
                    Message::Ignored => "map",
                    Message::Rectangle(Rect::Raw { pixels, .. }) => {
                        painted.extend(pixels);
                        "band"
                    }
                    Message::UpdateEnd => "end",
                    Message::Bell => "bell",
                    _ => panic!("unexpected event"),
                });
            }
        }
        assert_eq!(painted, pixels);
        assert_eq!(events, ["map", "band", "band", "end", "bell"]);
    }

    #[test]
    fn last_rect_ends_the_update_early() {
        let mut update = vec![0u8, 0];
        update.extend_from_slice(&2u16.to_be_bytes());
        update.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
        update.extend_from_slice(&(-224i32).to_be_bytes());
        let mut incoming = Incoming::default();
        incoming.push(&update);
        let Some(rect) = incoming.take().expect("parsed") else {
            panic!("an update");
        };
        assert!(matches!(rect, Message::UpdateEnd));
    }
}
