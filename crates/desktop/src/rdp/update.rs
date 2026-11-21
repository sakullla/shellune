//! Bitmap rectangles from the Share Data and Fast-Path output PDUs.
//!
//! Each bitmap produces one packed BGRA patch for the frame mailbox to composite.
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
    /// Only this rectangle's bitmap stream, after any compression header.
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
    let width = (right - left)
        .checked_add(1)
        .ok_or_else(|| invalid("invalid bitmap width"))?;
    let height = (bottom - top)
        .checked_add(1)
        .ok_or_else(|| invalid("invalid bitmap height"))?;
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
    let length = usize::from(p.le16()?);
    let mut stream = Packet(p.take(length)?);
    if flags & 1 != 0 && flags & 0x0400 == 0 {
        stream.take(8)?; // TS_CD_HEADER is included in bitmapLength.
    }
    if stream.0.is_empty()
        || u64::from(stated_width) * u64::from(stated_height) * 4 > crate::MAX_FRAME_BYTES as u64
    {
        return Err(invalid("invalid bitmap size"));
    }
    let data = stream.0;
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

/// Interleaved RLE describes the bitmap from the bottom row up. Some xrdp
/// sessions also use the 24-bit encoding on a 32-bit desktop.
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

    fn color(&mut self, bpp: u16) -> io::Result<[u8; 3]> {
        if matches!(bpp, 15 | 16) {
            Ok(high_color([self.byte()?, self.byte()?], bpp))
        } else {
            Ok([self.byte()?, self.byte()?, self.byte()?])
        }
    }
}

fn high_color(bytes: [u8; 2], bpp: u16) -> [u8; 3] {
    let value = u16::from_le_bytes(bytes);
    let (red, green) = if bpp == 16 {
        let g = ((value >> 5) & 0x3f) as u8;
        ((value >> 11) as u8, (g << 2) | (g >> 4))
    } else {
        let g = ((value >> 5) & 0x1f) as u8;
        (((value >> 10) & 0x1f) as u8, (g << 3) | (g >> 2))
    };
    let blue = (value & 0x1f) as u8;
    [(blue << 3) | (blue >> 2), green, (red << 3) | (red >> 2)]
}

fn legacy_rle(data: &[u8], width: usize, height: usize, bpp: u16) -> io::Result<(Vec<u8>, usize)> {
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
                colour1 = rle.color(bpp)?;
                colour2 = rle.color(bpp)?;
            }
            3 => colour2 = rle.color(bpp)?,
            6 | 7 => {
                mix = rle.color(bpp)?;
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
            if opcode == 3 {
                // REGULAR_COLOR_RUN can cover several scanlines. Write each
                // contiguous span in one fill while keeping the bottom-up rows.
                let span = count.min(width - x);
                let [blue, green, red] = colour2;
                out[start..start + span * 4]
                    .as_chunks_mut::<4>()
                    .0
                    .fill([blue, green, red, 0xff]);
                count -= span;
                x += span;
                continue;
            }
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
                4 => rle.color(bpp)?,
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
    if rows != 0 || x != width {
        return Err(invalid("incomplete interleaved bitmap"));
    }
    Ok((out, rle.at))
}

/// The 32-bit planar RLE: a marker byte, then four bit planes (alpha, red,
/// green, blue) whose rows are stored bottom-up with per-row deltas.
fn planar_rle(
    data: &[u8],
    width: usize,
    height: usize,
    planes: &[usize],
) -> io::Result<(Vec<u8>, usize)> {
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
    for pixel in out.as_chunks_mut::<4>().0 {
        pixel[3] = 0xff;
    }
    Ok((out, rle.at))
}

/// A signed delta, zig-zagged into one byte: 0 stays 0, 1 is -1, 2 is 1, ...
fn delta(byte: u8) -> i32 {
    if byte & 1 != 0 {
        -((i32::from(byte) >> 1) + 1)
    } else {
        i32::from(byte >> 1)
    }
}

