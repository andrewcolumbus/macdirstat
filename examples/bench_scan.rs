//! Time a scan: `cargo run --release --example bench_scan -- <path>`.
//! Set MACDIRSTAT_THREADS to compare walker thread counts.

use std::path::PathBuf;
use std::time::Instant;

use macdirstat::scan::{self, ScanOptions, ScanOutcome, ScanProgress};
use macdirstat::tree::SizeMetric;

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let start = Instant::now();
    let progress = ScanProgress::default();
    match scan::run(
        &root,
        ScanOptions::default(),
        SizeMetric::Allocated,
        &progress,
    ) {
        ScanOutcome::Done(tree, summary) => {
            let root_node = tree.node(macdirstat::tree::ROOT);
            println!(
                "{} files, {} folders, {} in {:.2}s (walk+build), {} unreadable",
                root_node.files,
                root_node.dirs,
                macdirstat::format::bytes(tree.total()),
                start.elapsed().as_secs_f64(),
                summary.errors
            );
        }
        ScanOutcome::Failed(err) => eprintln!("{err}"),
    }
}
