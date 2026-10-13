//! MacDirStat window: toolbar, directory tree, extension list and treemap,
//! laid out like WinDirStat.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{
    Align, Color32, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText, Sense, pos2, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::access;
use crate::format;
use crate::scan::{self, ScanHandle, ScanOptions, ScanOutcome};
use crate::theme::{self, palette, spacing};
use crate::tree::{
    ExtId, FLAG_DENIED, FLAG_FREE_SPACE, FLAG_INCOMPLETE, FLAG_SKIPPED, FLAG_UNKNOWN, FLAG_VOLUME,
    NO_NODE, NodeId, ROOT, SizeMetric, Tree,
};
use crate::treemap::{self, FRect, RenderOptions, TreemapLayout};
use crate::volumes::{self, Volume};
use crate::worker::{TreemapKey, TreemapWorker};

const PREFS_KEY: &str = "macdirstat_prefs";
const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 14.0;
const MAX_RECENT: usize = 8;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    metric: SizeMetric,
    cushion: bool,
    grid: bool,
    scan: ScanOptions,
    recent: Vec<PathBuf>,
    show_free_space: bool,
    show_unknown: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            metric: SizeMetric::Allocated,
            cushion: true,
            grid: false,
            scan: ScanOptions::default(),
            recent: Vec::new(),
            show_free_space: true,
            show_unknown: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortColumn {
    Name,
    Size,
    Items,
    Modified,
}

#[derive(Clone, Copy)]
struct Row {
    id: NodeId,
    depth: u16,
}

/// The treemap texture on screen and the render that is in flight.
#[derive(Default)]
struct TreemapView {
    texture: Option<egui::TextureHandle>,
    shown: Option<TreemapKey>,
    pending: Option<TreemapKey>,
    layout: Option<TreemapLayout>,
}

struct Notice {
    text: String,
    error: bool,
    at: Instant,
}

struct Summary {
    elapsed: Duration,
    errors: u64,
    cancelled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nav {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug)]
enum Action {
    Select { id: NodeId, reveal: bool },
    ToggleExpand(NodeId),
    Zoom(NodeId),
    ZoomOut,
    ZoomSelected,
    Navigate(Nav),
    RevealInFinder(NodeId),
    Open(NodeId),
    CopyPath(NodeId),
    AskTrash(NodeId),
    Trash(NodeId),
    HighlightExt(Option<ExtId>),
    Sort(SortColumn),
    SetMetric(SizeMetric),
    Scan(PathBuf),
    ChooseFolder,
    Rescan,
    CancelScan,
    CloseScan,
    ShowAccessPrompt,
    SetSyntheticVisible { mask: u8, visible: bool },
}

pub struct MacDirStatApp {
    prefs: Prefs,
    /// Shared with the treemap worker; mutations go through `Arc::make_mut`.
    tree: Option<Arc<Tree>>,
    summary: Option<Summary>,
    scan: Option<ScanHandle>,
    zoom: NodeId,
    selected: Option<NodeId>,
    hovered: Option<NodeId>,
    menu_target: Option<NodeId>,
    expanded: HashSet<NodeId>,
    rows: Vec<Row>,
    rows_dirty: bool,
    scroll_to: Option<NodeId>,
    sort: (SortColumn, bool),
    highlight_ext: Option<ExtId>,
    treemap: TreemapView,
    worker: TreemapWorker,
    confirm_trash: Option<NodeId>,
    notice: Option<Notice>,
    volumes: Vec<Volume>,
    volumes_at: Option<Instant>,
    has_full_disk_access: bool,
    access_prompt: bool,
}

