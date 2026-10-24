use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    /// Space the volume reports as used that no scanned file accounts for: snapshots,
    /// purgeable space, other volumes in the container, and folders that couldn't be read.
    /// Only a volume's root gets one, and it has no path on disk.
    Hidden,
}

#[derive(Debug)]
pub struct Node {
    pub name: Box<str>,
    pub parent: Option<NodeId>,
    pub kind: Kind,
    /// Allocated bytes on disk (`st_blocks * 512`), summed over descendants for directories.
    pub size: u64,
    pub file_count: u32,
    /// Sorted by size, largest first.
    pub children: Vec<NodeId>,
}

#[derive(Debug)]
pub struct ScanError {
    pub path: PathBuf,
    pub error: io::Error,
}

#[derive(Debug)]
pub struct Tree {
    nodes: Vec<Node>,
    /// Named with the full scanned path, so [`Tree::path`] yields absolute paths.
    root: NodeId,
    pub errors: Vec<ScanError>,
}

impl Tree {
    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut names = vec![];
        let mut cur = Some(id);
        while let Some(id) = cur {
            let node = self.node(id);
            names.push(&*node.name);
            cur = node.parent;
        }
        names.iter().rev().collect()
    }

    /// Detaches `id` from its parent and subtracts its size and file count from every
    /// ancestor, keeping each ancestor's children sorted. The root can't be removed.
    pub fn remove(&mut self, id: NodeId) {
        let Some(parent) = self.node(id).parent else {
            return;
        };
        let (size, files) = (self.node(id).size, self.node(id).file_count);
        self.nodes[parent.0 as usize].children.retain(|&c| c != id);
        let mut cur = Some(parent);
        while let Some(id) = cur {
            let node = &mut self.nodes[id.0 as usize];
            node.size -= size;
            node.file_count -= files;
            cur = node.parent;
            if let Some(parent) = cur {
                let mut children = std::mem::take(&mut self.nodes[parent.0 as usize].children);
                children.sort_by_key(|&c| std::cmp::Reverse(self.node(c).size));
                self.nodes[parent.0 as usize].children = children;
            }
        }
    }
}

/// Live counters for a running scan, shared with the thread that started it.
#[derive(Default)]
pub struct Progress {
    files: AtomicU64,
    bytes: AtomicU64,
    current: Mutex<PathBuf>,
    cancelled: AtomicBool,
    /// Directories near the root in the order the walk found them, so every parent comes
    /// before its children.
    live: Mutex<Vec<LiveDir>>,
}

/// The deepest directory level a running scan tracks for [`Progress::snapshot`]. The
/// chart draws five rings, and anything deeper counts toward its ancestor at this level.
const LIVE_DEPTH: usize = 5;

struct LiveDir {
    name: Box<str>,
    parent: Option<usize>,
    /// Bytes found so far in this directory and in its descendants that aren't tracked.
    bytes: Arc<AtomicU64>,
}

impl Progress {
    pub fn files(&self) -> u64 {
        self.files.load(Ordering::Relaxed)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// A directory the walk recently entered.
    pub fn current(&self) -> PathBuf {
        self.current.lock().unwrap().clone()
    }

    /// Makes the running [`scan`] stop and return [`io::ErrorKind::Interrupted`].
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// The directories found so far near the root, sized by what the walk has counted in
    /// each, for drawing a scan while it runs. Files aren't included. `None` before the
    /// walk has started.
    pub fn snapshot(&self) -> Option<Tree> {
        let live = self.live.lock().unwrap();
        let mut nodes: Vec<Node> = live
            .iter()
            .map(|dir| Node {
                name: dir.name.clone(),
                parent: dir.parent.map(|p| NodeId(p as u32)),
                kind: Kind::Dir,
                size: dir.bytes.load(Ordering::Relaxed),
                file_count: 0,
                children: vec![],
            })
            .collect();
        drop(live);
        if nodes.is_empty() {
            return None;
        }
        for i in (1..nodes.len()).rev() {
            let parent = nodes[i].parent.unwrap().0 as usize;
            nodes[parent].size += nodes[i].size;
            nodes[parent].children.push(NodeId(i as u32));
        }
        for i in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[i].children);
            children.sort_unstable_by_key(|c| std::cmp::Reverse(nodes[c.0 as usize].size));
            nodes[i].children = children;
        }
        Some(Tree {
            nodes,
            root: NodeId(0),
            errors: vec![],
        })
    }

    fn track(&self, name: &str, parent: Option<usize>) -> (usize, Arc<AtomicU64>) {
        let bytes = Arc::new(AtomicU64::new(0));
        let mut live = self.live.lock().unwrap();
        live.push(LiveDir {
            name: name.into(),
            parent,
            bytes: bytes.clone(),
        });
        (live.len() - 1, bytes)
    }
}

