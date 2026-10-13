//! Squarified cushion treemap, after WinDirStat / SequoiaView.
//!
//! Layout runs in physical pixels. Every node's rectangle is subdivided with
//! the squarified algorithm (children are already sorted largest-first), and
//! each nesting level adds a parabolic "ridge" to a per-rectangle surface.
//! Leaves are shaded by lighting that surface, which gives the familiar
//! pillowed look where folder boundaries read as valleys.
//!
//! Treemap pixels are content rather than chrome, so they use the fixed
//! extension palette below instead of the theme palette.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::tree::{
    ExtId, FLAG_FREE_SPACE, FLAG_UNKNOWN, FLAG_VOLUME, NO_EXT, NodeId, NodeKind, Tree,
};

// WinDirStat's default cushion parameters.
const HEIGHT: f64 = 0.40;
const SCALE_FACTOR: f64 = 0.90;
const AMBIENT: f64 = 0.15;
const BRIGHTNESS: f64 = 0.84;
const PALETTE_BRIGHTNESS: f64 = 0.6;
const LIGHT: [f64; 3] = [-1.0, -1.0, 10.0];

/// Rectangles narrower than this are drawn as one block instead of being
/// subdivided further.
const MIN_SUBDIVIDE_PX: f64 = 2.0;
const BAND_ROWS: usize = 16;

/// Extension hues (cornflower, amber, cyan, magenta, green, orange, ...).
/// They are brightness-equalized before use so that no hue dominates the map
/// by luminance alone.
const EXT_HUES: [[u8; 3]; 12] = [
    [100, 149, 237],
    [255, 80, 80],
    [100, 255, 100],
    [255, 200, 50],
    [50, 200, 255],
    [255, 80, 200],
    [255, 140, 80],
    [160, 110, 255],
    [60, 200, 170],
    [255, 160, 160],
    [190, 255, 90],
    [150, 200, 255],
];
const OTHER_GRAY: [u8; 3] = [120, 120, 120];
const FOLDER_GRAY: [u8; 3] = [95, 95, 100];
// Disk-accounting blocks: deliberately desaturated so they read as "not
// files" next to the extension hues.
const FREE_SPACE: [u8; 3] = [70, 78, 74];
const UNKNOWN: [u8; 3] = [150, 125, 80];
const VOLUME: [u8; 3] = [95, 105, 135];
const EMPTY_BG: [u8; 3] = [18, 18, 18];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl FRect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// The integer pixels whose centers fall inside the rectangle. Adjacent
    /// rectangles share their float edge, so ownership never overlaps.
    pub fn pixels(&self) -> (i32, i32, i32, i32) {
        (span(self.x0), span(self.y0), span(self.x1), span(self.y1))
    }

    pub fn owns_pixel(&self, px: i32, py: i32) -> bool {
        let (x0, y0, x1, y1) = self.pixels();
        px >= x0 && px < x1 && py >= y0 && py < y1
    }
}

fn span(v: f64) -> i32 {
    (v - 0.5).ceil() as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderOptions {
    pub cushion: bool,
    pub grid: bool,
    pub highlight_ext: Option<ExtId>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            cushion: true,
            grid: false,
            highlight_ext: None,
        }
    }
}

/// Rectangles from the last layout, for hit testing and outlines.
pub struct TreemapLayout {
    pub width: u32,
    pub height: u32,
    pub root: NodeId,
    rects: HashMap<NodeId, FRect>,
}

impl TreemapLayout {
    pub fn rect(&self, id: NodeId) -> Option<FRect> {
        self.rects.get(&id).copied()
    }

