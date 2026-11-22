use crate::MAX_FRAME_BYTES;

/// Resizes a validated, tightly packed framebuffer while retaining its overlap.
pub(crate) fn resize_bgra(pixels: &mut Vec<u8>, old: (usize, usize), new: (usize, usize)) {
    let (old_stride, new_stride) = (old.0 * 4, new.0 * 4);
    let rows = old.1.min(new.1);
    let len = new_stride * new.1;
    let old_len = pixels.len();
    if len > old_len {
        if len > pixels.capacity() {
            // Nearby negotiated sizes can reuse one allocation during a window
            // drag. Bound the extra capacity even on a Retina or 4K desktop.
            let spare = (len / 16).min(256 * 1024).min(MAX_FRAME_BYTES - len);
            pixels.reserve_exact(len + spare - old_len);
        }
        pixels.resize(len, 0);
    }
    if new_stride > old_stride {
        for row in (0..rows).rev() {
            let from = row * old_stride;
            let to = row * new_stride;
            pixels.copy_within(from..from + old_stride, to);
            pixels[to + old_stride..to + new_stride].fill(0);
        }
    } else if new_stride < old_stride {
        for row in 1..rows {
            let from = row * old_stride;
            pixels.copy_within(from..from + new_stride, row * new_stride);
        }
    }
    let trailing = rows * new_stride;
    let initialized = old_len.min(len);
    if trailing < initialized {
        pixels[trailing..initialized].fill(0);
    }
    pixels.truncate(len);
    if len <= pixels.capacity() / 2 {
        pixels.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::resize_bgra;

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn nearby_resizes_reuse_the_allocation_and_a_large_shrink_releases_it() {
        let mut pixels = vec![0; 800 * 600 * 4];
        resize_bgra(&mut pixels, (800, 600), (801, 600));
        let allocation = pixels.as_ptr();
        for w in 802..=810 {
            resize_bgra(&mut pixels, (w - 1, 600), (w, 600));
            assert_eq!(pixels.as_ptr(), allocation);
        }
        resize_bgra(&mut pixels, (810, 600), (80, 60));
        assert_eq!(pixels.len(), 80 * 60 * 4);
        assert!(pixels.capacity() < 800 * 600 * 4);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn in_place_resizes_match_a_fresh_canvas() {
        for width in 1..=8 {
            for height in 1..=8 {
                let source: Vec<_> = (0..width * height)
                    .flat_map(|i| [i as u8, (i * 3) as u8, 47, 255])
                    .collect();
                for new_width in 1..=8 {
                    for new_height in 1..=8 {
                        let mut expected = vec![0; new_width * new_height * 4];
                        let overlap = width.min(new_width) * 4;
                        for row in 0..height.min(new_height) {
                            let dst = row * new_width * 4;
                            let src = row * width * 4;
                            expected[dst..dst + overlap]
                                .copy_from_slice(&source[src..src + overlap]);
                        }
                        let mut actual = source.clone();
                        resize_bgra(&mut actual, (width, height), (new_width, new_height));
                        assert_eq!(
                            actual, expected,
                            "{width}x{height} to {new_width}x{new_height}"
                        );
                    }
                }
            }
        }
    }
}
