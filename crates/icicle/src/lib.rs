use std::collections::HashSet;

use scan::{Kind, NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub max_depth: usize,
    /// Children narrower than this (a fraction of the focused folder's width) are merged
    /// into one "smaller objects" block.
    pub min_width: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_depth: 5,
            min_width: 0.6 / 360.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Node(NodeId),
    /// Children of `parent` too small to draw on their own.
    Smaller {
        parent: NodeId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub target: Target,
    pub depth: usize,
    /// Fractions of the focused folder's width, measured from its left edge.
    pub x: f32,
    pub width: f32,
}

impl Block {
    pub fn end(&self) -> f32 {
        self.x + self.width
    }
}

/// A block ready to paint. `top` and `bottom` are in row units: row `n` spans `n..n + 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawBlock {
    pub target: Target,
    pub top: f32,
    pub bottom: f32,
    pub x: f32,
    pub width: f32,
    pub opacity: f32,
}

/// Blocks for one focused directory, one `Vec` per row, each sorted by `x`.
#[derive(Debug)]
pub struct Layout {
    pub focus: NodeId,
    pub options: Options,
    pub rows: Vec<Vec<Block>>,
}

impl Layout {
    pub fn new(tree: &Tree, focus: NodeId, options: Options) -> Self {
        let mut rows = vec![vec![]; options.max_depth];
        place(tree, focus, 0, 0.0, 1.0, &options, &mut rows);
        Self {
            focus,
            options,
            rows,
        }
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.rows.iter().flatten()
    }

    pub fn draw_blocks(&self) -> Vec<DrawBlock> {
        self.blocks()
            .map(|b| DrawBlock {
                target: b.target,
                top: b.depth as f32,
                bottom: b.depth as f32 + 1.0,
                x: b.x,
                width: b.width,
                opacity: 1.0,
            })
            .collect()
    }

    /// The block in row `depth` that covers `x`, a fraction of the row's width.
    pub fn block_at(&self, depth: usize, x: f32) -> Option<&Block> {
        let row = self.rows.get(depth)?;
        let i = row.partition_point(|b| b.x <= x).checked_sub(1)?;
        let block = &row[i];
        (x < block.end()).then_some(block)
    }
}

fn place(
    tree: &Tree,
    id: NodeId,
    depth: usize,
    x: f32,
    width: f32,
    options: &Options,
    rows: &mut [Vec<Block>],
) {
    let node = tree.node(id);
    if depth >= rows.len() || node.size == 0 {
        return;
    }
    let mut cursor = x;
    let mut smaller = 0.0;
    for &child_id in &node.children {
        let child = tree.node(child_id);
        let child_width = width * (child.size as f64 / node.size as f64) as f32;
        if child_width < options.min_width {
            smaller += child_width;
            continue;
        }
        rows[depth].push(Block {
            target: Target::Node(child_id),
            depth,
            x: cursor,
            width: child_width,
        });
        if child.kind == Kind::Dir {
            place(
                tree,
                child_id,
                depth + 1,
                cursor,
                child_width,
                options,
                rows,
            );
        }
        cursor += child_width;
    }
    if smaller >= options.min_width {
        rows[depth].push(Block {
            target: Target::Smaller { parent: id },
            depth,
            x: cursor,
            width: smaller,
        });
    }
}