    /// Deepest laid-out node under the pixel, descending from the root.
    pub fn hit_test(&self, tree: &Tree, px: i32, py: i32) -> Option<NodeId> {
        let mut cur = self.root;
        if !self.rect(cur)?.owns_pixel(px, py) {
            return None;
        }
        loop {
            let node = tree.node(cur);
            if node.kind == NodeKind::File {
                return Some(cur);
            }
            let next = node
                .children
                .iter()
                .copied()
                .find(|c| self.rects.get(c).is_some_and(|r| r.owns_pixel(px, py)));
            match next {
                Some(c) => cur = c,
                None => return Some(cur),
            }
        }
    }
}

pub struct Rendered {
    /// RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub layout: TreemapLayout,
}

struct Leaf {
    px: (i32, i32, i32, i32),
    surface: [f64; 4],
    color: [u8; 3],
}

/// Equalized colors for extension ranks, computed once per render.
struct Colors {
    hues: Vec<[u8; 3]>,
    other: [u8; 3],
    folder: [u8; 3],
}

impl Colors {
    fn new() -> Self {
        Self {
            hues: EXT_HUES
                .iter()
                .map(|&c| make_bright(c, PALETTE_BRIGHTNESS))
                .collect(),
            other: make_bright(OTHER_GRAY, PALETTE_BRIGHTNESS * 0.8),
            folder: make_bright(FOLDER_GRAY, PALETTE_BRIGHTNESS * 0.7),
        }
    }
}

/// Swatch color for a synthetic disk-accounting entry, by its flag.
pub fn synthetic_color(flags: u8) -> Option<[u8; 3]> {
    let base = if flags & FLAG_FREE_SPACE != 0 {
        FREE_SPACE
    } else if flags & FLAG_UNKNOWN != 0 {
        UNKNOWN
    } else if flags & FLAG_VOLUME != 0 {
        VOLUME
    } else {
        return None;
    };
    Some(make_bright(base, PALETTE_BRIGHTNESS * 0.7))
}

/// The swatch color for an extension's rank in the size ranking.
pub fn ext_color(rank: u32) -> [u8; 3] {
    match EXT_HUES.get(rank as usize) {
        Some(&c) => make_bright(c, PALETTE_BRIGHTNESS),
        None => make_bright(OTHER_GRAY, PALETTE_BRIGHTNESS * 0.8),
    }
}

pub fn render(
    tree: &Tree,
    root: NodeId,
    width: u32,
    height: u32,
    options: RenderOptions,
) -> Rendered {
    let mut builder = Builder {
        tree,
        options,
        colors: Colors::new(),
        leaves: Vec::new(),
        rects: HashMap::new(),
    };
    let full = FRect::new(0.0, 0.0, f64::from(width), f64::from(height));
    if width > 0 && height > 0 && tree.size(root) > 0 {
        builder.layout(root, full, [0.0; 4], HEIGHT, true);
    }
    let rgba = rasterize(&builder.leaves, width as usize, height as usize, options);
    Rendered {
        rgba,
        layout: TreemapLayout {
            width,
            height,
            root,
            rects: builder.rects,
        },
    }
}

struct Builder<'a> {
    tree: &'a Tree,
    options: RenderOptions,
    colors: Colors,
    leaves: Vec<Leaf>,
    rects: HashMap<NodeId, FRect>,
}

impl Builder<'_> {
    fn layout(&mut self, id: NodeId, rect: FRect, surface: [f64; 4], h: f64, is_root: bool) {
        let (x0, y0, x1, y1) = rect.pixels();
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.rects.insert(id, rect);

        let mut surface = surface;
        if !is_root {
            add_ridge(&rect, &mut surface, h);
        }

        let node = self.tree.node(id);
        let too_small = rect.width() < MIN_SUBDIVIDE_PX || rect.height() < MIN_SUBDIVIDE_PX;
        if node.kind == NodeKind::File || node.children.is_empty() || too_small {
            let color = self.leaf_color(id);
            self.leaves.push(Leaf {
                px: (x0, y0, x1, y1),
                surface,
                color,
            });
            return;
        }

        let sizes: Vec<u64> = node.children.iter().map(|&c| self.tree.size(c)).collect();
        let rects = squarify(&sizes, rect);
        let children = node.children.clone();
        for (i, child_rect) in rects {
            self.layout(children[i], child_rect, surface, h * SCALE_FACTOR, false);
        }
    }

    fn leaf_color(&self, id: NodeId) -> [u8; 3] {
        let node = self.tree.node(id);
        let base = if let Some(c) = synthetic_color(node.flags) {
            c
        } else if node.kind == NodeKind::File && node.ext != NO_EXT {
            let rank = self.tree.exts[node.ext as usize].rank;
            self.colors
                .hues
                .get(rank as usize)
                .copied()
                .unwrap_or(self.colors.other)
        } else {
            self.colors.folder
        };
        match self.options.highlight_ext {
            Some(ext) if node.kind != NodeKind::File || node.ext != ext => dim(base),
            _ => base,
        }
    }
}

