//! Parallel directory scanner.
//!
//! Each folder is listed with `readdir` + `lstat` (symlinks are never
//! followed) and its subfolders are scanned concurrently on a dedicated rayon
//! pool. The raw result is flattened into a [`Tree`] arena at the end.
//!
//! macOS specifics:
//! - Hard-linked files are counted once, keyed by (device, inode).
//! - Scanning `/` skips `/System/Volumes/Data`: its contents are already
//!   reached through the system volume's firmlinks (`/Users`, `/Applications`,
//!   ...), and on recent macOS both report the same device number, so a
//!   device check alone cannot catch the duplicate.
//! - Unless "cross volume boundaries" is on, folders on another device
//!   (external drives under `/Volumes`, `/System/Volumes/VM`, ...) are shown
//!   but not entered.
//! - Folders the process may not read (privacy-protected locations without
//!   Full Disk Access) are kept and flagged rather than failing the scan.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::bulk::{self, BulkError, Entry, EntryKind};

use crate::tree::{
    FLAG_DENIED, FLAG_FREE_SPACE, FLAG_INCOMPLETE, FLAG_SKIPPED, FLAG_UNKNOWN, FLAG_VOLUME, NO_EXT,
    NO_NODE, Node, NodeId, NodeKind, SizeMetric, Tree, extension_of,
};

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct ScanOptions {
    pub cross_volumes: bool,
}

/// Live counters shared with the UI while a scan runs.
#[derive(Default)]
pub struct ScanProgress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

pub struct ScanSummary {
    pub elapsed: Duration,
    pub errors: u64,
    pub cancelled: bool,
}

pub enum ScanOutcome {
    Done(Box<Tree>, ScanSummary),
    Failed(String),
}

pub struct ScanHandle {
    pub root: PathBuf,
    pub progress: Arc<ScanProgress>,
    pub started: Instant,
    rx: mpsc::Receiver<ScanOutcome>,
}

impl ScanHandle {
    pub fn try_finish(&self) -> Option<ScanOutcome> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(ScanOutcome::Failed(
                "The scanner stopped unexpectedly.".to_owned(),
            )),
        }
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

/// Start scanning `root` on a background thread. `notify` is called when
/// the result is ready (use it to request a repaint).
pub fn start(
    root: PathBuf,
    options: ScanOptions,
    metric: SizeMetric,
    notify: impl Fn() + Send + 'static,
) -> ScanHandle {
    let progress = Arc::new(ScanProgress::default());
    let (tx, rx) = mpsc::channel();
    let thread_progress = Arc::clone(&progress);
    let thread_root = root.clone();
    let spawned = std::thread::Builder::new()
        .name("macdirstat-scan".to_owned())
        // Flattening recurses once per directory level.
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let outcome = run(&thread_root, options, metric, &thread_progress);
            let _ = tx.send(outcome);
            notify();
        });
    if let Err(err) = spawned {
        // The receiver reports a disconnect; log the reason for diagnosis.
        eprintln!("MacDirStat: could not start scanner thread: {err}");
    }
    ScanHandle {
        root,
        progress,
        started: Instant::now(),
        rx,
    }
}

/// Scan synchronously. Used by the background thread and by tests.
pub fn run(
    root: &Path,
    options: ScanOptions,
    metric: SizeMetric,
    progress: &ScanProgress,
) -> ScanOutcome {
    let started = Instant::now();
    let root_md = match fs::metadata(root) {
        Ok(md) => md,
        Err(err) => return ScanOutcome::Failed(format!("Cannot open {}: {err}", root.display())),
    };
    if !root_md.is_dir() {
        return ScanOutcome::Failed(format!("{} is not a folder.", root.display()));
    }

    let pool = match rayon::ThreadPoolBuilder::new()
        .num_threads(walker_threads())
        .thread_name(|i| format!("macdirstat-walk-{i}"))
        .stack_size(16 * 1024 * 1024)
        .build()
    {
        Ok(pool) => pool,
        Err(err) => return ScanOutcome::Failed(format!("Cannot start scanner threads: {err}")),
    };

    let walker = Walker::new(root, &root_md, options, progress);
    let display_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let raw = pool.install(|| walker.scan_dir(root, display_name.into(), root_md.mtime()));

    let walked = started.elapsed();
    let mut tree = Tree::new(root.to_path_buf(), metric);
    flatten(&mut tree, raw, NO_NODE);
    tree.aggregate();
    if !progress.cancel.load(Ordering::Relaxed)
        && let Some(acc) = crate::volumes::accounting_for(root, options.cross_volumes)
    {
        add_accounting(&mut tree, &acc);
        tree.aggregate();
    }
    tree.sort_all();
    if std::env::var_os("MACDIRSTAT_TIMING").is_some() {
        eprintln!(
            "walk {:.2}s, build {:.2}s",
            walked.as_secs_f64(),
            (started.elapsed() - walked).as_secs_f64()
        );
    }

    let cancelled = progress.cancel.load(Ordering::Relaxed);
    ScanOutcome::Done(
        Box::new(tree),
        ScanSummary {
            elapsed: started.elapsed(),
            errors: progress.errors.load(Ordering::Relaxed),
            cancelled,
        },
    )
}

