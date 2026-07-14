//! Camera source for the live preview (VIS-1 surfaced in the console).
//!
//! Two source kinds, so a live preview works whatever the operator's setup:
//!
//! * [`Source::File`] — re-read an image file each grab. Any capture app that
//!   writes a frame to disk (or a saved still) drives the preview; works on
//!   every platform and is fully testable headless. This is the default.
//! * [`Source::Device`] — open a webcam by index via `nokhwa`, behind the
//!   `camera` cargo feature (needs a real camera + platform backend: v4l2 /
//!   AVFoundation / MSMF). On the operator's Windows machine, build with
//!   `--features native,camera` for a true webcam feed.
//!
//! A grab returns a grayscale frame (the detectors and overlays work in gray);
//! the console converts it to a texture and, in Live mode, re-grabs each frame.

use image::GrayImage;

/// Where preview frames come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Re-read this image file on every grab.
    File(String),
    /// Open camera device `index` (requires the `camera` feature).
    Device(u32),
}

/// Grab one frame from `source` as grayscale.
pub fn grab(source: &Source) -> Result<GrayImage, String> {
    match source {
        Source::File(path) => {
            let p = crate::clean_path(path);
            if p.is_empty() {
                return Err("set a frame file path".into());
            }
            Ok(image::open(&p)
                .map_err(|e| format!("open {p}: {e}"))?
                .to_luma8())
        }
        Source::Device(index) => grab_device(*index),
    }
}

/// Enumerate available camera devices as `(index, label)`. Empty without the
/// `camera` feature.
pub fn list_devices() -> Vec<(u32, String)> {
    list_devices_impl()
}

#[cfg(feature = "camera")]
fn grab_device(index: u32) -> Result<GrayImage, String> {
    use nokhwa::Camera;
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};

    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut cam = Camera::new(CameraIndex::Index(index), requested)
        .map_err(|e| format!("open camera {index}: {e}"))?;
    cam.open_stream().map_err(|e| format!("stream: {e}"))?;
    let frame = cam.frame().map_err(|e| format!("frame: {e}"))?;
    let rgb = frame
        .decode_image::<RgbFormat>()
        .map_err(|e| format!("decode: {e}"))?;
    // RGB → luma.
    let (w, h) = (rgb.width(), rgb.height());
    Ok(GrayImage::from_fn(w, h, |x, y| {
        let p = rgb.get_pixel(x, y).0;
        let l = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
        image::Luma([l as u8])
    }))
}

#[cfg(not(feature = "camera"))]
fn grab_device(_index: u32) -> Result<GrayImage, String> {
    Err(
        "camera device support not built — rebuild with `--features native,camera`, \
         or use a File source (any capture app that writes a frame to disk)"
            .into(),
    )
}

#[cfg(feature = "camera")]
fn list_devices_impl() -> Vec<(u32, String)> {
    use nokhwa::query;
    use nokhwa::utils::ApiBackend;
    query(ApiBackend::Auto)
        .map(|infos| {
            infos
                .into_iter()
                .map(|i| (i.index().as_index().unwrap_or(0), i.human_name()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(feature = "camera"))]
fn list_devices_impl() -> Vec<(u32, String)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_source_grabs_a_frame() {
        let dir = std::env::temp_dir().join(format!("ui-cam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.png");
        image::GrayImage::from_pixel(40, 30, image::Luma([128]))
            .save(&path)
            .unwrap();

        let img = grab(&Source::File(path.to_string_lossy().into())).unwrap();
        assert_eq!(img.dimensions(), (40, 30));
    }

    #[test]
    fn file_source_strips_quotes() {
        let dir = std::env::temp_dir().join(format!("ui-cam-q-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.png");
        image::GrayImage::from_pixel(8, 8, image::Luma([10]))
            .save(&path)
            .unwrap();
        let quoted = format!("\"{}\"", path.to_string_lossy());
        assert!(grab(&Source::File(quoted)).is_ok());
    }

    #[test]
    fn empty_file_and_missing_file_error() {
        assert!(grab(&Source::File(String::new())).is_err());
        assert!(grab(&Source::File("/no/such/frame.png".into())).is_err());
    }

    #[test]
    fn device_without_feature_reports_how_to_enable() {
        // Without the `camera` feature the device path explains itself.
        let r = grab(&Source::Device(0));
        #[cfg(not(feature = "camera"))]
        assert!(r.unwrap_err().contains("--features"));
        #[cfg(feature = "camera")]
        let _ = r; // with a real backend this depends on hardware
    }
}
