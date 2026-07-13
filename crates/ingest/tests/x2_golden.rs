//! ING-3 / ING-4 validation.
//!
//! * ING-3 cross-check: the X2-attributed parse must produce geometry
//!   *identical* to the plain [`load_gerber`] path — attribute tracking must
//!   not perturb a single vertex. `load_gerber` is itself golden-validated
//!   against KiCad's own SVG render in `kicad_golden.rs` (≥99.5 %), so this
//!   equality carries that agreement over to the X2 path (the "X2 layer vs
//!   SVG layer" done-when), transitively through the shared KiCad ground
//!   truth. See docs/decisions.md.
//! * Attribute test: a known pad is flagged with its net and reference, and a
//!   hand-authored fiducial aperture is flagged (valdemo2 has no fiducials).
//! * ING-4: two real nets rasterize to distinct IDs.
//!
//! The KiCad-dependent tests self-skip when kicad-cli is absent.

use std::path::{Path, PathBuf};

use ingest::gerber::{self, parse_gerber_x2};
use ingest::kicad_cli::KicadCli;
use ingest::net_raster::net_raster;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-x2-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn x2_geometry_is_identical_to_the_plain_parse() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    for board in ["valdemo.kicad_pcb", "valdemo2.kicad_pcb"] {
        let dir = tmp_dir("eq");
        let g = cli
            .export_gerbers(
                &repo_root().join("samples/kicad").join(board),
                &["F.Cu"],
                &dir,
            )
            .unwrap();
        let plain = gerber::load_gerber(&g[0]).unwrap();
        let x2 = gerber::load_gerber_x2(&g[0]).unwrap();
        assert_eq!(
            x2.layer().polys,
            plain.polys,
            "X2 parse changed geometry on {board}"
        );
        assert!(!plain.polys.is_empty());
    }
}

#[test]
fn x2_flags_known_pads_and_nets() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    let dir = tmp_dir("attr");
    let g = cli
        .export_gerbers(
            &repo_root().join("samples/kicad/valdemo2.kicad_pcb"),
            &["F.Cu"],
            &dir,
        )
        .unwrap();
    let x2 = gerber::load_gerber_x2(&g[0]).unwrap();

    // The two named nets are present.
    assert_eq!(x2.net_names(), vec!["GND".to_string(), "VCC".to_string()]);

    // J1 pin 1 is a component pad on VCC (from the board source).
    let j1_1 = x2
        .objects()
        .iter()
        .find(|o| o.pad.as_ref() == Some(&("J1".to_string(), "1".to_string())))
        .expect("J1 pad 1 present");
    assert!(j1_1.is_pad(), "J1.1 must be flagged a pad");
    assert_eq!(j1_1.net.as_deref(), Some("VCC"));

    // valdemo2 carries no fiducials.
    assert!(x2.fiducials().is_empty());
}

#[test]
fn x2_flags_a_fiducial_aperture() {
    // Hand-authored: one fiducial-pad flash. (`%TA.AperFunction,FiducialPad`.)
    let src = "%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*%\n%MOMM*%\nG04 fid*\n%LPD*%\n\
               %TA.AperFunction,FiducialPad,Local*%\n%ADD10C,1.000000*%\n%TD*%\n\
               D10*\nX5000000Y5000000D03*\nM02*\n";
    let x2 = parse_gerber_x2(src).unwrap();
    assert_eq!(x2.fiducials().len(), 1, "one fiducial pad");
    assert!(x2.fiducials()[0].is_fiducial());
    assert!(x2.fiducials()[0].is_pad());
}

#[test]
fn net_raster_gives_vcc_and_gnd_distinct_ids() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    let dir = tmp_dir("raster");
    let g = cli
        .export_gerbers(
            &repo_root().join("samples/kicad/valdemo2.kicad_pcb"),
            &["F.Cu"],
            &dir,
        )
        .unwrap();
    let x2 = gerber::load_gerber_x2(&g[0]).unwrap();
    let (img, names) = net_raster(&x2, 50);
    assert_eq!(names, vec!["GND".to_string(), "VCC".to_string()]);
    let (gnd, vcc) = (1u16, 2u16);

    // Both nets occupy pixels, and the IDs are distinct.
    assert!(img.count(gnd) > 0, "GND rasterized");
    assert!(img.count(vcc) > 0, "VCC rasterized");
    assert_ne!(gnd, vcc);

    // A point inside each net's copper maps to that net's ID. Sample the
    // centroid of a *pad* on the net (pads are convex, so the centroid is
    // interior) — taken from the parsed geometry, so it is frame-correct
    // without any GUI coordinates.
    for (refdes, pin, id) in [("J1", "1", vcc), ("J1", "2", gnd)] {
        let pad = x2
            .objects()
            .iter()
            .find(|o| o.pad.as_ref() == Some(&(refdes.to_string(), pin.to_string())))
            .unwrap_or_else(|| panic!("pad {refdes}.{pin} present"));
        let (cx, cy) = centroid(&pad.polys[0].outer);
        let probe = pcb_core::P::new(cx as i64, cy as i64);
        assert_eq!(
            img.id_at_nm(probe),
            id,
            "pad {refdes}.{pin} centroid maps to its net id"
        );
    }
}

fn centroid(ring: &[pcb_core::P]) -> (f64, f64) {
    let n = ring.len().max(1) as f64;
    (
        ring.iter().map(|p| p.x as f64).sum::<f64>() / n,
        ring.iter().map(|p| p.y as f64).sum::<f64>() / n,
    )
}
