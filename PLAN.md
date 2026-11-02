# Spacee: plan

A native macOS disk-usage visualizer: scan a volume or folder, show it as an interactive icicle chart, drill in, find the big stuff, and trash it.

Stack: Rust (1.97), [GPUI](https://docs.rs/gpui) for rendering, [gpui-kit](https://github.com/longbridge/gpui-kit) (`gpui-kit = "=0.8.0"`) for the chrome around it (sidebar, lists, buttons, dialogs, notifications, theme). gpui-kit pins its own GPUI version and re-exports it, so we depend on gpui-kit only and never on `gpui` directly. That keeps the two from drifting apart.

---

## 1. What v1 does

1. **Volumes screen.** The startup disk gets a large panel: free space as a big number, a bar and a status (Plenty of space, Filling up from 75% used, Almost full above 90%), and buttons to scan the disk or the home folder. Other volumes sit below in compact rows grouped as Drives, Disk images and Network, each with a bar, free space and a Scan button. A disk image is told apart by Disk Arbitration's device model "Disk Image"; network shares (SMB, NFS, AFP, WebDAV) are listed with their host and a note that scanning them is slow. You can also drop a folder onto the window to scan it.
2. **Scan.** A fast parallel scan that streams progress (files counted, bytes counted, current path). The chart grows while the scan runs.
3. **Icicle view.** A bar for the current folder across the top, showing its name and size, with one row per directory level below it. Each block's width is proportional to its size, and a row's blocks fill it from left to right. Blocks big enough to hold one carry a label with the name, size and %.
   - Hovering a block highlights it and shows its name, size, file count and % in the pill above the chart.
   - Clicking a directory zooms into it with an animation. Clicking the root bar, pressing Esc or using the breadcrumb goes back up.
   - Items too small to draw are merged into one grey "smaller objects" block per directory.
4. **Side list.** The current folder's children sorted by size, each with a size bar. It stays in sync with the chart on hover and click.
5. **Trash tray.** Drag a block or a list row into a bottom tray, or right-click it and choose Mark for Trash. Marking a folder drops anything already marked inside it. "Move to Trash" moves everything in the tray to the Trash after a confirmation dialog. Tree sizes then update in place, with no rescan. Items that fail to move stay in the tray with the error beside them.
6. **Actions.** Right-click menu with Reveal in Finder, Quick Look and Copy Path. Space bar opens Quick Look on the hovered item, or on the current folder when nothing is hovered.
7. **Unaccounted space.** When the scan root is a volume's mount point, the gap between what the volume reports as used (total minus available, as the volumes card shows it) and what the walk found becomes a grey "Unaccounted space" item under the root: other volumes in the APFS container (Preboot, Recovery, VM), snapshots, purgeable space, and folders the app couldn't read. It can't be revealed or collected. The scan view says how many items couldn't be read, and when the app lacks Full Disk Access, both that bar and the volumes screen offer a button that opens the Full Disk Access pane in System Settings. Purgeable space and snapshots aren't split out yet.
8. **Settings view.** Opened from the sidebar and with ⌘,. Appearance is Dark, Light or System, and System follows macOS live. The choice is saved in `~/Library/Application Support/com.rlejnieks.spacee/settings.json` and applied before the first frame, so the window never flashes the wrong theme.

Out of scope for v1: duplicate finder, scheduled scans, fast network scanning (shares are listed and scan through the normal walk), the App Store (sandboxing makes a whole-disk scanner close to impossible there).

---

## 2. Architecture

A Cargo workspace with three crates. All the hard logic lives in crates that have no UI, so it can be tested without a window.

```
spacee/
├── Cargo.toml              # workspace
├── crates/
│   ├── scan/               # filesystem walk → Tree. No gpui.
│   ├── icicle/             # Tree + view state → block geometry, hit testing. No gpui.
│   └── app/                # gpui-kit app: windows, views, painting, actions
```

### 2.1 `scan`: the data model

The tree is a flat arena, not boxed nodes. This makes it cache-friendly, cheap to send between threads, and easy to update after a delete.

```rust
pub struct NodeId(u32);

pub struct Node {
    pub name: Box<str>,          // file name only; the full path is rebuilt by walking parents
    pub parent: Option<NodeId>,
    pub kind: Kind,              // File | Dir | Symlink | Package (.app etc.)
    pub size: u64,               // allocated bytes (st_blocks * 512), summed for dirs
    pub file_count: u32,
    pub children: Vec<NodeId>,   // sorted by size desc once the scan finishes
}

pub struct Tree { pub nodes: Vec<Node>, pub root: NodeId, pub errors: Vec<ScanError> }
```

How the walk works:
- **Parallel walk** on rayon, listing each directory with `getattrlistbulk(2)`, which returns a batch of entries with type, flags, file ID, link count and allocated size in one syscall, where `readdir` needs an `lstat` per entry. Measured warm against the `readdir` + `lstat` walker it replaced: `~` (2.3M files) 12.6–12.9 s against 16.7–17.7 s, with system CPU 65–67 s against 91–95 s; `/` 17.8 s against 24.0 s. Totals match apart from files that changed between runs. A mount point now counts as the folder it covers (a few KiB less on `/`), since that is what `getattrlistbulk` reports for it. Cold-cache runs weren't measured, since `purge` needs root.
- **Count allocated size, not logical size.** Use `st_blocks * 512`. Sparse files and compressed files would be wrong otherwise.
- **Count hard links once** by deduplicating on `(dev, ino)` for files with `nlink > 1`.
- **Stay on one device.** Compare `st_dev` and don't cross mount points.
- **Firmlinks and nested mounts.** On macOS, `/` and `/System/Volumes/Data` are stitched together with firmlinks (`/usr/share/firmlinks`), and both volumes report the same `st_dev`, so the device check alone doesn't stop the walk. A naive scan of `/` walked the Data volume twice, once through the firmlinks (`/Users`, `/Applications`, …) and once through `/System/Volumes/Data`: 829 GiB on a disk with 460 GiB used. The walk now also skips every mount point below the root (from `getmntinfo`), so the Data volume is reached only through the firmlinks. Its few top-level folders outside them (`.Spotlight-V100`, `.fseventsd`, `MobileSoftwareUpdate`) are root-only and land in hidden space.
- **Don't follow symlinks.** Record them as zero-size leaves.
- **Permission errors** go into `errors` and are not fatal. The UI shows a count ("1,204 items couldn't be read") and offers to grant Full Disk Access.
- **Don't descend into cloud-only folders.** iCloud Drive and other File Provider folders can be "dataless" (`SF_DATALESS` in `st_flags`): their contents live only in the cloud. Listing one makes macOS fetch it over the network, which blocked for 3–10 s per folder in `~/Library/Mobile Documents`. They take no local space, so the walk records them without listing them.
- **Progress.** The walker updates a shared `Progress` (atomic file and byte counters, the most recent directory, a cancel flag) that the UI reads every 100 ms. The walk checks the flag before each directory and each entry, and a cancelled scan returns `ErrorKind::Interrupted`. Measured on `~` (2.3M files): cancel stops in 2–25 ms.
- **Growing chart.** The walk builds subtrees bottom-up, so the finished tree only exists at the end. To draw the scan while it runs, the walker also registers every directory down to depth 5 (the chart's row count) in `Progress`, each with an atomic byte counter, and adds every entry's bytes to its nearest registered ancestor. `Progress::snapshot()` sums those counters into a `Tree` of folders, which the app lays out and paints on each 100 ms progress tick. On `/` that is about 22k folders and takes about 1 ms, and the walk's time is unchanged (23.8 s with it against 23.4–25.5 s without).

### 2.2 `icicle`: pure geometry

Input: `&Tree`, the focused `NodeId`, the max row depth (default 5), and the minimum block width (0.6/360, about 0.17% of the focused folder).
Output: a `Layout` with one `Vec<Block>` per row, each sorted by `x`, where

```rust
pub enum Target { Node(NodeId), Smaller { parent: NodeId } }   // Smaller = "smaller objects" aggregate

pub struct Block {
    pub target: Target,
    pub depth: usize,           // 0 = the row under the root bar
    pub x: f32, pub width: f32, // fractions of the focused folder's width
}
```

- **Layout.** Recurse from the focus. A child's width is `parent_width * child.size / parent.size`. Children below the minimum width are merged into one aggregate block.
- **Color.** Each block is coloured by what fills it: video, audio, images, documents, archives and disk images, code and builds, apps, or other. A file's type comes from its extension. A folder takes the type with the most bytes inside it, except bundles such as `.app` and `.photoslibrary`, which count as a whole. A legend under the chart lists the types on screen. Rows fade with depth, and files are duller than directories. While a scan runs the chart is slate grey, since the live snapshot sees only folders. (Earlier versions coloured blocks by their position, which showed where a subtree sat but said nothing about it.)
- **Hit testing.** Do it with math, not per-shape checks. The root bar is checked first. Below it, the row index is the cursor's distance from the top of the rows divided by the row height, and its horizontal position as a fraction of the chart's width picks the block: `block_at` binary-searches that row's blocks by `x`. This costs O(log n) per mouse move.
- **Zoom transitions.** `zoom(tree, from, to, t)` moves a camera: the visible horizontal range (from the whole width, `0..1`, down to the descendant's span) and the row offset interpolate between the two foci (one is always an ancestor of the other, so the path between them fixes the geometry). Blocks in both layouts move continuously, the rest fade, and anything pushed outside the chart is clipped. The app runs `t` from 0 to 1 over 350 ms with a cubic ease-out.

This crate is the best place for unit tests: widths sum to 1 per row, the aggregate threshold works, hit tests land on the right node, and interpolation endpoints equal the inputs.

### 2.3 `app`: the gpui-kit UI

```
main.rs        app entry, window, sidebar, volumes screen, scan progress with the live chart, key bindings
browser.rs     a finished scan: breadcrumb and hover pill, icicle, children list, Trash tray, zoom
chart.rs       chart geometry in pixels, block painting, labels, file-type colours
volumes.rs     mounted volumes and how full they are
settings.rs    appearance setting, saved as JSON
style.rs       palette and shared widgets (cards, tags, buttons, bars)
platform.rs    trash, reveal, quicklook, FDA check (macOS-only FFI)
```

- **Chart rendering.** Use GPUI's `canvas(prepaint, paint)` element. The browser keeps the `Layout` from the `icicle` crate and rebuilds it only when the focus or tree changes. In `paint`, each block is one `window.paint_quad`: rounded corners, a vertical gradient that lights it a little from the top, and a 1 px border that brightens on hover. The 3 px gap between blocks comes from shrinking each block rather than drawing strokes. The root bar is painted the same way across the top. Labels are ordinary elements positioned over the canvas, and only blocks big enough to hold one get a label. `SPACEE_PERF` prints the block count and paint time per frame.
- **Mouse.** Wrap the canvas in a `div` with `on_mouse_move` and `on_click`. Turn the event position into canvas-local coordinates and hit-test through the `icicle` crate.
- **Threading.** Run the scan on a background thread (`cx.background_spawn`). A foreground task drains the channel, updates the `AppState` entity, and calls `cx.notify()` at most ~30 times per second.
- **Theme.** gpui-kit's theme provides dark and light modes, following the system setting. Chart colors come from our own palette, tuned for each mode.

### 2.4 macOS integration (`platform.rs`)

| Need | How |
|---|---|
| Move to Trash | `NSFileManager.trashItemAtURL` via `objc2-foundation` (recoverable; never `remove_dir_all`) |
| Reveal in Finder | `open -R <path>` (`NSWorkspace.activateFileViewerSelectingURLs` if it ever needs more than one item) |
| Quick Look | `qlmanage -p <path>` to start; a real `QLPreviewPanel` later |
| Volumes | `getmntinfo` / `statfs` (or the `sysinfo` crate's `Disks`) |
| Full Disk Access | Can't be requested from code. Detect it by trying to read a TCC-protected path (e.g. `~/Library/Safari`), then open `x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles` |
| Bundle | `cargo-packager` → `.app`, signed with the Apple Development cert (by hash, see §5) on every build so the Full Disk Access grant persists; Developer ID + notarization + `.dmg` when shared |

---

## 3. Milestones

Each milestone ends in something you can run and check.

| # | Deliverable | How to verify |
|---|---|---|
| 0 | Workspace skeleton; gpui-kit window opens with an empty sidebar and a Hello view; `scripts/run.sh` builds, bundles and signs the `.app` | `scripts/run.sh` opens the signed app; `codesign -dv` shows your team ID |
| 1 | `scan` crate + a CLI (`cargo run -p scan -- ~/Downloads`) that prints the top 20 entries | Totals within ~1% of `du -sk`; hard-link and symlink tests pass on a temp-dir fixture |
| 2 | Static chart of a finished scan; hover tooltip | Visual check; `icicle` unit tests; frame time measured with ~3k blocks |
| 3 | Zoom in/out with animation, breadcrumb, side list kept in sync | Click through `~/Library` and back without flicker or wrong blocks |
| 4 | Streaming scan with progress UI and cancel; volumes screen; settings view with Dark / Light / System | Scanning shows live file and byte counts (the growing chart moved to milestone 7); cancel stops within ~100 ms; appearance choice survives a relaunch, and System flips live when macOS changes |
| 5 | Trash tray + in-place size update; reveal / Quick Look | Trash a test folder → it appears in Finder's Trash and parent sizes drop by exactly its size |
| 6 | Boot-volume correctness: firmlinks, unaccounted space, FDA flow | Scanned total + unaccounted ≈ `df` used for `/` |
| 7 | Polish: colors, typography, empty and error states, chart that grows during the scan, app icon, `.dmg` | Screenshots reviewed in dark and light |
| 8 (opt.) | `getattrlistbulk` walker | Benchmark vs. the `readdir` walker on a full home-dir scan, run cold and warm |

---

## 4. Risks

- **GPUI API churn.** GPUI isn't stable, and its APIs change between Zed releases. Mitigation: depend only on gpui-kit, pin the exact version, and keep the GPUI-touching code thin (geometry stays in `icicle`).
- **Paint performance.** Every block is a gradient quad with a border. If frame time with thousands of blocks on screen is poor, fall back to rendering the chart into a cached image and redrawing only the hover highlight.
- **Scan accuracy on APFS.** Clones and snapshots mean "size" is ambiguous: two cloned files report their full size each but share blocks. v1 documents it rather than solving it.
- **Full Disk Access UX.** Without it, large parts of `~/Library` are unreadable. The app has to explain this clearly, not just show smaller numbers.

---

## 5. Decisions

1. **Visualization: icicle.** No treemap.
2. **Deletion is in v1.** The Trash tray ships in milestone 5. Everything goes through the Trash, never permanent deletion.
3. **Distribution: personal use for now, signed with the existing Apple Developer account.** Sign every build from milestone 0, not ad-hoc. macOS ties the Full Disk Access grant to the app's signing identity. An ad-hoc signature changes on every rebuild, so you'd have to grant Full Disk Access again after each `cargo build`. With a stable bundle ID (`com.rlejnieks.spacee`) and team ID, the grant survives rebuilds.
   - Identity: an **Apple Development** certificate, passed by SHA-1 hash in `SPACEE_SIGN_IDENTITY`. Sign by hash, not by name, or `codesign` stops when the keychain holds two certificates with the same name.
   - When it's shared, there are two routes:
     - **Direct download:** create a **Developer ID Application** certificate (developer.apple.com → Certificates), then notarize and build the `.dmg`. No sandbox, and Full Disk Access works as in development.
     - **TestFlight / App Store** (as with readerr): requires App Sandbox. The app can then only read folders the user picks in an open panel, kept across launches with security-scoped bookmarks. Upload with an Apple Distribution + Mac Installer Distribution certificate and a provisioning profile through `xcrun altool` or Transporter, since there's no Xcode project to do it automatically.
   - To keep the sandbox route open at no cost: start every scan from an `NSOpenPanel` choice or a volume the user picked, never from a hard-coded path like `~` or `/`.
