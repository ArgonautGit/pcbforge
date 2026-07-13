//! `xtask` — repo-local tooling for PCBForge (the standard cargo-xtask pattern).
//!
//! Invoke via the workspace alias: `cargo xtask <command>`.
//!
//! ## Commands
//!
//! * `fixtures` — validate the test-fixture tree and (re)write
//!   `samples/MANIFEST.toml`. See [`fixtures`].
//! * `seed-defect` — inject a known defect (copper sliver or local trace
//!   thinning) into a board's F.Cu artwork and emit golden-checked modified
//!   artwork. See [`xtask::seed_defect`].
//!
//! INF-3 / QA-5. This crate is `publish = false` and is only ever run as a
//! dev tool; nothing in the shipped workspace depends on it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::{Digest, Sha256};

/// Top-level usage line.
const USAGE: &str = "usage: cargo xtask <fixtures | seed-defect ...>";

/// Minimum number of KiCad board projects `samples/kicad` must contain.
const MIN_KICAD_PROJECTS: usize = 2;

/// Expected LightBurn schema samples in `samples/lbrn2`.
///
/// The real playbook §0.3 sample list was never provided to this workspace, so
/// this is an **agent-authored stand-in** (recorded in `docs/decisions.md`),
/// derived from the dimensions EMIT-1 needs to diff: a `base` reference plus
/// one file per single-setting variation, plus a `uv-` variant. When the real
/// §0.3 names arrive, edit this array — nothing else changes.
const EXPECTED_LBRN2: &[&str] = &[
    "base.lbrn2",
    // MOPA fiber: fluence is set by pulse width + frequency, not the (often
    // fixed) Max Power %. Peak power ~= P_avg / (frequency * pulse_width), so
    // the Q-pulse-width variant is a primary process knob. See decisions.md.
    "pulse-width.lbrn2",
    "speed.lbrn2",
    "frequency.lbrn2",
    "interval.lbrn2",
    "passes.lbrn2",
    "fill-angle.lbrn2",
    "line-vs-fill.lbrn2",
    "two-layer.lbrn2",
    "uv-base.lbrn2",
];

/// Name of the manifest emitted into `samples/`.
const MANIFEST_NAME: &str = "MANIFEST.toml";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fixtures") => {
            let root = workspace_root();
            match fixtures(&root) {
                Ok(report) => {
                    println!("{report}");
                    ExitCode::SUCCESS
                }
                Err(problems) => {
                    eprintln!("fixture validation failed:");
                    for p in &problems {
                        eprintln!("  - {p}");
                    }
                    ExitCode::FAILURE
                }
            }
        }
        Some("seed-defect") => {
            let rest: Vec<String> = args.collect();
            match xtask::seed_defect::cli(&rest) {
                Ok(report) => {
                    println!("{report}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("seed-defect failed: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!(
                "unknown xtask command: {other}\n\n{USAGE}\n{}",
                xtask::seed_defect::USAGE
            );
            ExitCode::from(2)
        }
        None => {
            eprintln!("{USAGE}\n{}", xtask::seed_defect::USAGE);
            ExitCode::from(2)
        }
    }
}

/// The workspace root: the parent of this crate's directory. `CARGO_MANIFEST_DIR`
/// is resolved at compile time to `<root>/xtask`, so the parent is stable
/// regardless of the caller's working directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask crate is nested under the workspace root")
        .to_path_buf()
}

/// One fixture file recorded in the manifest.
struct Entry {
    /// Path relative to the workspace root, using `/` separators.
    rel: String,
    sha256: String,
}

