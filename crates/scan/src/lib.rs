use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
}

/// Scans `root` without following symlinks or crossing onto other devices.
/// Unreadable entries are recorded in [`Tree::errors`] and the scan continues.
pub fn scan(root: &Path) -> io::Result<Tree> {
    let meta = fs::symlink_metadata(root)?;
    let walker = Walker {
        dev: meta.dev(),
        hard_links: Mutex::new(HashSet::new()),
        errors: Mutex::new(vec![]),
    };
    let built = walker.entry(root.as_os_str().to_string_lossy().into(), root, &meta);
    let mut nodes = Vec::new();
    flatten(built, None, &mut nodes);
    Ok(Tree {
        nodes,
        root: NodeId(0),
        errors: walker.errors.into_inner().unwrap(),
    })
}

struct Walker {
    dev: u64,
    hard_links: Mutex<HashSet<(u64, u64)>>,
    errors: Mutex<Vec<ScanError>>,
}

/// A subtree built by one worker, flattened into the arena once the walk finishes.
struct Built {
    name: Box<str>,
    kind: Kind,
    size: u64,
    file_count: u32,
    children: Vec<Built>,
}

impl Walker {
    fn entry(&self, name: Box<str>, path: &Path, meta: &fs::Metadata) -> Built {
        let ft = meta.file_type();
        let own = self.allocated(meta);
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
            return Built {
                name,
                kind: Kind::File,
                size: own,
                file_count: 1,
                children: vec![],
            };
        }
        let children = if meta.dev() == self.dev {
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

        let tree = scan(root).unwrap();
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

        let tree = scan(dir.path()).unwrap();
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

        let tree = scan(dir.path()).unwrap();
        let both = child(&tree, tree.root(), "orig").size + child(&tree, tree.root(), "link").size;
        assert_eq!(both, blocks(&dir.path().join("orig")));
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(&target.path().join("payload"), 300_000);
        symlink(target.path(), dir.path().join("link")).unwrap();

        let tree = scan(dir.path()).unwrap();
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

        let tree = scan(dir.path());
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let tree = tree.unwrap();

        assert_eq!(tree.errors.len(), 1);
        assert_eq!(tree.errors[0].path, locked);
        assert_eq!(tree.node(tree.root()).file_count, 1);
    }

    #[test]
    fn path_rebuilds_full_path_from_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        write(&dir.path().join("sub/f"), 1);

        let tree = scan(dir.path()).unwrap();
        let sub = tree.node(tree.root()).children[0];
        let f = tree.node(sub).children[0];
        assert_eq!(tree.path(f), dir.path().join("sub/f"));
    }
}