impl MacDirStatApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::configure_context(&cc.egui_ctx);
        let prefs = cc
            .storage
            .and_then(|s| eframe::get_value::<Prefs>(s, PREFS_KEY))
            .unwrap_or_default();
        let repaint = cc.egui_ctx.clone();
        let worker = TreemapWorker::new(move || repaint.request_repaint());
        let has_full_disk_access = access::has_full_disk_access();
        let mut app = Self {
            prefs,
            tree: None,
            summary: None,
            scan: None,
            zoom: ROOT,
            selected: None,
            hovered: None,
            menu_target: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: true,
            scroll_to: None,
            sort: (SortColumn::Size, true),
            highlight_ext: None,
            treemap: TreemapView::default(),
            worker,
            confirm_trash: None,
            notice: None,
            volumes: Vec::new(),
            volumes_at: None,
            has_full_disk_access,
            // Ask up front: without it, protected folders read as Unknown.
            access_prompt: !has_full_disk_access,
        };
        if let Some(path) = initial {
            app.start_scan(&cc.egui_ctx, path);
        }
        app
    }

    // ------------------------------------------------------------------
    // State changes
    // ------------------------------------------------------------------

    fn start_scan(&mut self, ctx: &egui::Context, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(old) = &self.scan {
            old.cancel();
        }
        self.has_full_disk_access = access::has_full_disk_access();
        self.tree = None;
        self.summary = None;
        self.treemap = TreemapView::default();
        self.notice = None;
        self.prefs.recent.retain(|p| p != &path);
        self.prefs.recent.insert(0, path.clone());
        self.prefs.recent.truncate(MAX_RECENT);
        let repaint = ctx.clone();
        self.scan = Some(scan::start(
            path.clone(),
            self.prefs.scan,
            self.prefs.metric,
            move || {
                repaint.request_repaint();
            },
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "MacDirStat \u{2014} Scanning {}",
            path.display()
        )));
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(handle) = &self.scan else { return };
        match handle.try_finish() {
            None => ctx.request_repaint_after(Duration::from_millis(100)),
            Some(ScanOutcome::Done(tree, summary)) => {
                self.scan = None;
                self.install_tree(ctx, *tree, summary);
            }
            Some(ScanOutcome::Failed(message)) => {
                self.scan = None;
                self.set_notice(message, true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("MacDirStat".to_owned()));
            }
        }
    }

    fn install_tree(&mut self, ctx: &egui::Context, mut tree: Tree, summary: scan::ScanSummary) {
        tree.set_metric(self.prefs.metric);
        tree.set_synthetic_visible(FLAG_FREE_SPACE, self.prefs.show_free_space);
        tree.set_synthetic_visible(FLAG_UNKNOWN | FLAG_VOLUME, self.prefs.show_unknown);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "MacDirStat \u{2014} {}",
            tree.root_path.display()
        )));
        self.tree = Some(Arc::new(tree));
        self.summary = Some(Summary {
            elapsed: summary.elapsed,
            errors: summary.errors,
            cancelled: summary.cancelled,
        });
        self.zoom = ROOT;
        self.selected = None;
        self.hovered = None;
        self.menu_target = None;
        self.expanded = HashSet::from([ROOT]);
        self.rows_dirty = true;
        self.highlight_ext = None;
        self.treemap = TreemapView::default();
    }

    fn set_notice(&mut self, text: String, error: bool) {
        self.notice = Some(Notice {
            text,
            error,
            at: Instant::now(),
        });
    }

    fn reveal_in_tree(&mut self, id: NodeId) {
        let Some(tree) = &self.tree else { return };
        for a in tree.ancestors(id) {
            self.expanded.insert(a);
        }
        self.rows_dirty = true;
        self.scroll_to = Some(id);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        self.rows_dirty = false;
        let Some(tree) = &self.tree else { return };
        let mut stack = vec![(ROOT, 0u16)];
        while let Some((id, depth)) = stack.pop() {
            self.rows.push(Row { id, depth });
            if self.expanded.contains(&id) {
                let children = sorted_children(tree, id, self.sort);
                stack.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|c| (c, depth.saturating_add(1))),
                );
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Select { id, reveal } => {
                self.selected = Some(id);
                if reveal {
                    self.reveal_in_tree(id);
                }
            }
            Action::ToggleExpand(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
                self.rows_dirty = true;
            }
            Action::Zoom(id) => {
                if let Some(tree) = &self.tree
                    && tree.node(id).is_dir()
                {
                    self.zoom = id;
                }
            }
            Action::ZoomOut => {
                if let Some(tree) = &self.tree
                    && self.zoom != ROOT
                {
                    self.zoom = tree.node(self.zoom).parent;
                }
            }
            Action::ZoomSelected => {
                if let (Some(tree), Some(sel)) = (&self.tree, self.selected) {
                    let target = if tree.node(sel).is_dir() {
                        sel
                    } else {
                        tree.node(sel).parent
                    };
                    if target != NO_NODE {
                        self.zoom = target;
                    }
                }
            }
            Action::Navigate(nav) => self.navigate(nav),
            Action::RevealInFinder(id) => self.open_path(id, true),
            Action::Open(id) => self.open_path(id, false),
            Action::CopyPath(id) => {
                if let Some(tree) = &self.tree {
                    ctx.copy_text(tree.path(id).display().to_string());
                }
            }
            Action::AskTrash(id) => {
                if self
                    .tree
                    .as_ref()
                    .is_some_and(|t| t.node(id).is_synthetic())
                {
                    // Free space and accounting entries are not files.
                } else if id == ROOT {
                    self.set_notice(
                        "The scanned folder itself cannot be moved to the Trash here.".to_owned(),
                        true,
                    );
                } else {
                    self.confirm_trash = Some(id);
                }
            }
            Action::Trash(id) => self.trash(id),
            Action::HighlightExt(ext) => self.highlight_ext = ext,
            Action::Sort(column) => {
                self.sort = if self.sort.0 == column {
                    (column, !self.sort.1)
                } else {
                    (column, column != SortColumn::Name)
                };
                self.rows_dirty = true;
            }
            Action::SetMetric(metric) => {
                self.prefs.metric = metric;
                if let Some(tree) = &mut self.tree {
                    Arc::make_mut(tree).set_metric(metric);
                }
                self.rows_dirty = true;
            }
            Action::SetSyntheticVisible { mask, visible } => {
                if mask & FLAG_FREE_SPACE != 0 {
                    self.prefs.show_free_space = visible;
                } else {
                    self.prefs.show_unknown = visible;
                }
                if let Some(tree) = &mut self.tree {
                    Arc::make_mut(tree).set_synthetic_visible(mask, visible);
                }
                self.rows_dirty = true;
            }
            Action::ShowAccessPrompt => {
                self.has_full_disk_access = access::has_full_disk_access();
                self.access_prompt = true;
            }
            Action::Scan(path) => self.start_scan(ctx, path),
            Action::ChooseFolder => {
                let mut dialog = rfd::FileDialog::new().set_title("Choose a Folder to Scan");
                if let Some(tree) = &self.tree {
                    dialog = dialog.set_directory(&tree.root_path);
                } else if let Some(home) = std::env::home_dir() {
                    dialog = dialog.set_directory(home);
                }
                if let Some(path) = dialog.pick_folder() {
                    self.start_scan(ctx, path);
                }
            }
            Action::Rescan => {
                if let Some(path) = self.tree.as_ref().map(|t| t.root_path.clone()) {
                    self.start_scan(ctx, path);
                }
            }
            Action::CancelScan => {
                if let Some(handle) = &self.scan {
                    handle.cancel();
                }
            }
            Action::CloseScan => {
                if let Some(handle) = self.scan.take() {
                    handle.cancel();
                }
                self.tree = None;
                self.summary = None;
                self.treemap = TreemapView::default();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("MacDirStat".to_owned()));
            }
        }
    }

    fn navigate(&mut self, nav: Nav) {
        if self.rows_dirty {
            self.rebuild_rows();
        }
        let Some(tree) = &self.tree else { return };
        if self.rows.is_empty() {
            return;
        }
        let index = self
            .selected
            .and_then(|s| self.rows.iter().position(|r| r.id == s));
        let Some(index) = index else {
            self.selected = Some(self.rows[0].id);
            self.scroll_to = self.selected;
            return;
        };
        let id = self.rows[index].id;
        let node = tree.node(id);
        let target = match nav {
            Nav::Up => self.rows[index.saturating_sub(1)].id,
            Nav::Down => self.rows[(index + 1).min(self.rows.len() - 1)].id,
            Nav::Left => {
                if self.expanded.contains(&id) && node.is_dir() {
                    self.expanded.remove(&id);
                    self.rows_dirty = true;
                    id
                } else if node.parent != NO_NODE {
                    node.parent
                } else {
                    id
                }
            }
            Nav::Right => {
                if !node.is_dir() || node.children.is_empty() {
                    id
                } else if self.expanded.insert(id) {
                    self.rows_dirty = true;
                    id
                } else {
                    sorted_children(tree, id, self.sort)[0]
                }
            }
        };
        self.selected = Some(target);
        self.scroll_to = Some(target);
    }

    fn open_path(&mut self, id: NodeId, reveal: bool) {
        let Some(tree) = &self.tree else { return };
        let path = tree.path(id);
        let mut cmd = std::process::Command::new("open");
        if reveal {
            cmd.arg("-R");
        }
        if let Err(err) = cmd.arg(&path).spawn() {
            self.set_notice(format!("Could not open {}: {err}", path.display()), true);
        }
    }

    fn trash(&mut self, id: NodeId) {
        let Some(tree) = &mut self.tree else { return };
        if id == ROOT || !tree.is_live(id) || tree.node(id).is_synthetic() {
            return;
        }
        let path = tree.path(id);
        let size = tree.size(id);
        match move_to_trash(&path) {
            Ok(()) => {
                let tree = Arc::make_mut(tree);
                let parent = tree.node(id).parent;
                tree.remove_subtree(id);
                if self.selected.is_some_and(|s| !tree.is_live(s)) {
                    self.selected = Some(parent);
                }
                if !tree.is_live(self.zoom) {
                    self.zoom = parent;
                }
                self.hovered = None;
                self.menu_target = None;
                self.rows_dirty = true;
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.set_notice(
                    format!(
                        "Moved \u{201C}{name}\u{201D} to the Trash ({}).",
                        format::bytes(size)
                    ),
                    false,
                );
            }
            Err(err) => self.set_notice(
                format!("Could not move {} to the Trash: {err}", path.display()),
                true,
            ),
        }
    }

    fn shortcuts(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.confirm_trash.is_some()
            || self.access_prompt
            || ctx.any_popup_open()
            || ctx.memory(|m| m.focused().is_some())
        {
            return;
        }
        let has_tree = self.tree.is_some();
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::O) {
                actions.push(Action::ChooseFolder);
            }
            if i.consume_key(Modifiers::COMMAND, Key::R) {
                actions.push(Action::Rescan);
            }
            if !has_tree {
                return;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Backspace)
                && let Some(sel) = self.selected
            {
                actions.push(Action::AskTrash(sel));
            }
            if i.consume_key(Modifiers::NONE, Key::Escape)
                || i.consume_key(Modifiers::NONE, Key::Backspace)
                || i.pointer.button_pressed(egui::PointerButton::Extra1)
            {
                actions.push(Action::ZoomOut);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) {
                actions.push(Action::ZoomSelected);
            }
            for (key, nav) in [
                (Key::ArrowUp, Nav::Up),
                (Key::ArrowDown, Nav::Down),
                (Key::ArrowLeft, Nav::Left),
                (Key::ArrowRight, Nav::Right),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    actions.push(Action::Navigate(nav));
                }
            }
            if i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                && let Some(sel) = self.selected
            {
                actions.push(Action::CopyPath(sel));
            }
        });
    }

    // ------------------------------------------------------------------
    // Chrome
    // ------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = palette();
        let ctx = ui.ctx().clone();
        let has_tree = self.tree.is_some();
        ui.horizontal_centered(|ui| {
            // MenuBar claims the full row width, so give it its own slot.
            let menu_slot = vec2(92.0, ui.available_height());
            ui.allocate_ui_with_layout(menu_slot, Layout::left_to_right(Align::Center), |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("Scan", |ui| {
                        let open = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
                        if ui
                            .add(
                                egui::Button::new("Choose Folder...")
                                    .shortcut_text(ctx.format_shortcut(&open)),
                            )
                            .clicked()
                        {
                            actions.push(Action::ChooseFolder);
                        }
                        if let Some(home) = std::env::home_dir()
                            && ui.button("Home Folder").clicked()
                        {
                            actions.push(Action::Scan(home));
                        }
                        ui.menu_button("Volumes", |ui| {
                            for v in volumes::list() {
                                if ui.button(&v.name).clicked() {
                                    actions.push(Action::Scan(v.path.clone()));
                                }
                            }
                        });
                        ui.add_enabled_ui(!self.prefs.recent.is_empty(), |ui| {
                            ui.menu_button("Recent", |ui| {
                                for path in &self.prefs.recent {
                                    if ui.button(path.display().to_string()).clicked() {
                                        actions.push(Action::Scan(path.clone()));
                                    }
                                }
                            });
                        });
                        ui.separator();
                        let rescan = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
                        if ui
                            .add_enabled(
                                has_tree,
                                egui::Button::new("Rescan")
                                    .shortcut_text(ctx.format_shortcut(&rescan)),
                            )
                            .clicked()
                        {
                            actions.push(Action::Rescan);
                        }
                        if ui
                            .add_enabled(self.scan.is_some(), egui::Button::new("Stop Scan"))
                            .clicked()
                        {
                            actions.push(Action::CancelScan);
                        }
                        if ui
                            .add_enabled(
                                has_tree || self.scan.is_some(),
                                egui::Button::new("Close"),
                            )
                            .clicked()
                        {
                            actions.push(Action::CloseScan);
                        }
                        ui.separator();
                        if ui.button("Full Disk Access...").clicked() {
                            actions.push(Action::ShowAccessPrompt);
                        }
                        ui.checkbox(
                        &mut self.prefs.scan.cross_volumes,
                        "Cross Volume Boundaries",
                    )
                    .on_hover_text(
                        "Enter other mounted volumes (external drives, VM swap) on the next scan.",
                    );
                    });
                    ui.menu_button("View", |ui| {
                        for metric in [SizeMetric::Allocated, SizeMetric::Logical] {
                            if ui
                                .radio(
                                    self.prefs.metric == metric,
                                    format!("Size: {}", metric.label()),
                                )
                                .clicked()
                            {
                                actions.push(Action::SetMetric(metric));
                            }
                        }
                        ui.separator();
                        ui.checkbox(&mut self.prefs.cushion, "Cushion Shading");
                        ui.checkbox(&mut self.prefs.grid, "Grid Lines");
                        ui.separator();
                        let mut free = self.prefs.show_free_space;
                        if ui
                            .checkbox(&mut free, "Show Free Space")
                            .on_hover_text("When a whole volume is scanned, show its free space")
                            .changed()
                        {
                            actions.push(Action::SetSyntheticVisible {
                                mask: FLAG_FREE_SPACE,
                                visible: free,
                            });
                        }
                        let mut unknown = self.prefs.show_unknown;
                        if ui
                            .checkbox(&mut unknown, "Show Unknown and Other Volumes")
                            .on_hover_text(
                                "When a whole volume is scanned, show used space that no visible \
         file accounts for, and other volumes sharing the disk",
                            )
                            .changed()
                        {
                            actions.push(Action::SetSyntheticVisible {
                                mask: FLAG_UNKNOWN | FLAG_VOLUME,
                                visible: unknown,
                            });
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                has_tree && self.zoom != ROOT,
                                egui::Button::new("Zoom Out").shortcut_text("Esc"),
                            )
                            .clicked()
                        {
                            actions.push(Action::ZoomOut);
                        }
                        if ui
                            .add_enabled(
                                has_tree && self.zoom != ROOT,
                                egui::Button::new("Zoom to Top"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Zoom(ROOT));
                        }
                        if ui
                            .add_enabled(
                                self.highlight_ext.is_some(),
                                egui::Button::new("Clear Highlight"),
                            )
                            .clicked()
                        {
                            actions.push(Action::HighlightExt(None));
                        }
                    });
                });
            });

            ui.add_space(spacing::GROUP);
            if ui.button("Choose Folder...").clicked() {
                actions.push(Action::ChooseFolder);
            }
            if ui
                .add_enabled(has_tree, egui::Button::new("Rescan"))
                .clicked()
            {
                actions.push(Action::Rescan);
            }
            ui.add_space(spacing::GROUP);
            ui.label(RichText::new("Size:").color(p.text.label));
            for metric in [SizeMetric::Allocated, SizeMetric::Logical] {
                if ui
                    .selectable_label(self.prefs.metric == metric, metric.label())
                    .on_hover_text(match metric {
                        SizeMetric::Allocated => "Space the files occupy on disk (like du)",
                        SizeMetric::Logical => {
                            "The files' byte length (like Finder's Get Info size)"
                        }
                    })
                    .clicked()
                {
                    actions.push(Action::SetMetric(metric));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let Some(tree) = &self.tree else { return };
                let mono =
                    |text: String, color: Color32| RichText::new(text).monospace().color(color);
                if let Some(summary) = &self.summary {
                    ui.label(mono(
                        format::duration(summary.elapsed.as_secs_f64()),
                        p.text.muted,
                    ))
                    .on_hover_text("Scan time");
                }
                let root = tree.node(ROOT);
                ui.label(mono(
                    format!("{} folders", format::count(root.dirs)),
                    p.text.label,
                ));
                ui.label(mono(
                    format!("{} files", format::count(root.files)),
                    p.text.label,
                ));
                let used = tree.total() - tree.synthetic_size(FLAG_FREE_SPACE);
                ui.label(mono(format::bytes(used), p.accent.readout))
                    .on_hover_text(format!(
                        "{} bytes used ({})",
                        format::count(used),
                        tree.metric.label()
                    ));
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = palette();
        ui.horizontal_centered(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.has_full_disk_access {
                    let resp = ui
                        .add(
                            egui::Label::new(
                                RichText::new("No Full Disk Access")
                                    .small()
                                    .color(p.status.warning),
                            )
                            .sense(Sense::click()),
                        )
                        .on_hover_text("Click to grant Full Disk Access");
                    if resp.clicked() {
                        actions.push(Action::ShowAccessPrompt);
                    }
                }
                if let Some(summary) = &self.summary {
                    if summary.errors > 0 {
                        ui.label(
                            RichText::new(format!("{} unreadable", format::count(summary.errors)))
                                .small()
                                .color(p.status.warning),
                        )
                        .on_hover_text(
                            "Folders that could not be read (macOS privacy protection or \
                             permissions). Their contents are not counted; on a whole-volume \
                             scan that space shows up as <Unknown>.",
                        );
                    }
                    if summary.cancelled {
                        ui.label(
                            RichText::new("Scan stopped - partial results")
                                .small()
                                .color(p.status.warning),
                        );
                    }
                }
                if let Some(notice) = &self.notice
                    && notice.at.elapsed() < Duration::from_secs(8)
                {
                    let color = if notice.error {
                        p.status.error
                    } else {
                        p.status.ok
                    };
                    ui.label(RichText::new(&notice.text).small().color(color));
                }

                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let (Some(tree), Some(id)) = (&self.tree, self.hovered.or(self.selected))
                    else {
                        return;
                    };
                    let node = tree.node(id);
                    ui.label(
                        RichText::new(format::bytes(node.size(tree.metric)))
                            .small()
                            .color(p.text.primary),
                    );
                    if let Some(description) = synthetic_description(node.flags) {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}: {description}", node.name))
                                    .small()
                                    .color(p.text.label),
                            )
                            .truncate(),
                        );
                        return;
                    }
                    if node.is_dir() {
                        ui.label(
                            RichText::new(format!("{} items", format::count(node.items())))
                                .small()
                                .color(p.text.muted),
                        );
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(tree.path(id).display().to_string())
                                .small()
                                .color(p.text.label),
                        )
                        .truncate(),
                    );
                });
            });
        });
    }

    // ------------------------------------------------------------------
    // Screens
    // ------------------------------------------------------------------

    fn welcome(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self
            .volumes_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
        {
            self.volumes = volumes::list();
            self.volumes_at = Some(Instant::now());
        }
        let p = palette();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.06).max(24.0));
                ui.label(RichText::new("MacDirStat").font(theme::bold_font(26.0)).color(p.text.primary));
                ui.label(RichText::new("See where your disk space went.").color(p.text.muted));
                ui.add_space(20.0);

                let width = ui.available_width().min(560.0);
                ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(width);
                    section_label(ui, "SCAN");
                    ui.horizontal(|ui| {
                        if ui.button("Choose Folder...").clicked() {
                            actions.push(Action::ChooseFolder);
                        }
                        if let Some(home) = std::env::home_dir()
                            && ui.button("Home Folder").clicked() {
                                actions.push(Action::Scan(home));
                            }
                    });

                    ui.add_space(spacing::GROUP * 2.0);
                    section_label(ui, "VOLUMES");
                    for (i, volume) in self.volumes.iter().enumerate() {
                        let resp = list_row(ui, ("volume", i), |ui| volume_row(ui, volume));
                        if resp.clicked() {
                            actions.push(Action::Scan(volume.path.clone()));
                        }
                        ui.add_space(spacing::ROW);
                    }

                    if !self.prefs.recent.is_empty() {
                        ui.add_space(spacing::GROUP * 2.0);
                        section_label(ui, "RECENT");
                        for (i, path) in self.prefs.recent.iter().enumerate() {
                            let resp = list_row(ui, ("recent", i), |ui| {
                                ui.horizontal(|ui| {
                                    paint_folder_icon(ui, p.text.icon);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(path.display().to_string()).color(p.text.primary),
                                        )
                                        .truncate(),
                                    );
                                });
                            });
                            if resp.clicked() {
                                actions.push(Action::Scan(path.clone()));
                            }
                            ui.add_space(spacing::ROW);
                        }
                    }

                    ui.add_space(spacing::GROUP * 2.0);
                    ui.label(
                        RichText::new(
                            "Some folders (Mail, Messages, Safari, other users) are protected by macOS. \
                             For a complete scan, grant Full Disk Access in System Settings > \
                             Privacy & Security.",
                        )
                        .small()
                        .color(p.text.muted),
                    );
                    if let Some(notice) = &self.notice {
                        ui.add_space(spacing::GROUP);
                        let color = if notice.error { p.status.error } else { p.status.ok };
                        ui.label(RichText::new(&notice.text).color(color));
                    }
                });
            });
        });
    }

    fn scan_view(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(handle) = &self.scan else { return };
        let p = palette();
        let progress = &handle.progress;
        let cancelling = progress.cancel.load(Ordering::Relaxed);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.22).max(24.0));
            let width = ui.available_width().min(480.0);
            ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                egui::Frame::new()
                    .fill(p.surface.card)
                    .stroke(egui::Stroke::new(1.0, p.surface.card_stroke))
                    .corner_radius(spacing::CORNER_RADIUS)
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0).color(p.accent.cornflower));
                            ui.label(
                                RichText::new(if cancelling { "Stopping" } else { "Scanning" })
                                    .font(theme::bold_font(15.0))
                                    .color(p.text.primary),
                            );
                        });
                        ui.add(
                            egui::Label::new(
                                RichText::new(handle.root.display().to_string())
                                    .color(p.text.label),
                            )
                            .truncate(),
                        );
                        ui.add_space(spacing::GROUP);
                        egui::Grid::new("scan_stats")
                            .num_columns(2)
                            .spacing([24.0, 4.0])
                            .show(ui, |ui| {
                                let stat =
                                    |ui: &mut egui::Ui,
                                     label: &str,
                                     value: String,
                                     color: Color32| {
                                        ui.label(RichText::new(label).color(p.text.muted));
                                        ui.label(RichText::new(value).monospace().color(color));
                                        ui.end_row();
                                    };
                                stat(
                                    ui,
                                    "Size",
                                    format::bytes(progress.bytes.load(Ordering::Relaxed)),
                                    p.accent.readout,
                                );
                                stat(
                                    ui,
                                    "Files",
                                    format::count(progress.files.load(Ordering::Relaxed)),
                                    p.text.primary,
                                );
                                stat(
                                    ui,
                                    "Folders",
                                    format::count(progress.dirs.load(Ordering::Relaxed)),
                                    p.text.primary,
                                );
                                let errors = progress.errors.load(Ordering::Relaxed);
                                stat(
                                    ui,
                                    "Unreadable",
                                    format::count(errors),
                                    if errors > 0 {
                                        p.status.warning
                                    } else {
                                        p.text.primary
                                    },
                                );
                                stat(
                                    ui,
                                    "Elapsed",
                                    format::duration(handle.started.elapsed().as_secs_f64()),
                                    p.text.primary,
                                );
                            });
                        ui.add_space(spacing::GROUP);
                        let current = progress
                            .current
                            .lock()
                            .map(|c| c.clone())
                            .unwrap_or_default();
                        ui.add(
                            egui::Label::new(RichText::new(current).small().color(p.text.muted))
                                .truncate(),
                        );
                        ui.add_space(spacing::GROUP);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(!cancelling, egui::Button::new("Stop"))
                                .on_hover_text("Stop scanning and show what has been found so far")
                                .clicked()
                            {
                                actions.push(Action::CancelScan);
                            }
                        });
                    });
            });
        });
    }

    // ------------------------------------------------------------------
    // Panels
    // ------------------------------------------------------------------

    fn directory_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.rows_dirty {
            self.rebuild_rows();
        }
        let scroll_to = self.scroll_to.take();
        let Some(tree) = &self.tree else { return };
        let p = palette();
        let rows = &self.rows;
        let scroll_index = scroll_to.and_then(|id| rows.iter().position(|r| r.id == id));

        theme::panel_header(ui, "Directory", |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(tree.root_path.display().to_string())
                        .small()
                        .color(p.text.muted),
                )
                .truncate(),
            );
        });

        let mut table = TableBuilder::new(ui)
            .id_salt("directory_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(320.0).at_least(140.0).clip(true))
            .column(Column::initial(110.0).at_least(40.0).clip(true))
            .column(Column::initial(54.0).at_least(40.0).clip(true))
            .column(Column::initial(76.0).at_least(50.0).clip(true))
            .column(Column::initial(70.0).at_least(40.0).clip(true))
            .column(Column::remainder().at_least(90.0).clip(true))
            .min_scrolled_height(0.0)
            .auto_shrink(false);
        if let Some(i) = scroll_index {
            table = table.scroll_to_row(i, Some(Align::Center));
        }

        let sort = self.sort;
        let selected = self.selected;
        let expanded = &self.expanded;
        table
            .header(20.0, |mut header| {
                sort_header(&mut header, "Name", SortColumn::Name, sort, false, actions);
                header.col(|ui| {
                    ui.label(RichText::new("Subtree").size(11.0).color(p.text.label));
                });
                header.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("%").size(11.0).color(p.text.label));
                    });
                });
                sort_header(&mut header, "Size", SortColumn::Size, sort, true, actions);
                sort_header(&mut header, "Items", SortColumn::Items, sort, true, actions);
                sort_header(
                    &mut header,
                    "Modified",
                    SortColumn::Modified,
                    sort,
                    false,
                    actions,
                );
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                    let r = rows[row.index()];
                    let node = tree.node(r.id);
                    let is_selected = selected == Some(r.id);
                    row.set_selected(is_selected);
                    let text_color = if is_selected {
                        Color32::WHITE
                    } else {
                        p.text.primary
                    };
                    let sub_color = if is_selected {
                        Color32::WHITE
                    } else {
                        p.text.label
                    };
                    let parent_size = if node.parent == NO_NODE {
                        node.size(tree.metric)
                    } else {
                        tree.size(node.parent)
                    };
                    let frac = format::fraction(node.size(tree.metric), parent_size);

                    row.col(|ui| {
                        name_cell(ui, tree, r, expanded.contains(&r.id), is_selected, actions)
                    });
                    row.col(|ui| subtree_bar(ui, frac));
                    row.col(|ui| {
                        right_label(
                            ui,
                            format::percent(node.size(tree.metric), parent_size),
                            sub_color,
                        )
                    });
                    row.col(|ui| {
                        right_label(ui, format::bytes(node.size(tree.metric)), text_color)
                    });
                    row.col(|ui| {
                        let items = if node.is_dir() {
                            format::count(node.items())
                        } else {
                            String::new()
                        };
                        right_label(ui, items, sub_color);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(format::date(node.mtime)).color(sub_color));
                    });

                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        actions.push(Action::Select {
                            id: r.id,
                            reveal: false,
                        });
                    }
                    if resp.double_clicked() && node.is_dir() {
                        actions.push(Action::ToggleExpand(r.id));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, r.id, actions));
                });
            });
    }

    fn extensions_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(tree) = &self.tree else { return };
        let p = palette();
        theme::panel_header(ui, "Extensions", |ui| {
            ui.label(
                RichText::new(format!(
                    "{} types",
                    format::count(tree.ext_order.len() as u64)
                ))
                .small()
                .color(p.text.muted),
            );
        });
        // Percent of file data, excluding free space and accounting entries.
        let total: u64 = tree
            .ext_order
            .iter()
            .map(|&e| tree.exts[e as usize].size(tree.metric))
            .sum();
        let highlight = self.highlight_ext;
        TableBuilder::new(ui)
            .id_salt("extension_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(18.0))
            .column(Column::initial(96.0).at_least(60.0).clip(true))
            .column(Column::initial(76.0).at_least(50.0).clip(true))
            .column(Column::initial(52.0).at_least(40.0).clip(true))
            .column(Column::remainder().at_least(50.0).clip(true))
            .min_scrolled_height(0.0)
            .auto_shrink(false)
            .header(20.0, |mut header| {
                header.col(|_| {});
                for (label, right) in [
                    ("Extension", false),
                    ("Size", true),
                    ("%", true),
                    ("Files", true),
                ] {
                    header.col(|ui| {
                        let layout = if right {
                            Layout::right_to_left(Align::Center)
                        } else {
                            Layout::left_to_right(Align::Center)
                        };
                        ui.with_layout(layout, |ui| {
                            ui.label(RichText::new(label).size(11.0).color(p.text.label));
                        });
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, tree.ext_order.len(), |mut row| {
                    let ext_id = tree.ext_order[row.index()];
                    let ext = &tree.exts[ext_id as usize];
                    let is_selected = highlight == Some(ext_id);
                    row.set_selected(is_selected);
                    let text_color = if is_selected {
                        Color32::WHITE
                    } else {
                        p.text.primary
                    };
                    let sub_color = if is_selected {
                        Color32::WHITE
                    } else {
                        p.text.label
                    };
                    row.col(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        let c = treemap::ext_color(ext.rank);
                        ui.painter()
                            .rect_filled(rect, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(ext.display_name()).color(text_color))
                                .truncate(),
                        );
                    });
                    row.col(|ui| right_label(ui, format::bytes(ext.size(tree.metric)), text_color));
                    row.col(|ui| {
                        right_label(ui, format::percent(ext.size(tree.metric), total), sub_color)
                    });
                    row.col(|ui| right_label(ui, format::count(ext.files), sub_color));
                    let resp = row.response();
                    if resp.clicked() {
                        actions.push(Action::HighlightExt(if is_selected {
                            None
                        } else {
                            Some(ext_id)
                        }));
                    }
                    resp.on_hover_text("Click to highlight these files in the treemap");
                });
            });
    }

    fn treemap_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = palette();
        let Some(tree) = &self.tree else { return };

        theme::panel_header(ui, "Treemap", |ui| {
            ui.label(
                RichText::new(
                    "Click to select \u{00B7} double-click to zoom in \u{00B7} Esc to zoom out",
                )
                .small()
                .color(p.text.muted),
            );
        });

        // Breadcrumbs for the zoom root.
        egui::Frame::new()
            .inner_margin(Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let can_zoom_out = self.zoom != ROOT;
                    let up = ui
                        .add_enabled(
                            can_zoom_out,
                            egui::Button::new("").min_size(vec2(26.0, 20.0)),
                        )
                        .on_hover_text("Zoom out (Esc)");
                    let arrow = if can_zoom_out {
                        p.text.primary
                    } else {
                        p.text.muted
                    };
                    paint_triangle(ui.painter(), up.rect.center(), Facing::Up, 4.5, arrow);
                    if up.clicked() {
                        actions.push(Action::ZoomOut);
                    }
                    let mut chain: Vec<NodeId> = tree.ancestors(self.zoom).collect();
                    chain.reverse();
                    chain.push(self.zoom);
                    for (i, &id) in chain.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new("\u{203A}").color(p.text.muted));
                        }
                        let name = if id == ROOT {
                            tree.root_path.display().to_string()
                        } else {
                            tree.node(id).name.to_string()
                        };
                        let current = id == self.zoom;
                        let text = if current {
                            RichText::new(name).color(Color32::WHITE)
                        } else {
                            RichText::new(name).color(p.text.label)
                        };
                        if ui
                            .add(egui::Button::new(text).selected(current).frame(current))
                            .clicked()
                        {
                            actions.push(Action::Zoom(id));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let size = tree.size(self.zoom);
                        ui.label(
                            RichText::new(format!(
                                "{}  \u{00B7}  {} of total",
                                format::bytes(size),
                                format::percent(size, tree.total())
                            ))
                            .monospace()
                            .color(p.text.label),
                        );
                    });
                });
            });

        // Canvas, aligned to the physical pixel grid so the texture maps 1:1.
        let avail = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        let origin = pos2(
            (avail.min.x * ppp).ceil() / ppp,
            (avail.min.y * ppp).ceil() / ppp,
        );
        let width_px = ((avail.max.x - origin.x) * ppp).floor().max(0.0) as u32;
        let height_px = ((avail.max.y - origin.y) * ppp).floor().max(0.0) as u32;
        let canvas =
            Rect::from_min_size(origin, vec2(width_px as f32 / ppp, height_px as f32 / ppp));
        let resp = ui.allocate_rect(avail, Sense::click());

        if tree.size(self.zoom) == 0 {
            ui.painter().text(
                avail.center(),
                egui::Align2::CENTER_CENTER,
                "This folder is empty.",
                egui::FontId::proportional(13.0),
                p.text.muted,
            );
            self.hovered = None;
            return;
        }

        let options = RenderOptions {
            cushion: self.prefs.cushion,
            grid: self.prefs.grid,
            highlight_ext: self.highlight_ext,
        };
        let key = TreemapKey {
            generation: tree.generation,
            tree_id: tree.id,
            zoom: self.zoom,
            width: width_px,
            height: height_px,
            options,
        };
        let view = &mut self.treemap;
        if let Some(done) = self.worker.latest() {
            // Results for a previous scan are stale; drop them.
            if done.key.tree_id == tree.id {
                let size = [done.key.width as usize, done.key.height as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &done.rendered.rgba);
                match &mut view.texture {
                    Some(texture) => texture.set(image, egui::TextureOptions::NEAREST),
                    None => {
                        view.texture = Some(ui.ctx().load_texture(
                            "treemap",
                            image,
                            egui::TextureOptions::NEAREST,
                        ));
                    }
                }
                view.layout = Some(done.rendered.layout);
                view.shown = Some(done.key);
                if view.pending == Some(done.key) {
                    view.pending = None;
                }
            }
        }
        if width_px > 0
            && height_px > 0
            && view.shown != Some(key)
            && view.pending != Some(key)
            && let Some(shared) = &self.tree
        {
            self.worker.submit(key, Arc::clone(shared));
            view.pending = Some(key);
        }

        let painter = ui.painter_at(avail);
        if let Some(texture) = &self.treemap.texture {
            painter.image(
                texture.id(),
                canvas,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let Some(layout) = &self.treemap.layout else {
            return;
        };

        // While a resize renders in the background, the previous image is
        // stretched to the canvas; scale its layout the same way.
        let sx = width_px as f32 / layout.width.max(1) as f32;
        let sy = height_px as f32 / layout.height.max(1) as f32;
        let to_screen = |r: FRect| {
            Rect::from_min_max(
                pos2(
                    origin.x + r.x0 as f32 * sx / ppp,
                    origin.y + r.y0 as f32 * sy / ppp,
                ),
                pos2(
                    origin.x + r.x1 as f32 * sx / ppp,
                    origin.y + r.y1 as f32 * sy / ppp,
                ),
            )
        };
        let hovered = resp.hover_pos().and_then(|pos| {
            let px = ((pos.x - origin.x) * ppp / sx).floor() as i32;
            let py = ((pos.y - origin.y) * ppp / sy).floor() as i32;
            layout.hit_test(tree, px, py)
        });
        self.hovered = hovered;

        if let Some(r) = hovered.and_then(|h| layout.rect(h)) {
            painter.rect_stroke(
                to_screen(r),
                0.0,
                egui::Stroke::new(1.0, Color32::from_white_alpha(170)),
                egui::StrokeKind::Inside,
            );
        }
        // Frame the selection, or its nearest ancestor large enough to draw.
        if let Some(sel) = self
            .selected
            .filter(|&s| tree.is_live(s) && tree.is_within(s, self.zoom))
        {
            let framed = std::iter::once(sel)
                .chain(tree.ancestors(sel))
                .find_map(|id| layout.rect(id));
            if let Some(r) = framed {
                let rect = to_screen(r);
                painter.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(2.0, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
                if rect.width() > 8.0 && rect.height() > 8.0 {
                    painter.rect_stroke(
                        rect.shrink(2.0),
                        0.0,
                        egui::Stroke::new(1.0, Color32::from_black_alpha(200)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        if resp.clicked()
            && let Some(h) = hovered
        {
            actions.push(Action::Select {
                id: h,
                reveal: true,
            });
        }
        if resp.double_clicked()
            && let Some(child) = hovered.and_then(|h| tree.child_toward(self.zoom, h))
            && tree.node(child).is_dir()
        {
            actions.push(Action::Zoom(child));
        }
        if resp.secondary_clicked() {
            self.menu_target = hovered;
            if let Some(h) = hovered {
                actions.push(Action::Select {
                    id: h,
                    reveal: true,
                });
            }
        }
        let menu_target = self.menu_target;
        resp.context_menu(|ui| match menu_target {
            Some(target) if tree.is_live(target) => node_menu(ui, tree, target, actions),
            _ => ui.close(),
        });
        if let Some(h) = hovered {
            resp.on_hover_ui_at_pointer(|ui| {
                let node = tree.node(h);
                ui.label(RichText::new(&*node.name).color(p.text.primary));
                ui.label(
                    RichText::new(format::bytes(node.size(tree.metric))).color(p.accent.readout),
                );
                if let Some(description) = synthetic_description(node.flags) {
                    ui.set_max_width(320.0);
                    ui.label(RichText::new(description).small().color(p.text.muted));
                } else if let Some(parent) = (node.parent != NO_NODE).then_some(node.parent) {
                    ui.label(
                        RichText::new(tree.path(parent).display().to_string())
                            .small()
                            .color(p.text.muted),
                    );
                }
            });
        }
    }

    /// Ask for Full Disk Access. macOS offers no in-app prompt for it, so
    /// this explains why and opens the right System Settings pane.
    fn access_modal(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.access_prompt || self.confirm_trash.is_some() {
            return;
        }
        let p = palette();
        let bundle = access::app_bundle();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("full_disk_access")).show(ctx, |ui| {
            ui.set_width(460.0);
            if self.has_full_disk_access {
                ui.label(
                    RichText::new("Full Disk Access is on")
                        .font(theme::bold_font(14.0))
                        .color(p.text.primary),
                );
                ui.add_space(spacing::GROUP);
                ui.label(
                    RichText::new("MacDirStat can measure every folder, including protected ones.")
                        .color(p.text.secondary),
                );
                ui.add_space(spacing::GROUP * 2.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        close = true;
                    }
                    if self.tree.is_some() && ui.button("Rescan").clicked() {
                        actions.push(Action::Rescan);
                        close = true;
                    }
                });
                return;
            }
            ui.label(
                RichText::new("Allow Full Disk Access")
                    .font(theme::bold_font(14.0))
                    .color(p.text.primary),
            );
            ui.add_space(spacing::GROUP);
            ui.label(
                RichText::new(
                    "macOS protects Mail, Messages, Safari, Photos libraries, app containers, \
                     Time Machine data and other users' folders. Without Full Disk Access \
                     MacDirStat cannot see inside them, so their space is reported as \
                     <Unknown>.",
                )
                .color(p.text.secondary),
            );
            ui.add_space(spacing::GROUP);
            let target = if bundle.is_some() {
                "MacDirStat"
            } else {
                "the terminal app that launches MacDirStat"
            };
            for (i, step) in [
                "Click Open System Settings.".to_owned(),
                format!("Turn on {target} in the Full Disk Access list."),
                format!(
                    "If it is not listed, click + and choose {target}{}.",
                    if bundle.is_some() {
                        " (Show in Finder reveals it)"
                    } else {
                        ""
                    }
                ),
                "Come back and click Check Again, then rescan.".to_owned(),
            ]
            .iter()
            .enumerate()
            {
                ui.label(RichText::new(format!("{}. {step}", i + 1)).color(p.text.label));
            }
            ui.add_space(spacing::GROUP * 2.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open =
                    egui::Button::new(RichText::new("Open System Settings").color(Color32::WHITE))
                        .fill(p.accent.selection_fill);
                if ui.add(open).clicked()
                    && let Err(err) = access::open_settings()
                {
                    self.notice = Some(Notice {
                        text: format!("Could not open System Settings: {err}"),
                        error: true,
                        at: Instant::now(),
                    });
                }
                if ui.button("Check Again").clicked() {
                    self.has_full_disk_access = access::has_full_disk_access();
                }
                if let Some(bundle) = &bundle
                    && ui.button("Show in Finder").clicked()
                {
                    let _ = std::process::Command::new("open")
                        .arg("-R")
                        .arg(bundle)
                        .spawn();
                }
                if ui.button("Not Now").clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.access_prompt = false;
        }
    }

    fn trash_modal(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(id) = self.confirm_trash else { return };
        let Some(tree) = &self.tree else {
            self.confirm_trash = None;
            return;
        };
        let p = palette();
        let node = tree.node(id);
        let path = tree.path(id);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("confirm_trash")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new("Move to Trash?").font(theme::bold_font(14.0)).color(p.text.primary));
            ui.add_space(spacing::GROUP);
            let what = if node.is_dir() {
                format!(
                    "The folder \u{201C}{}\u{201D} and its {} items ({}) will be moved to the Trash.",
                    node.name,
                    format::count(node.items()),
                    format::bytes(node.size(tree.metric))
                )
            } else {
                format!(
                    "\u{201C}{}\u{201D} ({}) will be moved to the Trash.",
                    node.name,
                    format::bytes(node.size(tree.metric))
                )
            };
            ui.label(RichText::new(what).color(p.text.secondary));
            ui.add(egui::Label::new(RichText::new(path.display().to_string()).small().color(p.text.muted)).wrap());
            ui.add_space(spacing::GROUP * 2.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let confirm = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE))
                    .fill(Color32::from_rgb(180, 60, 60));
                if ui.add(confirm).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    actions.push(Action::Trash(id));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.confirm_trash = None;
        }
    }
}

impl eframe::App for MacDirStatApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_scan(&ctx);
        let mut actions = Vec::new();
        self.shortcuts(&ctx, &mut actions);

        let p = palette();
        // Gaps between panels show this as dark dividers.
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, p.surface.divider);

        egui::Panel::top("toolbar")
            .exact_size(30.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.surface.root)
                    .inner_margin(Margin::symmetric(6, 0)),
            )
            .show(ui, |ui| self.toolbar(ui, &mut actions));
        egui::Panel::bottom("status")
            .exact_size(22.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.surface.root)
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| self.status_bar(ui, &mut actions));

        if self.scan.is_some() {
            egui::CentralPanel::default()
                .frame(cell_frame())
                .show(ui, |ui| self.scan_view(ui, &mut actions));
        } else if self.tree.is_none() {
            egui::CentralPanel::default()
                .frame(cell_frame())
                .show(ui, |ui| self.welcome(ui, &mut actions));
        } else {
            let available = ui.available_height();
            egui::Panel::top("lists")
                .resizable(true)
                .default_size(available * 0.4)
                .size_range(120.0..=(available - 160.0).max(121.0))
                .show_separator_line(false)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    egui::Panel::right("extensions")
                        .resizable(true)
                        .default_size(330.0)
                        .size_range(200.0..=640.0)
                        .show_separator_line(false)
                        .frame(cell_frame())
                        .show(ui, |ui| self.extensions_panel(ui, &mut actions));
                    egui::CentralPanel::default()
                        .frame(cell_frame())
                        .show(ui, |ui| self.directory_panel(ui, &mut actions));
                });
            egui::CentralPanel::default()
                .frame(cell_frame())
                .show(ui, |ui| self.treemap_panel(ui, &mut actions));
        }

        self.trash_modal(&ctx, &mut actions);
        self.access_modal(&ctx, &mut actions);
        for action in actions {
            self.apply(&ctx, action);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.prefs);
    }
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn cell_frame() -> egui::Frame {
    theme::panel_frame().outer_margin(Margin::same(1))
}

