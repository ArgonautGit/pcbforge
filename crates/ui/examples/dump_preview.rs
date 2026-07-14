//! Render the console's preview panel image for a board to a PNG, so the
//! rasterizer output can be eyeballed. Usage:
//!   cargo run -p ui --example dump_preview -- <copper.gbr> <outline.gbr> <out.png> [offset_mm]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: dump_preview <copper.gbr> <outline.gbr> <out.png> [offset_mm]");
        std::process::exit(2);
    }
    let offset: f64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let (img, note) = ui::preview_image(&a[1], &a[2], offset).expect("preview");
    println!("{note}");
    let [w, h] = img.size;
    let mut buf = image::RgbaImage::new(w as u32, h as u32);
    for (i, px) in img.pixels.iter().enumerate() {
        let (x, y) = ((i % w) as u32, (i / w) as u32);
        buf.put_pixel(x, y, image::Rgba([px.r(), px.g(), px.b(), px.a()]));
    }
    buf.save(&a[3]).expect("save png");
    println!("wrote {}", a[3]);
}
