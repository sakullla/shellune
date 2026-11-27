//! Decoding Sixel images (R4).
//!
//! A program draws an image by sending `ESC P <params> q <data> ESC \`. The data is a
//! stream of six-pixel-tall columns ("sixels"), one character each, with a few
//! commands mixed in:
//!
//! * `#n;2;r;g;b` defines colour register `n` in RGB percent (0-100), `#n;1;h;l;s`
//!   in HLS, and a bare `#n` selects it;
//! * `!count<sixel>` repeats one sixel `count` times;
//! * `$` returns to the left edge of the current band, `-` moves down one band;
//! * `"pan;pad;width;height` (raster attributes) states the image size.
//!
//! [`decode`] turns that into RGBA pixels. Everything is bounded: an image wider or
//! taller than [`MAX_SIDE`] pixels is refused, as is data that is not Sixel at all,
//! so a program can never make the terminal allocate without limit. Pixels no sixel
//! touched are transparent, the same as in other terminals with a transparent
//! background (`P2 = 1`); with `P2` other than 1 they take the background colour
//! the caller passes.

/// Largest width or height, in pixels, of an image that is kept.
pub const MAX_SIDE: usize = 4096;

/// How many colour registers a program may define.
const REGISTERS: usize = 1024;

/// A decoded image: `width * height` pixels, four bytes each, rows top first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SixelImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// RGBA, straight (not premultiplied) alpha.
    pub rgba: Vec<u8>,
}

/// Why an image was not decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SixelError {
    /// The image would be wider or taller than [`MAX_SIDE`].
    TooLarge,
    /// No pixel was drawn.
    Empty,
    /// A byte that has no meaning in Sixel data.
    Malformed,
}

impl std::fmt::Display for SixelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the image is larger than a terminal keeps",
            Self::Empty => "the image has no pixels",
            Self::Malformed => "the image data is not Sixel",
        })
    }
}

impl std::error::Error for SixelError {}

/// The parameters before `q`: `P1;P2;P3`. Only `P2` (background) changes pixels.
fn transparent_background(params: &[u8]) -> bool {
    let p2 = params
        .split(|byte| *byte == b';')
        .nth(1)
        .and_then(|field| std::str::from_utf8(field).ok())
        .and_then(|field| field.parse::<u32>().ok())
        .unwrap_or(0);
    p2 == 1
}

/// The 16 colours VT340 terminals start with, as RGB.
const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 33, 33],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [135, 135, 135],
    [66, 66, 66],
    [84, 84, 153],
    [153, 66, 66],
    [84, 153, 84],
    [153, 84, 153],
    [84, 153, 153],
    [153, 153, 84],
    [204, 204, 204],
];

fn percent(value: u32) -> u8 {
    ((value.min(100) * 255 + 50) / 100) as u8
}

