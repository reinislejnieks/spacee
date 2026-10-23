use std::f32::consts::TAU;

use scan::{Kind, NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub max_depth: usize,
    /// Children narrower than this (radians) are merged into one "smaller objects" arc.
    pub min_sweep: f32,
    /// Radius of the center disc as a fraction of the outer radius.
    pub hole: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_depth: 5,
            min_sweep: 0.6_f32.to_radians(),
            hole: 0.3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node(NodeId),
    /// Children of `parent` too small to draw on their own.
    Smaller {
        parent: NodeId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arc {
    pub target: Target,
    pub depth: usize,
    /// Radians clockwise from 12 o'clock.
    pub start: f32,
    pub sweep: f32,
    /// Hue in `0..1` inherited from the ancestor in the innermost ring.
    pub hue: f32,
}

impl Arc {
    pub fn end(&self) -> f32 {
        self.start + self.sweep
    }
}

/// Arcs for one focused directory, one `Vec` per ring, each sorted by `start`.
#[derive(Debug)]
pub struct Layout {
    pub focus: NodeId,
    pub options: Options,
    pub rings: Vec<Vec<Arc>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit<'a> {
    Center,
    Arc(&'a Arc),
}

impl Layout {
    pub fn new(tree: &Tree, focus: NodeId, options: Options) -> Self {
        let mut rings = vec![vec![]; options.max_depth];
        place(tree, focus, 0, 0.0, TAU, None, &options, &mut rings);
        Self {
            focus,
            options,
            rings,
        }
    }

    pub fn arcs(&self) -> impl Iterator<Item = &Arc> {
        self.rings.iter().flatten()
    }

    /// Inner and outer radius of ring `depth` for a sunburst of outer radius `radius`.
    pub fn ring_radii(&self, depth: usize, radius: f32) -> (f32, f32) {
        let hole = radius * self.options.hole;
        let width = (radius - hole) / self.options.max_depth as f32;
        (
            hole + width * depth as f32,
            hole + width * (depth + 1) as f32,
        )
    }

    /// What lies under the point `(x, y)`, relative to the center with y pointing down.
    pub fn hit(&self, x: f32, y: f32, radius: f32) -> Option<Hit<'_>> {
        let r = x.hypot(y);
        let hole = radius * self.options.hole;
        if r < hole {
            return Some(Hit::Center);
        }
        if r >= radius {
            return None;
        }
        let depth = ((r - hole) / (radius - hole) * self.options.max_depth as f32) as usize;
        let ring = self.rings.get(depth)?;
        let angle = x.atan2(-y).rem_euclid(TAU);
        let i = ring.partition_point(|a| a.start <= angle).checked_sub(1)?;
        let arc = &ring[i];
        (angle < arc.end()).then_some(Hit::Arc(arc))
    }
}

#[allow(clippy::too_many_arguments)]
fn place(
    tree: &Tree,
    id: NodeId,
    depth: usize,
    start: f32,
    sweep: f32,
    hue: Option<f32>,
    options: &Options,
    rings: &mut [Vec<Arc>],
) {
    let node = tree.node(id);
    if depth >= rings.len() || node.size == 0 {
        return;
    }
    let mut cursor = start;
    let mut smaller = 0.0;
    for &child_id in &node.children {
        let child = tree.node(child_id);
        let child_sweep = sweep * (child.size as f64 / node.size as f64) as f32;
        if child_sweep < options.min_sweep {
            smaller += child_sweep;
            continue;
        }
        let child_hue = hue.unwrap_or((cursor + child_sweep / 2.0) / TAU);
        rings[depth].push(Arc {
            target: Target::Node(child_id),
            depth,
            start: cursor,
            sweep: child_sweep,
            hue: child_hue,
        });
        if child.kind == Kind::Dir {
            place(
                tree,
                child_id,
                depth + 1,
                cursor,
                child_sweep,
                Some(child_hue),
                options,
                rings,
            );
        }
        cursor += child_sweep;
    }
    if smaller >= options.min_sweep {
        rings[depth].push(Arc {
            target: Target::Smaller { parent: id },
            depth,
            start: cursor,
            sweep: smaller,
            hue: hue.unwrap_or((cursor + smaller / 2.0) / TAU),
        });
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
        let tree = scan::scan(dir.path()).unwrap();
        (dir, tree)
    }

    fn named(tree: &Tree, arc: &Arc) -> String {
        match arc.target {
            Target::Node(id) => tree.node(id).name.to_string(),
            Target::Smaller { .. } => "<smaller>".into(),
        }
    }

    #[test]
    fn sweeps_are_proportional_to_size() {
        let (_dir, tree) = fixture(&[("a", 400_000), ("b", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let root = tree.node(tree.root()).size as f32;
        for arc in &layout.rings[0] {
            let Target::Node(id) = arc.target else {
                panic!("unexpected aggregate")
            };
            let expected = TAU * tree.node(id).size as f32 / root;
            assert!(
                (arc.sweep - expected).abs() < 1e-4,
                "{}: {} vs {expected}",
                named(&tree, arc),
                arc.sweep
            );
        }
    }

    #[test]
    fn arcs_in_a_ring_are_contiguous_and_ordered() {
        let (_dir, tree) = fixture(&[("a", 400_000), ("b", 200_000), ("c", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let ring = &layout.rings[0];
        assert_eq!(ring[0].start, 0.0);
        for pair in ring.windows(2) {
            assert!((pair[0].end() - pair[1].start).abs() < 1e-6);
        }
    }

    #[test]
    fn children_nest_inside_their_parent_arc() {
        let (_dir, tree) = fixture(&[
            ("big/x", 300_000),
            ("big/y", 200_000),
            ("big/deep/z", 100_000),
            ("other", 300_000),
        ]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        for ring in layout.rings.windows(2) {
            for child in &ring[1] {
                let parent = ring[0]
                    .iter()
                    .find(|p| p.start <= child.start + 1e-5 && child.end() <= p.end() + 1e-5);
                assert!(
                    parent.is_some(),
                    "{} has no enclosing parent",
                    named(&tree, child)
                );
                assert_eq!(parent.unwrap().hue, child.hue);
            }
        }
        assert!(!layout.rings[2].is_empty(), "big/deep/z reaches ring 2");
    }

    #[test]
    fn tiny_children_merge_into_one_smaller_arc() {
        let mut files = vec![("big", 10_000_000)];
        let names: Vec<String> = (0..50).map(|i| format!("tiny{i}")).collect();
        files.extend(names.iter().map(|n| (n.as_str(), 4_096)));
        let (_dir, tree) = fixture(&files);

        let layout = Layout::new(&tree, tree.root(), Options::default());
        let ring = &layout.rings[0];
        assert_eq!(
            ring.iter().map(|a| named(&tree, a)).collect::<Vec<_>>(),
            ["big", "<smaller>"]
        );
        assert_eq!(
            ring[1].target,
            Target::Smaller {
                parent: tree.root()
            }
        );
    }

    #[test]
    fn rings_stop_at_max_depth() {
        let (_dir, tree) = fixture(&[("a/b/c/d/e", 100_000)]);
        let layout = Layout::new(
            &tree,
            tree.root(),
            Options {
                max_depth: 2,
                ..Options::default()
            },
        );
        assert_eq!(layout.rings.len(), 2);
        assert_eq!(layout.arcs().map(|a| a.depth).max(), Some(1));
    }

    #[test]
    fn hit_finds_the_arc_under_the_point() {
        let (_dir, tree) = fixture(&[("a", 300_000), ("b", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let radius = 100.0;
        let (inner, outer) = layout.ring_radii(0, radius);
        let mid = (inner + outer) / 2.0;

        for arc in &layout.rings[0] {
            let angle = arc.start + arc.sweep / 2.0;
            let (x, y) = (mid * angle.sin(), -mid * angle.cos());
            assert_eq!(
                layout.hit(x, y, radius),
                Some(Hit::Arc(arc)),
                "{}",
                named(&tree, arc)
            );
        }
        assert_eq!(layout.hit(0.0, 0.0, radius), Some(Hit::Center));
        assert_eq!(layout.hit(radius, radius, radius), None);
        let (_, outer1) = layout.ring_radii(1, radius);
        assert_eq!(
            layout.hit(0.0, -(outer + outer1) / 2.0, radius),
            None,
            "files have no ring-1 children"
        );
    }

    #[test]
    fn first_arc_starts_at_twelve_oclock_and_runs_clockwise() {
        let (_dir, tree) = fixture(&[("a", 300_000), ("b", 100_000)]);
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let (inner, outer) = layout.ring_radii(0, 100.0);
        let mid = (inner + outer) / 2.0;
        let just_right_of_top = (mid * 0.01_f32.sin(), -mid * 0.01_f32.cos());
        let Some(Hit::Arc(arc)) = layout.hit(just_right_of_top.0, just_right_of_top.1, 100.0)
        else {
            panic!("no arc at top");
        };
        assert_eq!(named(&tree, arc), "a");
    }
}