/// Squarified layout of `sizes` (sorted largest-first) into `rect`.
/// Returns the index and rectangle of every non-zero item. The union of the
/// rectangles is exactly `rect`.
pub fn squarify(sizes: &[u64], rect: FRect) -> Vec<(usize, FRect)> {
    let count = sizes.iter().take_while(|&&s| s > 0).count();
    let mut out = Vec::with_capacity(count);
    let mut remaining = rect;
    let mut remaining_size: f64 = sizes[..count].iter().map(|&s| s as f64).sum();
    let mut head = 0;

    while head < count {
        let w = remaining.width();
        let h = remaining.height();
        if w <= 0.0 || h <= 0.0 || remaining_size <= 0.0 {
            break;
        }
        // Lay the next row along the shorter side.
        let vertical_strip = w >= h;
        let side = if vertical_strip { h } else { w };
        let extent = if vertical_strip { w } else { h };
        let area_per_unit = (w * h) / remaining_size;
        // Side length squared, expressed in size units.
        let side_sq = side * side / area_per_unit;

        let rmax = sizes[head] as f64;
        let mut sum = 0.0;
        let mut worst = f64::INFINITY;
        let mut end = head;
        while end < count {
            let s = sizes[end] as f64;
            let next_sum = sum + s;
            let ss = next_sum * next_sum;
            let ratio = (side_sq * rmax / ss).max(ss / (side_sq * s));
            if ratio > worst {
                break;
            }
            worst = ratio;
            sum = next_sum;
            end += 1;
        }

        let last_row = end == count;
        let thickness = if last_row {
            extent
        } else {
            (sum / remaining_size * extent).min(extent)
        };

        let (strip, rest) = if vertical_strip {
            let cut = remaining.x0 + thickness;
            let cut = if last_row { remaining.x1 } else { cut };
            (
                FRect::new(remaining.x0, remaining.y0, cut, remaining.y1),
                FRect::new(cut, remaining.y0, remaining.x1, remaining.y1),
            )
        } else {
            let cut = remaining.y0 + thickness;
            let cut = if last_row { remaining.y1 } else { cut };
            (
                FRect::new(remaining.x0, remaining.y0, remaining.x1, cut),
                FRect::new(remaining.x0, cut, remaining.x1, remaining.y1),
            )
        };

        let mut cumulative = 0.0;
        let mut start = if vertical_strip { strip.y0 } else { strip.x0 };
        let stop = if vertical_strip { strip.y1 } else { strip.x1 };
        for (i, &size) in sizes.iter().enumerate().take(end).skip(head) {
            cumulative += size as f64;
            let edge = if i + 1 == end {
                stop
            } else {
                start.max(
                    (if vertical_strip { strip.y0 } else { strip.x0 }) + cumulative / sum * side,
                )
            };
            let item = if vertical_strip {
                FRect::new(strip.x0, start, strip.x1, edge)
            } else {
                FRect::new(start, strip.y0, edge, strip.y1)
            };
            out.push((i, item));
            start = edge;
        }

        remaining = rest;
        remaining_size -= sum;
        head = end;
    }
    out
}

