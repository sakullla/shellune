//! Device-independent bitmaps, the `CF_DIB`/`CF_DIBV5` clipboard formats, to and from
//! the images GPUI keeps.
//!
//! A clipboard DIB is a BMP file without its 14-byte file header: a bitmap header,
//! optional colour masks and palette, then the pixel rows. Screenshots (PrintScreen,
//! the snipping overlay) and many older programs put only this on the clipboard, and
//! many programs (Paint, Office) paste only this, so the clipboard reads it when no
//! encoded image is offered and writes it beside PNG.
//!
//! This file depends on nothing but `image` and `anyhow`, so its tests can be run on
//! their own; see `vendor/README.md`.

use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, ImageDecoder, RgbaImage, codecs::bmp::BmpDecoder};

/// The largest decoded image a clipboard DIB may claim, in bytes of RGBA. A header
/// is only a few numbers; this stops one from asking for an absurd allocation.
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// The largest DIB written beside an image, in bytes of pixel rows. A larger image
/// is still written in its own format; only the bitmap copy is left out.
const MAX_WRITTEN_DIB_BYTES: u64 = 256 * 1024 * 1024;

/// `sizeof(BITMAPINFOHEADER)`.
const BITMAPINFOHEADER_SIZE: u32 = 40;
/// `BI_RGB`: uncompressed pixels with no masks.
const BI_RGB: u32 = 0;

/// Decodes a clipboard DIB (the contents of `CF_DIB` or `CF_DIBV5`) to PNG bytes.
///
/// A 32-bit DIB whose alpha channel is zero everywhere is made opaque: such bitmaps
/// keep the fourth byte reserved, and reading it as alpha would paste an invisible
/// image.
pub(crate) fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>> {
    let decoder = BmpDecoder::new_without_file_header(std::io::Cursor::new(dib))
        .context("clipboard bitmap header is not readable")?;
    if decoder.total_bytes() > MAX_DECODED_BYTES {
        let (width, height) = decoder.dimensions();
        bail!("clipboard bitmap is too large ({width}x{height})");
    }
    let image =
        DynamicImage::from_decoder(decoder).context("clipboard bitmap pixels are not readable")?;
    let image = opaque_if_alpha_unused(image);

    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .context("clipboard bitmap could not be encoded as PNG")?;
    Ok(png)
}

/// The image with an opaque alpha channel when it has one that is zero everywhere.
fn opaque_if_alpha_unused(image: DynamicImage) -> DynamicImage {
    if !image.color().has_alpha() {
        return image;
    }
    let mut rgba = image.into_rgba8();
    if rgba.pixels().all(|pixel| pixel.0[3] == 0) {
        for pixel in rgba.pixels_mut() {
            pixel.0[3] = u8::MAX;
        }
    }
    DynamicImage::ImageRgba8(rgba)
}

/// Encodes an image as a clipboard DIB: a `BITMAPINFOHEADER`, then 32-bit `BI_RGB`
/// rows bottom-up in blue, green, red, alpha order.
///
/// `BI_RGB` keeps the alpha byte where programs that understand it find it, while
/// programs that do not treat the bitmap as opaque. `None` when the image is too
/// large to be worth a second copy on the clipboard.
pub(crate) fn image_to_dib(image: &DynamicImage) -> Option<Vec<u8>> {
    let rgba = image.to_rgba8();
    rgba_to_dib(&rgba)
}