/// Scans `root` without following symlinks or crossing onto other devices or mount points.
/// Unreadable entries are recorded in [`Tree::errors`] and the scan continues. When `root`
/// is a volume's mount point, the space the volume uses beyond what the walk found
/// becomes a [`Kind::Hidden`] child of the root.
pub fn scan(root: &Path, progress: &Progress) -> io::Result<Tree> {
    let mut mounts = mount_points();
    mounts.remove(root);
    let mut tree = walk(root, mounts, progress)?;
    if let Some(used) = volume_used(root) {
        let scanned = tree.node(tree.root).size;
        tree.add_hidden(used.saturating_sub(scanned));
    }
    Ok(tree)
}

fn walk(root: &Path, mounts: HashSet<PathBuf>, progress: &Progress) -> io::Result<Tree> {
    let meta = Meta::of(&fs::symlink_metadata(root)?);
    let walker = Walker {
        mounts,
        hard_links: Mutex::new(HashSet::new()),
        errors: Mutex::new(vec![]),
        progress,
    };
    let built = walker.entry(root.as_os_str().to_string_lossy().into(), root, &meta, None);
    if progress.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let mut nodes = Vec::new();
    flatten(built, None, &mut nodes);
    Ok(Tree {
        nodes,
        root: NodeId(0),
        errors: walker.errors.into_inner().unwrap(),
    })
}

/// Marks a file or directory whose contents live only in the cloud (iCloud Drive and
/// other File Provider folders). Listing such a directory makes macOS fetch it from the
/// network, which blocks for seconds, so the walk doesn't descend into it.
const SF_DATALESS: u32 = 0x4000_0000;

struct Walker<'a> {
    /// Mount points below the root, which the walk doesn't enter. getattrlistbulk(2)
    /// reports a mount point with the attributes of the folder it covers, device number
    /// included, so only the path tells it apart.
    mounts: HashSet<PathBuf>,
    /// File IDs of hard-linked files already counted. Everything the walk reaches shares
    /// the root's device number, so the ID alone identifies a file.
    hard_links: Mutex<HashSet<u64>>,
    errors: Mutex<Vec<ScanError>>,
    progress: &'a Progress,
}

/// The tracked directory nearest above an entry, where the entry's bytes are counted
/// while the scan runs.
#[derive(Clone, Copy)]
struct Live<'a> {
    index: usize,
    depth: usize,
    bytes: &'a AtomicU64,
}

/// A subtree built by one worker, flattened into the arena once the walk finishes.
struct Built {
    name: Box<str>,
    kind: Kind,
    size: u64,
    file_count: u32,
    children: Vec<Built>,
}