/// What a synthetic disk-accounting entry stands for.
fn synthetic_description(flags: u8) -> Option<&'static str> {
    if flags & FLAG_FREE_SPACE != 0 {
        Some("space still available on this volume")
    } else if flags & FLAG_UNKNOWN != 0 {
        Some(
            "used space that no readable file accounts for: folders macOS would not let \
             MacDirStat read, local Time Machine snapshots, purgeable caches and filesystem \
             metadata",
        )
    } else if flags & FLAG_VOLUME != 0 {
        Some(
            "another volume in this disk's APFS container (macOS support data such as \
             Preboot, Recovery, VM swap and updates)",
        )
    } else {
        None
    }
}

fn sorted_children(tree: &Tree, id: NodeId, sort: (SortColumn, bool)) -> Vec<NodeId> {
    let mut children = tree.node(id).children.clone();
    let (column, descending) = sort;
    match column {
        // Children are stored largest-first already.
        SortColumn::Size => {}
        SortColumn::Name => children.sort_by(|&a, &b| {
            let (na, nb) = (&tree.node(a).name, &tree.node(b).name);
            na.to_lowercase().cmp(&nb.to_lowercase()).reverse()
        }),
        SortColumn::Items => children.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).items())),
        SortColumn::Modified => children.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).mtime)),
    }
    // Every arm above leaves the list in descending order.
    if !descending {
        children.reverse();
    }
    children
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(palette().text.label));
    ui.add_space(spacing::TIGHT);
}

