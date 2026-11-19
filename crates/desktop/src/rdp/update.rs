//! Bitmap rectangles from the Share Data and Fast-Path output PDUs.
//!
//! The session keeps one BGRA desktop buffer; each rectangle is copied into it and
//! then published as the dirty region. Only formats the client advertises are
//! accepted: a rectangle in any other compression is an error, never a guess.
use super::wire::{Packet, invalid};
use std::io;

/// One rectangle as the server described it.
pub(super) struct Bitmap<'a> {
    /// Destination corner and the part of the bitmap that lands on the desktop.
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) width: u16,
    pub(super) height: u16,
    /// The bitmap itself may be larger than its destination when the server
    /// clipped the rectangle at a screen edge.
    pub(super) bitmaps: (u16, u16),
    pub(super) bpp: u16,
    pub(super) compressed: bool,
    pub(super) data: &'a [u8],
}

/// Reads the common BITMAP_DATA header used by both the slow and fast paths.
/// `left/top/right/bottom` are inclusive, as the protocol defines them.
pub(super) fn rect<'a>(p: &mut Packet<'a>) -> io::Result<Bitmap<'a>> {
    let left = p.le16()?;
    let top = p.le16()?;
    let right = p.le16()?;
    let bottom = p.le16()?;
    if right < left || bottom < top {
        return Err(invalid("inverted RDP bitmap rectangle"));
    }
    let width = right - left + 1;
    let height = bottom - top + 1;
    // The header repeats the size; the destination extents stay authoritative so
    // a rectangle that disagrees with its own bounds is rejected below.
    let stated_width = p.le16()?;
    let stated_height = p.le16()?;
    if stated_width < width || stated_height < height {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "RDP bitmap of {stated_width}x{stated_height} cannot fill its \
                 {width}x{height} destination"
            ),
        ));
    }
    let bpp = p.le16()?;
    let flags = p.le16()?;
    let mut length = usize::try_from(p.le32()?).map_err(|_| invalid("RDP bitmap too long"))?;
    // Some servers count the two-byte pad of the compressed bitmap header in the
    // length but never send it, leaving the next bitmap two bytes early. The
    // length field then equals the body size plus eight while only six header
    // bytes were written; subtracting the two keeps the stream aligned.
    if p.0.len() >= 8
        && matches!(p.0[6], 0x10 | 0x30)
        && usize::from(u16::from_le_bytes([p.0[0], p.0[1]])) + 8 == length
    {
        length -= 2;
    }
    let data = p.take(length)?;
    Ok(Bitmap {
        x: left,
        y: top,
        width,
        height,
        bitmaps: (stated_width, stated_height),
        bpp,
        compressed: flags & 1 != 0,
        data,
    })
}

/// The legacy pixel RLE: one stream of orders that describes the bitmap from the
/// bottom row up, three bytes per pixel. It is what xrdp emits for a compressed
/// bitmap even when the desktop depth is 32 bits, so it is decoded here rather
/// than refused.
struct Rle<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Rle<'a> {
    fn byte(&mut self) -> io::Result<u8> {
        let value = *self
            .data
            .get(self.at)
            .ok_or_else(|| invalid("truncated RLE bitmap"))?;
        self.at += 1;
        Ok(value)
    }
}

