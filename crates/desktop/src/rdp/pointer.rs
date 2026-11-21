//! Color and cached pointer updates for both RDP output paths. Stores server-supplied shapes and their hotspots.
use super::wire::{Packet, invalid};
use crate::PointerShape;
use std::io;

pub(super) const CACHE_SIZE: u16 = 20;

#[derive(Default)]
pub(super) struct Pointers {
    cache: [Option<PointerShape>; CACHE_SIZE as usize],
    palette: Vec<[u8; 3]>,
}

impl Pointers {
    pub(super) fn update(&mut self, data: &[u8]) -> io::Result<Option<PointerShape>> {
        let mut p = Packet(data);
        let kind = p.le16()?;
        p.take(2)?;
        let shape = match kind {
            1 => match p.le32()? {
                0 => PointerShape::Hidden,
                0x7f00 => PointerShape::Default,
                _ => return Ok(None),
            },
            6 => return self.shape(24, &mut p).map(Some),
            7 => return self.cached(&mut p),
            8 => return self.new_pointer(&mut p).map(Some),
            // Remote position notifications must not rewind local input in flight.
            _ => return Ok(None),
        };
        Ok(Some(shape))
    }

    pub(super) fn cached(&self, p: &mut Packet<'_>) -> io::Result<Option<PointerShape>> {
        Ok(self
            .cache
            .get(usize::from(p.le16()?))
            .and_then(Clone::clone))
    }

    pub(super) fn new_pointer(&mut self, p: &mut Packet<'_>) -> io::Result<PointerShape> {
        let depth = p.le16()?;
        self.shape(depth, p)
    }

    pub(super) fn palette(&mut self, p: &mut Packet<'_>) -> io::Result<()> {
        p.take(2)?; // padding after updateType
        let count = p.le32()? as usize;
        if count > 256 {
            return Err(invalid("invalid pointer palette size"));
        }
        self.palette = p
            .take(count * 3)?
            .as_chunks::<3>()
            .0
            .iter()
            .map(|rgb| [rgb[2], rgb[1], rgb[0]])
            .collect();
        Ok(())
    }

    pub(super) fn shape(&mut self, depth: u16, p: &mut Packet<'_>) -> io::Result<PointerShape> {
        let index = usize::from(p.le16()?);
        let slot = self
            .cache
            .get_mut(index)
            .ok_or_else(|| invalid("pointer cache index out of range"))?;
        let shape = color_pointer(p, depth, &self.palette)?;
        *slot = Some(shape.clone());
        Ok(shape)
    }
}