/// A clickable gray row, as in a file browser.
fn list_row(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let p = palette();
    let inner = egui::Frame::new()
        .fill(p.surface.card)
        .corner_radius(spacing::CORNER_RADIUS)
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    let rect = inner.response.rect;
    let resp = ui.interact(rect, ui.id().with(id_salt), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, spacing::CORNER_RADIUS, Color32::from_white_alpha(12));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn volume_row(ui: &mut egui::Ui, volume: &Volume) {
    let p = palette();
    ui.horizontal(|ui| {
        paint_disk_icon(ui, p.text.icon);
        ui.label(RichText::new(&volume.name).color(p.text.primary));
        ui.label(
            RichText::new(volume.path.display().to_string())
                .small()
                .color(p.text.muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} free of {}",
                    format::bytes(volume.free),
                    format::bytes(volume.total)
                ))
                .small()
                .color(p.text.label),
            );
        });
    });
    let frac = format::fraction(volume.used(), volume.total) as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, p.surface.field);
    let fill = if frac > 0.95 {
        p.status.error
    } else if frac > 0.85 {
        p.status.warning
    } else {
        p.accent.selection_fill
    };
    let mut used = rect;
    used.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(used, 3.0, fill);
}

fn sort_header(
    header: &mut TableRow<'_, '_>,
    label: &str,
    column: SortColumn,
    sort: (SortColumn, bool),
    right: bool,
    actions: &mut Vec<Action>,
) {
    let p = palette();
    header.col(|ui| {
        let active = sort.0 == column;
        let color = if active { p.text.primary } else { p.text.label };
        let text = RichText::new(label).size(11.0).color(color);
        let layout = if right {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        };
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let paint_arrow = |ui: &mut egui::Ui| {
                if active {
                    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    let facing = if sort.1 { Facing::Down } else { Facing::Up };
                    paint_triangle(ui.painter(), rect.center(), facing, 3.0, color);
                }
            };
            // Right-to-left layouts place the first widget rightmost.
            if right {
                paint_arrow(ui);
            }
            if ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .clicked()
            {
                actions.push(Action::Sort(column));
            }
            if !right {
                paint_arrow(ui);
            }
        });
    });
}