fn legacy_rle(data: &[u8], width: usize, height: usize) -> io::Result<Vec<u8>> {
    let stride = width * 4;
    let mut out = vec![0u8; stride * height];
    let mut rle = Rle { data, at: 0 };
    // The row counter counts down: the first decoded row is the bitmap's last.
    let mut rows = height;
    let mut x = width;
    let mut row: Option<usize> = None;
    let mut previous: Option<usize> = None;
    let mut last_opcode: i32 = -1;
    let mut insert_mix = false;
    let mut bicolour = false;
    let mut colour1 = [0u8; 3];
    let mut colour2 = [0u8; 3];
    let mut mix = [0xffu8; 3];
    let mut mask = 0u8;
    let mut mixmask;
    let mut fom_mask;

    while rle.at < rle.data.len() {
        fom_mask = 0;
        let code = rle.byte()?;
        let mut opcode = i32::from(code >> 4);
        let mut count;
        let offset;
        match opcode {
            0xc..=0xe => {
                opcode -= 6;
                count = usize::from(code & 0x0f);
                offset = 16;
            }
            0xf => {
                opcode = i32::from(code & 0x0f);
                count = if opcode < 9 {
                    let low = usize::from(rle.byte()?);
                    low | (usize::from(rle.byte()?) << 8)
                } else if opcode < 0xb {
                    8
                } else {
                    1
                };
                offset = 0;
            }
            _ => {
                opcode >>= 1;
                count = usize::from(code & 0x1f);
                offset = 32;
            }
        }
        if offset != 0 {
            let fill_or_mix = opcode == 2 || opcode == 7;
            if count == 0 {
                count = usize::from(rle.byte()?) + if fill_or_mix { 1 } else { offset };
            } else if fill_or_mix {
                count <<= 3;
            }
        }
        match opcode {
            0 => {
                if last_opcode == 0 && !(x == width && previous.is_none()) {
                    insert_mix = true;
                }
            }
            8 => {
                colour1 = [rle.byte()?, rle.byte()?, rle.byte()?];
                colour2 = [rle.byte()?, rle.byte()?, rle.byte()?];
            }
            3 => colour2 = [rle.byte()?, rle.byte()?, rle.byte()?],
            6 | 7 => {
                mix = [rle.byte()?, rle.byte()?, rle.byte()?];
                opcode -= 5;
            }
            9 => {
                mask = 0x03;
                opcode = 2;
                fom_mask = 3;
            }
            0x0a => {
                mask = 0x05;
                opcode = 2;
                fom_mask = 5;
            }
            _ => {}
        }
        last_opcode = opcode;
        mixmask = 0;
        while count > 0 {
            if x >= width {
                if rows == 0 {
                    return Err(invalid("RLE data runs past the bitmap"));
                }
                x = 0;
                rows -= 1;
                previous = row;
                row = Some(rows * stride);
            }
            let start = row.unwrap_or(0) + x * 4;
            let below = previous.map(|line| line + x * 4);
            let pixel: [u8; 3] = match opcode {
                0 => {
                    if insert_mix {
                        insert_mix = false;
                        match below {
                            Some(at) => {
                                [out[at] ^ mix[0], out[at + 1] ^ mix[1], out[at + 2] ^ mix[2]]
                            }
                            None => mix,
                        }
                    } else {
                        match below {
                            Some(at) => [out[at], out[at + 1], out[at + 2]],
                            None => [0, 0, 0],
                        }
                    }
                }
                1 => match below {
                    Some(at) => [out[at] ^ mix[0], out[at + 1] ^ mix[1], out[at + 2] ^ mix[2]],
                    None => mix,
                },
                2 => {
                    mixmask <<= 1;
                    if mixmask == 0 {
                        mask = if fom_mask != 0 { fom_mask } else { rle.byte()? };
                        mixmask = 1;
                    }
                    match below {
                        Some(at) if mask & mixmask != 0 => {
                            [out[at] ^ mix[0], out[at + 1] ^ mix[1], out[at + 2] ^ mix[2]]
                        }
                        Some(at) => [out[at], out[at + 1], out[at + 2]],
                        None if mask & mixmask != 0 => mix,
                        None => [0, 0, 0],
                    }
                }
                3 => colour2,
                4 => [rle.byte()?, rle.byte()?, rle.byte()?],
                8 => {
                    if bicolour {
                        bicolour = false;
                        colour2
                    } else {
                        bicolour = true;
                        count += 1;
                        colour1
                    }
                }
                0xd => [0xff, 0xff, 0xff],
                0xe => [0, 0, 0],
                other => {
                    return Err(invalid(if other == 0 {
                        "RLE copy order without a pixel"
                    } else {
                        "unhandled RLE bitmap order"
                    }));
                }
            };
            out[start..start + 4].copy_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            count -= 1;
            x += 1;
        }
    }
    Ok(out)
}

/// The 32-bit planar RLE: a marker byte, then four bit planes (alpha, red,
/// green, blue) whose rows are stored bottom-up with per-row deltas.
fn planar_rle(data: &[u8], width: usize, height: usize, planes: &[usize]) -> io::Result<Vec<u8>> {
    let mut rle = Rle { data, at: 1 };
    let stride = width * 4;
    let mut out = vec![0u8; stride * height];
    for &offset in planes {
        let mut color: i32;
        let mut previous: Option<usize> = None;
        for line in 0..height {
            let row = (height - 1 - line) * stride;
            let mut x = 0usize;
            color = 0;
            while x < width {
                let code = rle.byte()?;
                let mut replen = usize::from(code & 0x0f);
                let mut collen = usize::from((code >> 4) & 0x0f);
                let reversed = (replen << 4) | collen;
                if (16..=47).contains(&reversed) {
                    replen = reversed;
                    collen = 0;
                }
                while x < width && collen > 0 {
                    let byte = rle.byte()?;
                    // color keeps the row delta, not the reconstructed value: the
                    // run that follows repeats this same delta over the row below.
                    let value = match previous {
                        Some(at) => {
                            color = delta(byte);
                            out[at + x * 4 + offset] as i32 + color
                        }
                        None => {
                            color = i32::from(byte);
                            color
                        }
                    };
                    out[row + x * 4 + offset] = value as u8;
                    x += 1;
                    collen -= 1;
                }
                while x < width && replen > 0 {
                    let value = match previous {
                        Some(at) => out[at + x * 4 + offset] as i32 + color,
                        None => color,
                    };
                    out[row + x * 4 + offset] = value as u8;
                    x += 1;
                    replen -= 1;
                }
            }
            previous = Some(row);
        }
    }
    for pixel in out.chunks_exact_mut(4) {
        pixel[3] = 0xff;
    }
    Ok(out)
}