/// Device number as macOS reports it: `st_dev` is a signed 32-bit value that
/// std sign-extends, while `getattrlistbulk` hands back the raw bits.
fn dev_of(md: &fs::Metadata) -> u64 {
    md.dev() as u32 as u64
}

/// Double-checks a zero entry count from the parent's listing, which
/// describes the stub under a graft point (MobileAsset data) rather than the
/// grafted folder. An APFS folder links once per entry plus two, and an
/// `lstat` is far cheaper than opening folders inside app containers.
fn confirmed_empty(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|md| md.nlink() <= 2)
}

/// Directory listing is dominated by blocking `readdir`/`lstat` calls, so
/// the walker runs more threads than cores to keep the SSD queue full.
/// `MACDIRSTAT_THREADS` overrides the count (used for benchmarking).
fn walker_threads() -> usize {
    if let Some(n) = std::env::var("MACDIRSTAT_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    let cores = std::thread::available_parallelism().map_or(8, |n| n.get());
    (cores * WALKER_THREADS_PER_CORE).clamp(4, 64)
}

const WALKER_THREADS_PER_CORE: usize = 1;

struct RawFile {
    name: Box<str>,
    logical: u64,
    allocated: u64,
    mtime: i64,
}

struct RawDir {
    name: Box<str>,
    mtime: i64,
    flags: u8,
    files: Vec<RawFile>,
    dirs: Vec<RawDir>,
}

impl RawDir {
    fn empty(name: Box<str>, mtime: i64, flags: u8) -> Self {
        Self {
            name,
            mtime,
            flags,
            files: Vec::new(),
            dirs: Vec::new(),
        }
    }
}

struct Walker<'a> {
    progress: &'a ScanProgress,
    /// List with readdir + lstat instead of getattrlistbulk.
    force_std: bool,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
    allowed_devs: Vec<u64>,
    cross_volumes: bool,
    /// Data-volume paths already reached through a firmlink (only set when
    /// scanning `/`).
    firmlinked: Vec<PathBuf>,
}

const ALWAYS_SKIP: [&str; 4] = [
    // Device nodes and automounter maps: not real disk usage, and the
    // automounts can hang on unreachable network shares.
    "/dev",
    "/net",
    "/home",
    "/System/Volumes/Data/home",
];

const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Data-volume locations that the system volume exposes through firmlinks
/// (`/usr/share/firmlinks` maps `/Users` to `Users`, and so on).
fn firmlinked_data_paths() -> Vec<PathBuf> {
    let list = fs::read_to_string("/usr/share/firmlinks").unwrap_or_default();
    parse_firmlinks(&list)
}

fn parse_firmlinks(list: &str) -> Vec<PathBuf> {
    list.lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(|target| Path::new(DATA_VOLUME).join(target))
        .collect()
}

impl<'a> Walker<'a> {
    fn new(
        root: &Path,
        root_md: &fs::Metadata,
        options: ScanOptions,
        progress: &'a ScanProgress,
    ) -> Self {
        let root_dev = dev_of(root_md);
        let mut allowed_devs = vec![root_dev];
        // The system volume's firmlinks lead into the Data volume; treat the
        // two as one so a scan of `/` includes /Users, /Applications, etc.
        if let (Ok(slash), Ok(data)) = (fs::metadata("/"), fs::metadata(DATA_VOLUME))
            && (root_dev == dev_of(&slash) || root_dev == dev_of(&data))
        {
            allowed_devs.push(dev_of(&slash));
            allowed_devs.push(dev_of(&data));
        }
        Self {
            progress,
            force_std: std::env::var("MACDIRSTAT_BACKEND").is_ok_and(|v| v == "std"),
            hardlinks: Mutex::new(HashSet::new()),
            allowed_devs,
            cross_volumes: options.cross_volumes,
            firmlinked: if root == Path::new("/") {
                firmlinked_data_paths()
            } else {
                Vec::new()
            },
        }
    }