fn name_cell(
    ui: &mut egui::Ui,
    tree: &Tree,
    row: Row,
    expanded: bool,
    selected: bool,
    actions: &mut Vec<Action>,
) {
    let p = palette();
    let node = tree.node(row.id);
    ui.add_space(f32::from(row.depth) * INDENT);
    ui.spacing_mut().item_spacing.x = 4.0;

    let (rect, resp) = ui.allocate_exact_size(vec2(12.0, ROW_HEIGHT), Sense::click());
    if node.is_dir() && !node.children.is_empty() {
        let color = if selected {
            Color32::WHITE
        } else {
            p.text.icon
        };
        paint_disclosure(ui.painter(), rect.center(), expanded, color);
        if resp.clicked() {
            actions.push(Action::ToggleExpand(row.id));
        }
    }

    if node.is_dir() {
        let color = if node.flags & FLAG_DENIED != 0 {
            p.status.warning
        } else if selected {
            Color32::WHITE
        } else {
            p.text.icon
        };
        paint_folder_icon(ui, color);
    } else {
        let (rect, _) = ui.allocate_exact_size(vec2(14.0, ROW_HEIGHT), Sense::hover());
        let rank = tree
            .exts
            .get(node.ext as usize)
            .map_or(u32::MAX, |e| e.rank);
        let c = treemap::synthetic_color(node.flags).unwrap_or_else(|| treemap::ext_color(rank));
        ui.painter().rect_filled(
            Rect::from_center_size(rect.center(), vec2(9.0, 9.0)),
            2.0,
            Color32::from_rgb(c[0], c[1], c[2]),
        );
    }

    let name = if row.id == ROOT {
        tree.root_path.display().to_string()
    } else {
        node.name.to_string()
    };
    let (color, note) = if node.flags & FLAG_DENIED != 0 {
        (
            p.status.warning,
            Some("Could not be read (permission or privacy protection)"),
        )
    } else if node.flags & FLAG_SKIPPED != 0 {
        (
            p.text.muted,
            Some("Another volume or a system pseudo-filesystem; not entered"),
        )
    } else if node.flags & FLAG_INCOMPLETE != 0 {
        (
            p.text.muted,
            Some("The scan was stopped before this folder was read"),
        )
    } else if let Some(description) = synthetic_description(node.flags) {
        (
            if selected {
                Color32::WHITE
            } else {
                p.text.label
            },
            Some(description),
        )
    } else if selected {
        (Color32::WHITE, None)
    } else {
        (p.text.primary, None)
    };
    let label = ui.add(egui::Label::new(RichText::new(name).color(color)).truncate());
    if let Some(note) = note {
        label.on_hover_text(note);
    }
}