fn add_ridge(rect: &FRect, surface: &mut [f64; 4], h: f64) {
    let width = rect.width();
    let height = rect.height();
    let h4 = 4.0 * h;
    if width > 0.0 {
        let wf = h4 / width;
        surface[2] += wf * (rect.x1 + rect.x0);
        surface[0] -= wf;
    }
    if height > 0.0 {
        let hf = h4 / height;
        surface[3] += hf * (rect.y1 + rect.y0);
        surface[1] -= hf;
    }
}

fn rasterize(leaves: &[Leaf], width: usize, height: usize, options: RenderOptions) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for px in rgba.as_chunks_mut::<4>().0 {
        *px = [EMPTY_BG[0], EMPTY_BG[1], EMPTY_BG[2], 255];
    }
    if width == 0 || height == 0 {
        return rgba;
    }

    let bands = height.div_ceil(BAND_ROWS);
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, leaf) in leaves.iter().enumerate() {
        let (_, y0, _, y1) = leaf.px;
        let first = (y0.max(0) as usize) / BAND_ROWS;
        let last = ((y1 - 1).max(0) as usize).min(height - 1) / BAND_ROWS;
        for bucket in &mut buckets[first..=last] {
            bucket.push(i as u32);
        }
    }

    let norm = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1] + LIGHT[2] * LIGHT[2]).sqrt();
    let light = [LIGHT[0] / norm, LIGHT[1] / norm, LIGHT[2] / norm];
    let grid = super::theme::palette().surface.divider;
    let grid_rgb = [grid.r(), grid.g(), grid.b()];

    rgba.par_chunks_mut(BAND_ROWS * width * 4)
        .zip(buckets.par_iter())
        .enumerate()
        .for_each(|(band, (chunk, members))| {
            let band_y0 = (band * BAND_ROWS) as i32;
            let band_y1 = (band_y0 + BAND_ROWS as i32).min(height as i32);
            for &i in members {
                let leaf = &leaves[i as usize];
                let (x0, y0, x1, y1) = leaf.px;
                let (x0, x1) = (x0.max(0), x1.min(width as i32));
                for py in y0.max(band_y0)..y1.min(band_y1) {
                    let row = ((py - band_y0) as usize) * width * 4;
                    let ny = -(2.0 * leaf.surface[1] * (f64::from(py) + 0.5) + leaf.surface[3]);
                    for px in x0..x1 {
                        let rgb = if options.grid && (px == x1 - 1 || py == y1 - 1) {
                            grid_rgb
                        } else if options.cushion {
                            let nx =
                                -(2.0 * leaf.surface[0] * (f64::from(px) + 0.5) + leaf.surface[2]);
                            shade(leaf.color, nx, ny, light)
                        } else {
                            leaf.color
                        };
                        let o = row + (px as usize) * 4;
                        chunk[o..o + 3].copy_from_slice(&rgb);
                    }
                }
            }
        });
    rgba
}

fn shade(color: [u8; 3], nx: f64, ny: f64, light: [f64; 3]) -> [u8; 3] {
    let cosa =
        ((nx * light[0] + ny * light[1] + light[2]) / (nx * nx + ny * ny + 1.0).sqrt()).min(1.0);
    let diffuse = ((1.0 - AMBIENT) * cosa).max(0.0);
    let pixel = (diffuse + AMBIENT) * BRIGHTNESS / PALETTE_BRIGHTNESS;
    normalize_color([
        (f64::from(color[0]) * pixel) as i32,
        (f64::from(color[1]) * pixel) as i32,
        (f64::from(color[2]) * pixel) as i32,
    ])
}