fn color_pointer(p: &mut Packet<'_>, depth: u16, palette: &[[u8; 3]]) -> io::Result<PointerShape> {
    if !matches!(depth, 1 | 4 | 8 | 15 | 16 | 24 | 32) {
        return Err(invalid("invalid pointer depth"));
    }
    let hotspot = (p.le16()?, p.le16()?);
    let (width, height) = (p.le16()?, p.le16()?);
    let (w, h) = (usize::from(width), usize::from(height));
    let (and_len, xor_len) = (usize::from(p.le16()?), usize::from(p.le16()?));
    let bits = if depth == 15 { 16 } else { usize::from(depth) };
    let xor_stride = (w * bits).div_ceil(8).next_multiple_of(2);
    let and_stride = w.div_ceil(8).next_multiple_of(2);
    if w == 0
        || h == 0
        || w > 96
        || h > 96
        || hotspot.0 >= width
        || hotspot.1 >= height
        || xor_len != xor_stride * h
        || (and_len != and_stride * h && !(depth == 32 && and_len == 0))
    {
        return Err(invalid("invalid RDP pointer dimensions or masks"));
    }
    let xor = p.take(xor_len)?;
    let and = p.take(and_len)?;
    let uses_alpha =
        depth == 32 && (and.is_empty() || xor.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
    let mut pixels = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        // Windows monochrome pointers are top-down; color bitmaps are bottom-up.
        let row = if depth == 1 { y } else { h - 1 - y };
        let line = &xor[row * xor_stride..(row + 1) * xor_stride];
        for col in 0..w {
            let bit = 0x80 >> (col % 8);
            let masked = !and.is_empty() && and[row * and_stride + col / 8] & bit != 0;
            let (bgr, alpha) = match depth {
                1 => ([if line[col / 8] & bit != 0 { 255 } else { 0 }; 3], 255),
                4 | 8 => {
                    let index = if depth == 8 {
                        line[col]
                    } else {
                        (line[col / 2] >> (4 * (1 - col % 2))) & 15
                    };
                    (
                        palette
                            .get(usize::from(index))
                            .copied()
                            .unwrap_or([if depth == 4 { index * 17 } else { index }; 3]),
                        255,
                    )
                }
                15 | 16 => {
                    let value = u16::from_le_bytes([line[col * 2], line[col * 2 + 1]]);
                    let expand = |n: u16| ((n << 3) | (n >> 2)) as u8;
                    (
                        [
                            expand(value & 31),
                            expand((value >> 5) & 31),
                            expand((value >> 10) & 31),
                        ],
                        255,
                    )
                }
                24 => ([line[col * 3], line[col * 3 + 1], line[col * 3 + 2]], 255),
                32 => (
                    [line[col * 4], line[col * 4 + 1], line[col * 4 + 2]],
                    if uses_alpha { line[col * 4 + 3] } else { 255 },
                ),
                _ => unreachable!(),
            };
            // XOR-inverted pixels have no fixed RGB value on a transparent
            // cursor. A black/white pattern remains visible on either background.
            let pixel = if uses_alpha {
                [bgr[0], bgr[1], bgr[2], alpha]
            } else if masked && bgr == [0; 3] {
                [0; 4]
            } else if masked && bgr == [255; 3] {
                let v = if (col + y) % 2 == 0 { 0 } else { 255 };
                [v, v, v, 255]
            } else {
                [bgr[0], bgr[1], bgr[2], 255]
            };
            pixels.extend_from_slice(&pixel);
        }
    }
    Ok(PointerShape::Bitmap {
        width,
        height,
        hotspot,
        pixels: pixels.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_resize_cursor_keeps_orientation_and_transparent_edges() {
        // Three pixels wide, hot spot on the lower row. Each scanline is word-aligned.
        let mut pdu = Vec::new();
        for word in [8u16, 0, 1, 2, 1, 1, 3, 2, 4, 4] {
            pdu.extend_from_slice(&word.to_le_bytes());
        }
        pdu.extend_from_slice(&[0x40, 0, 0, 0, 0xa0, 0, 0x40, 0]);
        let mut pointers = Pointers::default();
        let shape = pointers.update(&pdu).unwrap().unwrap();
        let PointerShape::Bitmap {
            hotspot, pixels, ..
        } = &shape
        else {
            panic!("resize bitmap")
        };
        assert_eq!(*hotspot, (1, 1));
        assert_eq!(pixels[..12], [0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0]);
        assert_eq!(pixels[12..], [0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 255]);
        assert_eq!(pointers.update(&[7, 0, 0, 0, 2, 0]).unwrap(), Some(shape));
    }

    #[test]
    fn color_pointer_preserves_hotspot_mask_orientation_and_cache() {
        // Two rows of one BGR pixel. Both masks are padded to 16-bit rows.
        let mut bytes = vec![6, 0, 0, 0, 3, 0, 0, 0, 1, 0, 1, 0, 2, 0, 4, 0, 8, 0];
        bytes.extend_from_slice(&[0, 0, 0, 0, 7, 8, 9, 0, 0x80, 0, 0, 0]);
        let mut pointers = Pointers::default();
        let shape = pointers.update(&bytes).unwrap().unwrap();
        let PointerShape::Bitmap {
            hotspot, pixels, ..
        } = &shape
        else {
            panic!("bitmap");
        };
        assert_eq!(*hotspot, (0, 1));
        assert_eq!(&**pixels, &[7, 8, 9, 255, 0, 0, 0, 0]);
        assert_eq!(pointers.update(&[7, 0, 0, 0, 3, 0]).unwrap(), Some(shape));
        assert_eq!(
            pointers.update(&[1, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
            Some(PointerShape::Hidden)
        );
        assert!(pointers.update(&bytes[..bytes.len() - 1]).is_err());
    }
}
