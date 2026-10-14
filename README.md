# MacDirStat

[![CI](https://github.com/andrewcolumbus/macdirstat/actions/workflows/ci.yml/badge.svg)](https://github.com/andrewcolumbus/macdirstat/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/andrewcolumbus/macdirstat)](https://github.com/andrewcolumbus/macdirstat/releases/latest)
[![macOS 11+](https://img.shields.io/badge/macOS-11%2B-blue?logo=apple)](#install)
[![License: MIT](https://img.shields.io/github/license/andrewcolumbus/macdirstat)](LICENSE)

A disk usage viewer for macOS, modeled on WinDirStat and written in Rust with
[egui](https://github.com/emilk/egui). It draws every file as a rectangle
sized by the space it takes and colored by file type, with WinDirStat's
cushion shading so you can see the folder structure.

<p align="center">
  <a href="https://github.com/andrewcolumbus/macdirstat/releases/latest/download/MacDirStat.dmg">
    <img src="docs/download.svg" alt="Download MacDirStat for macOS" width="352">
  </a>
</p>

<p align="center">
  <img src="docs/treemap.jpg" alt="Cushion treemap of a home folder" width="800">
</p>

## Install

Download `MacDirStat.dmg` from the
[latest release](https://github.com/andrewcolumbus/macdirstat/releases/latest),
open it and drag MacDirStat to Applications. The app runs natively on Apple
silicon and Intel Macs with macOS 11 or later.

The release build isn't notarized, so macOS blocks it the first time you open
it. Click Done, then go to System Settings > Privacy & Security, scroll down
and click Open Anyway. Or clear the quarantine flag from Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/MacDirStat.app
```

On first launch MacDirStat opens System Settings so you can give it
[Full Disk Access](#full-disk-access). macOS ties that grant to the app's
signature, and each release build has a new one, so turn Full Disk Access
back on after you update.

## Features

- A squarified treemap with WinDirStat's cushion shading. It renders on a
  background thread, so resizing the window doesn't block the UI.
- A directory list you can sort by name, size, item count or modification
  date, with subtree bars and percentages. Clicking a rectangle expands the
  list to that file.
- An extension list showing how much space each file type takes, in its
  treemap color. Click an extension to highlight its files.
- Whole-disk accounting. When you scan a volume such as `/`, the treemap gets
  entries for `<Free Space>`, the other APFS volumes in the container
  (Preboot, Recovery, VM and others) and `<Unknown>` for used space the scan
  couldn't attribute. The total then matches what Finder reports. The View
  menu can hide these entries.
- A parallel scanner that lists folders with `getattrlistbulk(2)`, which
  returns a few hundred entries per system call instead of one. Hard links
  count once, symlinks aren't followed, and firmlinked paths on the startup
  disk aren't counted twice.
- Reveal in Finder, Open, Copy Path, Zoom and Move to Trash, from the
  right-click menu on any rectangle or row. Move to Trash asks first.
- A choice of on-disk (allocated) or logical sizes.

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

Without Full Disk Access, macOS hides Mail, Messages, Safari, Photos
libraries, other apps' containers and Time Machine data from MacDirStat. Apps
can't request this permission, so MacDirStat opens the right page of System
Settings instead. Turn MacDirStat on there and rescan. Space MacDirStat can't
read is counted under `<Unknown>`. A few root-owned system folders, such as
`/private/var/db`, stay unreadable even with Full Disk Access.

## Why is my disk full when the folders add up to less?

Scan `/` and look at the entries in angle brackets:

- `<Preboot volume>`, `<Recovery volume>` and `<VM volume>` are APFS volumes
  that share the container with your data. The VM volume holds swap files and
  grows when memory runs short.
- `<Unknown>` is used space the scan couldn't see: protected folders (Full
  Disk Access shrinks this), root-only system folders, local Time Machine
  snapshots and file system metadata.
- `<Free Space>` is what's left.

## Building from source

You need Rust 1.95 or newer on macOS 11 or later.

```bash
cargo run --release     # run from source
scripts/bundle.sh       # build target/release/MacDirStat.app
```

`bundle.sh` signs the app with the first "Developer ID Application" identity
in your keychain, using the hardened runtime. If it finds none, it signs ad
hoc. macOS ties the Full Disk Access grant to the signature, so a stable
Developer ID keeps the grant across rebuilds.

The app icon is drawn by `src/icon.rs`. With Xcode 26 or later installed,
`bundle.sh` compiles a layered Icon Composer version into `Assets.car`, which
macOS 26 and later render with Liquid Glass in the default, dark, clear and
tinted styles. Without Xcode the app gets only the flat `.icns`, which older
systems use too.

These environment variables change what it builds:

| Variable | Effect |
|----------|--------|
| `MACDIRSTAT_SIGN_IDENTITY` | Sign with this identity instead |
| `MACDIRSTAT_UNIVERSAL=1` | Build for arm64 and x86_64 and merge the two with `lipo` |
| `MACDIRSTAT_DMG=1` | Also package `target/release/MacDirStat.dmg` |
| `MACDIRSTAT_NOTARIZE=1` | Notarize and staple the app, and the DMG if there is one |

Notarizing needs Apple credentials. Store them once in the keychain:

```bash
xcrun notarytool store-credentials macdirstat-notary --apple-id <apple id> --team-id <team id>
MACDIRSTAT_NOTARIZE=1 MACDIRSTAT_DMG=1 scripts/bundle.sh
```

There are also two example programs:

```bash
cargo run --release --example bench_scan -- ~              # time a scan
cargo run --release --example render_treemap -- ~ out.png  # render a treemap without the UI
```

`MACDIRSTAT_THREADS` sets the walker thread count, `MACDIRSTAT_BACKEND=std`
lists folders with `readdir` and `lstat` instead of `getattrlistbulk`, and
`MACDIRSTAT_TIMING=1` prints how long each scan phase took.

### Scan speed

The first scan in a while is slower. macOS asks a system service whether
MacDirStat may read each folder in `~/Library/Containers`. Each check takes
about 15 ms and they run one at a time, whatever the thread count, so a home
folder with hundreds of app containers adds several seconds. The system
caches the answers, so later scans are much faster.

## Releasing

Bump `version` in `Cargo.toml`, commit, then push a matching `vX.Y.Z` tag. The
[release workflow](.github/workflows/release.yml) builds a universal
`MacDirStat.dmg`, signed ad hoc, and publishes it as a GitHub release.

## Credits

The treemap layout and cushion shading follow
[WinDirStat](https://windirstat.net/). The interface uses the system Lucida
Grande font, with [Noto Sans](https://notofonts.github.io/) (SIL Open Font
License, see `assets/fonts/NotoSans-OFL.txt`) for other scripts.

## License

MIT, see [LICENSE](LICENSE).
