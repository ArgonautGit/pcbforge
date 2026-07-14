//! Render the drag-to-place composite (job overlaid on a bed frame at a chosen
//! placement) to a PNG, so the placement can be eyeballed without a camera.
//!
//!   cargo run -p ui --example dump_place -- <copper.gbr> <outline.gbr> <out.png>

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: dump_place <copper.gbr> <outline.gbr> <out.png>");
        std::process::exit(2);
    }
    let ppm = 8.0_f64;

    // A blank bed frame with a faint glare gradient (no holes — this is about
    // placement, not detection).
    let (w, h) = (640u32, 480u32);
    let frame = image::GrayImage::from_fn(w, h, |x, y| {
        let bg = 150.0 + 60.0 * (x as f64 + y as f64) / (w + h) as f64;
        image::Luma([bg.clamp(0.0, 255.0) as u8])
    });

    // Job geometry (to-ablate regions) in the Gerber frame.
    let (_board, _copper, ablate) = ui::job_shapes(&a[1], &a[2], 0.0).expect("job shapes");
    let pivot = ui::bbox_center_mm(&ablate);

    // Place the job at bed (40, 30) mm, rotated 15°.
    let placement = ui::Placement {
        tx_mm: 40.0,
        ty_mm: 30.0,
        rot_deg: 15.0,
        pivot_mm: pivot,
    };
    println!("register correspondences: {}", placement.correspondences());

    let img = ui::composite(&frame, &ablate, &placement, ppm, [0xf0, 0x50, 0x30], 0.55);
    let [ow, oh] = img.size;
    let mut buf = image::RgbaImage::new(ow as u32, oh as u32);
    for (i, px) in img.pixels.iter().enumerate() {
        buf.put_pixel(
            (i % ow) as u32,
            (i / ow) as u32,
            image::Rgba([px.r(), px.g(), px.b(), 255]),
        );
    }
    buf.save(&a[3]).expect("save png");
    println!("wrote {}", a[3]);
}
