//! The clipboard a remote desktop shares with this computer (R1-R4).
//!
//! Both directions are pure functions, so the format rules are tested without a
//! window, a server or the native libraries:
//!
//! * [local_offer] turns what this computer's clipboard holds into the entries the
//!   backend offers the remote side: the text, and an image as both PNG and DIB,
//!   because Windows programs paste one or the other and the remote side picks.
//! * [local_item] turns what the remote side copied into the one item this
//!   computer's clipboard takes: the text and the image together, a DIB decoded to
//!   PNG first because that is what this computer's clipboard carries.
//! * [prints] names the parts of a clipboard item, so the pane can tell content it
//!   already shared - or just received - from content it has not.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;

use gpui::{ClipboardItem, Image, ImageFormat};
use image::codecs::bmp::BmpDecoder;
use image::{DynamicImage, ImageDecoder, RgbaImage};

use crate::app::desktop::{ClipboardEntry, ClipboardKind};

/// The largest image entry the backend takes or delivers, in bytes.
pub const MAX_IMAGE_BYTES: usize = shellune_desktop::MAX_CLIPBOARD_IMAGE_BYTES;

/// The largest picture a clipboard image may decode to, in bytes of RGBA. A header
/// is only a few numbers; this stops one from asking for an absurd allocation.
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// `sizeof(BITMAPINFOHEADER)`.
const BITMAPINFOHEADER_SIZE: u32 = 40;
/// `BI_RGB`: uncompressed pixels with no masks.
const BI_RGB: u32 = 0;

/// What this computer's clipboard offers the remote side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalOffer {
    /// The entries to send, each kind at most once. Empty when nothing could be
    /// offered.
    pub entries: Vec<ClipboardEntry>,
    /// Whether an image was on the clipboard and was left out because neither of
    /// its forms fits the bound.
    pub image_too_large: bool,
}

/// The entries this computer's clipboard offers the remote side.
///
/// Text goes as it is; the backend cuts text over its own bound and says so. The
/// first image goes as PNG and as DIB: a PNG on the clipboard is passed through as
/// it is, any other format is decoded and encoded again. A form over the image
/// bound is left out rather than refused, so the other form still reaches the remote
/// side; an image neither form of which fits is reported. A format this build cannot
/// decode (SVG, a damaged file) is left out without a word, because the remote side
/// could not paste it either. File lists are not clipboard content here: a folder
/// mapping carries files (R5).
#[must_use]
pub fn local_offer(item: &ClipboardItem) -> LocalOffer {
    offer_within(item, MAX_IMAGE_BYTES)
}

/// [local_offer] with the image bound as a parameter, so the bound is tested
/// without encoding a picture of tens of megabytes.
fn offer_within(item: &ClipboardItem, bound: usize) -> LocalOffer {
    let mut offer = LocalOffer::default();
    if let Some(text) = item.text().filter(|text| !text.is_empty()) {
        offer.entries.push(ClipboardEntry::text(&text));
    }
    let image = item.entries().iter().find_map(|entry| match entry {
        gpui::ClipboardEntry::Image(image) => Some(image),
        _ => None,
    });
    if let Some(image) = image
        && let Some(decoded) = decode(image)
    {
        let png = if image.format == ImageFormat::Png {
            Some(image.bytes.clone())
        } else {
            encode_png(&decoded)
        };
        let dib = rgba_to_dib(&decoded.to_rgba8());
        let png = png.filter(|png| png.len() <= bound);
        let dib = dib.filter(|dib| dib.len() <= bound);
        offer.image_too_large = png.is_none() && dib.is_none();
        offer.entries.extend(png.map(ClipboardEntry::png));
        offer.entries.extend(dib.map(ClipboardEntry::dib));
    }
    offer
}

