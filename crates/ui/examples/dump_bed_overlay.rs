//! Render the camera-feed bed overlay to a PNG for eyeballing: the laser work
//! area and a homography-aligned 50 mm scale projected onto the bed through the
//! anchor. Mirrors `ConsoleApp::draw_bed_overlay` with the image crate (the GUI
//! version draws the same geometry through the pan/zoom transform).
//!
//! The fixture's grid only spans 0..60 mm, so this frames a 60 mm work area at
//! its centre rather than the 140 mm galvo-field default.
//!
//!   cargo run -p ui --example dump_bed_overlay -- out.png

use nalgebra::Point2;

const FIELD_MM: f64 = 60.0;
const FIELD_C: (f64, f64) = (30.0, 30.0);

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = a.get(1).cloned().unwrap_or_else(|| "bed.png".into());
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration/grid-7x7-10mm-distorted.png"
    );
    let img = image::open(fixture).expect("fixture").to_luma8();
    let grid = calib::GridSpec {
        origin_mm: (0.0, 0.0),
        pitch_mm: 10.0,
        n: 7,
    };
    let corners = [
        (42.506, 632.64),
        (620.163, 618.878),
        (606.277, 39.892),
        (43.744, 41.83),
    ];
    let cal = calib::fit_camera_to_machine(&img, corners, &grid, 2.0, calib::DotKind::Dark)
        .expect("anchor fit");
    let inv = cal.px_to_mm.try_inverse().expect("invertible");
    let proj = |mx: f64, my: f64| {
        let p = inv.apply(Point2::new(mx, my));
        (p.x, p.y)
    };

    let (w, h) = (img.width(), img.height());
    let mut buf = image::RgbImage::from_fn(w, h, |x, y| {
        let g = img.get_pixel(x, y)[0];
        image::Rgb([g, g, g])
    });

    let yellow = image::Rgb([0xf0, 0xd0, 0x40]);
    let green = image::Rgb([0x30, 0xd0, 0x80]);
    let white = image::Rgb([0xff, 0xff, 0xff]);

    // Work-area square.
    let (cx, cy) = FIELD_C;
    let hh = FIELD_MM / 2.0;
    let sq = [
        (cx - hh, cy - hh),
        (cx + hh, cy - hh),
        (cx + hh, cy + hh),
        (cx - hh, cy + hh),
    ];
    let sp: Vec<(f64, f64)> = sq.iter().map(|&(x, y)| proj(x, y)).collect();
    for i in 0..4 {
        let (x0, y0) = sp[i];
        let (x1, y1) = sp[(i + 1) % 4];
        draw_line(&mut buf, x0, y0, x1, y1, yellow);
    }

    // 50 mm scale L at the work area's lower-left corner.
    let base = (cx - hh, cy - hh);
    let b = proj(base.0, base.1);
    for end in [proj(base.0 + 50.0, base.1), proj(base.0, base.1 + 50.0)] {
        draw_line(&mut buf, b.0, b.1, end.0, end.1, white);
        // perpendicular end caps
        let (dx, dy) = (end.0 - b.0, end.1 - b.1);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let (px, py) = (-dy / len * 5.0, dx / len * 5.0);
        draw_line(&mut buf, b.0 - px, b.1 - py, b.0 + px, b.1 + py, white);
        draw_line(
            &mut buf,
            end.0 - px,
            end.1 - py,
            end.0 + px,
            end.1 + py,
            white,
        );
    }

    // Machine origin + axes.
    let o = proj(0.0, 0.0);
    for end in [proj(20.0, 0.0), proj(0.0, 20.0)] {
        draw_line(&mut buf, o.0, o.1, end.0, end.1, green);
    }
    ring(&mut buf, o.0, o.1, 4.0, green);

    buf.save(&out).expect("save");
    eprintln!("wrote {out} (work area {FIELD_MM} mm @ {FIELD_C:?})");
}

fn put(b: &mut image::RgbImage, x: i32, y: i32, c: image::Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < b.width() && (y as u32) < b.height() {
        b.put_pixel(x as u32, y as u32, c);
    }
}
fn draw_line(b: &mut image::RgbImage, x0: f64, y0: f64, x1: f64, y1: f64, c: image::Rgb<u8>) {
    let n = (((x1 - x0).abs().max((y1 - y0).abs())) as i32).max(1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        put(
            b,
            (x0 + (x1 - x0) * t) as i32,
            (y0 + (y1 - y0) * t) as i32,
            c,
        );
    }
}
fn ring(b: &mut image::RgbImage, cx: f64, cy: f64, r: f64, c: image::Rgb<u8>) {
    for a in 0..48 {
        let t = a as f64 * std::f64::consts::PI / 24.0;
        put(b, (cx + r * t.cos()) as i32, (cy + r * t.sin()) as i32, c);
    }
}
