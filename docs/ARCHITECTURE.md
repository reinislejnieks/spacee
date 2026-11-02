# Architecture

Spacee is a Cargo workspace with three crates. The `scan` and `icicle` crates have no UI code, so you can test them without a window.

```
crates/
├── scan/     walks the file system and builds the size tree
├── icicle/   turns the tree into chart geometry and finds the block under the pointer
└── app/      the GPUI app: window, screens, chart painting, macOS calls
```

The app uses [gpui-kit](https://github.com/longbridge/gpui-kit) for its widgets and theme. gpui-kit pins its own GPUI version and exports it. The workspace pins gpui-kit to an exact version (`=0.8.0`) and does not use `gpui` directly. GPUI is not stable, so do not change this version without a test of the full app.

## `scan`

### Data model

The tree is a flat list of nodes. Each node refers to its parent and children by index (`NodeId`). This makes the tree fast to read and easy to change after a move to the Trash.

```rust
pub struct Node {
    pub name: Box<str>,          // file name only; Tree::path builds the full path
    pub parent: Option<NodeId>,
    pub kind: Kind,              // File, Dir, Symlink or Unaccounted
    pub file_type: FileType,     // what fills the node (see Colours)
    pub size: u64,               // allocated bytes, summed for folders
    pub file_count: u32,
    pub children: Vec<NodeId>,   // largest first
}
```

A volume's root can get one `Unaccounted` node. Its size is the space the volume reports as used minus the size the walk found. This space includes snapshots, purgeable space, the other volumes in the APFS container, and folders that the app cannot read. An `Unaccounted` node has no path on disk.

### Walk rules

- The walk runs in parallel on rayon. It lists each folder with `getattrlistbulk(2)`, which returns the type, flags, file ID, link count and allocated size of many entries in one system call.
- The size of a file is its allocated size (`st_blocks * 512`), not its logical size. This gives correct values for sparse and compressed files.
- A file with more than one hard link counts only at the first link that the walk finds.
- Symbolic links are not followed. They count as 0 bytes.
- The walk does not go into a mount point below the root (from `getmntinfo(3)`). This keeps the walk on one volume. On the startup disk, it also prevents a second count of the Data volume, which `/` reaches through firmlinks and through `/System/Volumes/Data`.
- The walk does not go into "dataless" folders (`SF_DATALESS`), such as iCloud Drive folders that are only in the cloud. If the walk lists such a folder, macOS downloads its contents, which can block for seconds. These folders use no local space.
- A folder that the walk cannot read does not stop the scan. The error goes into `Tree::errors`, and the app shows the number of these items.

### Progress and cancel

The walk updates a shared `Progress`: file and byte counters, the current folder, and a cancel flag. The walk checks the flag before each folder and each entry. A cancelled scan returns `io::ErrorKind::Interrupted`.

The walk builds the final tree from the bottom up, so the tree is complete only at the end. For the chart during a scan, the walk also records each folder to depth 5 in `Progress` with a byte counter. It adds the bytes of each entry to the nearest recorded folder above it. `Progress::snapshot()` makes a `Tree` of these folders. A snapshot has no files, so each folder has a file count of 0 and the file type `Other`.

## `icicle`

### Layout

`Layout::new(tree, focus, options)` makes one row of blocks for each folder level below the focused folder. The default is 5 rows.

```rust
pub struct Block {
    pub target: Target,        // Node(id), or Smaller { parent } for merged small items
    pub depth: usize,          // 0 = the row below the root bar
    pub x: f32, pub width: f32 // fractions of the focused folder's width
}
```

- The width of a child is `parent_width * child.size / parent.size`.
- Children narrower than the minimum width (`0.6 / 360` of the focused folder) merge into one "Smaller objects" block for each parent.
- Each row is sorted by `x`.

### Hit test

The row under the pointer is the vertical distance from the top of the rows divided by the row height. `Layout::block_at` then does a binary search of that row by `x`. Each mouse move costs O(log n).

### Zoom

`zoom(tree, from, to, t)` moves between two layouts. One focus is always an ancestor of the other. The visible range and the row offset change gradually from one focus to the other. Blocks that are in both layouts move, other blocks fade, and the chart clips blocks outside its bounds. The app runs `t` from 0 to 1 in 350 ms with a cubic ease-out.

## `app`

| File | Contents |
| --- | --- |
| `main.rs` | App start, window, sidebar, volumes screen, scan screen, key bindings |
| `browser.rs` | Results of a scan: breadcrumb, chart, folder list, Trash tray, zoom |
| `chart.rs` | Chart geometry in pixels, block paint, labels, file-type colours |
| `volumes.rs` | Mounted volumes, their type and how full they are |
| `settings.rs` | Appearance setting, saved as JSON |
| `style.rs` | Palette and shared widgets: cards, tags, buttons, bars |
| `platform.rs` | Trash, Reveal in Finder, Quick Look, Full Disk Access check |

### Chart paint

The chart is a GPUI `canvas`. The browser keeps the `Layout` and makes a new one only when the focus or the tree changes. Each block is one `paint_quad` with round corners, a vertical gradient and a 1 px border. A 3 px gap separates the blocks. Labels are usual elements on top of the canvas. Only blocks with sufficient space get a label. If you set `SPACEE_PERF=1`, the app logs the paint time for each frame.

### Colours

A file gets its type from its extension: video, audio, images, documents, archives and disk images, code and builds, apps, or other. A folder gets the type that has the most bytes in it. Bundles such as `.app` and `.photoslibrary` count as one item of their bundle type. "Smaller objects" and Unaccounted space are grey. Deeper rows are fainter. During a scan, all blocks have the colour of "Other", because the snapshot has no files.

### Threads

`cx.background_spawn` runs the scan on a background thread. A timer redraws the window every 100 ms while the scan runs. Each redraw reads the counters and a new snapshot from `Progress`. When the scan ends, the app replaces the scan screen with the results.

### macOS calls

| Function | Implementation |
| --- | --- |
| Move to Trash | `NSFileManager trashItemAtURL` through `objc2-foundation`. The app never deletes files permanently. |
| Reveal in Finder | `open -R <path>` |
| Quick Look | `qlmanage -p <path>` |
| Volumes | `getmntinfo(3)`. Disk Arbitration identifies disk images by the device model "Disk Image". Network shares are `smbfs`, `nfs`, `afpfs` and `webdav` mounts. |
| Full Disk Access | Code cannot request it. The app tries to open `/Library/Application Support/com.apple.TCC/TCC.db`. If this fails, the app shows a button that opens the Full Disk Access pane in System Settings. |
| App bundle | `scripts/run.sh` builds the binary, makes `Spacee.app`, compiles the icon with `actool` and signs the bundle. |

## Known limits

- APFS clones share blocks on disk, but each clone counts at its full size. The total for a folder with clones can be larger than the space it uses.
- Unaccounted space does not show snapshots and purgeable space as separate items.
- A scan of a network share uses the usual walk, so it is slow.
- Without Full Disk Access, macOS hides parts of `~/Library` and other protected folders. Their space goes into Unaccounted space.
- The app needs file access outside a sandbox, so it cannot go into the Mac App Store as it is. To share a build, sign it with a Developer ID certificate and notarize it.