/// The item this computer's clipboard takes for what the remote side copied, or
/// `None` when nothing in it can be pasted here.
///
/// Text and image are written together as one item, so a program here pastes
/// whichever it understands. The image is the PNG when the remote side offered one,
/// otherwise its DIB decoded to PNG; an image that does not decode is left out and
/// the text still arrives.
///
/// Line breaks are written in this computer's own style (R1): a Windows server
/// copies CRLF, which a program on macOS or Linux would paste with a stray `\r` on
/// every line, and a Linux VNC server copies LF, which older Windows programs run
/// together.
#[must_use]
pub fn local_item(entries: &[ClipboardEntry]) -> Option<ClipboardItem> {
    let mut parts = Vec::new();
    if let Some(text) = entries
        .iter()
        .find_map(ClipboardEntry::as_text)
        .filter(|text| !text.is_empty())
    {
        parts.push(gpui::ClipboardEntry::from(local_line_breaks(
            text,
            cfg!(windows),
        )));
    }
    let png = entries
        .iter()
        .find(|entry| entry.kind == ClipboardKind::Png && is_png(&entry.bytes))
        .map(|entry| entry.bytes.clone())
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.kind == ClipboardKind::Dib)
                .and_then(|entry| dib_to_png(&entry.bytes))
        });
    if let Some(png) = png {
        parts.push(gpui::ClipboardEntry::Image(Image::from_bytes(
            ImageFormat::Png,
            png,
        )));
    }
    if parts.is_empty() {
        return None;
    }
    Some(ClipboardItem::new_entries(parts))
}

/// One number per part of a clipboard item - its text, each image - or nothing for
/// an item with neither.
///
/// Text is compared with its line breaks unified, because each side writes them its
/// own way. A platform may read back only part of what was written - a write carries
/// text, PNG and DIB, while a read returns at most one text and one image, and an
/// image may come back in another encoding - so the pane compares parts, see
/// [already_shared].
#[must_use]
pub fn prints(item: &ClipboardItem) -> Vec<u64> {
    let mut prints = Vec::new();
    if let Some(text) = item.text().filter(|text| !text.is_empty()) {
        let mut hasher = DefaultHasher::new();
        "text".hash(&mut hasher);
        text.replace("\r\n", "\n").hash(&mut hasher);
        prints.push(hasher.finish());
    }
    for entry in item.entries() {
        if let gpui::ClipboardEntry::Image(image) = entry {
            let mut hasher = DefaultHasher::new();
            "image".hash(&mut hasher);
            image.format.hash(&mut hasher);
            image.bytes.hash(&mut hasher);
            prints.push(hasher.finish());
        }
    }
    prints
}

/// The text with every line break - CRLF, a lone LF, a lone CR - written as CRLF
/// when `crlf` is set and as LF otherwise.
#[must_use]
fn local_line_breaks(text: &str, crlf: bool) -> String {
    let break_as = if crlf { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['\r', '\n']) {
        out.push_str(&rest[..at]);
        out.push_str(break_as);
        let skip = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
        rest = &rest[at + skip..];
    }
    out.push_str(rest);
    out
}

/// Whether every part of `current` is among the parts last shared, so offering it
/// again would tell the remote side nothing new.
#[must_use]
pub fn already_shared(current: &[u64], shared: &[u64]) -> bool {
    !current.is_empty() && current.iter().all(|print| shared.contains(print))
}

/// Whether the bytes start like a PNG file.
fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// The picture of a clipboard image, or `None` for a format this build cannot draw.
fn decode(image: &Image) -> Option<DynamicImage> {
    let format = match image.format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        // A vector picture has no pixels until it is drawn at a size.
        ImageFormat::Svg => return None,
    };
    image::load_from_memory_with_format(&image.bytes, format).ok()
}