fn subtree_bar(ui: &mut egui::Ui, frac: f64) {
    let p = palette();
    let rect = ui.available_rect_before_wrap();
    let bar = Rect::from_min_size(
        pos2(rect.left() + 2.0, rect.center().y - 5.0),
        vec2((rect.width() - 6.0).max(0.0), 10.0),
    );
    ui.painter().rect_filled(bar, 2.0, p.surface.field);
    let mut fill = bar;
    fill.set_width(bar.width() * frac.clamp(0.0, 1.0) as f32);
    if fill.width() > 0.0 {
        ui.painter().rect_filled(fill, 2.0, p.accent.selection_fill);
    }
    ui.allocate_rect(rect, Sense::hover());
}

fn right_label(ui: &mut egui::Ui, text: String, color: Color32) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(RichText::new(text).color(color));
    });
}

fn node_menu(ui: &mut egui::Ui, tree: &Tree, id: NodeId, actions: &mut Vec<Action>) {
    let node = tree.node(id);
    if let Some(description) = synthetic_description(node.flags) {
        ui.set_max_width(320.0);
        ui.label(RichText::new(&*node.name).color(palette().text.primary));
        ui.label(
            RichText::new(description)
                .small()
                .color(palette().text.muted),
        );
        ui.separator();
        let mask = if node.flags & FLAG_FREE_SPACE != 0 {
            FLAG_FREE_SPACE
        } else {
            FLAG_UNKNOWN | FLAG_VOLUME
        };
        if ui.button("Hide").clicked() {
            actions.push(Action::SetSyntheticVisible {
                mask,
                visible: false,
            });
        }
        return;
    }
    if ui.button("Reveal in Finder").clicked() {
        actions.push(Action::RevealInFinder(id));
    }
    if ui
        .button(if node.is_dir() {
            "Open in Finder"
        } else {
            "Open"
        })
        .clicked()
    {
        actions.push(Action::Open(id));
    }
    if ui.button("Copy Path").clicked() {
        actions.push(Action::CopyPath(id));
    }
    ui.separator();
    let zoom_target = if node.is_dir() { id } else { node.parent };
    if zoom_target != NO_NODE {
        let label = if node.is_dir() {
            "Zoom Into Folder"
        } else {
            "Zoom Into Enclosing Folder"
        };
        if ui.button(label).clicked() {
            actions.push(Action::Zoom(zoom_target));
        }
    }
    if node.ext != crate::tree::NO_EXT && ui.button("Highlight This Extension").clicked() {
        actions.push(Action::HighlightExt(Some(node.ext)));
    }
    ui.separator();
    if ui
        .add_enabled(id != ROOT, egui::Button::new("Move to Trash..."))
        .clicked()
    {
        actions.push(Action::AskTrash(id));
    }
}

