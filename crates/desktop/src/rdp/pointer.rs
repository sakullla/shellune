//! Color and cached pointer updates for both RDP output paths. We negotiate
//! color pointers without the optional multi-depth pointer capability.
use super::wire::{Packet, invalid};
use crate::PointerShape;
use std::io;

pub(super) const CACHE_SIZE: u16 = 20;

#[derive(Default)]
pub(super) struct Pointers {
    cache: [Option<PointerShape>; CACHE_SIZE as usize],
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
            6 | 7 => return self.shape(kind == 7, &mut p),
            // Remote position notifications must not rewind local input in flight.
            _ => return Ok(None),
        };
        Ok(Some(shape))
    }

    pub(super) fn shape(
        &mut self,
        cached: bool,
        p: &mut Packet<'_>,
    ) -> io::Result<Option<PointerShape>> {
        let index = usize::from(p.le16()?);
        if cached {
            return Ok(self.cache.get(index).and_then(Clone::clone));
        }
        let slot = self
            .cache
            .get_mut(index)
            .ok_or_else(|| invalid("pointer cache index out of range"))?;
        let shape = color_pointer(p)?;
        *slot = Some(shape.clone());
        Ok(Some(shape))
    }
}

fn color_pointer(p: &mut Packet<'_>) -> io::Result<PointerShape> {
    let hotspot = (p.le16()?, p.le16()?);
    let (width, height) = (p.le16()?, p.le16()?);
    let (w, h) = (usize::from(width), usize::from(height));
    let (and_len, xor_len) = (usize::from(p.le16()?), usize::from(p.le16()?));
    let xor_stride = (w * 3).next_multiple_of(2);
    let and_stride = w.div_ceil(8).next_multiple_of(2);
    if w == 0
        || h == 0
        || w > 96
        || h > 96
        || hotspot.0 >= width
        || hotspot.1 >= height
        || xor_len != xor_stride * h
        || and_len != and_stride * h
    {
        return Err(invalid("invalid 24-bit color pointer"));
    }
    let xor = p.take(xor_len)?;
    let and = p.take(and_len)?;
    let mut pixels = Vec::with_capacity(w * h * 4);
    for row in (0..h).rev() {
        for col in 0..w {
            let at = row * xor_stride + col * 3;
            let bgr = &xor[at..at + 3];
            let masked = and[row * and_stride + col / 8] & (0x80 >> (col % 8)) != 0;
            // An XOR inversion has no fixed color on an alpha-composited cursor.
            // Keep its foreground visible when the destination color is unknown.
            pixels.extend_from_slice(bgr);
            pixels.push(if masked && bgr == [0, 0, 0] { 0 } else { 255 });
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