/// Decodes one compressed rectangle into a packed BGRA image, top row first,
/// together with the number of bytes used by its codec.
pub(super) fn decompress(bitmap: &Bitmap<'_>) -> io::Result<(Vec<u8>, usize)> {
    let width = usize::from(bitmap.bitmaps.0);
    let height = usize::from(bitmap.bitmaps.1);
    let mut failure = None;
    // RDP6 also permits uncompressed planes inside a compressed rectangle.
    // The exact size distinguishes them from an interleaved RLE order.
    if bitmap.bpp == 32
        && let Some((&header @ (0 | 0x20), data)) = bitmap.data.split_first()
    {
        let plane_size = width * height;
        let has_alpha = header == 0;
        let plane_bytes = plane_size * if has_alpha { 4 } else { 3 };
        if data.len() == plane_bytes || data.len() == plane_bytes + 1 {
            let rgb = &data[if has_alpha { plane_size } else { 0 }..];
            let mut pixels = Vec::with_capacity(plane_size * 4);
            for row in (0..height).rev() {
                for i in row * width..(row + 1) * width {
                    pixels.extend_from_slice(&[
                        rgb[2 * plane_size + i],
                        rgb[plane_size + i],
                        rgb[i],
                        0xff,
                    ]);
                }
            }
            return Ok((pixels, bitmap.data.len()));
        }
    }
    if bitmap.bpp == 32
        && let Some(&marker @ (0x30 | 0x10)) = bitmap.data.first()
    {
        let planes: &[usize] = if marker == 0x30 {
            &[2, 1, 0]
        } else {
            &[3, 2, 1, 0]
        };
        match planar_rle(bitmap.data, width, height, planes) {
            Ok((pixels, used)) => return Ok((pixels, used)),
            Err(error) => failure = Some(error),
        }
    }
    match bitmap.bpp {
        15 | 16 | 24 | 32 => match legacy_rle(bitmap.data, width, height, bitmap.bpp) {
            Ok(result) => Ok(result),
            Err(error) => Err(failure.unwrap_or(error)),
        },
        _ => Err(failure.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "compressed bitmap depth was not negotiated",
            )
        })),
    }
}

/// Decodes the visible part of one bitmap, top row first.
pub(super) fn pixels(
    bitmap: &Bitmap<'_>,
    desktop_width: u16,
    desktop_height: u16,
) -> io::Result<Vec<u8>> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Err(invalid("empty RDP bitmap rectangle"));
    }
    if u32::from(bitmap.x) + u32::from(bitmap.width) > u32::from(desktop_width)
        || u32::from(bitmap.y) + u32::from(bitmap.height) > u32::from(desktop_height)
    {
        return Err(invalid("RDP bitmap lies outside the desktop"));
    }
    if !bitmap.compressed {
        return unpack(bitmap);
    }
    let (decoded, _) = decompress(bitmap).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{}x{} compressed bitmap: {error}",
                bitmap.bitmaps.0, bitmap.bitmaps.1
            ),
        )
    })?;
    if bitmap.bitmaps == (bitmap.width, bitmap.height) {
        return Ok(decoded);
    }
    // Only clipped patches need another allocation. Keep the mailbox allocation
    // proportional to the visible area even when the source bitmap was much larger.
    let row_bytes = usize::from(bitmap.width) * 4;
    let mut visible = Vec::with_capacity(row_bytes * usize::from(bitmap.height));
    for row in decoded
        .chunks_exact(usize::from(bitmap.bitmaps.0) * 4)
        .take(usize::from(bitmap.height))
    {
        visible.extend_from_slice(&row[..row_bytes]);
    }
    Ok(visible)
}

