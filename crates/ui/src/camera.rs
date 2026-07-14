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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use image::GrayImage;

/// Where preview frames come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Re-read this image file on every grab.
    File(String),
    /// Open camera device `index` (requires the `camera` feature).
    Device(u32),
}

/// A background capture thread that continuously grabs frames from a [`Source`]
/// and hands the newest to the UI over a 1-slot channel — so camera I/O never
/// blocks the GUI. The UI polls [`latest`](Capture::latest) each frame; the
/// thread stops when the `Capture` is dropped.
pub struct Capture {
    rx: Receiver<Result<GrayImage, String>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Capture {
    /// Spawn a capture thread for `source`.
    pub fn start(source: Source) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // Depth 1: the thread keeps only the freshest frame; the UI drains it.
        let (tx, rx) = mpsc::sync_channel::<Result<GrayImage, String>>(1);
        let stop_t = stop.clone();
        let handle = thread::spawn(move || capture_loop(source, tx, stop_t));
        Self {
            rx,
            stop,
            handle: Some(handle),
        }
    }

    /// The most recent frame, draining any staler ones. `None` if nothing new
    /// has arrived since the last poll.
    pub fn latest(&self) -> Option<Result<GrayImage, String>> {
        let mut last = None;
        while let Ok(f) = self.rx.try_recv() {
            last = Some(f);
        }
        last
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Push `frame` into the 1-slot channel, dropping it if the slot is still full
/// (the UI hasn't consumed the previous one) — we only ever want the latest.
fn offer(tx: &SyncSender<Result<GrayImage, String>>, frame: Result<GrayImage, String>) -> bool {
    match tx.try_send(frame) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn capture_loop(source: Source, tx: SyncSender<Result<GrayImage, String>>, stop: Arc<AtomicBool>) {
    #[cfg(feature = "camera")]
    if let Source::Device(index) = source {
        device_loop(index, &tx, &stop);
        return;
    }
    // File source (and Device without the feature) — re-grab with a throttle.
    while !stop.load(Ordering::Relaxed) {
        let frame = grab(&source);
        let slow = frame.is_err();
        if !offer(&tx, frame) {
            return; // receiver gone
        }
        thread::sleep(Duration::from_millis(if slow { 300 } else { 40 }));
    }
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
fn open_camera(index: u32) -> Result<nokhwa::Camera, String> {
    use nokhwa::Camera;
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut cam = Camera::new(CameraIndex::Index(index), requested)
        .map_err(|e| format!("open camera {index}: {e}"))?;
    cam.open_stream().map_err(|e| format!("stream: {e}"))?;
    Ok(cam)
}

#[cfg(feature = "camera")]
fn frame_to_gray(cam: &mut nokhwa::Camera) -> Result<GrayImage, String> {
    use nokhwa::pixel_format::RgbFormat;
    let frame = cam.frame().map_err(|e| format!("frame: {e}"))?;
    let rgb = frame
        .decode_image::<RgbFormat>()
        .map_err(|e| format!("decode: {e}"))?;
    let (w, h) = (rgb.width(), rgb.height());
    Ok(GrayImage::from_fn(w, h, |x, y| {
        let p = rgb.get_pixel(x, y).0;
        let l = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
        image::Luma([l as u8])
    }))
}

#[cfg(feature = "camera")]
fn grab_device(index: u32) -> Result<GrayImage, String> {
    frame_to_gray(&mut open_camera(index)?)
}

/// Persistent device capture: open the camera once, then stream frames until
/// stopped — avoids the per-frame reopen that would stutter a live feed.
#[cfg(feature = "camera")]
fn device_loop(index: u32, tx: &SyncSender<Result<GrayImage, String>>, stop: &Arc<AtomicBool>) {
    let mut cam = match open_camera(index) {
        Ok(c) => c,
        Err(e) => {
            let _ = offer(tx, Err(e));
            return;
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let frame = frame_to_gray(&mut cam);
        let slow = frame.is_err();
        if !offer(tx, frame) {
            return;
        }
        if slow {
            thread::sleep(Duration::from_millis(300));
        }
    }
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
    fn background_capture_delivers_frames_without_blocking() {
        let dir = std::env::temp_dir().join(format!("ui-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.png");
        image::GrayImage::from_pixel(24, 16, image::Luma([77]))
            .save(&path)
            .unwrap();

        let cap = Capture::start(Source::File(path.to_string_lossy().into()));
        // Poll (non-blocking) until a frame arrives — the thread does the I/O.
        let mut got = None;
        for _ in 0..200 {
            if let Some(Ok(f)) = cap.latest() {
                got = Some(f);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(cap); // stops + joins the thread
        assert_eq!(got.expect("a frame arrived").dimensions(), (24, 16));
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
