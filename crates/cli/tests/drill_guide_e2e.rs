//! End-to-end test of `pcbforge drill-guide` (ORC-7, software half): a
//! three-hole board is walked start to finish across process invocations —
//! prompt, drill (synthetically), confirm, advance — with an undrilled hole
//! refusing to advance, and the final invocation archiving the overlay.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-guide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Two 1.0 mm holes and one 0.4 mm hole → largest-first order:
/// (5,5,1.0), (15,5,1.0), (10,10,0.4).
const DRL: &str = "\
M48
FMAT,2
METRIC
T1C0.400
T2C1.000
%
G90
G05
T1
X10.0Y10.0
T2
X5.0Y5.0
X15.0Y5.0
M30
";

/// A camera frame (10 px/mm, 250×250) showing exactly `drilled` as dark holes.
fn write_frame(path: &Path, drilled: &[(f64, f64, f64)]) {
    const PPM: f64 = 10.0;
    let img = image::GrayImage::from_fn(250, 250, |x, y| {
        let mut cover = 0.0;
        for sy in 0..4 {
            for sx in 0..4 {
                let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                if drilled.iter().any(|&(hx, hy, d)| {
                    ((px - hx * PPM).powi(2) + (py - hy * PPM).powi(2)).sqrt() < d * PPM / 2.0
                }) {
                    cover += 1.0_f64 / 16.0;
                }
            }
        }
        image::Luma([(200.0 - 160.0 * cover).clamp(0.0, 255.0) as u8])
    });
    img.save(path).unwrap();
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn guided_drilling_walks_three_holes_to_done() {
    let dir = tmp();
    let drl = dir.join("board.drl");
    std::fs::write(&drl, DRL).unwrap();
    let state = dir.join("state.txt");
    let overlay = dir.join("overlay.png");
    let frame = dir.join("frame.png");

    let base_args = |frame_path: Option<&Path>| -> Vec<String> {
        let mut a: Vec<String> = vec![
            "drill-guide".into(),
            "--drills".into(),
            drl.to_str().unwrap().into(),
            "--state".into(),
            state.to_str().unwrap().into(),
            "--overlay".into(),
            overlay.to_str().unwrap().into(),
            "--px-per-mm".into(),
            "10".into(),
        ];
        if let Some(f) = frame_path {
            a.push("--frame".into());
            a.push(f.to_str().unwrap().into());
        }
        a
    };
    fn argv(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    // 1. First invocation: no frame needed — prompts the first (largest-bit)
    //    hole and writes the overlay + state.
    let (ok, out, err) = run(&argv(&base_args(None)));
    assert!(ok, "first step: {err}");
    assert!(
        out.contains("hole #1/3") && out.contains("5.000, 5.000") && out.contains("1.00 mm"),
        "prompts the first largest-bit hole: {out}"
    );
    assert!(overlay.exists() && state.exists());

    // 2. Operator "drills" hole 1 → frame shows it → confirmed, advance to #2.
    write_frame(&frame, &[(5.0, 5.0, 1.0)]);
    let (ok, out, err) = run(&argv(&base_args(Some(&frame))));
    assert!(ok, "confirm step: {err}");
    assert!(
        out.contains("confirmed hole #0") && out.contains("hole #2/3"),
        "confirms and prompts the next: {out}"
    );

    // 3. Rerun WITHOUT drilling hole 2 → refuses to advance.
    let (ok, _out, err) = run(&argv(&base_args(Some(&frame))));
    assert!(!ok, "an undrilled hole must refuse to advance");
    assert!(err.contains("no drilled hole"), "names the failure: {err}");

    // 4. Drill hole 2 (both now present) → confirmed, prompts #3 with a bit
    //    change to the 0.4 mm bit.
    write_frame(&frame, &[(5.0, 5.0, 1.0), (15.0, 5.0, 1.0)]);
    let (ok, out, err) = run(&argv(&base_args(Some(&frame))));
    assert!(ok, "second confirm: {err}");
    assert!(
        out.contains("hole #3/3") && out.contains("0.40 mm") && out.contains("fit the 0.40 mm bit"),
        "prompts the bit change: {out}"
    );

    // 5. Drill the last hole → all confirmed, archive written.
    write_frame(
        &frame,
        &[(5.0, 5.0, 1.0), (15.0, 5.0, 1.0), (10.0, 10.0, 0.4)],
    );
    let (ok, out, err) = run(&argv(&base_args(Some(&frame))));
    assert!(ok, "final step: {err}");
    assert!(
        out.contains("all 3 holes confirmed"),
        "reports completion: {out}"
    );
    assert!(overlay.exists(), "archive overlay written");

    // 6. A different drill file with the same state refuses (stale progress).
    let drl2 = dir.join("other.drl");
    std::fs::write(&drl2, DRL.replace("X10.0Y10.0", "X12.0Y10.0")).unwrap();
    let mut a2 = base_args(Some(&frame));
    a2[2] = drl2.to_str().unwrap().into();
    let (ok, _out, err) = run(&argv(&a2));
    assert!(!ok);
    assert!(
        err.contains("different drill file"),
        "stale state named: {err}"
    );
}
