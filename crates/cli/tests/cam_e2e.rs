//! End-to-end test of `pcbforge cam` (FLD-13 / VIS-1 CLI surface): grab a frame
//! from a File source and enumerate devices. The device backend needs the
//! `camera` feature + hardware, so these cover the always-available paths.

use std::path::PathBuf;
use std::process::Command;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-cam-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn cam_grab_from_file_writes_a_gray_png() {
    let dir = tmp("grab");
    let src = dir.join("src.png");
    image::GrayImage::from_fn(20, 12, |x, y| image::Luma([((x * 13 + y * 7) % 256) as u8]))
        .save(&src)
        .unwrap();
    let out = dir.join("out.png");

    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "cam",
            "--grab",
            out.to_str().unwrap(),
            "--file",
            src.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let grabbed = image::open(&out).unwrap().to_luma8();
    assert_eq!(grabbed.dimensions(), (20, 12));
}

#[test]
fn cam_list_runs_and_reports() {
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(["cam", "--list"])
        .output()
        .unwrap();
    assert!(result.status.success());
    // Without the `camera` feature there are no devices; the message guides the
    // operator to the feature or a File source.
    let out = String::from_utf8_lossy(&result.stdout);
    assert!(
        out.contains("no camera devices") || out.contains(':'),
        "unexpected list output: {out}"
    );
}

#[test]
fn cam_device_without_feature_errors_cleanly() {
    let dir = tmp("dev");
    let out = dir.join("x.png");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(["cam", "--grab", out.to_str().unwrap(), "--device", "0"])
        .output()
        .unwrap();
    // With the `camera` feature this depends on hardware; without it the CLI
    // fails with a message that names the feature.
    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr);
        assert!(err.contains("camera"), "stderr: {err}");
    }
}

#[test]
fn cam_with_no_action_is_a_usage_error() {
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(["cam"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("--list or --grab"),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
