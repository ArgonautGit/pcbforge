//! End-to-end test of `pcbforge gerbers`: point at a KiCad project and export
//! the copper + outline Gerbers the pipeline needs. Self-skips when kicad-cli
//! isn't installed (the export can't run without it).

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-gerbers-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn gerbers_exports_copper_and_outline_from_a_project() {
    let out = tmp();
    let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "gerbers",
            "--project",
            board.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr);
        if err.contains("kicad-cli not found") {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        panic!("gerbers failed: {err}");
    }

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("copper:") && stdout.contains("outline:"),
        "prints the two Gerber paths:\n{stdout}"
    );
    // The copper name carries its layer (default F.Cu), so a later back-side
    // export into the same directory lands beside it rather than on it.
    let copper = out.join("copper-F_Cu.gbr");
    let outline = out.join("outline.gbr");
    assert!(
        copper.is_file() && outline.is_file(),
        "stable-named Gerbers"
    );
    // Real Gerber output (has the `%` extended-command header).
    assert!(std::fs::read_to_string(&copper).unwrap().contains('%'));
    assert!(std::fs::read_to_string(&outline).unwrap().contains('%'));
}

#[test]
fn gerbers_accepts_a_project_directory() {
    // A directory with exactly one .kicad_pcb resolves to that board.
    let dir = tmp().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        repo_root().join("samples/kicad/valdemo2.kicad_pcb"),
        dir.join("proj.kicad_pcb"),
    )
    .unwrap();
    let out = tmp().join("dir-out");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "gerbers",
            "--project",
            dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr);
        if err.contains("kicad-cli not found") {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        panic!("gerbers on a dir failed: {err}");
    }
    assert!(out.join("copper-F_Cu.gbr").is_file());
}