#[derive(Clone, Copy)]
enum Facing {
    Up,
    Down,
    Right,
}

/// Painted triangles: the bundled fonts have no geometric-shape glyphs.
fn paint_triangle(painter: &egui::Painter, center: Pos2, facing: Facing, s: f32, color: Color32) {
    let points = match facing {
        Facing::Down => vec![
            pos2(center.x - s, center.y - s * 0.6),
            pos2(center.x + s, center.y - s * 0.6),
            pos2(center.x, center.y + s * 0.8),
        ],
        Facing::Up => vec![
            pos2(center.x - s, center.y + s * 0.6),
            pos2(center.x, center.y - s * 0.8),
            pos2(center.x + s, center.y + s * 0.6),
        ],
        Facing::Right => vec![
            pos2(center.x - s * 0.6, center.y - s),
            pos2(center.x + s * 0.8, center.y),
            pos2(center.x - s * 0.6, center.y + s),
        ],
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

fn paint_disclosure(painter: &egui::Painter, center: Pos2, expanded: bool, color: Color32) {
    let facing = if expanded {
        Facing::Down
    } else {
        Facing::Right
    };
    paint_triangle(painter, center, facing, 3.5, color);
}

fn paint_folder_icon(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, ROW_HEIGHT.min(16.0)), Sense::hover());
    let body = Rect::from_center_size(rect.center() + vec2(0.0, 1.0), vec2(13.0, 9.0));
    let tab = Rect::from_min_size(body.min - vec2(0.0, 2.0), vec2(5.5, 3.0));
    let painter = ui.painter();
    painter.rect_filled(tab, 1.0, color.gamma_multiply(0.8));
    painter.rect_filled(body, 1.5, color.gamma_multiply(0.8));
}

