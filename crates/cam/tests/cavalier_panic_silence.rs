//! FLD-3 regression: cavalier_contours 0.7.0 panics internally on some
//! collapsing offsets. Those panics are caught (`catch_unwind`) so the result
//! is correct, but the *default panic hook* used to print
//! `panicked at cavalier_contours-…/pline_view.rs` + a backtrace note to
//! stderr, spamming the operator during `emit`/`noncopper`/`cut` on dense
//! boards. `geom::offset` now installs a filtering panic hook that swallows
//! cavalier's own panics and delegates everything else.
//!
//! The hook is process-global, so this verifies it in a child process: the
//! parent re-execs this same test with `FLD3_CHILD` set, the child hammers
//! `geom::offset` with pathological collapsing inputs and prints a completion
//! marker, and the parent asserts the child's stderr carried the marker but
//! none of cavalier's panic chatter.

use std::process::Command;

use pcb_core::{P, Poly, Ring};

fn sq(cx: i64, cy: i64, r: i64) -> Poly {
    Poly {
        outer: vec![
            P::new(cx - r, cy - r),
            P::new(cx + r, cy - r),
            P::new(cx + r, cy + r),
            P::new(cx - r, cy + r),
        ] as Ring,
        holes: vec![],
    }
}

/// Drive many inward offsets far past collapse — the #79 panic regime.
fn hammer() {
    for r in [50_000i64, 20_000, 5_000, 1_000, 200] {
        let polys: Vec<Poly> = (0..8).map(|i| sq(i * 200_000, 0, r)).collect();
        for d in [-r * 2, -r, -r / 2, -r * 10] {
            let _ = cam::geom::offset(&polys, d);
        }
    }
}

#[test]
fn cavalier_collapse_does_not_spam_stderr() {
    if std::env::var("FLD3_CHILD").is_ok() {
        // Child: the filtering hook is installed inside geom::offset. If the
        // fix regresses, cavalier's default-hook output lands on this stderr.
        hammer();
        eprintln!("FLD3_MARKER_OK");
        return;
    }

    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args([
            "cavalier_collapse_does_not_spam_stderr",
            "--exact",
            "--nocapture",
        ])
        .env("FLD3_CHILD", "1")
        .output()
        .expect("re-exec child");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FLD3_MARKER_OK"),
        "child did not reach the marker; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("cavalier_contours"),
        "cavalier panic chatter leaked to stderr (FLD-3 regressed):\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "a panic message leaked to stderr (FLD-3 regressed):\n{stderr}"
    );
}