/// A signed delta, zig-zagged into one byte: 0 stays 0, 1 is -1, 2 is 1, ...
fn delta(byte: u8) -> i32 {
    if byte & 1 != 0 {
        -((i32::from(byte) >> 1) + 1)
    } else {
        i32::from(byte >> 1)
    }
}

/// Decodes one compressed rectangle into a packed BGRA image, top row first.
pub(super) fn decompress(bitmap: &Bitmap<'_>) -> io::Result<Vec<u8>> {
    let width = usize::from(bitmap.bitmaps.0);
    let height = usize::from(bitmap.bitmaps.1);
    if bitmap.bpp == 32 {
        // The planar (RDP 6.0) stream starts with a marker byte: 0x10 carries an
        // alpha plane, 0x30 does not. Some servers put the three trailing 16-bit
        // size fields of the bitmap header in front of it, which is where the
        // marker is looked for when it is not first.
        if let Some((start, marker)) = planar_marker(bitmap.data) {
            let planes: &[usize] = if marker == 0x30 {
                &[2, 1, 0]
            } else {
                &[3, 2, 1, 0]
            };
            return planar_rle(&bitmap.data[start..], width, height, planes);
        }
    }
    match bitmap.bpp {
        24 | 32 => legacy_rle(bitmap.data, width, height),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "compressed bitmap depth was not negotiated",
        )),
    }
}

/// Finds the planar marker: at the start, or after the three 16-bit fields some
/// servers keep from the bitmap header.
fn planar_marker(data: &[u8]) -> Option<(usize, u8)> {
    match data.first() {
        Some(&marker @ (0x10 | 0x30)) => return Some((0, marker)),
        _ => {}
    }
    match data.get(6) {
        Some(&marker @ (0x10 | 0x30)) => Some((6, marker)),
        _ => None,
    }
}

/// Copies one rectangle into the desktop buffer. `stride` is the buffer's row
/// length in bytes; `desktop_height` bounds the copy so a lying header cannot
/// write past the buffer.
pub(super) fn draw(
    frame: &mut [u8],
    stride: usize,
    desktop_height: u16,
    bitmap: &Bitmap<'_>,
) -> io::Result<()> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Err(invalid("empty RDP bitmap rectangle"));
    }
    if u32::from(bitmap.y) + u32::from(bitmap.height) > u32::from(desktop_height) {
        return Err(invalid("RDP bitmap lies outside the desktop"));
    }
    let row = stride
        .checked_mul(usize::from(bitmap.y))
        .and_then(|offset| offset.checked_add(usize::from(bitmap.x) * 4))
        .ok_or_else(|| invalid("RDP bitmap offset overflow"))?;
    if row
        .checked_add(usize::from(bitmap.height - 1) * stride + usize::from(bitmap.width) * 4)
        .is_none_or(|end| end > frame.len())
    {
        return Err(invalid("RDP bitmap lies outside the desktop buffer"));
    }
    let row_bytes = usize::from(bitmap.width) * 4;
    // A decoded bitmap is packed BGRA at the bitmap's own size; only the part
    // that lands on the desktop is copied.
    let pixels = if bitmap.compressed {
        decompress(bitmap).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}x{} compressed bitmap: {error}",
                    bitmap.bitmaps.0, bitmap.bitmaps.1
                ),
            )
        })?
    } else {
        unpack(bitmap)?
    };
    let bitmap_row = usize::from(bitmap.bitmaps.0) * 4;
    if pixels.len() < bitmap_row * usize::from(bitmap.bitmaps.1) {
        return Err(invalid("truncated RDP bitmap data"));
    }
    for line in 0..usize::from(bitmap.height) {
        let source = line * bitmap_row;
        let start = row + line * stride;
        frame[start..start + row_bytes].copy_from_slice(&pixels[source..source + row_bytes]);
    }
    Ok(())
}

