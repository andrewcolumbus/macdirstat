//! The application icon: a flat squarified treemap on a dark tile.
//!
//! macOS 26 and later draw the layered Icon Composer document from
//! `write_icon_source`, compiled into Assets.car, with Liquid Glass lighting
//! and the default, dark, clear and tinted appearances. Older systems and
//! unbundled runs use the flat `render`, drawn on Apple's legacy 824/1024 grid
//! so the Dock does not put it on a gray plate.

use std::path::Path;

use crate::treemap::{self, FRect};

/// Icon Composer canvas size in points. The system masks it with the squircle.
const CANVAS: f64 = 1024.0;
/// Treemap bounds on the canvas, clear of the mask and the glass rim.
const CONTENT: FRect = FRect {
    x0: 172.0,
    y0: 172.0,
    x1: 852.0,
    y1: 852.0,
};
const GAP: f64 = 28.0;
const TILE_RADIUS: f64 = 40.0;
/// Relative tile sizes, largest first; colors follow the extension ranking.
const SIZES: [u64; 6] = [34, 22, 16, 12, 9, 8];
const BACKGROUND: [u8; 3] = [30, 30, 30];
/// Legacy grid: an 824 px squircle centered in the 1024 px image.
const BODY_INSET: f64 = 100.0;
const BODY_RADIUS: f64 = 185.4;
const SHADOW_OFFSET: f64 = 12.0;
const SHADOW_BLUR: f64 = 28.0;
const SHADOW_ALPHA: f64 = 0.35;

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub rect: FRect,
    pub color: [u8; 3],
}

/// The treemap tiles in canvas coordinates, `GAP` apart, with their outer
/// edges on `CONTENT`.
pub fn tiles() -> Vec<Tile> {
    let half = GAP / 2.0;
    let grown = FRect::new(
        CONTENT.x0 - half,
        CONTENT.y0 - half,
        CONTENT.x1 + half,
        CONTENT.y1 + half,
    );
    treemap::squarify(&SIZES, grown)
        .into_iter()
        .map(|(i, r)| Tile {
            rect: FRect::new(r.x0 + half, r.y0 + half, r.x1 - half, r.y1 - half),
            color: treemap::ext_color(i as u32),
        })
        .collect()
}

/// Write an Icon Composer document (a `.icon` folder) for `actool`: one SVG
/// layer per tile in a single lit group over a solid background.
pub fn write_icon_source(dir: &Path) -> std::io::Result<()> {
    let assets = dir.join("Assets");
    std::fs::create_dir_all(&assets)?;
    let tiles = tiles();
    let mut layers = Vec::with_capacity(tiles.len());
    for (i, tile) in tiles.iter().enumerate() {
        let image = format!("tile{i}.svg");
        std::fs::write(assets.join(&image), tile_svg(tile))?;
        // Tinted icons are monochrome; distinct grays keep the tiles apart.
        let gray = 1.0 - 0.08 * i as f64;
        layers.push(serde_json::json!({
            "name": format!("tile{i}"),
            "image-name": image,
            "fill-specializations": [{
                "appearance": "tinted",
                "value": { "solid": format!("gray:{gray:.5},1.00000") },
            }],
        }));
    }
    let doc = serde_json::json!({
        "fill-specializations": [
            { "value": { "solid": srgb(BACKGROUND) } },
            { "appearance": "dark", "value": "system-dark" },
        ],
        "groups": [{
            "layers": layers,
            "lighting": "individual",
            "specular": true,
            "shadow": { "kind": "neutral", "opacity": 0.5 },
            "translucency": { "enabled": false, "value": 0.5 },
        }],
        "supported-platforms": { "squares": ["macOS"] },
    });
    let json = serde_json::to_string_pretty(&doc).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("icon.json"), json)
}

fn srgb(c: [u8; 3]) -> String {
    let [r, g, b] = c.map(|v| f64::from(v) / 255.0);
    format!("srgb:{r:.5},{g:.5},{b:.5},1.00000")
}