impl Walker<'_> {
    /// `live` is `None` only for the root.
    fn entry(&self, name: Box<str>, path: &Path, meta: &Meta, live: Option<Live<'_>>) -> Built {
        let own = self.allocated(meta);
        self.progress.bytes.fetch_add(own, Ordering::Relaxed);
        if meta.kind == Kind::Symlink {
            return Built {
                name,
                kind: Kind::Symlink,
                size: 0,
                file_count: 0,
                children: vec![],
            };
        }
        if meta.kind == Kind::File {
            if let Some(live) = live {
                live.bytes.fetch_add(own, Ordering::Relaxed);
            }
            self.progress.files.fetch_add(1, Ordering::Relaxed);
            return Built {
                name,
                kind: Kind::File,
                size: own,
                file_count: 1,
                children: vec![],
            };
        }
        let tracked;
        let live = match live {
            Some(live) if live.depth >= LIVE_DEPTH => live,
            parent => {
                tracked = self.progress.track(&name, parent.map(|p| p.index));
                Live {
                    index: tracked.0,
                    depth: parent.map_or(0, |p| p.depth + 1),
                    bytes: &tracked.1,
                }
            }
        };
        live.bytes.fetch_add(own, Ordering::Relaxed);
        let children = if meta.flags & SF_DATALESS == 0 && !self.mounts.contains(path) {
            self.children(path, live)
        } else {
            vec![]
        };
        Built {
            name,
            kind: Kind::Dir,
            size: own + children.iter().map(|c| c.size).sum::<u64>(),
            file_count: children.iter().map(|c| c.file_count).sum(),
            children,
        }
    }

    fn children(&self, dir: &Path, live: Live<'_>) -> Vec<Built> {
        if self.progress.is_cancelled() {
            return vec![];
        }
        if let Ok(mut current) = self.progress.current.try_lock() {
            dir.clone_into(&mut current);
        }
        let entries = match list(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.error(dir.to_path_buf(), error);
                return vec![];
            }
        };
        entries
            .into_par_iter()
            .filter_map(|(name, meta)| {
                if self.progress.is_cancelled() {
                    return None;
                }
                let path = dir.join(&*name);
                match meta {
                    Ok(meta) => Some(self.entry(name, &path, &meta, Some(live))),
                    Err(error) => {
                        self.error(path, error);
                        None
                    }
                }
            })
            .collect()
    }

    /// A hard-linked file's blocks count only at the first link the walk reaches.
    fn allocated(&self, meta: &Meta) -> u64 {
        if meta.kind == Kind::File && meta.links > 1 {
            let first = self.hard_links.lock().unwrap().insert(meta.file_id);
            if !first {
                return 0;
            }
        }
        meta.allocated
    }

    fn error(&self, path: PathBuf, error: io::Error) {
        self.errors.lock().unwrap().push(ScanError { path, error });
    }
}

/// What the walk needs to know about an entry, read with getattrlistbulk(2) for a
/// folder's contents and with lstat for the root.
struct Meta {
    kind: Kind,
    /// Bytes on disk, before hard links are deduplicated.
    allocated: u64,
    file_id: u64,
    links: u32,
    flags: u32,
}

impl Meta {
    fn of(meta: &fs::Metadata) -> Meta {
        let ft = meta.file_type();
        Meta {
            kind: if ft.is_symlink() {
                Kind::Symlink
            } else if ft.is_dir() {
                Kind::Dir
            } else {
                Kind::File
            },
            allocated: meta.blocks() * 512,
            file_id: meta.ino(),
            links: meta.nlink() as u32,
            flags: meta.st_flags(),
        }
    }
}