/// Scale a color so its average channel equals `brightness` (0..1), pushing
/// any overflow into the other channels as WinDirStat does.
pub fn make_bright(color: [u8; 3], brightness: f64) -> [u8; 3] {
    let d = color.map(|c| f64::from(c) / 255.0);
    let sum = d[0] + d[1] + d[2];
    if sum <= 0.0 {
        let v = (brightness * 255.0) as u8;
        return [v, v, v];
    }
    let f = 3.0 * brightness / sum;
    normalize_color(d.map(|c| (c * f * 255.0) as i32))
}

fn normalize_color(c: [i32; 3]) -> [u8; 3] {
    let [mut r, mut g, mut b] = c;
    if r > 255 {
        distribute_first(&mut r, &mut g, &mut b);
    } else if g > 255 {
        distribute_first(&mut g, &mut r, &mut b);
    } else if b > 255 {
        distribute_first(&mut b, &mut r, &mut g);
    }
    [r, g, b].map(|v| v.clamp(0, 255) as u8)
}

fn distribute_first(first: &mut i32, second: &mut i32, third: &mut i32) {
    let h = (*first - 255) / 2;
    *first = 255;
    *second += h;
    *third += h;
    if *second > 255 {
        let h = *second - 255;
        *second = 255;
        *third += h;
    } else if *third > 255 {
        let h = *third - 255;
        *third = 255;
        *second += h;
    }
}

