//! Render the Fiducial-check overlay on a synthetic frame to a PNG, so the
//! markers can be eyeballed without a camera. Builds a frame mimicking the
//! operator's field photo (dark drilled holes on glary copper + a honeycomb-
//! style decoy), runs VIS-4 via the console's check, and saves the overlay.
//!
//!   cargo run -p ui --example dump_fiducials -- out.png

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fiducials.png".into());
    let ppm = 10.0_f64; // px per mm

    // Operator L-layout at (10,10)/(60,10)/(10,60) mm, board nudged ~0.4 mm,
    // plus a same-size decoy hole 2.5 mm from the first fiducial. Bed mm is
    // y-up from the frame's bottom-left; pixel rows grow downward.
    let (w, h) = (720u32, 720u32);
    let flip = |y_mm: f64| h as f64 - y_mm * ppm;
    let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    let (dx, dy) = (0.4, -0.3);
    let mut dots: Vec<(f64, f64, f64)> = expected
        .iter()
        .map(|(ex, ey)| ((ex + dx) * ppm, flip(ey + dy), 1.0 * ppm))
        .collect();
    dots.push(((10.0 + dx) * ppm + 25.0, flip(10.0 + dy), 1.0 * ppm)); // decoy
    let mut seed = 1u64;
    let frame = image::GrayImage::from_fn(w, h, |x, y| {
        // glare gradient
        let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
        // 4×4 supersampled dark discs
        let mut cover = 0.0;
        for sy in 0..4 {
            for sx in 0..4 {
                let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                if dots
                    .iter()
                    .any(|&(cx, cy, d)| ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0)
                {
                    cover += 1.0 / 16.0;
                }
            }
        }
        // deterministic noise
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let n = ((seed.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64 * 2.0
            - 1.0)
            * 5.0;
        image::Luma([(bg - 85.0 * cover + n).clamp(0.0, 255.0) as u8])
    });

    let profile = ui::ProfileKind::DarkDot.to_profile(ui::FidShape::Circle { diameter_mm: 1.0 });
    let r = ui::check_frame(&frame, &expected, ppm, &profile, 2.0);
    let (s, weak, m) = r.tally;
    println!("{s} strong, {weak} weak, {m} missed");
    for row in &r.rows {
        println!("  {}", row.text);
    }

    let [ow, oh] = r.overlay.size;
    let mut buf = image::RgbaImage::new(ow as u32, oh as u32);
    for (i, px) in r.overlay.pixels.iter().enumerate() {
        buf.put_pixel(
            (i % ow) as u32,
            (i / ow) as u32,
            image::Rgba([px.r(), px.g(), px.b(), 255]),
        );
    }
    buf.save(&out).expect("save png");
    println!("wrote {out}");
}
