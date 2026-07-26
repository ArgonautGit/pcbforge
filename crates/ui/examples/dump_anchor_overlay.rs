//! Render the laser-anchor calibration overlay to a PNG, so the visual feedback
//! can be eyeballed outside the GUI. Loads the committed distorted-grid fixture,
//! fits the camera→laser anchor, and draws what the console draws: the machine
//! coordinate grid the camera reconstructs (blue mesh), the origin + axes
//! (green), a quality-colored ring per detected dot with an exaggerated residual
//! vector (orange, commanded→detected), and a red ✕ for any dot that didn't
//! lock.
//!
//!   cargo run -p ui --example dump_anchor_overlay -- out.png

use nalgebra::Point2;

const EXAGG: f64 = 6.0; // residual-vector exaggeration

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = a.get(1).cloned().unwrap_or_else(|| "anchor.png".into());
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
    // Corner dots from the fixture's JSON sidecar (LL, LR, UR, UL).
    let corners = [
        (42.506, 632.64),
        (620.163, 618.878),
        (606.277, 39.892),
        (43.744, 41.83),
    ];
    let cal = calib::fit_camera_to_machine(&img, corners, &grid, 2.0, calib::DotKind::Dark)
        .expect("anchor fit");
    let worst = cal.dots.iter().map(|d| d.resid_um).fold(0.0_f64, f64::max);
    eprintln!(
        "anchor {}/{} dots, RMS {:.0} µm, worst {:.0} µm",
        cal.found, cal.total, cal.rms_um, worst
    );

    let (w, h) = (img.width(), img.height());
    let mut buf = image::RgbImage::from_fn(w, h, |x, y| {
        let g = img.get_pixel(x, y)[0];
        image::Rgb([g, g, g])
    });

    // mm → px projection (inverse of the fitted camera-px → mm anchor).
    let inv = cal.px_to_mm.try_inverse().expect("invertible");
    let proj = |mx: f64, my: f64| {
        let p = inv.apply(Point2::new(mx, my));
        (p.x, p.y)
    };

    // Machine-grid mesh.
    let n = grid.n;
    let pts = grid.points();
    let node: Vec<(f64, f64)> = pts.iter().map(|&(mx, my)| proj(mx, my)).collect();
    let blue = image::Rgb([0x35, 0x70, 0xb0]);
    for r in 0..n {
        for c in 0..n {
            let (x0, y0) = node[r * n + c];
            if c + 1 < n {
                let (x1, y1) = node[r * n + c + 1];
                draw_line(&mut buf, x0, y0, x1, y1, blue);
            }
            if r + 1 < n {
                let (x1, y1) = node[(r + 1) * n + c];
                draw_line(&mut buf, x0, y0, x1, y1, blue);
            }
        }
    }

    // Undetected lattice sites: a red ✕.
    let red = image::Rgb([0xd0, 0x40, 0x40]);
    for &(mx, my) in &pts {
        let detected = cal
            .dots
            .iter()
            .any(|d| (d.mm.0 - mx).abs() < 1e-6 && (d.mm.1 - my).abs() < 1e-6);
        if !detected {
            let (x, y) = proj(mx, my);
            draw_line(&mut buf, x - 5.0, y - 5.0, x + 5.0, y + 5.0, red);
            draw_line(&mut buf, x - 5.0, y + 5.0, x + 5.0, y - 5.0, red);
        }
    }

    // Detected dots: residual vector (commanded→detected, exaggerated) + ring.
    let orange = image::Rgb([0xf0, 0x90, 0x30]);
    for d in &cal.dots {
        let (dx, dy) = (d.px.0, d.px.1);
        let (cx, cy) = proj(d.mm.0, d.mm.1);
        draw_line(
            &mut buf,
            cx,
            cy,
            cx + (dx - cx) * EXAGG,
            cy + (dy - cy) * EXAGG,
            orange,
        );
        let col = if d.resid_um < 50.0 {
            image::Rgb([0x40, 0xc0, 0x50])
        } else if d.resid_um < 200.0 {
            image::Rgb([0xe0, 0x90, 0x20])
        } else {
            red
        };
        ring(&mut buf, dx, dy, 4.0, col);
    }

    // Origin + axes.
    let green = image::Rgb([0x30, 0xd0, 0x80]);
    let (ox, oy) = grid.origin_mm;
    let (o_x, o_y) = proj(ox, oy);
    let (xx, xy) = proj(ox + grid.pitch_mm, oy);
    let (yx, yy) = proj(ox, oy + grid.pitch_mm);
    for dd in 0..3 {
        let f = dd as f64 * 0.5;
        draw_line(&mut buf, o_x, o_y + f, xx, xy + f, green);
        draw_line(&mut buf, o_x + f, o_y, yx + f, yy, green);
    }
    ring(&mut buf, o_x, o_y, 4.0, green);
    ring(&mut buf, o_x, o_y, 2.0, green);

    buf.save(&out).expect("save");
    eprintln!("wrote {out}");
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
