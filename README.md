# Spacee

A native macOS app that shows what's using your disk as an interactive sunburst,. Built in Rust with [GPUI](https://www.gpui.rs/) and [gpui-kit](https://github.com/longbridge/gpui-kit).

Pick a volume or a folder (or drop one onto the window) and Spacee scans it, then draws each subfolder as a ring segment sized by the disk space it takes. Hover a segment to see its name and size, click a folder to zoom in, and click the center to go back up.

To free space, drag segments or list rows into the collector tray at the bottom, or right-click them and choose Add to Collector, then press Move to Trash. Spacee only ever moves items to the Trash, never deletes them, and updates the sizes in place without a rescan. The right-click menu also has Reveal in Finder, Quick Look and Copy Path, and the space bar opens Quick Look on the hovered item.

The roadmap and design live in [PLAN.md](PLAN.md).

## Requirements

- macOS 13 or newer
- Rust 1.88 or newer (`rustup update stable`)
- Xcode, not just the Command Line Tools: GPUI compiles its Metal shaders at build time
- A code-signing identity for the bundled app (see [Signing](#signing))

## Run

```sh
scripts/run.sh                    # debug build: build, bundle, sign, open
PROFILE=release scripts/run.sh    # optimized build
scripts/run.sh --no-open          # build and sign without launching
```

The bundle is written to `target/<profile>/Spacee.app`.

To open straight into a scan of a folder, run the binary inside the bundle with the path:

```sh
target/release/Spacee.app/Contents/MacOS/spacee ~/Downloads
```

Set `SPACEE_PERF=1` to log how long each sunburst paint takes.

Settings (⌘,) has the Dark / Light / System appearance choice. It is saved in `~/Library/Application Support/com.rlejnieks.spacee/settings.json`.

## Install

Build a release bundle and copy it into Applications:

```sh
PROFILE=release scripts/run.sh --no-open
cp -R target/release/Spacee.app /Applications/
```

### Full Disk Access

Without Full Disk Access, macOS hides parts of `~/Library` and other protected folders, and the totals come out smaller than reality. To grant it, open System Settings → Privacy & Security → Full Disk Access, add `Spacee.app`, and relaunch it.

The grant is tied to the app's signing identity. Builds signed with the same identity keep it; an ad-hoc signed build loses it on every rebuild.

## Signing

`scripts/run.sh` signs the bundle with the identity in `SPACEE_SIGN_IDENTITY`. It defaults to the maintainer's Apple Development certificate, so on any other machine set your own:

```sh
security find-identity -v -p codesigning     # list identities; use the 40-character hash
SPACEE_SIGN_IDENTITY=<hash> scripts/run.sh
```

Sign by hash rather than by name: if the keychain holds two certificates with the same name, `codesign` refuses the ambiguous name. With no certificate at all, `SPACEE_SIGN_IDENTITY=-` signs ad hoc, at the cost of re-granting Full Disk Access after each build.

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

The workspace has three crates:

- `crates/scan` walks the filesystem in parallel and builds the size tree. It counts allocated bytes, counts hard links once, and does not follow symlinks or cross onto other volumes.
- `crates/sunburst` turns the tree into ring geometry, merges slivers into "smaller objects", hit-tests the pointer, and computes zoom animation frames. It has no UI code.
- `crates/app` is the GPUI app: the window, the painted sunburst, the breadcrumb and the side list.