/// Validate the fixture tree under `root` and, if everything is present, write
/// `samples/MANIFEST.toml`. Returns a human-readable report on success, or the
/// list of problems (each a `samples/...`-relative message) on failure. On
/// failure no manifest is written, so a committed manifest always reflects a
/// complete fixture set.
///
/// Checks:
/// * `samples/kicad` contains at least [`MIN_KICAD_PROJECTS`] `*.kicad_pcb`
///   files;
/// * `samples/lbrn2` contains every name in [`EXPECTED_LBRN2`];
/// * if `docs/capture-plan.md` exists (DRV-1's deliverable), a `captures/`
///   directory must exist — fuller layout matching lands with DRV-1.
pub fn fixtures(root: &Path) -> Result<String, Vec<String>> {
    let mut problems = Vec::new();
    let mut entries = Vec::new();

    // samples/kicad — >= MIN_KICAD_PROJECTS .kicad_pcb files.
    let kicad_dir = root.join("samples/kicad");
    let mut kicad_projects = list_files_with_ext(&kicad_dir, "kicad_pcb");
    kicad_projects.sort();
    if kicad_projects.len() < MIN_KICAD_PROJECTS {
        problems.push(format!(
            "samples/kicad: need >= {} .kicad_pcb projects, found {}",
            MIN_KICAD_PROJECTS,
            kicad_projects.len()
        ));
    }
    for path in &kicad_projects {
        push_entry(root, path, &mut entries, &mut problems);
    }

    // samples/lbrn2 — every expected named sample present.
    let lbrn2_dir = root.join("samples/lbrn2");
    for name in EXPECTED_LBRN2 {
        let path = lbrn2_dir.join(name);
        if path.is_file() {
            push_entry(root, &path, &mut entries, &mut problems);
        } else {
            problems.push(format!("samples/lbrn2/{name}: missing"));
        }
    }

    // captures/ — only enforced once DRV-1's capture plan exists.
    if root.join("docs/capture-plan.md").is_file() && !root.join("captures").is_dir() {
        problems.push(
            "captures/: docs/capture-plan.md is present but the captures/ directory is missing"
                .to_string(),
        );
    }

    if !problems.is_empty() {
        problems.sort();
        return Err(problems);
    }

    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    let manifest = render_manifest(&entries);
    let manifest_path = root.join("samples").join(MANIFEST_NAME);
    std::fs::write(&manifest_path, &manifest)
        .map_err(|e| vec![format!("could not write {}: {e}", manifest_path.display())])?;

    Ok(format!(
        "fixtures OK: {} kicad project(s), {} lbrn2 sample(s); wrote samples/{MANIFEST_NAME} ({} entries)",
        kicad_projects.len(),
        EXPECTED_LBRN2.len(),
        entries.len(),
    ))
}

/// Hash `path` and append its manifest entry, or record a read error.
fn push_entry(root: &Path, path: &Path, entries: &mut Vec<Entry>, problems: &mut Vec<String>) {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    match sha256_file(path) {
        Ok(sha256) => entries.push(Entry { rel, sha256 }),
        Err(e) => problems.push(format!("{rel}: {e}")),
    }
}

/// Sorted list of files directly inside `dir` whose extension is `ext`.
/// A missing directory yields an empty list (the count check reports it).
fn list_files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    out
}

