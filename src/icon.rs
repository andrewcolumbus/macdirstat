//! The application icon: a small cushion treemap drawn by the app's own
//! renderer, inset on a rounded dark tile.

use std::path::PathBuf;

use crate::tree::{NO_EXT, NO_NODE, Node, NodeId, NodeKind, ROOT, SizeMetric, Tree};
use crate::treemap::{self, RenderOptions};

const SIZE: u32 = 256;
const INSET: u32 = 26;
const RADIUS: f32 = 48.0;

fn dir(tree: &mut Tree, parent: NodeId) -> NodeId {
    let id = tree.push(Node {
        name: "d".into(),
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
    });
    if parent != NO_NODE {
        tree.nodes[parent as usize].children.push(id);
    }
    id
}

fn file(tree: &mut Tree, parent: NodeId, ext: &str, size: u64) {
    let ext = tree.record_ext(ext, size, size);
    let id = tree.push(Node {
        name: "f".into(),
        parent,
        children: Vec::new(),
        logical: size,
        allocated: size,
        mtime: 0,
        files: 1,
        dirs: 0,
        ext,
        kind: NodeKind::File,
        flags: 0,
    });
    tree.nodes[parent as usize].children.push(id);
}

fn sample_tree() -> Tree {
    let mut tree = Tree::new(PathBuf::from("/"), SizeMetric::Logical);
    dir(&mut tree, NO_NODE);
    let a = dir(&mut tree, ROOT);
    file(&mut tree, a, "mov", 420);
    file(&mut tree, a, "jpg", 160);
    file(&mut tree, a, "png", 90);
    let b = dir(&mut tree, ROOT);
    file(&mut tree, b, "jpg", 220);
    file(&mut tree, b, "zip", 120);
    file(&mut tree, b, "mov", 70);
    let c = dir(&mut tree, b);
    file(&mut tree, c, "png", 60);
    file(&mut tree, c, "app", 40);
    file(&mut tree, ROOT, "zip", 150);
    file(&mut tree, ROOT, "dmg", 110);
    file(&mut tree, ROOT, "app", 80);
    tree.aggregate();
    tree.sort_all();
    tree
}

pub fn app_icon() -> egui::IconData {
    let tree = sample_tree();
    let inner = SIZE - 2 * INSET;
    let map = treemap::render(&tree, ROOT, inner, inner, RenderOptions::default());

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let half = SIZE as f32 / 2.0;
    let edge = half - 8.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Rounded-square mask with a 1 px soft edge.
            let dx = ((x as f32 + 0.5 - half).abs() - (edge - RADIUS)).max(0.0);
            let dy = ((y as f32 + 0.5 - half).abs() - (edge - RADIUS)).max(0.0);
            let dist = (dx * dx + dy * dy).sqrt() - RADIUS;
            let alpha = (0.5 - dist).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            let o = ((y * SIZE + x) * 4) as usize;
            let inside = x >= INSET && y >= INSET && x < SIZE - INSET && y < SIZE - INSET;
            let rgb = if inside {
                let m = (((y - INSET) * inner + (x - INSET)) * 4) as usize;
                [map.rgba[m], map.rgba[m + 1], map.rgba[m + 2]]
            } else {
                [30, 30, 30]
            };
            rgba[o..o + 3].copy_from_slice(&rgb);
            rgba[o + 3] = (alpha * 255.0) as u8;
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

/// Encode RGBA8 pixels as an uncompressed PNG (stored deflate blocks).
/// Used by `--export-icon` so the bundle script can build an .icns.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_has_transparent_corners_and_opaque_center() {
        let icon = super::app_icon();
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert_eq!(icon.rgba[3], 0);
        let c = ((icon.height / 2 * icon.width + icon.width / 2) * 4 + 3) as usize;
        assert_eq!(icon.rgba[c], 255);
    }

    #[test]
    fn png_encoder_writes_a_valid_header_and_chunks() {
        let png = super::encode_png(&[255, 0, 0, 255, 0, 255, 0, 128], 2, 1);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // Known CRC of an empty IEND chunk.
        assert_eq!(&png[png.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
    }
}