// Missing from the libc crate.
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// Lists `dir` with each entry's metadata. getattrlistbulk(2) returns a batch of entries
/// with their attributes in one call, where readdir would need an lstat per entry.
fn list(dir: &Path) -> io::Result<Vec<(Box<str>, io::Result<Meta>)>> {
    let c_dir = CString::new(dir.as_os_str().as_bytes())?;
    let fd = unsafe { libc::open(c_dir.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: libc::ATTR_DIR_ALLOCSIZE,
        fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut entries = vec![];
    loop {
        let count = unsafe {
            libc::getattrlistbulk(
                fd.as_raw_fd(),
                (&raw mut request).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(entries);
        }
        let mut at = 0;
        for _ in 0..count {
            let len = read_u32(&buf, at) as usize;
            entries.push(parse(&buf[at..at + len]));
            at += len;
        }
    }
}

/// Reads one entry of a getattrlistbulk(2) buffer. Attributes come in the order of their
/// bits, each 4-byte aligned, and those the volume didn't return are left out.
fn parse(entry: &[u8]) -> (Box<str>, io::Result<Meta>) {
    let returned = libc::attribute_set_t {
        commonattr: read_u32(entry, 4),
        volattr: read_u32(entry, 8),
        dirattr: read_u32(entry, 12),
        fileattr: read_u32(entry, 16),
        forkattr: read_u32(entry, 20),
    };
    let mut at = 24;
    let mut next = |size: usize| {
        at += size;
        at - size
    };
    let error = if returned.commonattr & ATTR_CMN_ERROR != 0 {
        read_u32(entry, next(4))
    } else {
        0
    };
    let name_at = next(8);
    let name_start = name_at + read_u32(entry, name_at) as usize;
    let name = CStr::from_bytes_until_nul(&entry[name_start..])
        .map_or_else(|_| "".into(), |name| name.to_string_lossy().into());
    if error != 0 {
        return (name, Err(io::Error::from_raw_os_error(error as i32)));
    }
    let mut field = |bit: u32, group: u32, size: usize| (group & bit != 0).then(|| next(size));
    let kind = match field(libc::ATTR_CMN_OBJTYPE, returned.commonattr, 4) {
        Some(at) if read_u32(entry, at) == VDIR => Kind::Dir,
        Some(at) if read_u32(entry, at) == VLNK => Kind::Symlink,
        _ => Kind::File,
    };
    let flags =
        field(libc::ATTR_CMN_FLAGS, returned.commonattr, 4).map_or(0, |at| read_u32(entry, at));
    let file_id =
        field(libc::ATTR_CMN_FILEID, returned.commonattr, 8).map_or(0, |at| read_u64(entry, at));
    let dir_size = field(libc::ATTR_DIR_ALLOCSIZE, returned.dirattr, 8);
    let links =
        field(libc::ATTR_FILE_LINKCOUNT, returned.fileattr, 4).map_or(1, |at| read_u32(entry, at));
    let file_size = field(libc::ATTR_FILE_ALLOCSIZE, returned.fileattr, 8);
    let allocated = dir_size.or(file_size).map_or(0, |at| read_u64(entry, at));
    let meta = Meta {
        kind,
        allocated,
        file_id,
        links,
        flags,
    };
    (name, Ok(meta))
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(buf[at..at + 8].try_into().unwrap())
}

impl Tree {
    fn add_hidden(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            name: "Hidden space".into(),
            parent: Some(self.root),
            kind: Kind::Hidden,
            size: bytes,
            file_count: 0,
            children: vec![],
        });
        let root = &self.nodes[self.root.0 as usize];
        let at = root
            .children
            .partition_point(|&c| self.node(c).size >= bytes);
        let root = &mut self.nodes[self.root.0 as usize];
        root.children.insert(at, id);
        root.size += bytes;
    }
}

fn mount_points() -> HashSet<PathBuf> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 {
        return HashSet::new();
    }
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
    mounts.iter().map(mount_path).collect()
}

/// Bytes in use on the volume mounted at `path`, counted as total minus available, as
/// the volumes screen shows it. `None` when `path` isn't a mount point.
fn volume_used(path: &Path) -> Option<u64> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 || mount_path(&stats) != path {
        return None;
    }
    Some((stats.f_blocks - stats.f_bavail) * stats.f_bsize as u64)
}

fn mount_path(stats: &libc::statfs) -> PathBuf {
    let name = unsafe { std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()) };
    PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
}

