//! The console's durable diagnostic log — a dumb, dependency-free text sink.
//!
//! The console had no record of what it did: diagnosing a bad burn meant asking
//! the operator to screenshot the note line, which is slow, lossy, and gone the
//! moment the app restarts. This writes one plain-text record per line beside
//! the settings blob (`<db>.console-log`), so a session can be reconstructed
//! after the fact from the file alone.
//!
//! Deliberately dumb: it knows nothing about egui, geometry or calibration —
//! callers format their own records and hand over a string. That keeps the
//! module std-only and unit-testable without building a `ConsoleApp`, and keeps
//! the formatting next to the types it describes.
//!
//! Three properties the callers depend on:
//!
//! * **Flushed per record.** Records are infrequent (state changes and operator
//!   actions, never per frame), and the last one before a crash is usually the
//!   interesting one, so buffering would throw away exactly what is wanted.
//! * **Never fatal.** Every failure is swallowed and latched into
//!   [`Diag::failed`]; a logger that can panic or block would be worse than no
//!   logger at all.
//! * **Bounded.** The file is truncated at startup (the previous session is
//!   rotated to `.prev`, surviving exactly one restart) and hard-capped, so an
//!   unattended console cannot fill the disk.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Hard cap on the live log, bytes. Past this the sink writes one final "log
/// capped" record and goes quiet rather than growing without bound.
pub const DEFAULT_CAP_BYTES: u64 = 8 * 1024 * 1024;

/// The diagnostic log file for a database path: `<db>.console-log`, beside the
/// `<db>.console-settings` blob (see [`crate::settings::path_for_db`]).
pub fn path_for_db(db_path: &Path) -> PathBuf {
    db_path.with_extension("console-log")
}

/// The one-restart rotation of [`path_for_db`]. Built by extending the
/// extension, not replacing it — `with_extension("prev")` would turn
/// `pcbforge.console-log` into `pcbforge.prev`.
fn prev_path(log_path: &Path) -> PathBuf {
    let mut name = log_path.as_os_str().to_os_string();
    name.push(".prev");
    PathBuf::from(name)
}

/// Wall-clock timestamp as `seconds.millis` since the Unix epoch — the record
/// prefix. Mirrors `app::now_unix` at millisecond resolution rather than
/// pulling in a time crate; ordering within a session is what matters, and a
/// clock before the epoch just reads as 0.
pub fn now_millis() -> (u64, u32) {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_millis()))
        .unwrap_or((0, 0))
}

/// An append-only, per-record-flushed text log.
pub struct Diag {
    path: PathBuf,
    /// `None` once the file could not be opened, or once the cap closed it.
    file: Option<File>,
    written: u64,
    cap: u64,
    /// Latched: a write failed at some point this session. Never cleared — the
    /// app reports it once and then stops trying to say anything about it.
    failed: bool,
}

