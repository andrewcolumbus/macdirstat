# MacDirStat

A WinDirStat-style disk usage viewer for macOS, written in Rust with
[egui](https://github.com/emilk/egui).

Every file is drawn as a rectangle sized by the space it takes, colored by its
extension and shaded with WinDirStat's cushion lighting so the folder
structure shows through. Large files and folders stand out at a glance.

<p align="center">
  <img src="docs/treemap.jpg" alt="Cushion treemap of a home folder" width="800">
</p>

## Features

- **Cushion treemap**: squarified layout with WinDirStat's ridge shading,
  rendered in parallel on a background thread so the UI stays responsive
  while you resize.
- **Directory list**: sortable by name, size, items or modified date, with
  subtree bars and percentages. It stays in sync with the treemap.
- **Extension list**: space per file type, with the treemap colors. Click an
  extension to highlight its files.
- **Whole-disk accounting**: scanning a volume such as `/` adds
  `<Free Space>`, the other APFS volumes in the container (Preboot, Recovery,
  VM, ...) and `<Unknown>` for space the scan could not attribute, so the
  treemap adds up to what Finder reports as used. Both can be hidden from the
  View menu.
- **Fast multithreaded scanner**: walks folders in parallel and lists them
  with `getattrlistbulk(2)` (one system call per few hundred entries instead
  of one per file). Hard links are counted once, symlinks are not followed and
  firmlinked paths on the startup disk are not double counted.
- **Actions**: Reveal in Finder, Open, Copy Path, Zoom, and Move to Trash
  (with confirmation). Right-click any rectangle or row for these.
- **Size metric**: switch between on-disk (allocated) and logical sizes.

### Keyboard

| Shortcut | Action |
|----------|--------|
| Cmd+O | Choose a folder to scan |
| Cmd+R | Rescan |
| Return | Zoom into the selected folder |
| Esc / Backspace | Zoom out one level |
| Arrow keys | Move the selection |
| Cmd+C | Copy the selected path |
| Cmd+Backspace | Move the selection to the Trash |

## Full Disk Access

macOS hides Mail, Messages, Safari, Photos libraries, other apps' containers
and Time Machine data from apps without Full Disk Access. No API can request
it, so MacDirStat opens the right page of System Settings on first launch.
Turn on MacDirStat there, then rescan. Space MacDirStat can't read shows up
as `<Unknown>`. Some system folders (for example `/private/var/db`) belong to
root and stay unreadable even with Full Disk Access.

## Why is my disk full when the folders add up to less?

Scan `/` and look at the synthetic entries:

- `<Preboot volume>`, `<Recovery volume>`, `<VM volume>`: APFS volumes that
  share the container with your data. The VM volume holds swap files and
  grows under memory pressure.
- `<Unknown>`: space used but not visible to the scan. This covers protected
  folders (grant Full Disk Access to shrink it), root-only system folders,
  local Time Machine snapshots and file system metadata.
- `<Free Space>`: what is actually left.

## Building

Requires Rust 1.95 or newer (egui 0.36) on macOS 11 or later.

```bash
cargo run --release                   # run from source
scripts/bundle.sh                     # build target/release/MacDirStat.app
```

`bundle.sh` signs with the first "Developer ID Application" identity in your
keychain (or `MACDIRSTAT_SIGN_IDENTITY`), using the hardened runtime, and falls
back to an ad-hoc signature. A stable signature keeps the Full Disk Access
grant across rebuilds. To notarize, store credentials once and set
`MACDIRSTAT_NOTARIZE=1`:

```bash
xcrun notarytool store-credentials macdirstat-notary --apple-id <apple id> --team-id <team id>
MACDIRSTAT_NOTARIZE=1 scripts/bundle.sh
```

Other tools:

```bash
cargo run --release --example bench_scan -- ~             # time a scan
cargo run --release --example render_treemap -- ~ out.png  # render a treemap headlessly
```

Set `MACDIRSTAT_THREADS` to change the walker thread count,
`MACDIRSTAT_BACKEND=std` to list folders with `readdir` + `lstat` instead of
`getattrlistbulk`, and `MACDIRSTAT_TIMING=1` to print scan phase timings.

### Scan speed

On the first scan after a while, macOS checks access to each folder in
`~/Library/Containers` through a system service. That takes roughly 15 ms per
app container, and the checks run one at a time whatever the thread count,
so a home folder with hundreds of containers can take several extra seconds.
Later scans hit the system's cache and run much faster.

## Credits

The treemap layout and cushion shading follow
[WinDirStat](https://windirstat.net/). The interface uses the system Lucida
Grande font, with [Noto Sans](https://notofonts.github.io/) (SIL Open Font
License, see `assets/fonts/NotoSans-OFL.txt`) for other scripts.

## License

MIT, see [LICENSE](LICENSE).
