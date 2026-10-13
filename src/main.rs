//! MacDirStat: a WinDirStat-style disk usage treemap for macOS.

use std::path::PathBuf;
use std::sync::Arc;

use macdirstat::{app, icon};

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--export-icon")) {
        let out = args
            .next()
            .map_or_else(|| PathBuf::from("icon.png"), PathBuf::from);
        let icon = icon::app_icon();
        if let Err(err) =
            std::fs::write(&out, icon::encode_png(&icon.rgba, icon.width, icon.height))
        {
            eprintln!("MacDirStat: could not write {}: {err}", out.display());
            std::process::exit(1);
        }
        return Ok(());
    }
    // Finder passes -psn_* process serial numbers to older bundles.
    let initial = first
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MacDirStat")
            .with_app_id("io.github.andrewcolumbus.macdirstat")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([760.0, 480.0])
            .with_icon(Arc::new(icon::app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "MacDirStat",
        options,
        Box::new(move |cc| Ok(Box::new(app::MacDirStatApp::new(cc, initial)))),
    )
}