/// Lowercase hex SHA-256 of a file's bytes.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Render the manifest TOML: a header comment plus one `[[file]]` table per
/// entry, in the order given.
fn render_manifest(entries: &[Entry]) -> String {
    let mut s = String::new();
    s.push_str("# Auto-generated by `cargo xtask fixtures` (INF-3). Do not edit by hand.\n");
    s.push_str("# Each entry is a test-fixture file and the sha256 of its bytes.\n\n");
    for e in entries {
        s.push_str("[[file]]\n");
        let _ = writeln!(s, "path = \"{}\"", e.rel);
        let _ = writeln!(s, "sha256 = \"{}\"", e.sha256);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique scratch directory under the OS temp dir, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("pcbforge-xtask-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Populate `root` with a complete, valid synthetic fixture tree.
    fn make_valid(root: &Path) {
        write(
            &root.join("samples/kicad/alpha.kicad_pcb"),
            "(kicad_pcb alpha)",
        );
        write(
            &root.join("samples/kicad/beta.kicad_pcb"),
            "(kicad_pcb beta)",
        );
        for (i, name) in EXPECTED_LBRN2.iter().enumerate() {
            write(
                &root.join("samples/lbrn2").join(name),
                &format!("<lbrn2 {i}/>"),
            );
        }
    }

    #[test]
    fn valid_tree_passes_and_writes_manifest() {
        let tmp = TempRoot::new();
        make_valid(tmp.path());

        let report = fixtures(tmp.path()).expect("valid tree should pass");
        assert!(report.contains("fixtures OK"), "report: {report}");

        let manifest = std::fs::read_to_string(tmp.path().join("samples/MANIFEST.toml")).unwrap();
        // 2 kicad + 10 lbrn2 = 12 entries, sorted by path.
        assert_eq!(manifest.matches("[[file]]").count(), 12);
        assert!(manifest.contains("path = \"samples/kicad/alpha.kicad_pcb\""));
        assert!(manifest.contains("path = \"samples/lbrn2/base.lbrn2\""));
        // kicad sorts before lbrn2.
        let a = manifest.find("samples/kicad/alpha").unwrap();
        let b = manifest.find("samples/lbrn2/base").unwrap();
        assert!(a < b, "entries must be sorted by path");
    }

    #[test]
    fn sha256_matches_known_vector() {
        let tmp = TempRoot::new();
        let f = tmp.path().join("abc.txt");
        write(&f, "abc");
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn renamed_lbrn2_sample_fails() {
        let tmp = TempRoot::new();
        make_valid(tmp.path());
        // Simulate the done-when: rename one sample away from its expected name.
        let dir = tmp.path().join("samples/lbrn2");
        std::fs::rename(dir.join("passes.lbrn2"), dir.join("passes-RENAMED.lbrn2")).unwrap();

        let err = fixtures(tmp.path()).expect_err("a renamed sample must fail validation");
        assert!(
            err.iter().any(|p| p.contains("passes.lbrn2: missing")),
            "problems: {err:?}"
        );
        // No manifest written on failure.
        assert!(!tmp.path().join("samples/MANIFEST.toml").exists());
    }

    #[test]
    fn too_few_kicad_projects_fails() {
        let tmp = TempRoot::new();
        make_valid(tmp.path());
        std::fs::remove_file(tmp.path().join("samples/kicad/beta.kicad_pcb")).unwrap();

        let err = fixtures(tmp.path()).expect_err("one project is below the minimum");
        assert!(
            err.iter().any(|p| p.contains("need >= 2 .kicad_pcb")),
            "problems: {err:?}"
        );
    }

    #[test]
    fn missing_samples_dir_fails_without_panicking() {
        let tmp = TempRoot::new(); // empty
        let err = fixtures(tmp.path()).expect_err("empty tree must fail");
        // Reports both the kicad shortfall and every missing lbrn2 sample.
        assert!(err.iter().any(|p| p.contains("samples/kicad")));
        assert_eq!(
            err.iter().filter(|p| p.contains("samples/lbrn2/")).count(),
            EXPECTED_LBRN2.len()
        );
    }

    #[test]
    fn captures_required_only_when_plan_present() {
        let tmp = TempRoot::new();
        make_valid(tmp.path());
        // Without a capture plan, absence of captures/ is fine.
        assert!(fixtures(tmp.path()).is_ok());

        // With a plan but no captures dir, it must fail.
        write(&tmp.path().join("docs/capture-plan.md"), "# plan");
        let err = fixtures(tmp.path()).expect_err("plan present, captures/ missing");
        assert!(
            err.iter().any(|p| p.contains("captures/")),
            "problems: {err:?}"
        );

        // Adding the directory clears it.
        std::fs::create_dir_all(tmp.path().join("captures")).unwrap();
        assert!(fixtures(tmp.path()).is_ok());
    }
}
