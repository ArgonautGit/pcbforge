//! Render the camera-lens calibration overlay to a PNG: a printed grid imaged
//! with barrel distortion, the fitted correction, and the distortion arrows.
//!
//!   cargo run -p ui --example dump_lens -- out.png

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = a.get(1).cloned().unwrap_or_else(|| "lens.png".into());
    let grid = ui::GridSpec {
        origin_mm: (0.0, 0.0),
        pitch_mm: 10.0,
        n: 7,
    };
    let ppm = 9.0;
    let dot = 1.5;
    let base = |mx: f64, my: f64| (60.0 + ppm * mx + 0.02 * my, 60.0 + ppm * my - 0.015 * mx);
    let distort = |u: f64, v: f64| {
        let (cx, cy) = (330.0, 330.0);
        let (du, dv) = (u - cx, v - cy);
        let r2 = (du * du + dv * dv) / (330.0 * 330.0);
        let k = 0.05; // 5% barrel at the corner
        (cx + du * (1.0 + k * r2), cy + dv * (1.0 + k * r2))
    };
    let pts = grid.points();
    let img = image::GrayImage::from_fn(660, 660, |x, y| {
        let dark = pts.iter().any(|&(mx, my)| {
            let (u, v) = base(mx, my);
            let (u, v) = distort(u, v);
            (((x as f64) - u).powi(2) + ((y as f64) - v).powi(2)).sqrt() < 0.5 * dot * ppm
        });
        image::Luma([if dark { 40 } else { 205 }])
    });
    let corners = grid.corners_mm().map(|(mx, my)| {
        let (u, v) = base(mx, my);
        distort(u, v)
    });
    let cal = ui::fit_camera_lens(&img, corners, &grid, dot, ui::DotKind::Dark).expect("fit");
    eprintln!(
        "lens RMS {:.0} µm, worst {:.0} µm, {} dots",
        cal.lens.rms_um, cal.lens.max_um, cal.found
    );

    let mut buf = image::RgbImage::from_fn(660, 660, |x, y| {
        let g = img.get_pixel(x, y)[0];
        image::Rgb([g, g, g])
    });
    let scale = 15.0; // exaggerate the distortion arrows
    for d in &cal.dots {
        let (bx, by) = (d.px.0, d.px.1);
        let (tx, ty) = (bx + d.distort_px.0 * scale, by + d.distort_px.1 * scale);
        draw_line(&mut buf, bx, by, tx, ty, image::Rgb([0xd0, 0x50, 0xd0]));
        let col = if d.resid_um < 30.0 {
            image::Rgb([0x40, 0xc0, 0x50])
        } else if d.resid_um < 100.0 {
            image::Rgb([0xe0, 0x90, 0x20])
        } else {
            image::Rgb([0xd0, 0x40, 0x40])
        };
        ring(&mut buf, bx, by, 4.0, col);
    }
    buf.save(&out).unwrap();
    println!("wrote {out}");
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
    for a in 0..36 {
        let t = a as f64 * std::f64::consts::PI / 18.0;
        put(b, (cx + r * t.cos()) as i32, (cy + r * t.sin()) as i32, c);
    }
}