fn tile_svg(tile: &Tile) -> String {
    let r = tile.rect;
    let [red, green, blue] = tile.color;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1024\" height=\"1024\" viewBox=\"0 0 1024 1024\">\
         <rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" rx=\"{TILE_RADIUS}\" \
         fill=\"#{red:02x}{green:02x}{blue:02x}\"/></svg>\n",
        r.x0,
        r.y0,
        r.width(),
        r.height(),
    )
}

/// Signed distance from `(x, y)` to a rounded rectangle; negative inside.
fn rounded_rect_distance(x: f64, y: f64, r: FRect, radius: f64) -> f64 {
    let qx = (x - (r.x0 + r.x1) / 2.0).abs() - (r.width() / 2.0 - radius);
    let qy = (y - (r.y0 + r.y1) / 2.0).abs() - (r.height() / 2.0 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Flat RGBA render on the legacy grid, `size` pixels square: the tiles on a
/// dark squircle with a drop shadow, each tile lit from the top like the
/// glass version.
pub fn render(size: u32) -> Vec<u8> {
    let px = CANVAS / f64::from(size);
    let body = FRect::new(
        BODY_INSET,
        BODY_INSET,
        CANVAS - BODY_INSET,
        CANVAS - BODY_INSET,
    );
    let k = body.width() / CANVAS;
    let tiles: Vec<Tile> = tiles()
        .into_iter()
        .map(|t| Tile {
            rect: FRect::new(
                body.x0 + t.rect.x0 * k,
                body.y0 + t.rect.y0 * k,
                body.x0 + t.rect.x1 * k,
                body.y0 + t.rect.y1 * k,
            ),
            ..t
        })
        .collect();
    let coverage = |d: f64| (0.5 - d / px).clamp(0.0, 1.0);

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for (i, out) in rgba.chunks_exact_mut(4).enumerate() {
        let x = (f64::from(i as u32 % size) + 0.5) * px;
        let y = (f64::from(i as u32 / size) + 0.5) * px;

        let mut rgb = BACKGROUND.map(f64::from);
        for t in &tiles {
            let d = rounded_rect_distance(x, y, t.rect, TILE_RADIUS * k);
            let a = coverage(d);
            if a <= 0.0 {
                continue;
            }
            // Brighter toward the top, with a specular rim along the edge.
            let v = (y - t.rect.y0) / t.rect.height();
            let rim = (1.0 + d / 6.0).clamp(0.0, 1.0) * (1.0 - v).powi(2) * 0.45;
            let lift = 1.08 - 0.12 * v;
            for (c, &base) in rgb.iter_mut().zip(&t.color) {
                let lit = (f64::from(base) * lift).min(255.0);
                let lit = lit + (255.0 - lit) * rim;
                *c += (lit - *c) * a;
            }
        }

        let a = coverage(rounded_rect_distance(x, y, body, BODY_RADIUS));
        let d = rounded_rect_distance(x, y - SHADOW_OFFSET, body, BODY_RADIUS);
        let shadow = SHADOW_ALPHA * (0.5 - d / SHADOW_BLUR).clamp(0.0, 1.0);
        let alpha = a + shadow * (1.0 - a);
        if alpha > 0.0 {
            for (o, c) in out.iter_mut().zip(rgb) {
                *o = (c * a / alpha).round() as u8;
            }
            out[3] = (alpha * 255.0).round() as u8;
        }
    }
    rgba
}

/// The window and Dock icon for unbundled runs (`cargo run`).
pub fn app_icon() -> egui::IconData {
    const SIZE: u32 = 512;
    egui::IconData {
        rgba: render(SIZE),
        width: SIZE,
        height: SIZE,
    }
}

/// Whether `exe` runs from inside an .app bundle. There the Dock should show
/// the bundle's own icon: eframe's runtime icon would replace the Liquid
/// Glass one with a flat bitmap.
pub fn in_app_bundle(exe: &Path) -> bool {
    exe.parent()
        .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
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
    use super::*;

    fn alpha(rgba: &[u8], size: u32, x: u32, y: u32) -> u8 {
        rgba[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn tiles_fill_the_content_box_without_touching() {
        let tiles = tiles();
        assert_eq!(tiles.len(), SIZES.len());
        let eps = 1e-6;
        let min_x = tiles.iter().map(|t| t.rect.x0).fold(f64::MAX, f64::min);
        let max_x = tiles.iter().map(|t| t.rect.x1).fold(f64::MIN, f64::max);
        let min_y = tiles.iter().map(|t| t.rect.y0).fold(f64::MAX, f64::min);
        let max_y = tiles.iter().map(|t| t.rect.y1).fold(f64::MIN, f64::max);
        assert!((min_x - CONTENT.x0).abs() < eps && (max_x - CONTENT.x1).abs() < eps);
        assert!((min_y - CONTENT.y0).abs() < eps && (max_y - CONTENT.y1).abs() < eps);
        for (i, a) in tiles.iter().enumerate() {
            assert!(a.rect.width() > 2.0 * TILE_RADIUS && a.rect.height() > 2.0 * TILE_RADIUS);
            for b in &tiles[i + 1..] {
                let apart_x =
                    a.rect.x1 + GAP <= b.rect.x0 + eps || b.rect.x1 + GAP <= a.rect.x0 + eps;
                let apart_y =
                    a.rect.y1 + GAP <= b.rect.y0 + eps || b.rect.y1 + GAP <= a.rect.y0 + eps;
                assert!(apart_x || apart_y, "{a:?} and {b:?} are closer than GAP");
            }
        }
    }

    #[test]
    fn flat_icon_fills_the_legacy_grid_squircle() {
        let size = 1024;
        let rgba = render(size);
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        // Opaque across the whole 824 px body, edge to edge.
        assert_eq!(alpha(&rgba, size, 512, 512), 255);
        assert_eq!(alpha(&rgba, size, 101, 512), 255);
        assert_eq!(alpha(&rgba, size, 922, 512), 255);
        assert_eq!(alpha(&rgba, size, 512, 101), 255);
        // Transparent outside it, apart from the soft shadow below.
        assert_eq!(alpha(&rgba, size, 0, 0), 0);
        assert_eq!(alpha(&rgba, size, 120, 120), 0);
        assert_eq!(alpha(&rgba, size, 512, 80), 0);
        let below = alpha(&rgba, size, 512, 930);
        assert!(below > 0 && below < 128, "shadow alpha {below}");
    }

    #[test]
    fn icon_source_is_an_icon_composer_document() {
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("MacDirStat.icon");
        write_icon_source(&icon).unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(icon.join("icon.json")).unwrap()).unwrap();
        assert_eq!(doc["supported-platforms"]["squares"][0], "macOS");
        assert_eq!(doc["fill-specializations"][1]["appearance"], "dark");
        let layers = doc["groups"][0]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), tiles().len());
        for layer in layers {
            let name = layer["image-name"].as_str().unwrap();
            let svg = std::fs::read_to_string(icon.join("Assets").join(name)).unwrap();
            assert!(svg.contains("viewBox=\"0 0 1024 1024\""), "{svg}");
            assert!(svg.contains("<rect "), "{svg}");
        }
    }

    #[test]
    fn bundled_runs_keep_the_bundle_icon() {
        // eframe's runtime icon would cover the Liquid Glass Dock icon.
        assert!(in_app_bundle(Path::new(
            "/Applications/MacDirStat.app/Contents/MacOS/MacDirStat"
        )));
        assert!(!in_app_bundle(Path::new("target/release/macdirstat")));
        assert!(!in_app_bundle(Path::new("/usr/local/bin/macdirstat")));
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
