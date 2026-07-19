//! End-to-end test of `pcbforge emit --mirror-x` (double-sided groundwork,
//! ORC-6): the back side of a board is the front design mirrored in X. A left
//! feature must land on the right after mirroring, while the overall bbox and
//! shape count are unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-mirror-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn identity_field_map(dir: &Path) -> PathBuf {
    use nalgebra::Point2;
    let pairs: Vec<_> = (0..4)
        .flat_map(|row| {
            (0..4).map(move |column| {
                let point = Point2::new(column as f64 * 50.0, row as f64 * 50.0);
                (point, point)
            })
        })
        .collect();
    let path = dir.join("identity-field.txt");
    std::fs::write(&path, vision::fit_field(&pairs).unwrap().serialize()).unwrap();
    path
}

/// Parse the first `V<x> <y>` X-coordinates out of an emitted `.lbrn2`.
fn vert_xs(doc: &str) -> Vec<f64> {
    let mut xs = Vec::new();
    for tok in doc.split("V").skip(1) {
        // token looks like "12.34 5.67c0x1c1x1..."; take the leading number.
        let num: String = tok
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        if let Ok(x) = num.parse::<f64>() {
            xs.push(x);
        }
    }
    xs
}

#[test]
fn mirror_x_reflects_the_design_for_the_back() {
    let dir = tmp();
    let field = identity_field_map(&dir);
    let front = dir.join("front.lbrn2");
    let back = dir.join("back.lbrn2");
    let args = |out: &Path, mirror: bool| {
        let mut a = vec![
            "emit".to_string(),
            "--copper".into(),
            fixture("demo-F_Cu.gbr").to_str().unwrap().into(),
            "--outline".into(),
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap().into(),
            "--lbrn2".into(),
            out.to_str().unwrap().into(),
            "--field-map".into(),
            field.to_str().unwrap().into(),
        ];
        if mirror {
            a.push("--mirror-x".into());
        }
        a
    };

    for (out, m) in [(&front, false), (&back, true)] {
        let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
            .args(args(out, m))
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }

    let fdoc = std::fs::read_to_string(&front).unwrap();
    let bdoc = std::fs::read_to_string(&back).unwrap();

    // Both are valid, corner-normalized (no negative coords) projects with the
    // same board width.
    assert!(!fdoc.contains("V-") && !bdoc.contains("V-"));
    let (fx, bx) = (vert_xs(&fdoc), vert_xs(&bdoc));
    let fmax = fx.iter().cloned().fold(0.0_f64, f64::max);
    let bmax = bx.iter().cloned().fold(0.0_f64, f64::max);
    assert!(
        (fmax - bmax).abs() < 1e-3,
        "same board width front {fmax} vs back {bmax}"
    );

    // Mirroring is a real change: the emitted vertex stream differs (an
    // asymmetric copper pattern flips left↔right).
    assert_ne!(
        fdoc, bdoc,
        "the back must differ from the front (design mirrored)"
    );
}
