//! MacDirStat: a WinDirStat-style disk usage treemap for macOS.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use macdirstat::{app, icon};

/// Size of the flat icon PNG that `bundle.sh` turns into an .icns.
const EXPORT_SIZE: u32 = 1024;

type IconWriter = fn(&Path) -> std::io::Result<()>;

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let export: Option<(&str, IconWriter)> = match first.as_deref().and_then(|a| a.to_str()) {
        Some("--export-icon") => Some(("icon.png", |out| {
            let rgba = icon::render(EXPORT_SIZE);
            std::fs::write(out, icon::encode_png(&rgba, EXPORT_SIZE, EXPORT_SIZE))
        })),
        Some("--export-icon-source") => Some(("MacDirStat.icon", icon::write_icon_source)),
        _ => None,
    };
    if let Some((default, write)) = export {
        let out = args
            .next()
            .map_or_else(|| PathBuf::from(default), PathBuf::from);
        if let Err(err) = write(&out) {
            eprintln!("MacDirStat: could not write {}: {err}", out.display());
            std::process::exit(1);
        }
        return Ok(());
    }
    // Finder passes -psn_* process serial numbers to older bundles.
    let initial = first
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .map(PathBuf::from);
    // eframe skips an empty icon (a missing one becomes egui's default), so a
    // bundled app keeps the Dock icon from its bundle.
    let icon = if std::env::current_exe().is_ok_and(|exe| icon::in_app_bundle(&exe)) {
        egui::IconData::default()
    } else {
        icon::app_icon()
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MacDirStat")
            .with_app_id("io.github.andrewcolumbus.macdirstat")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([760.0, 480.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "MacDirStat",
        options,
        Box::new(move |cc| Ok(Box::new(app::MacDirStatApp::new(cc, initial)))),
    )
}