/// Copies visible pixels from bottom-up, four-byte-aligned scanlines.
fn unpack(bitmap: &Bitmap<'_>) -> io::Result<Vec<u8>> {
    let width = usize::from(bitmap.width);
    let height = usize::from(bitmap.height);
    let bytes_per_pixel = match bitmap.bpp {
        32 => 4,
        24 => 3,
        15 | 16 => 2,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "RDP bitmap depth was not negotiated",
            ));
        }
    };
    let source_height = usize::from(bitmap.bitmaps.1);
    let source_stride = (usize::from(bitmap.bitmaps.0) * bytes_per_pixel).next_multiple_of(4);
    let needed = source_stride
        .checked_mul(source_height)
        .ok_or_else(|| invalid("RDP bitmap size overflow"))?;
    if bitmap.data.len() < needed {
        return Err(invalid("truncated RDP bitmap data"));
    }
    let mut pixels = vec![0u8; width * height * 4];
    for line in 0..height {
        let offset = (source_height - 1 - line) * source_stride;
        let source = &bitmap.data[offset..offset + width * bytes_per_pixel];
        let target = &mut pixels[line * width * 4..(line + 1) * width * 4];
        match bitmap.bpp {
            32 => target.copy_from_slice(source),
            24 => {
                for (pixel, bytes) in target
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(source.as_chunks::<3>().0)
                {
                    pixel.copy_from_slice(&[bytes[0], bytes[1], bytes[2], 0xff]);
                }
            }
            15 | 16 => {
                for (pixel, bytes) in target
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(source.as_chunks::<2>().0)
                {
                    let [blue, green, red] = high_color(*bytes, bitmap.bpp);
                    pixel.copy_from_slice(&[blue, green, red, 0xff]);
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
    fn wire_lengths_separate_padded_raw_and_compressed_rectangles() {
        let mut wire = Vec::new();
        for field in [0u16, 0, 0, 1, 1, 2, 24, 0, 8] {
            wire.extend(field.to_le_bytes());
        }
        wire.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        for flags in [1, 0x401] {
            let header = if flags == 1 { 8 } else { 0 };
            for field in [1u16, 0, 1, 0, 1, 1, 24, flags, 4 + header] {
                wire.extend(field.to_le_bytes());
            }
            if header != 0 {
                wire.extend([0, 0, 4, 0, 4, 0, 4, 0]);
            }
            wire.extend([0x81, 7, 8, 9]);
        }
        let mut p = Packet(&wire);
        assert_eq!(
            pixels(&rect(&mut p).unwrap(), 2, 2).unwrap(),
            [4, 5, 6, 255, 1, 2, 3, 255]
        );
        for _ in 0..2 {
            assert_eq!(
                pixels(&rect(&mut p).unwrap(), 2, 2).unwrap(),
                [7, 8, 9, 255]
            );
        }
        assert!(p.0.is_empty());
    }

    #[test]
    fn a_legacy_rle_fill_and_copy_decode() {
        // Two rows of four black pixels: a fill order per row.
        let (pixels, used) = decompress(&bitmap(4, 2, 32, &[0x04, 0x04])).unwrap();
        assert_eq!(used, 2);
        assert_eq!(pixels, [0, 0, 0, 0xff].repeat(8));

        // One row of four copied pixels, then the same again for the next row.
        let mut data = vec![0x84];
        for value in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            data.push(value);
        }
        data.push(0x84);
        for value in [13u8, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24] {
            data.push(value);
        }
        let (pixels, _) = decompress(&bitmap(4, 2, 32, &data)).unwrap();
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
        let (pixels, used) = decompress(&bitmap(2, 1, 32, &data)).unwrap();
        assert_eq!(used, data.len());
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

    #[test]
    fn clipped_patches_keep_the_top_left_pixels_from_bottom_up_scanlines() {
        let raw = [9, 8, 7, 6, 5, 4, 0, 0, 1, 2, 3, 4, 5, 6, 0, 0];
        let rle = [0x84, 9, 8, 7, 6, 5, 4, 1, 2, 3, 4, 5, 6];
        for (data, compressed) in [(raw.as_slice(), false), (rle.as_slice(), true)] {
            let mut patch = bitmap(1, 1, 24, data);
            patch.bitmaps = (2, 2);
            patch.x = 639;
            patch.y = 479;
            patch.compressed = compressed;
            assert_eq!(pixels(&patch, 640, 480).unwrap(), [1, 2, 3, 255]);
            assert!(pixels(&patch, 639, 480).is_err());
            assert!(pixels(&patch, 640, 479).is_err());
        }
    }

    #[test]
    fn compressed_updates_cover_the_negotiated_color_depths() {
        for (bpp, green) in [(15, 0x03e0_u16), (16, 0x07e0)] {
            let [lo, hi] = green.to_le_bytes();
            let bytes = [0x61, lo, hi]; // one color run
            assert_eq!(
                pixels(&bitmap(1, 1, bpp, &bytes), 1, 1).unwrap(),
                [0, 255, 0, 255]
            );
        }
        for header in [0, 0x20] {
            let mut planes = vec![header];
            if header == 0 {
                planes.extend([255, 255]);
            }
            planes.extend([1, 2, 3, 4, 5, 6, 0]); // R, G, B planes and pad
            assert_eq!(
                pixels(&bitmap(1, 2, 32, &planes), 1, 2).unwrap(),
                [6, 4, 2, 255, 5, 3, 1, 255]
            );
        }
    }

    #[test]
    fn a_color_run_can_cross_scanlines_and_end_before_the_next_order() {
        let encoded = [0x65, 9, 8, 7, 0x61, 1, 2, 3];
        let mut expected = [9, 8, 7, 255].repeat(6);
        expected[8..12].copy_from_slice(&[1, 2, 3, 255]);
        assert_eq!(pixels(&bitmap(3, 2, 24, &encoded), 3, 2).unwrap(), expected);
    }
}