fn flatten(built: Built, parent: Option<NodeId>, nodes: &mut Vec<Node>) -> NodeId {
    let id = NodeId(nodes.len() as u32);
    nodes.push(Node {
        name: built.name,
        parent,
        kind: built.kind,
        size: built.size,
        file_count: built.file_count,
        children: vec![],
    });
    let mut children = built.children;
    children.sort_unstable_by_key(|c| std::cmp::Reverse(c.size));
    let ids = children
        .into_iter()
        .map(|c| flatten(c, Some(id), nodes))
        .collect();
    nodes[id.0 as usize].children = ids;
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, len: usize) {
        fs::write(path, vec![7u8; len]).unwrap();
    }

    fn child<'a>(tree: &'a Tree, parent: NodeId, name: &str) -> &'a Node {
        let id = tree
            .node(parent)
            .children
            .iter()
            .find(|&&c| &*tree.node(c).name == name);
        tree.node(*id.unwrap_or_else(|| panic!("no child named {name}")))
    }

    fn blocks(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[test]
    fn directory_size_is_allocated_bytes_of_itself_and_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        write(&root.join("a"), 10_000);
        write(&root.join("sub/b"), 100_000);

        let tree = scan(root, &Progress::default()).unwrap();
        let expected = blocks(root)
            + blocks(&root.join("a"))
            + blocks(&root.join("sub"))
            + blocks(&root.join("sub/b"));
        assert_eq!(tree.node(tree.root()).size, expected);
        assert_eq!(tree.node(tree.root()).file_count, 2);
        assert_eq!(child(&tree, tree.root(), "a").size, blocks(&root.join("a")));
        assert!(tree.errors.is_empty());
    }

    #[test]
    fn children_are_sorted_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("small"), 5_000);
        write(&dir.path().join("big"), 500_000);
        write(&dir.path().join("mid"), 50_000);

        let tree = scan(dir.path(), &Progress::default()).unwrap();
        let names: Vec<_> = tree
            .node(tree.root())
            .children
            .iter()
            .map(|&c| &*tree.node(c).name)
            .collect();
        assert_eq!(names, ["big", "mid", "small"]);
    }

    #[test]
    fn hard_links_are_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("orig"), 200_000);
        fs::hard_link(dir.path().join("orig"), dir.path().join("link")).unwrap();

        let tree = scan(dir.path(), &Progress::default()).unwrap();
        let both = child(&tree, tree.root(), "orig").size + child(&tree, tree.root(), "link").size;
        assert_eq!(both, blocks(&dir.path().join("orig")));
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(&target.path().join("payload"), 300_000);
        symlink(target.path(), dir.path().join("link")).unwrap();

        let tree = scan(dir.path(), &Progress::default()).unwrap();
        let link = child(&tree, tree.root(), "link");
        assert_eq!(
            (link.kind, link.size, link.children.len()),
            (Kind::Symlink, 0, 0)
        );
        assert_eq!(tree.node(tree.root()).size, blocks(dir.path()));
    }

    #[test]
    fn unreadable_directory_is_reported_and_scan_continues() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        write(&locked.join("secret"), 10_000);
        write(&dir.path().join("visible"), 10_000);
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();

        let tree = scan(dir.path(), &Progress::default());
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let tree = tree.unwrap();

        assert_eq!(tree.errors.len(), 1);
        assert_eq!(tree.errors[0].path, locked);
        assert_eq!(tree.node(tree.root()).file_count, 1);
    }

    #[test]
    fn folder_too_big_for_one_bulk_read_is_listed_in_full() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..2000 {
            write(&dir.path().join(format!("{i:0>120}")), 1);
        }

        let tree = scan(dir.path(), &Progress::default()).unwrap();
        assert_eq!(tree.node(tree.root()).file_count, 2000);
    }

    #[test]
    fn progress_ends_at_the_tree_totals() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        write(&dir.path().join("a"), 10_000);
        write(&dir.path().join("sub/b"), 100_000);

        let progress = Progress::default();
        let tree = scan(dir.path(), &progress).unwrap();
        let root = tree.node(tree.root());
        assert_eq!((progress.files(), progress.bytes()), (2, root.size));
    }

    #[test]
    fn cancelled_scan_stops_walking_and_reports_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a"), 10_000);

        let progress = Progress::default();
        progress.cancel();
        let err = scan(dir.path(), &progress).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert_eq!(progress.files(), 0);
    }

    #[test]
    fn remove_subtracts_from_ancestors_and_resorts_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/inner")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        write(&root.join("a/inner/big"), 500_000);
        write(&root.join("a/small"), 5_000);
        write(&root.join("b/mid"), 50_000);

        let mut tree = scan(root, &Progress::default()).unwrap();
        let top = tree.root();
        let (a, b) = (tree.node(top).children[0], tree.node(top).children[1]);
        assert_eq!((&*tree.node(a).name, &*tree.node(b).name), ("a", "b"));
        let inner = tree.node(a).children[0];
        let big = tree.node(inner).children[0];
        let before = (
            tree.node(top).size,
            tree.node(a).size,
            tree.node(inner).size,
        );
        let removed = tree.node(big).size;

        tree.remove(big);

        assert_eq!(
            (
                tree.node(top).size,
                tree.node(a).size,
                tree.node(inner).size
            ),
            (before.0 - removed, before.1 - removed, before.2 - removed)
        );
        assert_eq!((tree.node(top).file_count, tree.node(a).file_count), (2, 1));
        assert!(tree.node(inner).children.is_empty());
        assert_eq!(tree.node(top).children, [b, a]);
        assert_eq!(tree.node(a).children, [child_id(&tree, a, "small"), inner]);
    }

    #[test]
    fn removing_the_root_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("f"), 10_000);
        let mut tree = scan(dir.path(), &Progress::default()).unwrap();
        let size = tree.node(tree.root()).size;
        tree.remove(tree.root());
        assert_eq!(tree.node(tree.root()).size, size);
    }

    #[test]
    fn mount_points_below_the_root_are_not_entered() {
        let dir = tempfile::tempdir().unwrap();
        let mnt = dir.path().join("mnt");
        fs::create_dir(&mnt).unwrap();
        write(&mnt.join("inside"), 100_000);
        write(&dir.path().join("outside"), 10_000);

        let mounts = HashSet::from([mnt.clone()]);
        let tree = walk(dir.path(), mounts, &Progress::default()).unwrap();
        let mnt_node = child(&tree, tree.root(), "mnt");
        assert_eq!((mnt_node.size, mnt_node.children.len()), (blocks(&mnt), 0));
        assert_eq!(tree.node(tree.root()).file_count, 1);
    }

    #[test]
    fn hidden_space_is_added_under_the_root_in_size_order() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big"), 500_000);
        write(&dir.path().join("small"), 5_000);
        let mut tree = scan(dir.path(), &Progress::default()).unwrap();
        let before = tree.node(tree.root()).size;
        let hidden = blocks(&dir.path().join("big")) - 1;

        tree.add_hidden(hidden);
        tree.add_hidden(0);

        let root = tree.node(tree.root());
        let names: Vec<_> = root.children.iter().map(|&c| &*tree.node(c).name).collect();
        assert_eq!(names, ["big", "Hidden space", "small"]);
        assert_eq!(root.size, before + hidden);
        assert_eq!(child(&tree, tree.root(), "Hidden space").kind, Kind::Hidden);
    }

    #[test]
    fn snapshot_of_a_finished_scan_matches_its_folders() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c/d/e/f/g");
        fs::create_dir_all(&deep).unwrap();
        write(&deep.join("bottom"), 200_000);
        write(&dir.path().join("a/top"), 10_000);
        fs::create_dir(dir.path().join("z")).unwrap();

        let progress = Progress::default();
        let tree = scan(dir.path(), &progress).unwrap();
        let live = progress.snapshot().unwrap();

        let (root, live_root) = (tree.node(tree.root()), live.node(live.root()));
        assert_eq!((&*live_root.name, live_root.size), (&*root.name, root.size));
        let names: Vec<_> = live_root
            .children
            .iter()
            .map(|&c| &*live.node(c).name)
            .collect();
        assert_eq!(names, ["a", "z"]);
        let a = child(&tree, tree.root(), "a");
        assert_eq!(child(&live, live.root(), "a").size, a.size);
        assert_eq!(child(&live, live.root(), "a").children.len(), 1);
        assert_eq!(live.node_count(), 1 + LIVE_DEPTH + 1);
    }

    fn child_id(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
        *tree
            .node(parent)
            .children
            .iter()
            .find(|&&c| &*tree.node(c).name == name)
            .unwrap()
    }

    #[test]
    fn path_rebuilds_full_path_from_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        write(&dir.path().join("sub/f"), 1);

        let tree = scan(dir.path(), &Progress::default()).unwrap();
        let sub = tree.node(tree.root()).children[0];
        let f = tree.node(sub).children[0];
        assert_eq!(tree.path(f), dir.path().join("sub/f"));
    }
}