/// The picture as a PNG file.
fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Decodes a packed DIB (the contents of `CF_DIB` or `CF_DIBV5`) to PNG bytes.
///
/// A 32-bit DIB whose alpha channel is zero everywhere is made opaque: such bitmaps
/// keep the fourth byte reserved, and reading it as alpha would paste an invisible
/// picture.
#[must_use]
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let dib = with_masks_after_header(dib);
    let decoder = BmpDecoder::new_without_file_header(Cursor::new(dib.as_ref())).ok()?;
    if decoder.total_bytes() > MAX_DECODED_BYTES {
        return None;
    }
    let image = DynamicImage::from_decoder(decoder).ok()?;
    let image = if image.color().has_alpha() {
        let mut rgba = image.into_rgba8();
        if rgba.pixels().all(|pixel| pixel.0[3] == 0) {
            for pixel in rgba.pixels_mut() {
                pixel.0[3] = u8::MAX;
            }
        }
        DynamicImage::ImageRgba8(rgba)
    } else {
        image
    };
    encode_png(&image)
}

/// The DIB with the three colour masks after a `BITMAPV4HEADER` or `BITMAPV5HEADER`
/// that the decoder skips there.
///
/// With `BI_BITFIELDS` such a header holds its masks itself, and writers disagree on
/// whether the three are repeated after it: Windows' own `CF_DIBV5` repeats them,
/// browsers and most programs do not. The decoder always skips twelve bytes, which
/// shifts a picture written without them by three pixels and loses its last rows.
/// The bitmap's length says which it is - the pixels end the packed DIB - so a DIB
/// with no room for the masks gets them copied in.
fn with_masks_after_header(dib: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const BI_BITFIELDS: u32 = 3;
    const BITMAPV4HEADER_SIZE: u32 = 108;
    let field = |at: usize| -> Option<[u8; 4]> { dib.get(at..at + 4)?.try_into().ok() };
    let read = || -> Option<(usize, usize)> {
        let size = u32::from_le_bytes(field(0)?);
        if size < BITMAPV4HEADER_SIZE || u32::from_le_bytes(field(16)?) != BI_BITFIELDS {
            return None;
        }
        let width = u64::from(i32::from_le_bytes(field(4)?).unsigned_abs());
        let height = u64::from(i32::from_le_bytes(field(8)?).unsigned_abs());
        let bits = u64::from(u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?));
        let stride = (width * bits).div_ceil(32) * 4;
        let header = usize::try_from(size).ok()?;
        let pixels = usize::try_from(stride.checked_mul(height)?).ok()?;
        (dib.len() == header.checked_add(pixels)?).then_some((header, pixels))
    };
    match read() {
        Some((header, _)) => {
            let mut fixed = Vec::with_capacity(dib.len() + 12);
            fixed.extend_from_slice(&dib[..header]);
            // The red, green and blue masks of the header, where the decoder skips.
            fixed.extend_from_slice(&dib[40..52]);
            fixed.extend_from_slice(&dib[header..]);
            std::borrow::Cow::Owned(fixed)
        }
        None => std::borrow::Cow::Borrowed(dib),
    }
}

