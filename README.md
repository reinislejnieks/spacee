<div align="center">

# Spacee

**See what's filling your Mac's disk, and clear it out safely.**

A fast, native macOS disk-usage visualiser built in Rust with [GPUI](https://www.gpui.rs/).

[![macOS 13+](https://img.shields.io/badge/macOS-13%2B-0f141b?logo=apple&logoColor=white)](#requirements)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-0f141b?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![GPUI](https://img.shields.io/badge/UI-GPUI-4ade9b?labelColor=0f141b)](https://www.gpui.rs/)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-4ade9b?labelColor=0f141b)](LICENSE)

<img src="docs/screenshot.png" alt="Spacee showing a projects folder as an icicle chart, with its children listed by size and the Trash tray below" width="860">

</div>

## Features

- **Icicle chart.** Every folder is a block sized by the disk space it takes and coloured by what fills it: video, audio, images, documents, archives and disk images, code and builds, or apps. Click a block to zoom in; click the bar on top or press Esc to go back up.
- **Live scanning.** The chart grows while the scan runs, next to a file count, a files-per-second rate, how much has been found, and an estimate of the time left when scanning a whole volume.
- **Volumes at a glance.** The startup disk comes first with its free space and how full it is, followed by drives, disk images and network shares, each with a Scan button.
- **Safe cleanup.** Drag blocks or list rows into the Trash tray, or right-click and choose Mark for Trash, then press Move to Trash. Spacee only ever moves items to the Trash, never deletes them, and updates the sizes in place without a rescan.
- **Finder shortcuts.** The right-click menu has Reveal in Finder, Quick Look and Copy Path, and the space bar opens Quick Look on the hovered item.
- **Honest totals.** A whole-volume scan adds a grey "Unaccounted space" item for space the volume reports as used that no file accounts for, such as snapshots, purgeable space, the other system volumes in the container, and folders Spacee wasn't allowed to read.
- **Dark and light.** Follows the system appearance, or pick one in Settings (⌘,).

The roadmap and design notes live in [PLAN.md](PLAN.md).

## Requirements

- macOS 13 or newer
- Rust 1.88 or newer (`rustup update stable`)
- Xcode, not just the Command Line Tools, because GPUI compiles its Metal shaders at build time
- A code-signing identity for the bundled app (see [Signing](#signing))

## Quick start

```sh
git clone https://github.com/reinislejnieks/spacee.git
cd spacee
scripts/run.sh
```

| Command | What it does |
| --- | --- |
| `scripts/run.sh` | Debug build, bundle, sign and open |
| `PROFILE=release scripts/run.sh` | Optimised build |
| `scripts/run.sh --no-open` | Build and sign without launching |

The bundle is written to `target/<profile>/Spacee.app`. To open straight into a scan of a folder, run the binary inside the bundle with the path:

```sh
target/release/Spacee.app/Contents/MacOS/spacee ~/Downloads
```

Set `SPACEE_PERF=1` to log how long each chart paint takes. Settings are saved in `~/Library/Application Support/com.rlejnieks.spacee/settings.json`.

## Install

Build a signed release bundle and install it into Applications, replacing any earlier copy. A running Spacee is quit first:

```sh
./manage install
```

To build a disk image instead, with an Applications shortcut to drag the app onto:

```sh
scripts/dmg.sh    # writes target/release/Spacee-<version>.dmg
```

The image is signed but not notarized, so another Mac's Gatekeeper will refuse it until it's signed with a Developer ID certificate and notarized (see PLAN.md, Decisions).

### Full Disk Access

Without Full Disk Access, macOS hides parts of `~/Library` and other protected folders, and the totals come out smaller than reality. The volumes screen and the results show a Grant Full Disk Access… button while the app lacks it. To grant it, open System Settings → Privacy & Security → Full Disk Access, add `Spacee.app`, and relaunch it.

The grant is tied to the app's signing identity. Builds signed with the same identity keep it, and an ad-hoc signed build loses it on every rebuild.

## Signing

`scripts/run.sh` signs the bundle with the identity in `SPACEE_SIGN_IDENTITY`, and `./manage install` and `scripts/dmg.sh` build through it. It defaults to the maintainer's Apple Development certificate, so on any other machine set your own:

```sh
security find-identity -v -p codesigning     # list identities; use the 40-character hash
SPACEE_SIGN_IDENTITY=<hash> scripts/run.sh
```

Sign by hash rather than by name. If the keychain holds two certificates with the same name, `codesign` refuses the ambiguous name. With no certificate at all, `SPACEE_SIGN_IDENTITY=-` signs ad hoc, at the cost of re-granting Full Disk Access after each build.

## Command-line scanner

The scanner also runs on its own and prints a folder's total and its 20 largest entries:

```sh
cargo run --release -p scan -- ~/Downloads
```

Its totals match `du -skx` for the same folder.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt
```

A pre-commit hook in `.githooks` runs `cargo fmt --check` and `cargo clippy -- -D warnings` and blocks the commit if either fails. Enable it once per clone:

```sh
git config core.hooksPath .githooks
```

The app icon is a small icicle chart with light and dark versions. It lives in `crates/app/AppIcon.icon`, an Icon Composer package drawn by `scripts/icon.py` (needs Pillow). Edit and rerun it to change the icon. `scripts/run.sh` compiles it with `actool` into `Assets.car`, which macOS 26 uses to switch the icon with the appearance, and `AppIcon.icns` for older systems.

The workspace has three crates:

| Crate | Role |
| --- | --- |
| `crates/scan` | Walks the filesystem in parallel and builds the size tree. Counts allocated bytes, counts hard links once, and doesn't follow symlinks or cross onto other volumes or mount points. |
| `crates/icicle` | Turns the tree into a partition by depth and extent, merges slivers into "smaller objects", finds the block under the pointer, and computes zoom animation frames. No UI code. |
| `crates/app` | The GPUI app: the window, the painted icicle chart, the volumes and scan screens, the children list and the Trash tray. |

## License

Spacee is free software under the [GNU General Public License v3.0 or later](LICENSE). You can use, study, change and share it, including commercially. If you distribute a changed version, you must release its source under the same license.

Spacee comes with no warranty. It only ever moves items to the Trash, so anything removed by mistake can be put back from there until the Trash is emptied.