/// HLS (hue 0-360 with blue at 0, lightness and saturation 0-100) to RGB, as DEC
/// terminals define it.
fn hls(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let l = f64::from(lightness.min(100)) / 100.0;
    let s = f64::from(saturation.min(100)) / 100.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    // DEC puts blue at 0 degrees; the usual formula puts red there.
    let h = f64::from((hue + 240) % 360) / 360.0;
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// Reads decimal parameters separated by `;` starting at `data[*at]`, leaving
/// `*at` on the first byte after them. Each value saturates instead of overflowing.
fn numbers(data: &[u8], at: &mut usize) -> Vec<u32> {
    let mut values = vec![0u32];
    while let Some(&byte) = data.get(*at) {
        match byte {
            b'0'..=b'9' => {
                let last = values.last_mut().expect("values is never empty");
                *last = last
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
            }
            b';' => {
                if values.len() >= 8 {
                    // More parameters than any command takes: the rest are ignored.
                    *at += 1;
                    continue;
                }
                values.push(0);
            }
            _ => break,
        }
        *at += 1;
    }
    values
}

/// Decodes one Sixel image.
///
/// `params` are the bytes between `ESC P` and `q`; `data` is everything after `q`
/// up to (not including) the string terminator. `background` fills pixels no sixel
/// touched unless the parameters ask for a transparent background.
///
/// # Errors
///
/// [`SixelError::TooLarge`] when the image grows past [`MAX_SIDE`] in either
/// direction, [`SixelError::Empty`] when nothing was drawn, and
/// [`SixelError::Malformed`] for bytes that are not Sixel.
pub fn decode(params: &[u8], data: &[u8], background: [u8; 3]) -> Result<SixelImage, SixelError> {
    let transparent = transparent_background(params);
    let mut palette = [[0u8; 3]; REGISTERS];
    palette[..16].copy_from_slice(&DEFAULT_PALETTE);
    let mut colour = palette[0];
    // Pixels are kept as palette-resolved RGB with a "set" flag, grown as needed.
    let mut width = 0usize;
    let mut height = 0usize;
    let mut stated: Option<(usize, usize)> = None;
    let mut pixels: Vec<Option<[u8; 3]>> = Vec::new();
    let mut x = 0usize;
    let mut band = 0usize;
    let mut at = 0usize;

    let mut grow = |pixels: &mut Vec<Option<[u8; 3]>>,
                    width: &mut usize,
                    height: &mut usize,
                    need_w: usize,
                    need_h: usize|
     -> Result<(), SixelError> {
        if need_w > MAX_SIDE || need_h > MAX_SIDE {
            return Err(SixelError::TooLarge);
        }
        if need_w <= *width && need_h <= *height {
            return Ok(());
        }
        let new_w = need_w.max(*width);
        let new_h = need_h.max(*height);
        let mut next = vec![None; new_w * new_h];
        for row in 0..*height {
            let from = row * *width;
            next[row * new_w..row * new_w + *width].copy_from_slice(&pixels[from..from + *width]);
        }
        *pixels = next;
        *width = new_w;
        *height = new_h;
        Ok(())
    };

    while at < data.len() {
        let byte = data[at];
        match byte {
            b'"' => {
                at += 1;
                let values = numbers(data, &mut at);
                if let (Some(&w), Some(&h)) = (values.get(2), values.get(3))
                    && w > 0
                    && h > 0
                {
                    let (w, h) = (w as usize, h as usize);
                    if w > MAX_SIDE || h > MAX_SIDE {
                        return Err(SixelError::TooLarge);
                    }
                    stated = Some((w, h));
                }
            }
            b'#' => {
                at += 1;
                let values = numbers(data, &mut at);
                let register = values[0] as usize % REGISTERS;
                if values.len() >= 5 {
                    palette[register] = match values[1] {
                        1 => hls(values[2], values[3], values[4]),
                        2 => [percent(values[2]), percent(values[3]), percent(values[4])],
                        _ => palette[register],
                    };
                }
                colour = palette[register];
            }
            b'!' => {
                at += 1;
                let count = numbers(data, &mut at)[0].max(1) as usize;
                let Some(&sixel) = data.get(at) else {
                    break;
                };
                if !(b'?'..=b'~').contains(&sixel) {
                    return Err(SixelError::Malformed);
                }
                at += 1;
                if x.saturating_add(count) > MAX_SIDE {
                    return Err(SixelError::TooLarge);
                }
                draw(
                    &mut grow,
                    &mut pixels,
                    &mut width,
                    &mut height,
                    sixel - b'?',
                    x,
                    count,
                    band,
                    colour,
                )?;
                x += count;
            }
            b'?'..=b'~' => {
                at += 1;
                draw(
                    &mut grow,
                    &mut pixels,
                    &mut width,
                    &mut height,
                    byte - b'?',
                    x,
                    1,
                    band,
                    colour,
                )?;
                x += 1;
            }
            b'$' => {
                at += 1;
                x = 0;
            }
            b'-' => {
                at += 1;
                x = 0;
                band += 1;
                if band.saturating_mul(6) > MAX_SIDE {
                    return Err(SixelError::TooLarge);
                }
            }
            // Line breaks and other whitespace some encoders put between bands.
            b'\r' | b'\n' | b' ' | b'\t' => at += 1,
            _ => return Err(SixelError::Malformed),
        }
    }

    // A stated size is the image's size, as other decoders read it: encoders draw
    // whole six-pixel bands and state the real height, and some state a margin
    // they leave untouched. Pixels drawn past it are cut off.
    if let Some((w, h)) = stated {
        grow(&mut pixels, &mut width, &mut height, w, h)?;
        if w < width || h < height {
            let mut cropped = Vec::with_capacity(w * h);
            for row in 0..h {
                cropped.extend_from_slice(&pixels[row * width..row * width + w]);
            }
            pixels = cropped;
            width = w;
            height = h;
        }
    }
    if width == 0 || height == 0 || pixels.iter().all(Option::is_none) {
        return Err(SixelError::Empty);
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &pixels {
        match pixel {
            Some([r, g, b]) => rgba.extend_from_slice(&[*r, *g, *b, 255]),
            None if transparent => rgba.extend_from_slice(&[0, 0, 0, 0]),
            None => rgba.extend_from_slice(&[background[0], background[1], background[2], 255]),
        }
    }
    Ok(SixelImage {
        width,
        height,
        rgba,
    })
}

/// Paints `count` copies of one sixel (six vertical pixels, bit 0 on top).
#[allow(clippy::too_many_arguments)]
fn draw(
    grow: &mut impl FnMut(
        &mut Vec<Option<[u8; 3]>>,
        &mut usize,
        &mut usize,
        usize,
        usize,
    ) -> Result<(), SixelError>,
    pixels: &mut Vec<Option<[u8; 3]>>,
    width: &mut usize,
    height: &mut usize,
    bits: u8,
    x: usize,
    count: usize,
    band: usize,
    colour: [u8; 3],
) -> Result<(), SixelError> {
    if bits == 0 {
        // Still advances the image's width, as the column exists.
        return grow(pixels, width, height, x + count, *height);
    }
    let top = band * 6;
    grow(pixels, width, height, x + count, top + 6)?;
    for bit in 0..6 {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let row = top + bit;
        let start = row * *width + x;
        for pixel in &mut pixels[start..start + count] {
            *pixel = Some(colour);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [u8; 3] = [255, 255, 255];

    fn pixel(image: &SixelImage, x: usize, y: usize) -> [u8; 4] {
        let at = (y * image.width + x) * 4;
        [
            image.rgba[at],
            image.rgba[at + 1],
            image.rgba[at + 2],
            image.rgba[at + 3],
        ]
    }

    #[test]
    fn a_full_sixel_in_an_rgb_colour_is_six_pixels_of_that_colour() {
        let image = decode(b"0;1;0", b"#1;2;100;0;0#1~", WHITE).expect("decodes");
        assert_eq!((image.width, image.height), (1, 6));
        for y in 0..6 {
            assert_eq!(pixel(&image, 0, y), [255, 0, 0, 255]);
        }
    }

    #[test]
    fn repeats_carriage_returns_and_bands_place_pixels_where_they_belong() {
        // Band 0: three columns of the top pixel in red; then, over it, the second
        // pixel of column 1 in green. Band 1: one bottom pixel in blue.
        let data = b"#1;2;100;0;0#2;2;0;100;0#3;2;0;0;100#1!3@$#2?A-#3_";
        let image = decode(b"0;1", data, WHITE).expect("decodes");
        assert_eq!((image.width, image.height), (3, 12));
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 2, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&image, 0, 11), [0, 0, 255, 255]);
        // Untouched pixels are transparent with P2 = 1.
        assert_eq!(pixel(&image, 2, 5)[3], 0);
    }

    #[test]
    fn an_opaque_background_fills_what_was_not_drawn() {
        let image = decode(b"0;0", b"#1;2;0;0;0#1@", [10, 20, 30]).expect("decodes");
        assert_eq!(pixel(&image, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&image, 0, 1), [10, 20, 30, 255]);
    }

    #[test]
    fn hls_colours_follow_dec_with_blue_at_zero_degrees() {
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
        assert_eq!(hls(0, 100, 0), [255, 255, 255]);
        let image = decode(b"0;1", b"#5;1;120;50;100#5~", WHITE).expect("decodes");
        assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn raster_attributes_state_a_size_with_a_transparent_margin() {
        let image = decode(b"0;1", b"\"1;1;4;6#1~", WHITE).expect("decodes");
        assert_eq!((image.width, image.height), (4, 6));
        assert_eq!(pixel(&image, 3, 0)[3], 0);
    }

    #[test]
    fn a_stated_size_smaller_than_the_drawn_bands_crops_them() {
        // One full band is six pixels; the image says it is four tall.
        let image = decode(b"0;1", b"\"1;1;2;4#1~~", WHITE).expect("decodes");
        assert_eq!((image.width, image.height), (2, 4));
        assert_eq!(image.rgba.len(), 2 * 4 * 4);
        // Register 1 keeps its VT340 default when the data does not redefine it.
        assert_eq!(pixel(&image, 1, 3), [51, 51, 204, 255]);
    }

    #[test]
    fn an_image_past_the_limit_is_refused_before_it_is_allocated() {
        assert_eq!(
            decode(b"", b"\"1;1;5000;10#1~", WHITE),
            Err(SixelError::TooLarge)
        );
        assert_eq!(decode(b"", b"#1!5000~", WHITE), Err(SixelError::TooLarge));
        let tall: Vec<u8> = std::iter::repeat_n(b"~-".as_slice(), 700)
            .flatten()
            .copied()
            .collect();
        assert_eq!(decode(b"", &tall, WHITE), Err(SixelError::TooLarge));
    }

    #[test]
    fn data_that_is_not_sixel_or_draws_nothing_is_refused() {
        assert_eq!(decode(b"", b"\x01\x02", WHITE), Err(SixelError::Malformed));
        assert_eq!(decode(b"", b"#1;2;0;0;0", WHITE), Err(SixelError::Empty));
        assert_eq!(decode(b"", b"", WHITE), Err(SixelError::Empty));
        assert_eq!(decode(b"", b"???", WHITE), Err(SixelError::Empty));
        // A repeat cut off at the end draws what came before it.
        let image = decode(b"0;1", b"#1~!5", WHITE).expect("decodes the first column");
        assert_eq!(image.width, 1);
    }

    #[test]
    fn huge_numbers_saturate_instead_of_overflowing() {
        let image = decode(b"0;1", b"#99999999999;2;999;999;999#99999999999~", WHITE);
        let image = image.expect("decodes");
        assert_eq!(pixel(&image, 0, 0), [255, 255, 255, 255]);
    }
}
