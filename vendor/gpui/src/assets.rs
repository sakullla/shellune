use crate::{DevicePixels, Pixels, Result, SharedString, Size, size};
use smallvec::SmallVec;
use std::sync::Arc;

use image::{Delay, Frame};
use std::{
    borrow::Cow,
    fmt,
    hash::Hash,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};

/// A source of assets for this app to use.
pub trait AssetSource: 'static + Send + Sync {
    /// Load the given asset from the source path.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>>;

    /// List the assets at the given path.
    fn list(&self, path: &str) -> Result<Vec<SharedString>>;
}

impl AssetSource for () {
    fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

/// A unique identifier for the image cache
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ImageId(pub usize);

#[derive(PartialEq, Eq, Hash, Clone)]
pub(crate) struct RenderImageParams {
    pub(crate) image_id: ImageId,
    pub(crate) frame_index: usize,
}

/// A cached and processed image, in BGRA format
pub struct RenderImage {
    /// The ID associated with this image
    pub id: ImageId,
    /// The scale factor of this image on render.
    pub(crate) scale_factor: f32,
    data: ImageFrames,
}

enum ImageFrames {
    Animation(SmallVec<[Frame; 1]>),
    Bgra(Size<DevicePixels>, Arc<[u8]>),
}

impl PartialEq for RenderImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for RenderImage {}

impl RenderImage {
    /// Create a new image from the given data.
    pub fn new(data: impl Into<SmallVec<[Frame; 1]>>) -> Self {
        Self::from_frames(ImageFrames::Animation(data.into()))
    }

    /// Shares a packed BGRA buffer with its producer. Retained frames stay immutable.
    pub fn from_bgra(size: Size<DevicePixels>, pixels: Arc<[u8]>) -> Self {
        assert!(
            size.width.0 > 0
                && size.height.0 > 0
                && size.width.0 as u64 * size.height.0 as u64 * 4 == pixels.len() as u64
        );
        Self::from_frames(ImageFrames::Bgra(size, pixels))
    }

    fn from_frames(data: ImageFrames) -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

        Self {
            id: ImageId(NEXT_ID.fetch_add(1, SeqCst)),
            scale_factor: 1.0,
            data,
        }
    }

    /// Convert this image into a byte slice.
    pub fn as_bytes(&self, frame_index: usize) -> Option<&[u8]> {
        match &self.data {
            ImageFrames::Animation(frames) => frames
                .get(frame_index)
                .map(|frame| frame.buffer().as_raw().as_slice()),
            ImageFrames::Bgra(_, pixels) => (frame_index == 0).then_some(pixels.as_ref()),
        }
    }

    /// Get the size of this image, in pixels.
    pub fn size(&self, frame_index: usize) -> Size<DevicePixels> {
        match &self.data {
            ImageFrames::Animation(frames) => {
                let (w, h) = frames[frame_index].buffer().dimensions();
                size(w.into(), h.into())
            }
            ImageFrames::Bgra(size, _) => {
                assert_eq!(frame_index, 0);
                *size
            }
        }
    }

    /// Get the size of this image, in pixels for display, adjusted for the scale factor.
    pub(crate) fn render_size(&self, frame_index: usize) -> Size<Pixels> {
        self.size(frame_index)
            .map(|v| (v.0 as f32 / self.scale_factor).into())
    }

    /// Get the delay of this frame from the previous
    pub fn delay(&self, frame_index: usize) -> Delay {
        match &self.data {
            ImageFrames::Animation(frames) => frames[frame_index].delay(),
            ImageFrames::Bgra(..) => {
                assert_eq!(frame_index, 0);
                Delay::from_numer_denom_ms(0, 1)
            }
        }
    }

    /// Get the number of frames for this image.
    pub fn frame_count(&self) -> usize {
        match &self.data {
            ImageFrames::Animation(frames) => frames.len(),
            ImageFrames::Bgra(..) => 1,
        }
    }
}

impl fmt::Debug for RenderImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageData")
            .field("id", &self.id)
            .field("size", &self.size(0))
            .finish()
    }
}
