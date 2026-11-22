//! RFC 6143 ZRLE: a connection-wide zlib stream of independent 64×64 tiles.
use super::wire::Rect;
use flate2::{Decompress, FlushDecompress, Status};
use std::io::{self, ErrorKind, Read};

pub(super) struct Rectangle {
    x: u16,
    y: u16,
    width: usize,
    height: usize,
    col: usize,
    row: usize,
    remaining: usize,
    plain: Vec<u8>,
    offset: usize,
    retry_at: usize,
}

impl Rectangle {
    pub(super) fn new(x: u16, y: u16, w: u16, h: u16, bytes: usize) -> Self {
        Self {
            x,
            y,
            width: w.into(),
            height: h.into(),
            col: 0,
            row: 0,
            remaining: bytes,
            plain: Vec::new(),
            offset: 0,
            retry_at: 1,
        }
    }

    // A rectangle is complete after both its tiles and the zlib flush bytes.
    pub(super) fn take(
        &mut self,
        z: &mut Decompress,
        input: &mut &[u8],
    ) -> Result<(Option<Rect>, bool), String> {
        loop {
            let available = &self.plain[self.offset..];
            if self.row < self.height
                && !available.is_empty()
                && (available.len() >= self.retry_at || self.remaining == 0)
            {
                let w = (self.width - self.col).min(64);
                let h = (self.height - self.row).min(64);
                let mut src = available;
                match decode_tile(&mut src, w, h) {
                    Ok(pixels) => {
                        self.offset += available.len() - src.len();
                        self.retry_at = 1;
                        let rect = Rect::Raw {
                            x: self.x + self.col as u16,
                            y: self.y + self.row as u16,
                            width: w as u16,
                            height: h as u16,
                            pixels,
                        };
                        self.col += w;
                        if self.col == self.width {
                            self.col = 0;
                            self.row += h;
                        }
                        return Ok((Some(rect), false));
                    }
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                        // Raw and packed-palette tile lengths are known. Do not
                        // postpone a complete tile until bytes of the next arrive.
                        self.retry_at = match available[0] {
                            0 => 1 + w * h * 3,
                            1 => 4,
                            colors @ 2..=16 => {
                                let bits = if colors == 2 {
                                    1
                                } else if colors <= 4 {
                                    2
                                } else {
                                    4
                                };
                                1 + usize::from(colors) * 3 + h * (w * bits).div_ceil(8)
                            }
                            _ => (available.len() * 2).min(32768),
                        }
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            if self.row == self.height && !available.is_empty() {
                return Err("trailing ZRLE tile data".into());
            }
            if input.is_empty() && self.remaining != 0 {
                return Ok((None, false));
            }
            let n = self.remaining.min(input.len());
            let before = (z.total_in(), z.total_out());
            let mut chunk = [0; 16 * 1024];
            let status = z
                .decompress(&input[..n], &mut chunk, FlushDecompress::None)
                .map_err(|e| e.to_string())?;
            let used = (z.total_in() - before.0) as usize;
            let produced = (z.total_out() - before.1) as usize;
            *input = &input[used..];
            self.remaining -= used;
            if status == Status::StreamEnd {
                if self.remaining != 0 {
                    return Err("trailing ZRLE compressed bytes".into());
                }
                z.reset(true);
            }
            if used == 0 && produced == 0 {
                if self.row == self.height && self.remaining == 0 {
                    return Ok((None, true));
                }
                if self.remaining > n {
                    return Ok((None, false));
                }
                return Err("incomplete ZRLE rectangle".into());
            }
            if self.offset != 0 {
                self.plain.drain(..self.offset);
                self.offset = 0;
            }
            self.plain.extend_from_slice(&chunk[..produced]);
        }
    }
}

fn byte(src: &mut &[u8]) -> io::Result<u8> {
    let mut value = [0];
    src.read_exact(&mut value)?;
    Ok(value[0])
}

fn color(src: &mut &[u8]) -> io::Result<[u8; 4]> {
    let mut pixel = [255; 4];
    src.read_exact(&mut pixel[..3])?;
    Ok(pixel)
}

fn invalid() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "invalid ZRLE tile")
}

