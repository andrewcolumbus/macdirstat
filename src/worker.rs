//! Background treemap rendering.
//!
//! Layout and rasterization run on a dedicated thread (rasterization itself
//! fans out across rayon), so resizing the window or switching options never
//! stalls the UI. Requests coalesce: when several arrive while a render is in
//! progress, only the newest is rendered.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::tree::{NodeId, Tree};
use crate::treemap::{self, RenderOptions, Rendered};

/// Everything that determines a treemap image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreemapKey {
    pub generation: u64,
    pub tree_id: u64,
    pub zoom: NodeId,
    pub width: u32,
    pub height: u32,
    pub options: RenderOptions,
}

struct Job {
    key: TreemapKey,
    tree: Arc<Tree>,
}

pub struct Finished {
    pub key: TreemapKey,
    pub rendered: Rendered,
}

pub struct TreemapWorker {
    jobs: Sender<Job>,
    results: Receiver<Finished>,
}

impl TreemapWorker {
    /// `notify` runs after each finished render (request a repaint there).
    pub fn new(notify: impl Fn() + Send + 'static) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("macdirstat-treemap".to_owned())
            // Layout recurses once per directory level.
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    let k = job.key;
                    let rendered = treemap::render(&job.tree, k.zoom, k.width, k.height, k.options);
                    drop(job.tree);
                    if result_tx.send(Finished { key: k, rendered }).is_err() {
                        break;
                    }
                    notify();
                }
            });
        if let Err(err) = spawned {
            eprintln!("MacDirStat: could not start treemap thread: {err}");
        }
        Self {
            jobs: job_tx,
            results: result_rx,
        }
    }

    pub fn submit(&self, key: TreemapKey, tree: Arc<Tree>) {
        let _ = self.jobs.send(Job { key, tree });
    }

    /// The newest finished render, if any arrived since the last call.
    pub fn latest(&self) -> Option<Finished> {
        let mut latest = None;
        while let Ok(done) = self.results.try_recv() {
            latest = Some(done);
        }
        latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::ROOT;
    use crate::tree::tests::sample;
    use std::time::{Duration, Instant};

    fn key(width: u32) -> TreemapKey {
        TreemapKey {
            generation: 0,
            tree_id: 0,
            zoom: ROOT,
            width,
            height: 50,
            options: RenderOptions::default(),
        }
    }

    fn wait(worker: &TreemapWorker) -> Finished {
        let start = Instant::now();
        loop {
            if let Some(done) = worker.latest() {
                return done;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "worker timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn renders_off_thread_and_returns_the_key() {
        let worker = TreemapWorker::new(|| {});
        let tree = Arc::new(sample());
        worker.submit(key(80), Arc::clone(&tree));
        let done = wait(&worker);
        assert_eq!(done.key, key(80));
        assert_eq!(done.rendered.rgba.len(), 80 * 50 * 4);
        assert_eq!(done.rendered.layout.width, 80);
    }

    #[test]
    fn newest_request_wins() {
        let worker = TreemapWorker::new(|| {});
        let tree = Arc::new(sample());
        for w in [10, 20, 30, 40] {
            worker.submit(key(w), Arc::clone(&tree));
        }
        let start = Instant::now();
        let mut last = wait(&worker);
        while last.key != key(40) {
            assert!(start.elapsed() < Duration::from_secs(10));
            if let Some(done) = worker.latest() {
                last = done;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
