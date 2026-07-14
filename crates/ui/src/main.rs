//! `pcbforge-console` — the windowed operator console (UI-1).
//!
//! Thin entry point: parses `--db` / `--pcbforge` and launches the egui app
//! via eframe. Only compiled with the `native` feature (eframe + a display);
//! without it the binary prints how to build it, so a headless `cargo build`
//! of the whole workspace still succeeds.

#[cfg(feature = "native")]
fn main() -> eframe::Result<()> {
    let mut db = std::path::PathBuf::from("pcbforge.sqlite");
    // Default: shell the CLI via cargo so it works from a repo checkout with
    // nothing on PATH. --pcbforge <path> overrides with a prebuilt binary.
    let mut cli_cmd = ui::default_cli_cmd();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--db" => {
                if let Some(v) = args.next() {
                    db = v.into();
                }
            }
            "--pcbforge" => {
                if let Some(v) = args.next() {
                    cli_cmd = vec![v];
                }
            }
            _ => {}
        }
    }
    ui::run_native(db, cli_cmd)
}

#[cfg(not(feature = "native"))]
fn main() {
    eprintln!(
        "pcbforge-console was built without the `native` feature.\n\
         Rebuild on a machine with a display + GL/X11 dev libraries:\n\
         \n    cargo run -p ui --features native --bin pcbforge-console -- --db pcbforge.sqlite\n"
    );
    std::process::exit(2);
}
