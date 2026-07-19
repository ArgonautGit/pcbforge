//! Tiny dependency-free persistence for the console's input fields, so the
//! Gerber paths (and their neighbours) survive a restart.
//!
//! The format is one `key=value` per line under a version header — the same
//! shape as the drill-guide state file. Values are single-line strings (paths,
//! numbers); unknown keys are ignored and missing keys keep their default, so
//! the file is forward/backward compatible as fields come and go.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const HEADER: &str = "pcbforge console settings v1";

/// The settings file that sits beside the database (`pcbforge.sqlite` →
/// `pcbforge.console-settings`).
pub fn path_for_db(db_path: &Path) -> PathBuf {
    db_path.with_extension("console-settings")
}

/// Serialize `pairs` (in the given order) to the file blob.
pub fn blob(pairs: &[(&str, String)]) -> String {
    let mut s = String::from(HEADER);
    s.push('\n');
    for (k, v) in pairs {
        // Drop embedded newlines defensively so one field can't corrupt the
        // file (paths never contain them; a pasted value theoretically could).
        let v = v.replace(['\n', '\r'], " ");
        s.push_str(k);
        s.push('=');
        s.push_str(&v);
        s.push('\n');
    }
    s
}

/// Parse a blob into a key→value map. A missing/wrong header yields an empty
/// map (treated as "no saved settings"), so a corrupt file never blocks start.
pub fn parse(src: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = src.lines();
    if lines.next().map(str::trim) != Some(HEADER) {
        return out;
    }
    for line in lines {
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.to_string());
        }
    }
    out
}

/// Load the map from `path`, or an empty map if it is absent/unreadable.
pub fn load(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|s| parse(&s))
        .unwrap_or_default()
}

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// Durably replace a settings file without exposing a partially-written blob.
pub fn save(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid settings filename"))?;
    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_pairs() {
        let pairs = [
            ("copper", "/a/b c.gbr".to_string()),
            ("outline", "/x/y.gbr".to_string()),
            ("offset_mm", "0.05".to_string()),
        ];
        let m = parse(&blob(&pairs));
        assert_eq!(m.get("copper").map(String::as_str), Some("/a/b c.gbr"));
        assert_eq!(m.get("outline").map(String::as_str), Some("/x/y.gbr"));
        assert_eq!(m.get("offset_mm").map(String::as_str), Some("0.05"));
    }

    #[test]
    fn rejects_a_bad_header_as_empty() {
        assert!(parse("garbage\ncopper=x").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn a_value_may_contain_equals_signs() {
        let m = parse(&blob(&[("k", "a=b=c".to_string())]));
        assert_eq!(m.get("k").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn settings_path_sits_beside_the_db() {
        let p = path_for_db(Path::new("/data/pcbforge.sqlite"));
        assert_eq!(p, PathBuf::from("/data/pcbforge.console-settings"));
    }

    #[test]
    fn save_replaces_the_complete_file() {
        let dir = std::env::temp_dir().join(format!(
            "pcbforge-settings-test-{}-{}",
            std::process::id(),
            TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings");
        save(&path, "first").unwrap();
        save(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