/// The frame `t` (`0..=1`) of the zoom from `from` to `to`, where one focus is an
/// ancestor of the other. The view moves like a camera from one focus to the other.
/// Blocks present in both layouts move continuously; the rest fade, and anything
/// pushed outside the chart is clipped.
pub fn zoom(tree: &Tree, from: &Layout, to: &Layout, t: f32) -> Vec<DrawBlock> {
    let (outer, inner, u) = if is_ancestor(tree, from.focus, to.focus) {
        (from, to, t)
    } else if is_ancestor(tree, to.focus, from.focus) {
        (to, from, 1.0 - t)
    } else {
        return to.draw_blocks();
    };
    let focus = span(tree, outer.focus, inner.focus);
    let camera = Span {
        depth: lerp(-1.0, focus.depth, u),
        x: lerp(0.0, focus.x, u),
        width: lerp(1.0, focus.width, u),
    };
    let max = outer.options.max_depth as f32;
    let shared: HashSet<Target> = inner.blocks().map(|b| b.target).collect();
    let in_outer: HashSet<Target> = outer.blocks().map(|b| b.target).collect();

    let outer_blocks = outer
        .blocks()
        .filter(|b| !shared.contains(&b.target))
        .map(|b| (b, b.depth as f32, b.x, b.width, 1.0 - u));
    let inner_blocks = inner.blocks().map(|b| {
        let opacity = if in_outer.contains(&b.target) { 1.0 } else { u };
        (
            b,
            b.depth as f32 + focus.depth + 1.0,
            focus.x + b.x * focus.width,
            b.width * focus.width,
            opacity,
        )
    });
    outer_blocks
        .chain(inner_blocks)
        .filter_map(|(b, depth, x, width, opacity)| {
            let row = depth - camera.depth - 1.0;
            let to_view = |x: f32| ((x - camera.x) / camera.width).clamp(0.0, 1.0);
            let (left, right) = (to_view(x), to_view(x + width));
            let (top, bottom) = (row.max(0.0), (row + 1.0).min(max));
            (opacity > 0.0 && right > left && bottom > top).then_some(DrawBlock {
                target: b.target,
                top,
                bottom,
                x: left,
                width: right - left,
                opacity,
            })
        })
        .collect()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn is_ancestor(tree: &Tree, ancestor: NodeId, mut node: NodeId) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        match tree.node(node).parent {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

/// Where `descendant` sits in the layout focused on `ancestor`, which itself is row -1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    depth: f32,
    x: f32,
    width: f32,
}

fn span(tree: &Tree, ancestor: NodeId, descendant: NodeId) -> Span {
    let mut path = vec![];
    let mut node = descendant;
    while node != ancestor {
        path.push(node);
        node = tree.node(node).parent.expect("descendant of ancestor");
    }
    let (mut depth, mut x, mut width) = (-1.0, 0.0_f64, 1.0_f64);
    for &child in path.iter().rev() {
        let parent = tree.node(tree.node(child).parent.unwrap());
        let before: u64 = parent
            .children
            .iter()
            .take_while(|&&c| c != child)
            .map(|&c| tree.node(c).size)
            .sum();
        let size = parent.size.max(1) as f64;
        x += width * before as f64 / size;
        width *= tree.node(child).size as f64 / size;
        depth += 1.0;
    }
    Span {
        depth,
        x: x as f32,
        width: width as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; len]).unwrap();
    }

    fn fixture(files: &[(&str, usize)]) -> (tempfile::TempDir, Tree) {
        let dir = tempfile::tempdir().unwrap();
        for (name, len) in files {
            write(&dir.path().join(name), *len);
        }
        let tree = scan::scan(dir.path(), &scan::Progress::default()).unwrap();
        (dir, tree)
    }

    fn named(tree: &Tree, block: &Block) -> String {
        match block.target {
            Target::Node(id) => tree.node(id).name.to_string(),
            Target::Smaller { .. } => "<smaller>".into(),
        }
    }

    #[test]
    fn widths_are_proportional_to_size() {
        let (_dir, tree) = fixture(&[("a", 400_000), ("b", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let root = tree.node(tree.root()).size as f32;
        for block in &layout.rows[0] {
            let Target::Node(id) = block.target else {
                panic!("unexpected aggregate")
            };
            let expected = tree.node(id).size as f32 / root;
            assert!(
                (block.width - expected).abs() < 1e-5,
                "{}: {} vs {expected}",
                named(&tree, block),
                block.width
            );
        }
    }

    #[test]
    fn blocks_in_a_row_are_contiguous_and_ordered() {
        let (_dir, tree) = fixture(&[("a", 400_000), ("b", 200_000), ("c", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let row = &layout.rows[0];
        assert_eq!(row[0].x, 0.0);
        for pair in row.windows(2) {
            assert!((pair[0].end() - pair[1].x).abs() < 1e-6);
        }
        assert!(
            (row.last().unwrap().end() - 1.0).abs() < 1e-6,
            "row fills the width"
        );
    }

    #[test]
    fn children_nest_inside_their_parent_block() {
        let (_dir, tree) = fixture(&[
            ("big/x", 300_000),
            ("big/y", 200_000),
            ("big/deep/z", 100_000),
            ("other", 300_000),
        ]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        for rows in layout.rows.windows(2) {
            for child in &rows[1] {
                let parent = rows[0]
                    .iter()
                    .find(|p| p.x <= child.x + 1e-6 && child.end() <= p.end() + 1e-6);
                assert!(
                    parent.is_some(),
                    "{} has no enclosing parent",
                    named(&tree, child)
                );
            }
        }
        assert!(!layout.rows[2].is_empty(), "big/deep/z reaches row 2");
    }

    #[test]
    fn tiny_children_merge_into_one_smaller_block() {
        let mut files = vec![("big", 10_000_000)];
        let names: Vec<String> = (0..50).map(|i| format!("tiny{i}")).collect();
        files.extend(names.iter().map(|n| (n.as_str(), 4_096)));
        let (_dir, tree) = fixture(&files);

        let layout = Layout::new(&tree, tree.root(), Options::default());
        let row = &layout.rows[0];
        assert_eq!(
            row.iter().map(|b| named(&tree, b)).collect::<Vec<_>>(),
            ["big", "<smaller>"]
        );
        assert_eq!(
            row[1].target,
            Target::Smaller {
                parent: tree.root()
            }
        );
    }

    #[test]
    fn rows_stop_at_max_depth() {
        let (_dir, tree) = fixture(&[("a/b/c/d/e", 100_000)]);
        let layout = Layout::new(
            &tree,
            tree.root(),
            Options {
                max_depth: 2,
                ..Options::default()
            },
        );
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.blocks().map(|b| b.depth).max(), Some(1));
    }

    #[test]
    fn block_at_finds_the_block_covering_x() {
        let (_dir, tree) = fixture(&[("a", 300_000), ("b", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        for block in &layout.rows[0] {
            let x = block.x + block.width / 2.0;
            assert_eq!(
                layout.block_at(0, x),
                Some(block),
                "{}",
                named(&tree, block)
            );
        }
        assert_eq!(
            named(&tree, layout.block_at(0, 0.0).unwrap()),
            "a",
            "largest first"
        );
        assert_eq!(layout.block_at(0, 1.0), None);
        assert_eq!(
            layout.block_at(1, 0.1),
            None,
            "files have no row-1 children"
        );
        assert_eq!(layout.block_at(9, 0.1), None);
    }

    fn visible(mut blocks: Vec<DrawBlock>) -> Vec<(Target, i32, i32, i32, i32)> {
        let q = |v: f32| (v * 10_000.0).round() as i32;
        blocks.retain(|b| b.opacity > 0.0);
        assert!(
            blocks.iter().all(|b| b.opacity == 1.0),
            "an endpoint frame has no half-faded blocks"
        );
        let mut out: Vec<_> = blocks
            .iter()
            .map(|b| (b.target, q(b.top), q(b.bottom), q(b.x), q(b.width)))
            .collect();
        out.sort_by_key(|&(t, ..)| format!("{t:?}"));
        out
    }

    fn nested() -> (tempfile::TempDir, Tree) {
        fixture(&[
            ("big/x/1", 300_000),
            ("big/x/2", 100_000),
            ("big/y", 200_000),
            ("other/z", 250_000),
            ("other/w", 50_000),
            ("file", 150_000),
        ])
    }

    fn child_named(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
        *tree
            .node(parent)
            .children
            .iter()
            .find(|&&c| &*tree.node(c).name == name)
            .unwrap()
    }

    #[test]
    fn zoom_starts_at_the_old_view_and_ends_at_the_new_one() {
        let (_dir, tree) = nested();
        let other = child_named(&tree, tree.root(), "other");
        let parent = Layout::new(&tree, tree.root(), Options::default());
        let child = Layout::new(&tree, other, Options::default());

        for (from, to) in [(&parent, &child), (&child, &parent)] {
            assert_eq!(
                visible(zoom(&tree, from, to, 0.0)),
                visible(from.draw_blocks())
            );
            assert_eq!(
                visible(zoom(&tree, from, to, 1.0)),
                visible(to.draw_blocks())
            );
        }
    }

    #[test]
    fn zoom_across_several_levels_ends_at_the_new_view() {
        let (_dir, tree) = nested();
        let x = child_named(&tree, child_named(&tree, tree.root(), "big"), "x");
        let root = Layout::new(&tree, tree.root(), Options::default());
        let deep = Layout::new(&tree, x, Options::default());
        assert_eq!(
            visible(zoom(&tree, &deep, &root, 1.0)),
            visible(root.draw_blocks())
        );
        assert_eq!(
            visible(zoom(&tree, &root, &deep, 1.0)),
            visible(deep.draw_blocks())
        );
    }

    #[test]
    fn mid_zoom_stays_inside_the_chart() {
        let (_dir, tree) = nested();
        let big = child_named(&tree, tree.root(), "big");
        let from = Layout::new(&tree, tree.root(), Options::default());
        let to = Layout::new(&tree, big, Options::default());
        for i in 0..=10 {
            for b in zoom(&tree, &from, &to, i as f32 / 10.0) {
                assert!(b.x >= 0.0 && b.x + b.width <= 1.0 + 1e-6, "{b:?}");
                assert!(b.top >= 0.0 && b.bottom <= 5.0 && b.top < b.bottom, "{b:?}");
            }
        }
    }

    #[test]
    fn zoomed_in_child_sits_where_its_block_was() {
        let (_dir, tree) = nested();
        let big = child_named(&tree, tree.root(), "big");
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let block = layout
            .blocks()
            .find(|b| b.target == Target::Node(big))
            .unwrap();
        let s = span(&tree, tree.root(), big);
        assert_eq!((s.depth, s.x), (0.0, block.x));
        assert!((s.width - block.width).abs() < 1e-6);
    }
}
