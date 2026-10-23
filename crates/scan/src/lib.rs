use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
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
}

/// Scans `root` without following symlinks or crossing onto other devices.
/// Unreadable entries are recorded in [`Tree::errors`] and the scan continues.
pub fn scan(root: &Path, progress: &Progress) -> io::Result<Tree> {
    let meta = fs::symlink_metadata(root)?;
    let walker = Walker {
        dev: meta.dev(),
        hard_links: Mutex::new(HashSet::new()),
        errors: Mutex::new(vec![]),
        progress,
    };
    let built = walker.entry(root.as_os_str().to_string_lossy().into(), root, &meta);
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
    dev: u64,
    hard_links: Mutex<HashSet<(u64, u64)>>,
    errors: Mutex<Vec<ScanError>>,
    progress: &'a Progress,
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
    fn entry(&self, name: Box<str>, path: &Path, meta: &fs::Metadata) -> Built {
        let ft = meta.file_type();
        let own = self.allocated(meta);
        self.progress.bytes.fetch_add(own, Ordering::Relaxed);
        if ft.is_symlink() {
            return Built {
                name,
                kind: Kind::Symlink,
                size: 0,
                file_count: 0,
                children: vec![],
            };
        }
        if !ft.is_dir() {
            self.progress.files.fetch_add(1, Ordering::Relaxed);
            return Built {
                name,
                kind: Kind::File,
                size: own,
                file_count: 1,
                children: vec![],
            };
        }
        let children = if meta.dev() == self.dev && meta.st_flags() & SF_DATALESS == 0 {
            self.children(path)
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

    fn children(&self, dir: &Path) -> Vec<Built> {
        if self.progress.is_cancelled() {
            return vec![];
        }
        if let Ok(mut current) = self.progress.current.try_lock() {
            dir.clone_into(&mut current);
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.error(dir.to_path_buf(), error);
                return vec![];
            }
        };
        let entries: Vec<_> = entries
            .filter_map(|e| e.map_err(|error| self.error(dir.to_path_buf(), error)).ok())
            .collect();
        entries
            .into_par_iter()
            .filter_map(|entry| {
                if self.progress.is_cancelled() {
                    return None;
                }
                let path = entry.path();
                match entry.metadata() {
                    Ok(meta) => {
                        let name = entry.file_name().to_string_lossy().into();
                        Some(self.entry(name, &path, &meta))
                    }
                    Err(error) => {
                        self.error(path, error);
                        None
                    }
                }
            })
            .collect()
    }

    /// A hard-linked file's blocks count only at the first link the walk reaches.
    fn allocated(&self, meta: &fs::Metadata) -> u64 {
        let bytes = meta.blocks() * 512;
        if meta.is_file() && meta.nlink() > 1 {
            let first = self
                .hard_links
                .lock()
                .unwrap()
                .insert((meta.dev(), meta.ino()));
            if !first {
                return 0;
            }
        }
        bytes
    }

    fn error(&self, path: PathBuf, error: io::Error) {
        self.errors.lock().unwrap().push(ScanError { path, error });
    }
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