    fn should_skip(&self, path: &Path, entry: &Entry) -> bool {
        if ALWAYS_SKIP.iter().any(|p| path == Path::new(p)) {
            return true;
        }
        // Scanning `/` reaches most of the Data volume through firmlinks;
        // under /System/Volumes/Data only the rest is counted (Spotlight
        // index, document revisions, ...), so nothing is counted twice.
        if self.firmlinked.iter().any(|p| p == path) {
            return true;
        }
        if self.cross_volumes {
            return false;
        }
        // A bulk listing reports the covered directory's device for a mount
        // point; ask the mounted filesystem itself.
        let dev = if entry.mount_point {
            fs::symlink_metadata(path).map_or(entry.dev, |md| dev_of(&md))
        } else {
            entry.dev
        };
        !self.allowed_devs.contains(&dev)
    }

    /// List a directory, preferring `getattrlistbulk`.
    fn list(&self, path: &Path) -> Option<Vec<Entry>> {
        if !self.force_std {
            match bulk::read_dir(path) {
                Ok(entries) => return Some(entries),
                Err(BulkError::Io(_)) => return None,
                Err(BulkError::Unsupported) => {}
            }
        }
        let entries = fs::read_dir(path).ok()?;
        let mut out = Vec::new();
        for entry in entries {
            // `DirEntry::metadata` is an lstat on Unix: symlinks stay links.
            let Some((entry, md)) = entry.ok().and_then(|e| e.metadata().ok().map(|md| (e, md)))
            else {
                self.progress.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let ft = md.file_type();
            out.push(Entry {
                name: entry.file_name().to_string_lossy().into(),
                kind: if ft.is_dir() {
                    EntryKind::Dir
                } else if ft.is_symlink() {
                    EntryKind::Symlink
                } else if ft.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                },
                dev: dev_of(&md),
                inode: md.ino(),
                mtime: md.mtime(),
                mount_point: false,
                entry_count: None,
                nlink: md.nlink() as u32,
                logical: md.len(),
                allocated: md.blocks() * 512,
            });
        }
        Some(out)
    }

    fn scan_dir(&self, path: &Path, name: Box<str>, mtime: i64) -> RawDir {
        if self.progress.cancel.load(Ordering::Relaxed) {
            return RawDir::empty(name, mtime, FLAG_INCOMPLETE);
        }
        let mut out = RawDir::empty(name, mtime, 0);
        let Some(entries) = self.list(path) else {
            self.progress.errors.fetch_add(1, Ordering::Relaxed);
            out.flags |= FLAG_DENIED;
            return out;
        };

        let mut subdirs: Vec<(PathBuf, Box<str>, i64)> = Vec::new();
        let mut bytes = 0;
        for entry in entries {
            if entry.kind == EntryKind::Dir {
                let child = path.join(&*entry.name);
                if self.should_skip(&child, &entry) {
                    out.dirs
                        .push(RawDir::empty(entry.name, entry.mtime, FLAG_SKIPPED));
                } else if entry.entry_count == Some(0)
                    && !entry.mount_point
                    && confirmed_empty(&child)
                {
                    self.progress.dirs.fetch_add(1, Ordering::Relaxed);
                    out.dirs.push(RawDir::empty(entry.name, entry.mtime, 0));
                } else {
                    subdirs.push((child, entry.name, entry.mtime));
                }
                continue;
            }
            if entry.kind == EntryKind::File && entry.nlink > 1 {
                let first = self
                    .hardlinks
                    .lock()
                    .map(|mut seen| seen.insert((entry.dev, entry.inode)))
                    .unwrap_or(true);
                if !first {
                    continue;
                }
            }
            bytes += entry.allocated;
            out.files.push(RawFile {
                name: entry.name,
                logical: entry.logical,
                allocated: entry.allocated,
                mtime: entry.mtime,
            });
        }

        self.progress
            .files
            .fetch_add(out.files.len() as u64, Ordering::Relaxed);
        self.progress.bytes.fetch_add(bytes, Ordering::Relaxed);
        let dirs_seen = self.progress.dirs.fetch_add(1, Ordering::Relaxed);
        if dirs_seen.is_multiple_of(64)
            && let Ok(mut current) = self.progress.current.try_lock()
        {
            current.clear();
            current.push_str(&path.to_string_lossy());
        }

        let scanned: Vec<RawDir> = subdirs
            .into_par_iter()
            .map(|(child, name, mtime)| self.scan_dir(&child, name, mtime))
            .collect();
        out.dirs.extend(scanned);
        out
    }
}