impl Diag {
    /// Open (truncating) `path`, first rotating any existing file to
    /// `<path>.prev` so the previous session survives exactly one restart.
    ///
    /// Never fails: an unopenable path yields a sink that silently discards
    /// records with [`failed`](Self::failed) set.
    pub fn open(path: PathBuf, cap: u64) -> Self {
        // Best-effort rotation. A failed rename (file locked, no permission)
        // must not cost the new session its log, so the truncating open below
        // runs either way — losing the previous session, not this one.
        if path.exists() {
            let _ = std::fs::rename(&path, prev_path(&path));
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path);
        let failed = file.is_err();
        Self {
            path,
            file: file.ok(),
            written: 0,
            cap,
            failed,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A write (or the initial open) failed this session. Latched.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Append one timestamped record. Embedded newlines are folded to ` | ` so
    /// one record is always exactly one line — multi-line log text is common
    /// (the export records quote CLI output) and a reader greps by line.
    pub fn record(&mut self, text: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let (secs, millis) = now_millis();
        let line = format!("{secs}.{millis:03} {}\n", text.replace(['\n', '\r'], " | "));
        // Cap on the way in, so the closing record is itself written and the
        // file's last line explains why it stops.
        if self.written + line.len() as u64 > self.cap {
            let _ = writeln!(
                file,
                "{secs}.{millis:03} diag: log capped at {} bytes",
                self.cap
            );
            let _ = file.flush();
            self.file = None;
            return;
        }
        // Flushed immediately: a crash must not eat the record that explains it.
        if file
            .write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .is_err()
        {
            self.failed = true;
            self.file = None;
            return;
        }
        self.written += line.len() as u64;
    }
}

/// All shape vertices of an emitted `.lbrn2` document, mm.
///
/// Shared with the export readback rather than duplicated: the diagnostic that
/// says where a job actually landed must parse the file the same way the tests
/// that assert it do.
///
/// The coordinates are **commanded** mm when the export applied a field map and
/// **physical** mm otherwise — the two differ by the field pre-distortion, and
/// which one a reader is looking at is itself a candidate explanation for a
/// misplaced burn, so the record names the field-map state alongside the bbox.
pub fn lbrn2_verts(doc: &str) -> Vec<(f64, f64)> {
    doc.split("<VertList>")
        .skip(1)
        .flat_map(|s| s.split("</VertList>").next().unwrap_or("").split('V'))
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            let xy = t.split('c').next()?;
            let mut it = xy.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// `(min_x, min_y, max_x, max_y)` of `pts`.
pub fn verts_bbox(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    pts.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pcbforge-diag-test-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn log_path_sits_beside_the_db() {
        assert_eq!(
            path_for_db(Path::new("/data/pcbforge.sqlite")),
            PathBuf::from("/data/pcbforge.console-log")
        );
    }

    #[test]
    fn startup_rotates_the_previous_session_to_prev() {
        let dir = tmp_dir();
        let path = dir.join("pcbforge.console-log");

        let mut first = Diag::open(path.clone(), DEFAULT_CAP_BYTES);
        first.record("session one");
        drop(first);

        let mut second = Diag::open(path.clone(), DEFAULT_CAP_BYTES);
        second.record("session two");
        drop(second);

        // The rotation EXTENDS the extension: `.console-log.prev`, not `.prev`.
        let prev = dir.join("pcbforge.console-log.prev");
        assert!(prev.exists(), "the previous session was rotated aside");
        assert!(
            !dir.join("pcbforge.prev").exists(),
            "with_extension would have clobbered the extension"
        );
        assert!(
            std::fs::read_to_string(&prev)
                .unwrap()
                .contains("session one")
        );
        let live = std::fs::read_to_string(&path).unwrap();
        assert!(live.contains("session two"));
        assert!(
            !live.contains("session one"),
            "the live file is truncated, not appended to"
        );

        // Exactly one rotation: a third session pushes session one out.
        let mut third = Diag::open(path.clone(), DEFAULT_CAP_BYTES);
        third.record("session three");
        drop(third);
        let prev_text = std::fs::read_to_string(&prev).unwrap();
        assert!(prev_text.contains("session two"));
        assert!(!prev_text.contains("session one"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_cap_stops_appending_with_one_final_record() {
        let dir = tmp_dir();
        let path = dir.join("capped.console-log");
        let mut diag = Diag::open(path.clone(), 200);
        for i in 0..50 {
            diag.record(&format!(
                "record {i} with enough text to pass the cap quickly"
            ));
        }
        drop(diag);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.len() < 400,
            "the cap bounds the file; got {} bytes",
            text.len()
        );
        assert!(
            text.contains("diag: log capped at 200 bytes"),
            "the last line says why it stops: {text}"
        );
        assert_eq!(
            text.matches("log capped").count(),
            1,
            "the capped record is written exactly once"
        );
        assert!(text.contains("record 0"), "early records survive");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unopenable_path_is_swallowed_not_panicked() {
        // A parent directory that does not exist: the open fails with NotFound
        // on every platform, with no permission games.
        let missing = std::env::temp_dir()
            .join(format!("pcbforge-no-such-dir-{}", std::process::id()))
            .join("nested")
            .join("pcbforge.console-log");
        let mut diag = Diag::open(missing, DEFAULT_CAP_BYTES);
        assert!(
            diag.failed(),
            "the failure is latched for the app to report"
        );
        // The whole point: recording against a dead sink is a no-op, not a panic.
        diag.record("this goes nowhere");
        diag.record("so does this");
        assert!(diag.failed());
    }

    #[test]
    fn records_are_one_line_each_and_timestamped() {
        let dir = tmp_dir();
        let path = dir.join("lines.console-log");
        let mut diag = Diag::open(path.clone(), DEFAULT_CAP_BYTES);
        diag.record("first");
        diag.record("second\nwith an embedded newline");
        drop(diag);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one record per line: {text}");
        assert!(
            lines[1].contains("second | with an embedded newline"),
            "embedded newlines fold: {}",
            lines[1]
        );
        for line in &lines {
            let stamp = line.split_whitespace().next().unwrap();
            assert!(
                stamp.split_once('.').is_some_and(|(s, ms)| {
                    s.parse::<u64>().is_ok() && ms.len() == 3 && ms.parse::<u32>().is_ok()
                }),
                "each line starts with a seconds.millis stamp: {line}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lbrn2_bbox_reads_the_written_geometry() {
        let doc = "<VertList>V1 2c0x0M1V4 6c0x0M1</VertList>";
        let verts = lbrn2_verts(doc);
        assert_eq!(verts, vec![(1.0, 2.0), (4.0, 6.0)]);
        assert_eq!(verts_bbox(&verts), (1.0, 2.0, 4.0, 6.0));
    }
}