fn rgba_to_dib(rgba: &RgbaImage) -> Option<Vec<u8>> {
    let (width, height) = rgba.dimensions();
    let row_bytes = u64::from(width) * 4;
    let pixel_bytes = row_bytes * u64::from(height);
    if width == 0 || height == 0 || pixel_bytes > MAX_WRITTEN_DIB_BYTES {
        return None;
    }
    let signed_width = i32::try_from(width).ok()?;
    let signed_height = i32::try_from(height).ok()?;
    let image_size = u32::try_from(pixel_bytes).ok()?;

    let mut dib = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize + image_size as usize);
    dib.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes()); // biSize
    dib.extend_from_slice(&signed_width.to_le_bytes()); // biWidth
    // A positive height: rows are stored bottom-up, which every reader accepts.
    dib.extend_from_slice(&signed_height.to_le_bytes()); // biHeight
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&BI_RGB.to_le_bytes()); // biCompression
    dib.extend_from_slice(&image_size.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // Rows are four bytes per pixel, so they need no padding to a DWORD boundary.
    for row in rgba.rows().rev() {
        for pixel in row {
            let [red, green, blue, alpha] = pixel.0;
            dib.extend_from_slice(&[blue, green, red, alpha]);
        }
    }
    Some(dib)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// A `BITMAPINFOHEADER` for the given geometry.
    fn info_header(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&40u32.to_le_bytes());
        header.extend_from_slice(&width.to_le_bytes());
        header.extend_from_slice(&height.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&bit_count.to_le_bytes());
        header.extend_from_slice(&compression.to_le_bytes());
        header.extend_from_slice(&[0; 20]);
        header
    }

    fn decode_png(png: &[u8]) -> RgbaImage {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "the result is a PNG file");
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .expect("PNG decodes")
            .to_rgba8()
    }

    #[test]
    fn a_24_bit_bottom_up_dib_with_row_padding_becomes_the_same_png() {
        // 3x2: each row is 9 bytes of pixels padded to 12. The last row in the
        // buffer is the top of the image.
        let mut dib = info_header(3, 2, 24, BI_RGB);
        // Bottom row: blue, green, red (stored B, G, R).
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
        // Top row: white, black, grey.
        dib.extend_from_slice(&[255, 255, 255, 0, 0, 0, 128, 128, 128, 0, 0, 0]);

        let image = decode_png(&dib_to_png(&dib).expect("decodes"));

        assert_eq!(image.dimensions(), (3, 2));
        assert_eq!(image.get_pixel(0, 0), &Rgba([255, 255, 255, 255]));
        assert_eq!(image.get_pixel(1, 0), &Rgba([0, 0, 0, 255]));
        assert_eq!(image.get_pixel(2, 0), &Rgba([128, 128, 128, 255]));
        assert_eq!(image.get_pixel(0, 1), &Rgba([0, 0, 255, 255]));
        assert_eq!(image.get_pixel(1, 1), &Rgba([0, 255, 0, 255]));
        assert_eq!(image.get_pixel(2, 1), &Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn a_top_down_dib_keeps_its_row_order() {
        let mut dib = info_header(1, -2, 24, BI_RGB);
        dib.extend_from_slice(&[0, 0, 255, 0]); // first row: red, on top
        dib.extend_from_slice(&[255, 0, 0, 0]); // second row: blue

        let image = decode_png(&dib_to_png(&dib).expect("decodes"));

        assert_eq!(image.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(0, 1), &Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn a_32_bit_bi_rgb_dib_ignores_its_reserved_byte() {
        let mut dib = info_header(2, 1, 32, BI_RGB);
        dib.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);

        let image = decode_png(&dib_to_png(&dib).expect("decodes"));

        assert_eq!(image.get_pixel(0, 0), &Rgba([30, 20, 10, 255]));
        assert_eq!(image.get_pixel(1, 0), &Rgba([60, 50, 40, 255]));
    }

    /// A 1x1 32-bit `BITMAPV5HEADER` DIB with an alpha mask and the given B, G, R, A
    /// pixel.
    fn v5_dib_with_alpha(pixel: [u8; 4]) -> Vec<u8> {
        let masks = [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff];
        let mut dib = Vec::new();
        dib.extend_from_slice(&124u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
        dib.extend_from_slice(&[0; 20]);
        for mask in masks {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&0xff00_0000u32.to_le_bytes());
        dib.resize(124, 0);
        // With BI_BITFIELDS the three colour masks also follow the header, as for a
        // BITMAPINFOHEADER; `image` skips them, so the pixel comes after.
        for mask in masks {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&pixel);
        dib
    }

    #[test]
    fn a_v5_dib_with_an_alpha_mask_keeps_its_transparency() {
        let png = dib_to_png(&v5_dib_with_alpha([30, 20, 10, 128])).expect("decodes");

        assert_eq!(decode_png(&png).get_pixel(0, 0), &Rgba([10, 20, 30, 128]));
    }

    #[test]
    fn a_dib_whose_alpha_is_zero_everywhere_is_opaque() {
        let png = dib_to_png(&v5_dib_with_alpha([30, 20, 10, 0])).expect("decodes");

        assert_eq!(decode_png(&png).get_pixel(0, 0), &Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn a_dib_that_is_not_a_bitmap_is_an_error() {
        assert!(dib_to_png(&[]).is_err());
        assert!(dib_to_png(&[1, 2, 3, 4, 5, 6, 7, 8]).is_err());
        // A plausible header whose pixel rows are missing.
        assert!(dib_to_png(&info_header(4, 4, 24, BI_RGB)).is_err());
    }

    #[test]
    fn a_header_that_claims_a_huge_image_is_refused_before_allocating() {
        let dib = info_header(60_000, 60_000, 32, BI_RGB);
        let error = dib_to_png(&dib).expect_err("refused");
        assert!(error.to_string().contains("too large"), "{error:#}");
    }

    #[test]
    fn an_image_written_as_a_dib_reads_back_unchanged() {
        let mut rgba = RgbaImage::new(3, 2);
        rgba.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        rgba.put_pixel(1, 0, Rgba([0, 255, 0, 200]));
        rgba.put_pixel(2, 0, Rgba([0, 0, 255, 255]));
        rgba.put_pixel(0, 1, Rgba([1, 2, 3, 255]));
        rgba.put_pixel(1, 1, Rgba([4, 5, 6, 0]));
        rgba.put_pixel(2, 1, Rgba([7, 8, 9, 255]));

        let dib = image_to_dib(&DynamicImage::ImageRgba8(rgba.clone())).expect("written");

        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(dib[16..20].try_into().unwrap()), BI_RGB);
        // Bottom-up: the first stored row is the image's last, in B, G, R, A order.
        assert_eq!(&dib[40..44], &[3, 2, 1, 255]);

        // BI_RGB marks the fourth byte as reserved, so a reader takes the bitmap
        // as opaque; the colours come back unchanged.
        let image = decode_png(&dib_to_png(&dib).expect("decodes"));
        assert_eq!(image.dimensions(), (3, 2));
        for (x, y, pixel) in rgba.enumerate_pixels() {
            let [red, green, blue, _] = pixel.0;
            assert_eq!(
                image.get_pixel(x, y),
                &Rgba([red, green, blue, 255]),
                "({x}, {y})"
            );
        }
    }

    #[test]
    fn an_empty_image_has_no_dib() {
        assert!(image_to_dib(&DynamicImage::ImageRgba8(RgbaImage::new(0, 0))).is_none());
    }
}
