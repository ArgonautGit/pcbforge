//! Camera capture (VIS-1), shared by the console (`ui`) and the CLI
//! (`pcbforge cam`). Deliberately egui-free so the CLI can grab frames without
//! pulling in the GUI stack.
//!
//! Two source kinds, so capture works whatever the operator's setup:
//!
//! * [`Source::File`] — re-read an image file each grab. Any capture app that
//!   writes a frame to disk (or a saved still) drives it; works on every
//!   platform and is fully testable headless. This is the default.
//! * [`Source::Device`] — open a webcam by index via `nokhwa`, behind the
//!   `camera` cargo feature (needs a real camera + platform backend: v4l2 /
//!   AVFoundation / MSMF). On the operator's Windows machine, build with the
//!   `camera` feature for a true webcam feed.
//!
//! A grab returns a grayscale frame (the detectors and overlays work in gray);
//! callers convert it to a texture / save it as needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use image::GrayImage;

/// A 1-deep shared slot the capture thread overwrites with the freshest frame.
/// A plain `sync_channel(1)` drops the *newer* frame when full (keeping the
/// stale one); overwriting a shared slot keeps the newest, which is what a
/// live preview wants.
type Slot = Arc<Mutex<Option<Result<GrayImage, String>>>>;

/// Strip surrounding single/double quotes and whitespace from a pasted file
/// path (drag-and-drop and file managers often quote paths with spaces).
pub fn clean_path(s: &str) -> String {
    let t = s.trim();
    let inner = if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        &t[1..t.len() - 1]
    } else {
        t
    };
    inner.trim().to_string()
}

/// Where preview frames come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Re-read this image file on every grab.
    File(String),
    /// Open camera device `index` (requires the `camera` feature).
    Device(u32),
}

/// A background capture thread that continuously grabs frames from a [`Source`]
/// and hands the newest to the caller over a 1-slot channel — so camera I/O
/// never blocks the UI. The caller polls [`latest`](Capture::latest); the
/// thread stops when the `Capture` is dropped.
pub struct Capture {
    slot: Slot,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Capture {
    /// Spawn a capture thread for `source`.
    pub fn start(source: Source) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // The thread overwrites this slot with the freshest frame; the caller
        // takes it. Newest always wins — no fresh frame is ever dropped.
        let slot: Slot = Arc::new(Mutex::new(None));
        let stop_t = stop.clone();
        let slot_t = slot.clone();
        let handle = thread::spawn(move || capture_loop(source, slot_t, stop_t));
        Self {
            slot,
            stop,
            handle: Some(handle),
        }
    }

    /// The most recent frame, consuming it. `None` if nothing new has arrived
    /// since the last poll.
    pub fn latest(&self) -> Option<Result<GrayImage, String>> {
        self.slot.lock().unwrap().take()
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

/// Overwrite the shared slot with the freshest `frame`, discarding any
/// previous frame the caller hasn't consumed — we only want the latest.
fn offer(slot: &Slot, frame: Result<GrayImage, String>) {
    *slot.lock().unwrap() = Some(frame);
}

fn capture_loop(source: Source, slot: Slot, stop: Arc<AtomicBool>) {
    #[cfg(feature = "camera")]
    if let Source::Device(index) = source {
        device_loop(index, &slot, &stop);
        return;
    }
    // File source (and Device without the feature) — re-grab with a throttle.
    while !stop.load(Ordering::Relaxed) {
        let frame = grab(&source);
        let slow = frame.is_err();
        offer(&slot, frame);
        thread::sleep(Duration::from_millis(if slow { 300 } else { 40 }));
    }
}

/// Grab one frame from `source` as grayscale.
pub fn grab(source: &Source) -> Result<GrayImage, String> {
    match source {
        Source::File(path) => {
            let p = clean_path(path);
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
    // Prefer the sensor's highest resolution over frame rate: this is a metric
    // bed-vision tool (calibration, fiducials, placement on a mostly-static
    // bed), so pixels-per-mm — accuracy — matters far more than fps. On a 2K/4K
    // camera this negotiates the full sensor mode; `AbsoluteHighestFrameRate`
    // would instead pick a low-res high-fps mode and throw away the resolution.
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
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
        // Round, don't truncate: `as u8` floors, a ~0.5-LSB systematic
        // darkening of every pixel. `l ∈ [0, 255]`, so this never overflows.
        image::Luma([l.round() as u8])
    }))
}

#[cfg(feature = "camera")]
fn grab_device(index: u32) -> Result<GrayImage, String> {
    frame_to_gray(&mut open_camera(index)?)
}

/// Persistent device capture: open the camera once, then stream frames until
/// stopped — avoids the per-frame reopen that would stutter a live feed.
#[cfg(feature = "camera")]
fn device_loop(index: u32, slot: &Slot, stop: &Arc<AtomicBool>) {
    let mut cam = match open_camera(index) {
        Ok(c) => c,
        Err(e) => {
            offer(slot, Err(e));
            return;
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let frame = frame_to_gray(&mut cam);
        let slow = frame.is_err();
        offer(slot, frame);
        if slow {
            thread::sleep(Duration::from_millis(300));
        }
    }
}

#[cfg(not(feature = "camera"))]
fn grab_device(_index: u32) -> Result<GrayImage, String> {
    Err(
        "camera device support not built — rebuild with the `camera` feature, \
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
                // Keep only numeric-index devices. A string-indexed camera has
                // no `u32` to open by, and mapping them all to 0 (the old
                // `unwrap_or(0)`) silently collides distinct devices onto one.
                .filter_map(|i| i.index().as_index().ok().map(|idx| (idx, i.human_name())))
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
    fn clean_path_strips_quotes_and_whitespace() {
        assert_eq!(clean_path("  \"/a/b c.gbr\" "), "/a/b c.gbr");
        assert_eq!(clean_path("'/x/y.png'"), "/x/y.png");
        assert_eq!(clean_path("/plain/path.gbr"), "/plain/path.gbr");
        assert_eq!(clean_path("\"unbalanced"), "\"unbalanced");
        assert_eq!(clean_path(""), "");
    }

    #[test]
    fn file_source_grabs_a_frame() {
        let dir = std::env::temp_dir().join(format!("cap-cam-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("cap-cam-q-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("cap-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.png");
        image::GrayImage::from_pixel(24, 16, image::Luma([77]))
            .save(&path)
            .unwrap();

        let cap = Capture::start(Source::File(path.to_string_lossy().into()));
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
    fn a_slow_poller_gets_the_freshest_frame_not_a_stale_one() {
        // Two frames produced with no poll in between: the slot must hold the
        // *newer* one. The old sync_channel(1) kept the older frame and
        // dropped the fresh one (the LR-22 staleness bug).
        let slot: Slot = Arc::new(Mutex::new(None));
        offer(&slot, Ok(GrayImage::from_pixel(2, 2, image::Luma([1]))));
        offer(&slot, Ok(GrayImage::from_pixel(2, 2, image::Luma([2]))));
        let got = slot.lock().unwrap().take().unwrap().unwrap();
        assert_eq!(got.get_pixel(0, 0).0[0], 2, "newest frame wins");
    }

    #[test]
    fn device_without_feature_reports_how_to_enable() {
        let r = grab(&Source::Device(0));
        #[cfg(not(feature = "camera"))]
        assert!(r.unwrap_err().contains("camera"));
        #[cfg(feature = "camera")]
        let _ = r; // with a real backend this depends on hardware
    }
}
