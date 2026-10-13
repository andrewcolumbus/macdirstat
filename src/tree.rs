//! The scanned directory tree, stored as a flat arena of nodes.
//!
//! Directory totals (sizes, file and folder counts, newest modification
//! time) are aggregated once after a scan and kept consistent when a subtree
//! is removed. Children are kept sorted largest-first for the active size
//! metric because both the tree view and the squarified treemap want them in
//! that order.

use std::collections::HashMap;
use std::path::PathBuf;

pub type NodeId = u32;
pub type ExtId = u32;

pub const NO_NODE: NodeId = u32::MAX;
pub const NO_EXT: ExtId = u32::MAX;

/// Folder could not be read (usually macOS privacy protection).
pub const FLAG_DENIED: u8 = 1;
/// Folder is a mount point or pseudo filesystem that was not entered.
pub const FLAG_SKIPPED: u8 = 2;
/// Node was moved to the Trash and detached from its parent.
pub const FLAG_REMOVED: u8 = 4;
/// Scan was cancelled before this folder was read.
pub const FLAG_INCOMPLETE: u8 = 8;
/// Synthetic: free space on the scanned volume.
pub const FLAG_FREE_SPACE: u8 = 16;
/// Synthetic: used space no scanned file accounts for.
pub const FLAG_UNKNOWN: u8 = 32;
/// Synthetic: another volume sharing the scanned disk (APFS container).
pub const FLAG_VOLUME: u8 = 64;
pub const FLAG_SYNTHETIC: u8 = FLAG_FREE_SPACE | FLAG_UNKNOWN | FLAG_VOLUME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SizeMetric {
    /// Blocks actually allocated on disk, like `du`.
    #[default]
    Allocated,
    /// The files' logical length, like Finder's "size".
    Logical,
}

impl SizeMetric {
    pub fn label(self) -> &'static str {
        match self {
            SizeMetric::Allocated => "On Disk",
            SizeMetric::Logical => "Logical",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Box<str>,
    pub parent: NodeId,
    pub children: Vec<NodeId>,
    pub logical: u64,
    pub allocated: u64,
    /// Newest modification time in the subtree, Unix seconds.
    pub mtime: i64,
    /// Files in the subtree (1 for a file).
    pub files: u64,
    /// Folders in the subtree, excluding the node itself.
    pub dirs: u64,
    pub ext: ExtId,
    pub kind: NodeKind,
    pub flags: u8,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }

    pub fn size(&self, metric: SizeMetric) -> u64 {
        match metric {
            SizeMetric::Allocated => self.allocated,
            SizeMetric::Logical => self.logical,
        }
    }

    pub fn items(&self) -> u64 {
        self.files + self.dirs
    }

    pub fn is_synthetic(&self) -> bool {
        self.flags & FLAG_SYNTHETIC != 0
    }
}

#[derive(Debug, Clone)]
pub struct ExtStat {
    /// Lowercase extension without the dot; empty for "no extension".
    pub name: Box<str>,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    /// Position in the size ranking for the active metric.
    pub rank: u32,
}

impl ExtStat {
    pub fn size(&self, metric: SizeMetric) -> u64 {
        match metric {
            SizeMetric::Allocated => self.allocated,
            SizeMetric::Logical => self.logical,
        }
    }

    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            "(no extension)".to_owned()
        } else {
            format!(".{}", self.name)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub exts: Vec<ExtStat>,
    /// Extension ids sorted largest-first for the active metric.
    pub ext_order: Vec<ExtId>,
    pub root_path: PathBuf,
    pub metric: SizeMetric,
    /// Unique per scan, so views never confuse two trees.
    pub id: u64,
    /// Bumped on every change that affects layout, so views can invalidate.
    pub generation: u64,
    /// Synthetic root entries (free space, unknown, other volumes).
    pub synthetic: Vec<NodeId>,
    ext_index: HashMap<Box<str>, ExtId>,
}

pub const ROOT: NodeId = 0;

