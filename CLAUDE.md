# CLAUDE.md

Guidance for Claude Code and Codex. `agents.md` is a symlink to this file;
keep it that way and edit only this file.

## Project

MacDirStat is a WinDirStat-style disk usage treemap for macOS, built with
Rust and egui/eframe 0.36, with a dark theme (Lucida Grande, gray 28/30
surfaces, blue selection, green readouts).

| Module | Role |
|--------|------|
| `src/scan.rs` | Parallel folder walker (rayon), hard-link dedupe, firmlink handling for `/`, synthetic disk accounting entries |
| `src/bulk.rs` | `getattrlistbulk(2)` folder listing; `scan.rs` falls back to `readdir` + `lstat` |
| `src/tree.rs` | Flat arena tree, aggregation, sorting, extension stats, synthetic entries, removal |
| `src/treemap.rs` | Squarified layout, cushion shading, parallel rasterization, colors |
| `src/worker.rs` | Background treemap rendering thread (newest request wins) |
| `src/volumes.rs` | Mounted volumes, capacity, APFS container accounting (`diskutil apfs list`) |
| `src/access.rs` | Full Disk Access detection and the Settings deep link |
| `src/app.rs` | eframe app: toolbar, lists, treemap canvas, modals, shortcuts |
| `src/theme.rs` | Dark palette, fonts, spacing, panel headers |
| `src/icon.rs` | App icon: Icon Composer `.icon` source (Liquid Glass, via actool) and the flat legacy-grid render; minimal PNG encoder |

## Commands

```bash
cargo fmt --check                       # formatter (CI)
cargo clippy --all-targets -- -D warnings
cargo test
cargo run --release
scripts/bundle.sh                       # target/release/MacDirStat.app, Developer ID signed when available
MACDIRSTAT_UNIVERSAL=1 MACDIRSTAT_DMG=1 scripts/bundle.sh   # universal app + MacDirStat.dmg
cargo run --release --example bench_scan -- <path>
cargo run --release --example render_treemap -- <path> out.png
```

Run fmt, clippy and tests before committing. CI (`.github/workflows/ci.yml`)
runs the same three on macOS.

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`: it builds a
universal `MacDirStat.dmg` (ad-hoc signed, not notarized) and publishes a
GitHub release. The tag must match the `Cargo.toml` version. The README
download button links to `releases/latest/download/MacDirStat.dmg`, so keep
that asset name.

## Rules

- App name: **MacDirStat**. The project carries no company branding.
- Version lives in `Cargo.toml`; bump it per meaningful commit (semver) and tag `vX.Y.Z`.
- Do not bundle Apple fonts in the repo. Lucida Grande is loaded from
  `/System/Library/Fonts/LucidaGrande.ttc` at runtime.
- Unicode glyphs outside Lucida Grande/Noto Sans render as boxes; paint
  arrows and triangles with the painter instead.
- `st_dev` is a signed 32-bit value on macOS: compare devices through
  `dev_of` (`as u32 as u64`), matching what `getattrlistbulk` returns.
- A folder's zero entry count from `getattrlistbulk` is only trusted on
  writable APFS and after an `lstat` link-count check: the sealed system
  volume's firmlink stubs and MobileAsset graft points report zero entries.
- The bundled app must not set a runtime icon: eframe's `setApplicationIconImage`
  would replace the Liquid Glass Dock icon with a flat bitmap (`icon::in_app_bundle`).
  CI fails if `Assets.car` is missing from the bundle.
- Full Disk Access cannot be requested programmatically; the grant is tied to
  the code signature, so keep a stable Developer ID signature for testing.