/// WinDirStat-style entries that make a volume scan add up to the disk:
/// other APFS volumes, used space no visible file explains, and free space.
fn add_accounting(tree: &mut Tree, acc: &crate::volumes::DiskAccounting) {
    let scanned = tree.node(crate::tree::ROOT).allocated;
    let mut explained = scanned;
    for (name, bytes) in &acc.other_volumes {
        tree.add_synthetic(&format!("<{name} volume>"), *bytes, FLAG_VOLUME);
        explained += bytes;
    }
    let unknown = acc.used.saturating_sub(explained);
    if unknown > 0 {
        tree.add_synthetic("<Unknown>", unknown, FLAG_UNKNOWN);
    }
    if acc.free > 0 {
        tree.add_synthetic("<Free Space>", acc.free, FLAG_FREE_SPACE);
    }
}

fn flatten(tree: &mut Tree, raw: RawDir, parent: NodeId) -> NodeId {
    let id = tree.push(Node {
        name: raw.name,
        parent,
        children: Vec::with_capacity(raw.files.len() + raw.dirs.len()),
        logical: 0,
        allocated: 0,
        mtime: raw.mtime,
        files: 0,
        dirs: 0,
        ext: NO_EXT,
        kind: NodeKind::Dir,
        flags: raw.flags,
    });
    for file in raw.files {
        let ext = tree.record_ext(&extension_of(&file.name), file.logical, file.allocated);
        let child = tree.push(Node {
            name: file.name,
            parent: id,
            children: Vec::new(),
            logical: file.logical,
            allocated: file.allocated,
            mtime: file.mtime,
            files: 1,
            dirs: 0,
            ext,
            kind: NodeKind::File,
            flags: 0,
        });
        tree.nodes[id as usize].children.push(child);
    }
    for dir in raw.dirs {
        let child = flatten(tree, dir, id);
        tree.nodes[id as usize].children.push(child);
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::ROOT;

    fn scan(root: &Path) -> Tree {
        match run(
            root,
            ScanOptions::default(),
            SizeMetric::Logical,
            &ScanProgress::default(),
        ) {
            ScanOutcome::Done(tree, summary) => {
                assert!(!summary.cancelled);
                *tree
            }
            ScanOutcome::Failed(err) => panic!("scan failed: {err}"),
        }
    }

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; len]).unwrap();
    }

    fn child_named(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
        *tree
            .node(parent)
            .children
            .iter()
            .find(|&&c| &*tree.node(c).name == name)
            .unwrap_or_else(|| panic!("{name} not found"))
    }

    #[test]
    fn scan_builds_totals_and_sorted_children() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/a.bin"), 50_000);
        write(&dir.path().join("big/nested/b.bin"), 20_000);
        write(&dir.path().join("small.txt"), 10);
        fs::create_dir(dir.path().join("empty")).unwrap();

        let tree = scan(dir.path());
        let root = tree.node(ROOT);
        assert_eq!(root.logical, 70_010);
        assert_eq!(root.files, 3);
        assert_eq!(root.dirs, 3);
        assert_eq!(&*tree.node(root.children[0]).name, "big");
        let big = child_named(&tree, ROOT, "big");
        assert_eq!(tree.node(big).logical, 70_000);
        assert!(tree.node(big).allocated >= 70_000);
        assert_eq!(
            tree.path(child_named(&tree, big, "a.bin")),
            dir.path().join("big/a.bin")
        );
    }

    #[test]
    fn empty_folders_are_confirmed_by_link_count() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("empty")).unwrap();
        write(&dir.path().join("full/a.bin"), 10);
        assert!(confirmed_empty(&dir.path().join("empty")));
        assert!(!confirmed_empty(&dir.path().join("full")));
        assert!(!confirmed_empty(&dir.path().join("missing")));

        let tree = scan(dir.path());
        let empty = child_named(&tree, ROOT, "empty");
        assert!(tree.node(empty).is_dir());
        assert_eq!(tree.node(empty).flags, 0);
        assert_eq!(tree.node(child_named(&tree, ROOT, "full")).files, 1);
    }

    #[test]
    fn hard_links_are_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("one/data.bin"), 40_000);
        fs::create_dir(dir.path().join("two")).unwrap();
        fs::hard_link(
            dir.path().join("one/data.bin"),
            dir.path().join("two/data.bin"),
        )
        .unwrap();

        let tree = scan(dir.path());
        assert_eq!(tree.node(ROOT).logical, 40_000);
        assert_eq!(tree.node(ROOT).files, 1);
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("real/data.bin"), 30_000);
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();

        let tree = scan(dir.path());
        let link = child_named(&tree, ROOT, "link");
        assert_eq!(tree.node(link).kind, NodeKind::File);
        assert!(tree.node(ROOT).logical < 31_000);
    }

    #[test]
    fn unreadable_folders_are_flagged_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("locked/secret.bin"), 1000);
        write(&dir.path().join("open.bin"), 500);
        let locked = dir.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let tree = scan(dir.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let locked_id = child_named(&tree, ROOT, "locked");
        // Root ignores permission bits; only assert when they applied.
        if tree.node(locked_id).flags & FLAG_DENIED != 0 {
            assert_eq!(tree.node(ROOT).logical, 500);
        }
    }

    #[test]
    fn cancelled_scan_returns_partial_tree() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/b.bin"), 10);
        let progress = ScanProgress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        match run(
            dir.path(),
            ScanOptions::default(),
            SizeMetric::Logical,
            &progress,
        ) {
            ScanOutcome::Done(tree, summary) => {
                assert!(summary.cancelled);
                assert_ne!(tree.node(ROOT).flags & FLAG_INCOMPLETE, 0);
            }
            ScanOutcome::Failed(err) => panic!("{err}"),
        }
    }

    #[test]
    fn missing_root_fails_cleanly() {
        let outcome = run(
            Path::new("/definitely/not/here"),
            ScanOptions::default(),
            SizeMetric::Logical,
            &ScanProgress::default(),
        );
        assert!(matches!(outcome, ScanOutcome::Failed(_)));
    }

    #[test]
    fn slash_scan_counts_data_volume_extras_but_not_firmlinked_paths() {
        let progress = ScanProgress::default();
        let md = fs::metadata("/").unwrap();
        let walker = Walker::new(Path::new("/"), &md, ScanOptions::default(), &progress);
        let entry_for = |path: &Path| {
            let md = fs::symlink_metadata(path).ok()?;
            Some(Entry {
                name: "x".into(),
                kind: EntryKind::Dir,
                dev: dev_of(&md),
                inode: md.ino(),
                mtime: 0,
                mount_point: false,
                entry_count: None,
                nlink: 1,
                logical: 0,
                allocated: 0,
            })
        };
        let data = Path::new(DATA_VOLUME);
        if let Some(data_entry) = entry_for(data) {
            assert!(!walker.should_skip(data, &data_entry));
            let users = data.join("Users");
            if let Some(users_entry) = entry_for(&users) {
                assert!(
                    walker.should_skip(&users, &users_entry),
                    "reached via /Users"
                );
            }
        }
        assert!(walker.should_skip(Path::new("/dev"), &entry_for(Path::new("/dev")).unwrap()));
        let users = Path::new("/Users");
        assert!(!walker.should_skip(users, &entry_for(users).unwrap()));
    }

    #[test]
    fn firmlink_list_maps_into_the_data_volume() {
        let parsed = parse_firmlinks("/Users\tUsers\n/usr/local\tusr/local\n\n");
        assert_eq!(
            parsed,
            vec![
                PathBuf::from("/System/Volumes/Data/Users"),
                PathBuf::from("/System/Volumes/Data/usr/local"),
            ]
        );
    }

    #[test]
    fn accounting_entries_make_the_root_add_up_to_the_disk() {
        let mut tree = Tree::new(PathBuf::from("/"), SizeMetric::Allocated);
        flatten(&mut tree, RawDir::empty("/".into(), 0, 0), NO_NODE);
        let file = tree.push(Node {
            name: "a".into(),
            parent: crate::tree::ROOT,
            children: Vec::new(),
            logical: 100,
            allocated: 100,
            mtime: 0,
            files: 1,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::File,
            flags: 0,
        });
        tree.nodes[0].children.push(file);
        tree.aggregate();
        let acc = crate::volumes::DiskAccounting {
            used: 1000,
            free: 500,
            other_volumes: vec![("VM".to_owned(), 300)],
        };
        add_accounting(&mut tree, &acc);
        tree.aggregate();
        assert_eq!(tree.total(), 1500, "used + free");
        assert_eq!(tree.synthetic_size(FLAG_UNKNOWN), 600);
        assert_eq!(tree.synthetic_size(FLAG_VOLUME), 300);
        assert_eq!(tree.node(crate::tree::ROOT).files, 1);
    }
}