impl Tree {
    pub fn new(root_path: PathBuf, metric: SizeMetric) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            nodes: Vec::new(),
            exts: Vec::new(),
            ext_order: Vec::new(),
            root_path,
            metric,
            generation: 0,
            synthetic: Vec::new(),
            ext_index: HashMap::new(),
        }
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn size(&self, id: NodeId) -> u64 {
        self.node(id).size(self.metric)
    }

    pub fn total(&self) -> u64 {
        self.nodes.first().map_or(0, |n| n.size(self.metric))
    }

    /// Append a node without linking it to its parent's child list.
    pub fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId::try_from(self.nodes.len()).expect("more than 4 billion nodes");
        self.nodes.push(node);
        id
    }

    /// Intern an extension and add one file's sizes to it.
    pub fn record_ext(&mut self, ext: &str, logical: u64, allocated: u64) -> ExtId {
        let id = match self.ext_index.get(ext) {
            Some(&id) => id,
            None => {
                let id = ExtId::try_from(self.exts.len()).expect("too many extensions");
                self.exts.push(ExtStat {
                    name: ext.into(),
                    logical: 0,
                    allocated: 0,
                    files: 0,
                    rank: u32::MAX,
                });
                self.ext_index.insert(ext.into(), id);
                id
            }
        };
        let stat = &mut self.exts[id as usize];
        stat.logical += logical;
        stat.allocated += allocated;
        stat.files += 1;
        id
    }

    /// Recompute every folder's totals bottom-up from its children. Nodes
    /// are pushed parent-before-child, so a reverse pass sees each child
    /// before its parent.
    pub fn aggregate(&mut self) {
        for i in (0..self.nodes.len()).rev() {
            if !self.nodes[i].is_dir() {
                continue;
            }
            let (mut logical, mut allocated, mut files, mut dirs) = (0, 0, 0, 0);
            let mut mtime = self.nodes[i].mtime;
            for &c in &self.nodes[i].children {
                let child = &self.nodes[c as usize];
                logical += child.logical;
                allocated += child.allocated;
                files += child.files;
                dirs += child.dirs + u64::from(child.is_dir());
                mtime = mtime.max(child.mtime);
            }
            let node = &mut self.nodes[i];
            node.logical = logical;
            node.allocated = allocated;
            node.files = files;
            node.dirs = dirs;
            node.mtime = mtime;
        }
    }

    /// Switch size metric, re-sorting children and re-ranking extensions.
    pub fn set_metric(&mut self, metric: SizeMetric) {
        if self.metric != metric {
            self.metric = metric;
            self.sort_all();
        }
    }

    /// Sort every child list largest-first, in parallel: each list is
    /// sorted independently against the shared, read-only node sizes.
    pub fn sort_all(&mut self) {
        use rayon::prelude::*;

        let metric = self.metric;
        let nodes = &self.nodes;
        let sorted: Vec<(usize, Vec<NodeId>)> = (0..nodes.len())
            .into_par_iter()
            .filter(|&i| nodes[i].children.len() > 1)
            .map(|i| {
                let mut children = nodes[i].children.clone();
                children.sort_by(|&a, &b| compare_size_desc(nodes, metric, a, b));
                (i, children)
            })
            .collect();
        for (i, children) in sorted {
            self.nodes[i].children = children;
        }
        self.rank_extensions();
        self.generation += 1;
    }

    /// Add a synthetic entry (free space, unknown, another volume) under the
    /// root. Call `aggregate` and `sort_all` afterwards.
    pub fn add_synthetic(&mut self, name: &str, bytes: u64, flag: u8) -> NodeId {
        let id = self.push(Node {
            name: name.into(),
            parent: ROOT,
            children: Vec::new(),
            logical: bytes,
            allocated: bytes,
            mtime: 0,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::File,
            flags: flag,
        });
        self.nodes[ROOT as usize].children.push(id);
        self.synthetic.push(id);
        id
    }

    /// Show or hide the synthetic entries matching `mask`.
    pub fn set_synthetic_visible(&mut self, mask: u8, visible: bool) {
        let mut changed = false;
        for &id in &self.synthetic {
            if self.nodes[id as usize].flags & mask == 0 {
                continue;
            }
            let root = &mut self.nodes[ROOT as usize];
            let present = root.children.contains(&id);
            if visible && !present {
                root.children.push(id);
                changed = true;
            } else if !visible && present {
                root.children.retain(|&c| c != id);
                changed = true;
            }
        }
        if changed {
            self.aggregate();
            self.sort_all();
        }
    }

    /// Size of the visible synthetic entries matching `mask`.
    pub fn synthetic_size(&self, mask: u8) -> u64 {
        let root = &self.nodes[ROOT as usize];
        self.synthetic
            .iter()
            .filter(|&&id| self.node(id).flags & mask != 0 && root.children.contains(&id))
            .map(|&id| self.size(id))
            .sum()
    }

    fn sort_children_of(&mut self, id: NodeId) {
        let metric = self.metric;
        let mut children = std::mem::take(&mut self.nodes[id as usize].children);
        children.sort_by(|&a, &b| compare_size_desc(&self.nodes, metric, a, b));
        self.nodes[id as usize].children = children;
    }

    pub fn rank_extensions(&mut self) {
        let metric = self.metric;
        let mut order: Vec<ExtId> = (0..self.exts.len() as ExtId)
            .filter(|&e| self.exts[e as usize].files > 0)
            .collect();
        order.sort_by(|&a, &b| {
            let (ea, eb) = (&self.exts[a as usize], &self.exts[b as usize]);
            eb.size(metric)
                .cmp(&ea.size(metric))
                .then_with(|| ea.name.cmp(&eb.name))
        });
        for ext in &mut self.exts {
            ext.rank = u32::MAX;
        }
        for (rank, &e) in order.iter().enumerate() {
            self.exts[e as usize].rank = rank as u32;
        }
        self.ext_order = order;
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut names = Vec::new();
        let mut cur = id;
        while cur != ROOT && cur != NO_NODE {
            let node = self.node(cur);
            names.push(&*node.name);
            cur = node.parent;
        }
        let mut path = self.root_path.clone();
        for name in names.iter().rev() {
            path.push(name);
        }
        path
    }

    /// Ancestors from the node's parent up to the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut cur = self.node(id).parent;
        std::iter::from_fn(move || {
            if cur == NO_NODE {
                None
            } else {
                let out = cur;
                cur = self.node(cur).parent;
                Some(out)
            }
        })
    }

    /// True when `id` is `ancestor` or lies beneath it.
    pub fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        id == ancestor || self.ancestors(id).any(|a| a == ancestor)
    }

    /// The child of `ancestor` on the path down to `id`.
    pub fn child_toward(&self, ancestor: NodeId, id: NodeId) -> Option<NodeId> {
        let mut cur = id;
        loop {
            let parent = self.node(cur).parent;
            if parent == ancestor {
                return Some(cur);
            }
            if parent == NO_NODE {
                return None;
            }
            cur = parent;
        }
    }

    pub fn is_live(&self, id: NodeId) -> bool {
        (id as usize) < self.nodes.len()
            && self.node(id).flags & FLAG_REMOVED == 0
            && self
                .ancestors(id)
                .all(|a| self.node(a).flags & FLAG_REMOVED == 0)
    }

    /// Detach a subtree (after it was moved to the Trash) and subtract its
    /// totals from every ancestor and from the extension statistics.
    pub fn remove_subtree(&mut self, id: NodeId) {
        if id == ROOT || self.node(id).flags & (FLAG_REMOVED | FLAG_SYNTHETIC) != 0 {
            return;
        }
        let (logical, allocated, files, dirs) = {
            let n = self.node(id);
            (
                n.logical,
                n.allocated,
                n.files,
                n.dirs + u64::from(n.is_dir()),
            )
        };

        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let node = &self.nodes[cur as usize];
            if node.kind == NodeKind::File && node.ext != NO_EXT {
                let stat = &mut self.exts[node.ext as usize];
                stat.logical -= node.logical;
                stat.allocated -= node.allocated;
                stat.files -= 1;
            }
            stack.extend_from_slice(&self.nodes[cur as usize].children);
        }

        let parent = self.node(id).parent;
        self.nodes[parent as usize].children.retain(|&c| c != id);
        self.nodes[id as usize].flags |= FLAG_REMOVED;

        let mut cur = parent;
        while cur != NO_NODE {
            let node = &mut self.nodes[cur as usize];
            node.logical -= logical;
            node.allocated -= allocated;
            node.files -= files;
            node.dirs -= dirs;
            cur = node.parent;
        }
        // Only the ancestors shrank, so only the child lists that contain an
        // ancestor can be out of order.
        let ancestors: Vec<NodeId> = self.ancestors(id).collect();
        for a in ancestors {
            self.sort_children_of(a);
        }
        self.rank_extensions();
        self.generation += 1;
    }
}