fn dim(c: [u8; 3]) -> [u8; 3] {
    let avg = (u16::from(c[0]) + u16::from(c[1]) + u16::from(c[2])) / 3;
    c.map(|v| ((u16::from(v) + avg) / 2 / 4 + 10) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::ROOT;
    use crate::tree::tests::sample;

    fn area(r: &FRect) -> f64 {
        r.width() * r.height()
    }

    #[test]
    fn squarify_areas_are_proportional_and_cover_the_rect() {
        let sizes = [600, 250, 100, 50, 0];
        let rect = FRect::new(0.0, 0.0, 400.0, 250.0);
        let out = squarify(&sizes, rect);
        assert_eq!(out.len(), 4, "zero-sized items get no rectangle");
        let total: f64 = out.iter().map(|(_, r)| area(r)).sum();
        assert!((total - area(&rect)).abs() < 1e-6);
        for (i, r) in &out {
            let expected = sizes[*i] as f64 / 1000.0 * area(&rect);
            assert!((area(r) - expected).abs() < 1e-6, "item {i}");
            assert!(r.x0 >= rect.x0 && r.x1 <= rect.x1 && r.y0 >= rect.y0 && r.y1 <= rect.y1);
        }
    }

    #[test]
    fn squarify_rectangles_do_not_overlap() {
        let sizes: Vec<u64> = (1..=40).rev().map(|v| v * v).collect();
        let out = squarify(&sizes, FRect::new(0.0, 0.0, 300.0, 200.0));
        for (a, ra) in &out {
            for (b, rb) in &out {
                if a < b {
                    let ix = (ra.x1.min(rb.x1) - ra.x0.max(rb.x0)).max(0.0);
                    let iy = (ra.y1.min(rb.y1) - ra.y0.max(rb.y0)).max(0.0);
                    assert!(ix * iy < 1e-9, "{a} overlaps {b}");
                }
            }
        }
    }

    #[test]
    fn squarify_keeps_aspect_ratios_reasonable() {
        let sizes = [100u64; 16];
        let out = squarify(&sizes, FRect::new(0.0, 0.0, 400.0, 400.0));
        for (_, r) in &out {
            let ratio = (r.width() / r.height()).max(r.height() / r.width());
            assert!(ratio < 2.01, "ratio {ratio}");
        }
    }

    #[test]
    fn pixel_ownership_partitions_shared_edges() {
        let left = FRect::new(0.0, 0.0, 10.4, 5.0);
        let right = FRect::new(10.4, 0.0, 20.0, 5.0);
        for px in 0..20 {
            assert!(
                left.owns_pixel(px, 2) ^ right.owns_pixel(px, 2),
                "pixel {px}"
            );
        }
    }

    #[test]
    fn render_fills_every_pixel_and_hit_tests_to_files() {
        let tree = sample();
        let out = render(&tree, ROOT, 200, 120, RenderOptions::default());
        assert_eq!(out.rgba.len(), 200 * 120 * 4);
        for y in 0..120 {
            for x in 0..200 {
                let hit = out
                    .layout
                    .hit_test(&tree, x, y)
                    .expect("every pixel is covered");
                assert_eq!(tree.node(hit).kind, NodeKind::File, "pixel {x},{y}");
                let o = ((y * 200 + x) * 4) as usize;
                assert_ne!(
                    &out.rgba[o..o + 3],
                    &EMPTY_BG,
                    "pixel {x},{y} left unpainted"
                );
            }
        }
        assert!(out.layout.hit_test(&tree, 200, 0).is_none());
    }

    #[test]
    fn render_areas_follow_file_sizes() {
        let tree = sample();
        let out = render(&tree, ROOT, 400, 250, RenderOptions::default());
        let b = tree.nodes.iter().position(|n| &*n.name == "b.mov").unwrap() as NodeId;
        let r = out.layout.rect(b).unwrap();
        let expected = 600.0 / 1000.0 * 400.0 * 250.0;
        assert!((r.width() * r.height() - expected).abs() < 1e-6);
    }

    #[test]
    fn highlight_dims_other_extensions() {
        let tree = sample();
        let jpg = tree.exts.iter().position(|e| &*e.name == "jpg").unwrap() as ExtId;
        let plain = render(
            &tree,
            ROOT,
            100,
            100,
            RenderOptions {
                cushion: false,
                ..Default::default()
            },
        );
        let lit = render(
            &tree,
            ROOT,
            100,
            100,
            RenderOptions {
                cushion: false,
                highlight_ext: Some(jpg),
                ..Default::default()
            },
        );
        let b = tree.nodes.iter().position(|n| &*n.name == "b.mov").unwrap() as NodeId;
        let d = tree.nodes.iter().position(|n| &*n.name == "d.jpg").unwrap() as NodeId;
        let pixel = |img: &Rendered, id: NodeId| {
            let (x0, y0, _, _) = img.layout.rect(id).unwrap().pixels();
            let o = ((y0 as usize) * 100 + x0 as usize) * 4;
            [img.rgba[o], img.rgba[o + 1], img.rgba[o + 2]]
        };
        assert_eq!(pixel(&plain, d), pixel(&lit, d));
        let sum = |c: [u8; 3]| c.iter().map(|&v| u32::from(v)).sum::<u32>();
        assert!(sum(pixel(&lit, b)) < sum(pixel(&plain, b)) / 2);
    }

    #[test]
    fn colors_are_equalized_and_normalized() {
        for hue in EXT_HUES {
            let c = make_bright(hue, PALETTE_BRIGHTNESS);
            let avg = (u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])) as f64 / 3.0 / 255.0;
            assert!((avg - PALETTE_BRIGHTNESS).abs() < 0.03, "{hue:?} -> {c:?}");
        }
        assert_eq!(normalize_color([400, 100, 100]), [255, 172, 172]);
    }

    #[test]
    fn cushion_lights_the_upper_left() {
        // A single ridged rectangle should be brighter toward the light.
        let rect = FRect::new(0.0, 0.0, 100.0, 100.0);
        let mut s = [0.0; 4];
        add_ridge(&rect, &mut s, HEIGHT);
        let norm = (LIGHT[0].powi(2) + LIGHT[1].powi(2) + LIGHT[2].powi(2)).sqrt();
        let light = LIGHT.map(|v| v / norm);
        let at = |x: f64, y: f64| {
            let nx = -(2.0 * s[0] * x + s[2]);
            let ny = -(2.0 * s[1] * y + s[3]);
            shade([150, 150, 150], nx, ny, light)[0]
        };
        assert!(at(20.0, 20.0) > at(80.0, 80.0));
    }
}