fn decode_tile(src: &mut &[u8], w: usize, h: usize) -> io::Result<Vec<u8>> {
    let mode = byte(src)?;
    if matches!(mode, 17..=127 | 129) {
        return Err(invalid());
    }
    let colors = usize::from(mode & 127);
    let mut palette = [[0u8; 4]; 127];
    for entry in &mut palette[..colors] {
        *entry = color(src)?;
    }
    let mut out = Vec::with_capacity(w * h);
    match mode {
        0 => {
            let bytes = w * h * 3;
            let (raw, rest) = src
                .split_at_checked(bytes)
                .ok_or(ErrorKind::UnexpectedEof)?;
            *src = rest;
            out.extend(
                raw.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|&[b, g, r]| [b, g, r, 255]),
            );
        }
        1 => out.resize(w * h, palette[0]),
        2..=16 => {
            let bits = if colors == 2 {
                1
            } else if colors <= 4 {
                2
            } else {
                4
            };
            for _ in 0..h {
                let mut packed = 0;
                for x in 0..w {
                    if x * bits % 8 == 0 {
                        packed = byte(src)?;
                    }
                    let index = usize::from(packed >> (8 - bits));
                    if index >= colors {
                        return Err(invalid());
                    }
                    out.push(palette[index]);
                    packed <<= bits;
                }
            }
        }
        _ => {
            while out.len() < w * h {
                let (pixel, run) = if colors == 0 {
                    (color(src)?, true)
                } else {
                    let idx = byte(src)?;
                    let pixel = palette[..colors]
                        .get(usize::from(idx & 127))
                        .ok_or_else(invalid)?;
                    (*pixel, idx & 128 != 0)
                };
                let mut end = out.len() + 1;
                if run {
                    loop {
                        let n = byte(src)?;
                        end += usize::from(n);
                        if end > w * h {
                            return Err(invalid());
                        }
                        if n != 255 {
                            break;
                        }
                    }
                }
                out.resize(end, pixel);
            }
        }
    }
    Ok(out.into_flattened())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn packed_rows_and_rle_runs_preserve_pixel_order() {
        let a = [1, 2, 3, 255];
        let b = [4, 5, 6, 255];
        for (data, w, h, expected) in [
            (vec![0, 1, 2, 3, 4, 5, 6], 2, 1, vec![a, b]),
            (vec![1, 1, 2, 3], 3, 1, vec![a, a, a]),
            (
                vec![2, 1, 2, 3, 4, 5, 6, 0x40, 0xa0],
                3,
                2,
                vec![a, b, a, b, a, b],
            ),
            (vec![128, 1, 2, 3, 1, 4, 5, 6, 0], 3, 1, vec![a, a, b]),
            (vec![130, 1, 2, 3, 4, 5, 6, 128, 1, 1], 3, 1, vec![a, a, b]),
        ] {
            let mut src = data.as_slice();
            assert_eq!(
                decode_tile(&mut src, w, h).unwrap(),
                expected.into_flattened()
            );
            assert!(src.is_empty());
        }
        assert!(decode_tile(&mut &[129][..], 1, 1).is_err());
        assert!(decode_tile(&mut &[128, 1, 2, 3, 2][..], 1, 1).is_err());
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_complete_raw_tile_does_not_wait_for_bytes_of_the_next_tile() {
        let mut z = Decompress::new(true);
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        // Separate compressed flushes give the decoder part of the first tile,
        // its exact remainder, then a second tile that must not be needed yet.
        let mut raw = vec![0; 1 + 64 * 64 * 3];
        for (i, b) in raw[1..].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        encoder.write_all(&raw[..8000]).unwrap();
        encoder.flush().unwrap();
        let first = encoder.get_ref().len();
        encoder.write_all(&raw[8000..]).unwrap();
        encoder.flush().unwrap();
        let second = encoder.get_ref().len();
        encoder.write_all(&[1, 7, 8, 9]).unwrap();
        encoder.flush().unwrap();
        let data = encoder.get_ref();
        let mut rect = Rectangle::new(0, 0, 65, 64, data.len());
        assert!(rect.take(&mut z, &mut &data[..first]).unwrap().0.is_none());
        let (tile, ended) = rect.take(&mut z, &mut &data[first..second]).unwrap();
        assert!(matches!(
            tile,
            Some(Rect::Raw {
                x: 0,
                width: 64,
                height: 64,
                ..
            })
        ));
        assert!(!ended);
        let mut tail = &data[second..];
        let (tile, _) = rect.take(&mut z, &mut tail).unwrap();
        assert!(matches!(
            tile,
            Some(Rect::Raw {
                x: 64,
                width: 1,
                ..
            })
        ));
        assert!(rect.take(&mut z, &mut tail).unwrap().1);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn fragmented_rectangles_share_one_zlib_dictionary() {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut z = Decompress::new(true);
        let mut pos = 0;
        for _ in 0..2 {
            encoder.write_all(&[1, 5, 6, 7, 1, 9, 8, 7]).unwrap();
            encoder.flush().unwrap();
            let bytes = &encoder.get_ref()[pos..];
            let mut rectangle = Rectangle::new(1, 2, 65, 1, bytes.len());
            let mut pending = Vec::new();
            let mut tiles = Vec::new();
            let mut ended = false;
            for &byte in bytes {
                pending.push(byte);
                let mut input = pending.as_slice();
                loop {
                    let (tile, done) = rectangle.take(&mut z, &mut input).unwrap();
                    if let Some(Rect::Raw {
                        x,
                        y,
                        width,
                        pixels,
                        ..
                    }) = tile
                    {
                        tiles.push((x, y, width, pixels));
                        continue;
                    }
                    ended = done;
                    break;
                }
                let used = pending.len() - input.len();
                pending.drain(..used);
            }
            assert!(ended && pending.is_empty());
            assert_eq!(
                tiles,
                vec![
                    (1, 2, 64, [5, 6, 7, 255].repeat(64)),
                    (65, 2, 1, vec![9, 8, 7, 255])
                ]
            );
            pos = encoder.get_ref().len();
        }
    }
}