/// Encodes a picture as a packed DIB: a `BITMAPINFOHEADER`, then 32-bit `BI_RGB`
/// rows bottom-up in blue, green, red, alpha order, which every Windows program that
/// pastes a bitmap reads. The fourth byte is reserved in `BI_RGB` and most readers
/// draw the bitmap opaque, so translucency travels in the PNG form only. `None` for
/// an empty picture or one too large to address.
#[must_use]
pub fn rgba_to_dib(rgba: &RgbaImage) -> Option<Vec<u8>> {
    let (width, height) = rgba.dimensions();
    let pixel_bytes = u64::from(width) * 4 * u64::from(height);
    if width == 0 || height == 0 || pixel_bytes > MAX_DECODED_BYTES {
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
    // Four bytes per pixel, so a row needs no padding to a DWORD boundary.
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

    /// A 2x2 picture with four different opaque pixels, so a flipped or swapped copy
    /// shows. Opaque because a `BI_RGB` DIB carries no alpha readers agree on.
    fn picture() -> RgbaImage {
        let mut image = RgbaImage::new(2, 2);
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        image.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        image.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        image.put_pixel(1, 1, Rgba([10, 20, 30, 255]));
        image
    }

    fn encoded(image: &RgbaImage, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(image.clone())
            .to_rgb8()
            .write_to(&mut Cursor::new(&mut bytes), format)
            .expect("the picture encodes");
        bytes
    }

    fn png_of(image: &RgbaImage) -> Vec<u8> {
        encode_png(&DynamicImage::ImageRgba8(image.clone())).expect("the picture encodes")
    }

    fn decode_png(png: &[u8]) -> RgbaImage {
        assert!(is_png(png), "the result is a PNG file");
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .expect("the PNG decodes")
            .to_rgba8()
    }

    #[test]
    fn a_dib_written_here_reads_back_as_the_same_picture() {
        let image = picture();
        let dib = rgba_to_dib(&image).expect("a small picture has a DIB");
        assert_eq!(&dib[0..4], &40u32.to_le_bytes(), "a BITMAPINFOHEADER leads");
        assert_eq!(dib.len(), 40 + 2 * 2 * 4);
        // Bottom-up: the first row stored is the picture's last.
        assert_eq!(&dib[40..44], &[255, 0, 0, 255], "blue, green, red, alpha");
        let back = decode_png(&dib_to_png(&dib).expect("the DIB decodes"));
        assert_eq!(back, image, "every pixel survives the trip");
    }

    #[test]
    fn a_dib_with_no_alpha_in_use_pastes_opaque() {
        let mut image = picture();
        for pixel in image.pixels_mut() {
            pixel.0[3] = 0;
        }
        let dib = rgba_to_dib(&image).expect("a small picture has a DIB");
        let back = decode_png(&dib_to_png(&dib).expect("the DIB decodes"));
        assert!(
            back.pixels().all(|pixel| pixel.0[3] == 255),
            "a reserved fourth byte is not transparency"
        );
    }

    /// A packed `CF_DIBV5`: a `BITMAPV5HEADER` with its masks inside the header and
    /// `BI_BITFIELDS`, the form Windows programs that keep transparency copy, with the
    /// pixels right after the header, or after the three masks repeated as Windows'
    /// own `CF_DIBV5` has them.
    fn v5_dib(image: &RgbaImage, masks_repeated: bool) -> Vec<u8> {
        let (width, height) = image.dimensions();
        let mut dib = Vec::new();
        dib.extend_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib.extend_from_slice(&i32::try_from(width).unwrap().to_le_bytes());
        dib.extend_from_slice(&i32::try_from(height).unwrap().to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes()); // bV5Planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bV5BitCount
        dib.extend_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
        dib.extend_from_slice(&(width * height * 4).to_le_bytes()); // bV5SizeImage
        dib.extend_from_slice(&[0; 16]); // resolution, colours used and important
        dib.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // red mask
        dib.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // green mask
        dib.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // blue mask
        dib.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // alpha mask
        dib.extend_from_slice(&0x7352_4742u32.to_le_bytes()); // LCS_sRGB
        dib.extend_from_slice(&[0; 36]); // endpoints
        dib.extend_from_slice(&[0; 12]); // gamma
        dib.extend_from_slice(&4u32.to_le_bytes()); // LCS_GM_IMAGES
        dib.extend_from_slice(&[0; 12]); // profile data, profile size, reserved
        assert_eq!(dib.len(), 124);
        if masks_repeated {
            let masks = dib[40..52].to_vec();
            dib.extend_from_slice(&masks);
        }
        for row in image.rows().rev() {
            for pixel in row {
                let [red, green, blue, alpha] = pixel.0;
                dib.extend_from_slice(&[blue, green, red, alpha]);
            }
        }
        dib
    }

    #[test]
    fn a_remote_dibv5_pastes_as_the_same_picture_with_its_transparency() {
        let mut image = picture();
        image.put_pixel(1, 1, Rgba([10, 20, 30, 128]));
        for masks_repeated in [false, true] {
            let dib = v5_dib(&image, masks_repeated);
            let back = decode_png(&dib_to_png(&dib).expect("a DIBV5 decodes"));
            assert_eq!(back, image, "every pixel and its alpha survive");
        }

        let item = local_item(&[ClipboardEntry::dib(v5_dib(&image, false))]).expect("a picture");
        assert!(matches!(
            item.entries(),
            [gpui::ClipboardEntry::Image(pasted)] if decode_png(&pasted.bytes) == image
        ));
    }

    #[test]
    fn a_damaged_dib_is_left_out_instead_of_pasted() {
        assert_eq!(dib_to_png(&[]), None);
        assert_eq!(dib_to_png(&[40, 0, 0, 0, 1, 2]), None);
    }

    #[test]
    fn local_text_is_offered_alone() {
        let offer = local_offer(&ClipboardItem::new_string("你好\nworld".to_owned()));
        assert_eq!(offer.entries, vec![ClipboardEntry::text("你好\nworld")]);
        assert!(!offer.image_too_large);
        assert!(
            local_offer(&ClipboardItem::new_string(String::new()))
                .entries
                .is_empty(),
            "an empty clipboard offers nothing"
        );
    }

    #[test]
    fn a_local_png_is_offered_as_itself_and_as_a_dib() {
        let image = picture();
        let png = png_of(&image);
        let offer = local_offer(&ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            png.clone(),
        )));
        assert_eq!(offer.entries.len(), 2);
        assert_eq!(
            offer.entries[0],
            ClipboardEntry::png(png),
            "the PNG as it was"
        );
        assert_eq!(offer.entries[1].kind, ClipboardKind::Dib);
        let back = decode_png(&dib_to_png(&offer.entries[1].bytes).expect("the DIB decodes"));
        assert_eq!(back, image);
    }

    #[test]
    fn a_local_jpeg_with_text_is_offered_as_text_png_and_dib() {
        let image = picture();
        let jpeg = encoded(&image, image::ImageFormat::Jpeg);
        let item = ClipboardItem::new_entries(vec![
            gpui::ClipboardEntry::from("caption".to_owned()),
            gpui::ClipboardEntry::Image(Image::from_bytes(ImageFormat::Jpeg, jpeg)),
        ]);
        let kinds: Vec<ClipboardKind> = local_offer(&item)
            .entries
            .iter()
            .map(|entry| entry.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![ClipboardKind::Text, ClipboardKind::Png, ClipboardKind::Dib]
        );
    }

    #[test]
    fn an_image_this_build_cannot_draw_is_left_out_quietly() {
        let item = ClipboardItem::new_entries(vec![
            gpui::ClipboardEntry::from("label".to_owned()),
            gpui::ClipboardEntry::Image(Image::from_bytes(
                ImageFormat::Svg,
                b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            )),
        ]);
        let offer = local_offer(&item);
        assert_eq!(offer.entries, vec![ClipboardEntry::text("label")]);
        assert!(
            !offer.image_too_large,
            "an undrawable image is not too large"
        );
        let damaged = ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, vec![1, 2]));
        assert!(local_offer(&damaged).entries.is_empty());
    }

    #[test]
    fn an_image_whose_forms_exceed_the_bound_is_reported_and_not_offered() {
        let image = picture();
        let png = png_of(&image);
        let dib_len = rgba_to_dib(&image).expect("a DIB").len();
        let item = ClipboardItem::new_entries(vec![
            gpui::ClipboardEntry::from("still here".to_owned()),
            gpui::ClipboardEntry::Image(Image::from_bytes(ImageFormat::Png, png.clone())),
        ]);
        let offer = offer_within(&item, png.len().min(dib_len) - 1);
        assert!(offer.image_too_large);
        assert_eq!(
            offer.entries,
            vec![ClipboardEntry::text("still here")],
            "the text still goes"
        );

        // A form that fits still goes when the other does not, and that is no loss
        // worth a notice: the remote side pastes the form it has.
        let bound = png.len().max(dib_len) - 1;
        let offer = offer_within(&item, bound);
        assert!(!offer.image_too_large);
        assert_eq!(offer.entries.len(), 2, "the text and the form that fits");
        assert!(offer.entries.iter().all(|entry| entry.bytes.len() <= bound));
        assert_eq!(
            MAX_IMAGE_BYTES,
            32 * 1024 * 1024,
            "the bound the shim keeps"
        );
    }

    #[test]
    fn a_remote_copy_becomes_one_item_with_its_text_and_its_picture() {
        let image = picture();
        let dib = rgba_to_dib(&image).expect("a small picture has a DIB");
        let item = local_item(&[ClipboardEntry::text("hello"), ClipboardEntry::dib(dib)])
            .expect("something to paste");
        assert_eq!(item.text(), Some("hello".to_owned()));
        let pasted = item
            .entries()
            .iter()
            .find_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .expect("the picture is on the item");
        assert_eq!(pasted.format, ImageFormat::Png, "a DIB is pasted as PNG");
        assert_eq!(decode_png(&pasted.bytes), image);

        // A PNG the remote side offered is used as it is, ahead of its DIB.
        let png = png_of(&image);
        let item = local_item(&[
            ClipboardEntry::dib(vec![0; 3]),
            ClipboardEntry::png(png.clone()),
        ])
        .expect("something to paste");
        assert_eq!(item.text(), None);
        assert!(matches!(
            item.entries(),
            [gpui::ClipboardEntry::Image(image)] if image.bytes == png
        ));

        // Nothing usable is nothing written.
        assert!(local_item(&[]).is_none());
        assert!(local_item(&[ClipboardEntry::dib(vec![1, 2, 3])]).is_none());
        assert!(local_item(&[ClipboardEntry::text("")]).is_none());
    }

    #[test]
    fn remote_text_is_written_with_this_computers_line_breaks() {
        assert_eq!(local_line_breaks("a\r\nb\rc\nd", false), "a\nb\nc\nd");
        assert_eq!(local_line_breaks("a\r\nb\rc\nd", true), "a\r\nb\r\nc\r\nd");
        assert_eq!(
            local_line_breaks("\r\n\r\n你好\r", false),
            "\n\n你好\n",
            "breaks at the ends and next to each other each stay one break"
        );
        assert_eq!(local_line_breaks("no break", true), "no break");

        // What a Windows server copies pastes here without a stray CR, and what a
        // Linux server copies pastes on Windows with CRLF.
        let local = if cfg!(windows) {
            "one\r\ntwo"
        } else {
            "one\ntwo"
        };
        for remote in ["one\r\ntwo", "one\ntwo"] {
            let item = local_item(&[ClipboardEntry::text(remote)]).expect("text to paste");
            assert_eq!(item.text().as_deref(), Some(local));
            // Either form still counts as the same text, so it is not offered back.
            assert!(already_shared(
                &prints(&ClipboardItem::new_string(remote.to_owned())),
                &prints(&item)
            ));
        }
    }

    #[test]
    fn a_clipboard_already_shared_is_recognised_by_its_parts() {
        let text = ClipboardItem::new_string("a\r\nb".to_owned());
        let same = ClipboardItem::new_string("a\nb".to_owned());
        assert_eq!(prints(&text), prints(&same), "line breaks are unified");
        let png = png_of(&picture());
        let both = ClipboardItem::new_entries(vec![
            gpui::ClipboardEntry::from("a\nb".to_owned()),
            gpui::ClipboardEntry::Image(Image::from_bytes(ImageFormat::Png, png.clone())),
        ]);
        let shared = prints(&both);
        assert_eq!(shared.len(), 2);
        // A platform that reads back only the picture of what was written still
        // reads something already shared.
        let picture_only = ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, png));
        assert!(already_shared(&prints(&picture_only), &shared));
        assert!(already_shared(&prints(&same), &shared));
        let other = ClipboardItem::new_string("c".to_owned());
        assert!(!already_shared(&prints(&other), &shared));
        assert!(!already_shared(&[], &shared), "nothing is never shared");
    }
}
