//! Render a folder's cushion treemap to a PNG without opening a window:
//! `cargo run --release --example render_treemap -- <folder> <out.png> [width height]`.

use std::path::PathBuf;

use macdirstat::scan::{self, ScanOptions, ScanOutcome, ScanProgress};
use macdirstat::tree::{ROOT, SizeMetric};
use macdirstat::treemap::{self, RenderOptions};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(root), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: render_treemap <folder> <out.png> [width height]");
        std::process::exit(2);
    };
    let mut dims = args.filter_map(|a| a.to_str().and_then(|s| s.parse::<u32>().ok()));
    let width = dims.next().unwrap_or(1600);
    let height = dims.next().unwrap_or(900);

    let progress = ScanProgress::default();
    let tree = match scan::run(
        &PathBuf::from(root),
        ScanOptions::default(),
        SizeMetric::Allocated,
        &progress,
    ) {
        ScanOutcome::Done(tree, _) => tree,
        ScanOutcome::Failed(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    let rendered = treemap::render(&tree, ROOT, width, height, RenderOptions::default());
    let png = macdirstat::icon::encode_png(&rendered.rgba, width, height);
    if let Err(err) = std::fs::write(&out, png) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