/// Expands an uncompressed rectangle into packed BGRA at its own size.
fn unpack(bitmap: &Bitmap<'_>) -> io::Result<Vec<u8>> {
    let width = usize::from(bitmap.bitmaps.0);
    let height = usize::from(bitmap.bitmaps.1);
    let source_row = match bitmap.bpp {
        32 => width * 4,
        24 => width * 3,
        16 => width * 2,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "RDP bitmap depth was not negotiated",
            ));
        }
    };
    let needed = source_row
        .checked_mul(height)
        .ok_or_else(|| invalid("RDP bitmap size overflow"))?;
    if bitmap.data.len() < needed {
        return Err(invalid("truncated RDP bitmap data"));
    }
    let mut pixels = vec![0u8; width * height * 4];
    for line in 0..height {
        let source = &bitmap.data[line * source_row..(line + 1) * source_row];
        let target = &mut pixels[line * width * 4..(line + 1) * width * 4];
        match bitmap.bpp {
            32 => target.copy_from_slice(source),
            24 => {
                for (pixel, bytes) in target.chunks_exact_mut(4).zip(source.chunks_exact(3)) {
                    pixel.copy_from_slice(&[bytes[0], bytes[1], bytes[2], 0xff]);
                }
            }
            16 => {
                for (pixel, bytes) in target.chunks_exact_mut(4).zip(source.chunks_exact(2)) {
                    let value = u16::from_le_bytes([bytes[0], bytes[1]]);
                    let red = ((value >> 10) & 0x1f) as u8;
                    let green = ((value >> 5) & 0x1f) as u8;
                    let blue = (value & 0x1f) as u8;
                    pixel.copy_from_slice(&[
                        (blue << 3) | (blue >> 2),
                        (green << 3) | (green >> 2),
                        (red << 3) | (red >> 2),
                        0xff,
                    ]);
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(width: u16, height: u16, bpp: u16, data: &[u8]) -> Bitmap<'_> {
        Bitmap {
            x: 0,
            y: 0,
            width,
            height,
            bitmaps: (width, height),
            bpp,
            compressed: true,
            data,
        }
    }

    #[test]
    fn a_legacy_rle_fill_and_copy_decode() {
        // Two rows of four black pixels: a fill order per row.
        let pixels = decompress(&bitmap(4, 2, 32, &[0x04, 0x04])).unwrap();
        assert_eq!(pixels, vec![0, 0, 0, 0xff].repeat(8));

        // One row of four copied pixels, then the same again for the next row.
        let mut data = vec![0x84];
        for value in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            data.push(value);
        }
        data.push(0x84);
        for value in [13u8, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24] {
            data.push(value);
        }
        let pixels = decompress(&bitmap(4, 2, 32, &data)).unwrap();
        // The stream's first row is the bitmap's last one.
        assert_eq!(&pixels[16..20], &[1, 2, 3, 0xff]);
        assert_eq!(&pixels[0..4], &[13, 14, 15, 0xff]);
    }

    #[test]
    fn a_planar_rle_stream_decodes_its_four_planes() {
        // 2x1, four planes in the order the protocol sends them: alpha, red,
        // green, blue. A single row needs no deltas, so every byte is a value.
        let mut data = vec![0x10];
        for plane in [[10u8, 11], [20, 21], [30, 31], [40, 41]] {
            data.push(0x20); // two literal pixels
            data.push(plane[0]);
            data.push(plane[1]);
        }
        let pixels = decompress(&bitmap(2, 1, 32, &data)).unwrap();
        assert_eq!(&pixels[0..4], &[40, 30, 20, 0xff]);
        assert_eq!(&pixels[4..8], &[41, 31, 21, 0xff]);
    }

    #[test]
    fn a_truncated_stream_is_an_error_not_a_guess() {
        assert!(decompress(&bitmap(4, 2, 32, &[0x84, 1, 2])).is_err());
        assert!(decompress(&bitmap(2, 2, 32, &[0x10, 0x20, 1])).is_err());
        // A depth this client never asks for is refused rather than misread.
        assert!(decompress(&bitmap(2, 2, 8, &[0x04])).is_err());
    }
}

/// A tightly packed copy of one rectangle, the shape `Event::Frame` carries.
pub(super) fn region(frame: &[u8], stride: usize, bitmap: &Bitmap<'_>) -> Vec<u8> {
    let row = usize::from(bitmap.width) * 4;
    let mut pixels = Vec::with_capacity(row * usize::from(bitmap.height));
    for line in 0..usize::from(bitmap.height) {
        let start = usize::from(bitmap.y) * stride + line * stride + usize::from(bitmap.x) * 4;
        pixels.extend_from_slice(&frame[start..start + row]);
    }
    pixels
}