fn compare_size_desc(
    nodes: &[Node],
    metric: SizeMetric,
    a: NodeId,
    b: NodeId,
) -> std::cmp::Ordering {
    let (na, nb) = (&nodes[a as usize], &nodes[b as usize]);
    nb.size(metric)
        .cmp(&na.size(metric))
        .then_with(|| na.name.cmp(&nb.name))
}

/// Lowercase extension of a file name; empty when there is none. Leading-dot
/// names such as ".zshrc" have no extension, and implausibly long suffixes
/// (timestamps, hashes) are treated as part of the name.
pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => {
            let ext = &name[i + 1..];
            if ext.len() <= 12 && !ext.contains(' ') {
                ext.to_lowercase()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn dir(name: &str, parent: NodeId) -> Node {
        Node {
            name: name.into(),
            parent,
            children: Vec::new(),
            logical: 0,
            allocated: 0,
            mtime: 0,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::Dir,
            flags: 0,
        }
    }

    pub fn add_file(tree: &mut Tree, parent: NodeId, name: &str, size: u64, mtime: i64) -> NodeId {
        let ext = tree.record_ext(&extension_of(name), size, size);
        let id = tree.push(Node {
            name: name.into(),
            parent,
            children: Vec::new(),
            logical: size,
            allocated: size,
            mtime,
            files: 1,
            dirs: 0,
            ext,
            kind: NodeKind::File,
            flags: 0,
        });
        tree.nodes[parent as usize].children.push(id);
        id
    }

    pub fn add_dir(tree: &mut Tree, parent: NodeId, name: &str) -> NodeId {
        let id = tree.push(dir(name, parent));
        tree.nodes[parent as usize].children.push(id);
        id
    }

    /// root
    /// +-- a/            (b.mov 600, c.txt 100)
    /// +-- d.jpg 250
    /// +-- e/            (f.jpg 50)
    pub fn sample() -> Tree {
        let mut tree = Tree::new(PathBuf::from("/scan"), SizeMetric::Allocated);
        tree.push(dir("scan", NO_NODE));
        let a = add_dir(&mut tree, ROOT, "a");
        add_file(&mut tree, a, "b.mov", 600, 10);
        add_file(&mut tree, a, "c.txt", 100, 30);
        add_file(&mut tree, ROOT, "d.jpg", 250, 20);
        let e = add_dir(&mut tree, ROOT, "e");
        add_file(&mut tree, e, "f.JPG", 50, 5);
        tree.aggregate();
        tree.sort_all();
        tree
    }

    fn find(tree: &Tree, name: &str) -> NodeId {
        tree.nodes.iter().position(|n| &*n.name == name).unwrap() as NodeId
    }

    #[test]
    fn aggregate_sums_sizes_counts_and_newest_time() {
        let tree = sample();
        let root = tree.node(ROOT);
        assert_eq!(root.allocated, 1000);
        assert_eq!(root.files, 4);
        assert_eq!(root.dirs, 2);
        assert_eq!(root.mtime, 30);
        assert_eq!(tree.size(find(&tree, "a")), 700);
    }

    #[test]
    fn children_are_sorted_largest_first() {
        let tree = sample();
        let names: Vec<&str> = tree
            .node(ROOT)
            .children
            .iter()
            .map(|&c| &*tree.node(c).name)
            .collect();
        assert_eq!(names, ["a", "d.jpg", "e"]);
    }

    #[test]
    fn extensions_are_case_folded_and_ranked() {
        let tree = sample();
        let top = &tree.exts[tree.ext_order[0] as usize];
        assert_eq!(&*top.name, "mov");
        let jpg = &tree.exts[tree.ext_order[1] as usize];
        assert_eq!(&*jpg.name, "jpg");
        assert_eq!(jpg.files, 2);
        assert_eq!(jpg.allocated, 300);
    }

    #[test]
    fn paths_join_names_under_the_scan_root() {
        let tree = sample();
        assert_eq!(tree.path(ROOT), PathBuf::from("/scan"));
        assert_eq!(
            tree.path(find(&tree, "b.mov")),
            PathBuf::from("/scan/a/b.mov")
        );
    }

    #[test]
    fn remove_subtree_updates_ancestors_and_extensions() {
        let mut tree = sample();
        let a = find(&tree, "a");
        tree.remove_subtree(a);
        let root = tree.node(ROOT);
        assert_eq!(root.allocated, 300);
        assert_eq!(root.files, 2);
        assert_eq!(root.dirs, 1);
        assert!(!tree.node(ROOT).children.contains(&a));
        assert!(!tree.is_live(find(&tree, "b.mov")));
        assert_eq!(&*tree.exts[tree.ext_order[0] as usize].name, "jpg");
        assert!(
            tree.ext_order
                .iter()
                .all(|&e| &*tree.exts[e as usize].name != "mov")
        );
    }

    #[test]
    fn removal_resorts_reordered_ancestors() {
        let mut tree = sample();
        tree.remove_subtree(find(&tree, "b.mov"));
        let first = tree.node(ROOT).children[0];
        assert_eq!(&*tree.node(first).name, "d.jpg");
    }

    #[test]
    fn metric_switch_resorts_children() {
        let mut tree = sample();
        // Make e larger on the logical metric only.
        let e = find(&tree, "e");
        let f = find(&tree, "f.JPG");
        tree.nodes[f as usize].logical = 5000;
        tree.aggregate();
        tree.set_metric(SizeMetric::Logical);
        assert_eq!(tree.node(ROOT).children[0], e);
    }

    #[test]
    fn child_toward_finds_the_branch() {
        let tree = sample();
        let a = find(&tree, "a");
        let b = find(&tree, "b.mov");
        assert_eq!(tree.child_toward(ROOT, b), Some(a));
        assert_eq!(tree.child_toward(a, b), Some(b));
        assert_eq!(tree.child_toward(b, a), None);
        assert!(tree.is_within(b, a));
        assert!(!tree.is_within(a, b));
    }

    #[test]
    fn synthetic_entries_toggle_in_and_out_of_the_totals() {
        let mut tree = sample();
        let free = tree.add_synthetic("<Free Space>", 5000, FLAG_FREE_SPACE);
        tree.add_synthetic("<Unknown>", 300, FLAG_UNKNOWN);
        tree.aggregate();
        tree.sort_all();
        assert_eq!(tree.total(), 6300);
        assert_eq!(tree.node(ROOT).children[0], free);
        assert_eq!(tree.node(ROOT).files, 4, "synthetic entries are not files");
        assert_eq!(tree.synthetic_size(FLAG_FREE_SPACE), 5000);

        tree.set_synthetic_visible(FLAG_FREE_SPACE, false);
        assert_eq!(tree.total(), 1300);
        assert_eq!(tree.synthetic_size(FLAG_FREE_SPACE), 0);
        tree.set_synthetic_visible(FLAG_FREE_SPACE, true);
        assert_eq!(tree.total(), 6300);
        assert!(
            tree.ext_order
                .iter()
                .all(|&e| !tree.exts[e as usize].name.starts_with('<'))
        );
    }

    #[test]
    fn extension_rules() {
        assert_eq!(extension_of("movie.MOV"), "mov");
        assert_eq!(extension_of(".zshrc"), "");
        assert_eq!(extension_of("Makefile"), "");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of("trailing."), "");
        assert_eq!(extension_of("log.2024-01-01T10-00-00Z"), "");
    }
}