fn paint_disk_icon(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let body = Rect::from_center_size(rect.center(), vec2(15.0, 10.0));
    let painter = ui.painter();
    painter.rect_filled(body, 2.0, color.gamma_multiply(0.7));
    painter.circle_filled(
        pos2(body.right() - 3.5, body.bottom() - 3.0),
        1.2,
        Color32::from_rgb(80, 200, 80),
    );
}

/// Trash through NSFileManager rather than scripting Finder, which would
/// need Automation permission (and an Apple Events entitlement once the app
/// runs with the hardened runtime).
fn move_to_trash(path: &std::path::Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::tests::sample;

    fn names(tree: &Tree, ids: &[NodeId]) -> Vec<String> {
        ids.iter().map(|&i| tree.node(i).name.to_string()).collect()
    }

    #[test]
    fn sorted_children_by_each_column() {
        let tree = sample();
        assert_eq!(
            names(
                &tree,
                &sorted_children(&tree, ROOT, (SortColumn::Size, true))
            ),
            ["a", "d.jpg", "e"]
        );
        assert_eq!(
            names(
                &tree,
                &sorted_children(&tree, ROOT, (SortColumn::Size, false))
            ),
            ["e", "d.jpg", "a"]
        );
        assert_eq!(
            names(
                &tree,
                &sorted_children(&tree, ROOT, (SortColumn::Name, false))
            ),
            ["a", "d.jpg", "e"]
        );
        assert_eq!(
            names(
                &tree,
                &sorted_children(&tree, ROOT, (SortColumn::Modified, true))
            ),
            ["a", "d.jpg", "e"]
        );
    }
}
