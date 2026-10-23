use std::collections::HashSet;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
}

impl Arc {
    pub fn end(&self) -> f32 {
        self.start + self.sweep
    }
}

/// An arc ready to paint. Radii are in ring units: ring `n` spans `n..n + 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawArc {
    pub target: Target,
    pub inner: f32,
    pub outer: f32,
    pub start: f32,
    pub sweep: f32,
    pub opacity: f32,
}

impl DrawArc {
    /// Hue in `0..1` from the arc's position around the circle.
    pub fn hue(&self) -> f32 {
        (self.start + self.sweep / 2.0) / TAU
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
        place(tree, focus, 0, 0.0, TAU, &options, &mut rings);
        Self {
            focus,
            options,
            rings,
        }
    }

    pub fn arcs(&self) -> impl Iterator<Item = &Arc> {
        self.rings.iter().flatten()
    }

    pub fn draw_arcs(&self) -> Vec<DrawArc> {
        self.arcs()
            .map(|a| DrawArc {
                target: a.target,
                inner: a.depth as f32,
                outer: a.depth as f32 + 1.0,
                start: a.start,
                sweep: a.sweep,
                opacity: 1.0,
            })
            .collect()
    }

    /// Radius in pixels of the ring-unit position `ring` for a sunburst of outer radius `radius`.
    pub fn radius_at(&self, ring: f32, radius: f32) -> f32 {
        let hole = radius * self.options.hole;
        hole + (radius - hole) * ring / self.options.max_depth as f32
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

fn place(
    tree: &Tree,
    id: NodeId,
    depth: usize,
    start: f32,
    sweep: f32,
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
        rings[depth].push(Arc {
            target: Target::Node(child_id),
            depth,
            start: cursor,
            sweep: child_sweep,
        });
        if child.kind == Kind::Dir {
            place(
                tree,
                child_id,
                depth + 1,
                cursor,
                child_sweep,
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
        });
    }
}

/// The frame `t` (`0..=1`) of the zoom from `from` to `to`, where one focus is an
/// ancestor of the other. The view moves like a camera from one focus to the other.
/// Arcs present in both layouts move continuously; the rest fade, and anything
/// pushed outside the circle is clipped.
pub fn zoom(tree: &Tree, from: &Layout, to: &Layout, t: f32) -> Vec<DrawArc> {
    let (outer, inner, u) = if is_ancestor(tree, from.focus, to.focus) {
        (from, to, t)
    } else if is_ancestor(tree, to.focus, from.focus) {
        (to, from, 1.0 - t)
    } else {
        return to.draw_arcs();
    };
    let focus = span(tree, outer.focus, inner.focus);
    let camera = Span {
        depth: lerp(-1.0, focus.depth, u),
        start: lerp(0.0, focus.start, u),
        sweep: lerp(TAU, focus.sweep, u),
    };
    let max = outer.options.max_depth as f32;
    let shared: HashSet<Target> = inner.arcs().map(|a| a.target).collect();
    let in_outer: HashSet<Target> = outer.arcs().map(|a| a.target).collect();

    let outer_arcs = outer
        .arcs()
        .filter(|a| !shared.contains(&a.target))
        .map(|a| (a, a.depth as f32, a.start, a.sweep, 1.0 - u));
    let inner_arcs = inner.arcs().map(|a| {
        let opacity = if in_outer.contains(&a.target) { 1.0 } else { u };
        let scale = focus.sweep / TAU;
        let start = focus.start + a.start * scale;
        (
            a,
            a.depth as f32 + focus.depth + 1.0,
            start,
            a.sweep * scale,
            opacity,
        )
    });
    outer_arcs
        .chain(inner_arcs)
        .filter_map(|(a, depth, start, sweep, opacity)| {
            let ring = depth - camera.depth - 1.0;
            let to_view =
                |angle: f32| ((angle - camera.start) / camera.sweep * TAU).clamp(0.0, TAU);
            let (s, e) = (to_view(start), to_view(start + sweep));
            let (r0, r1) = (ring.max(0.0), (ring + 1.0).min(max));
            (opacity > 0.0 && e > s && r1 > r0).then_some(DrawArc {
                target: a.target,
                inner: r0,
                outer: r1,
                start: s,
                sweep: e - s,
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

/// Where `descendant` sits in the layout focused on `ancestor`, which itself is ring -1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    depth: f32,
    start: f32,
    sweep: f32,
}

fn span(tree: &Tree, ancestor: NodeId, descendant: NodeId) -> Span {
    let mut path = vec![];
    let mut node = descendant;
    while node != ancestor {
        path.push(node);
        node = tree.node(node).parent.expect("descendant of ancestor");
    }
    let (mut depth, mut start, mut sweep) = (-1.0, 0.0_f64, TAU as f64);
    for &child in path.iter().rev() {
        let parent = tree.node(tree.node(child).parent.unwrap());
        let before: u64 = parent
            .children
            .iter()
            .take_while(|&&c| c != child)
            .map(|&c| tree.node(c).size)
            .sum();
        let size = parent.size.max(1) as f64;
        start += sweep * before as f64 / size;
        sweep *= tree.node(child).size as f64 / size;
        depth += 1.0;
    }
    Span {
        depth,
        start: start as f32,
        sweep: sweep as f32,
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
        let (inner, outer) = (layout.radius_at(0.0, radius), layout.radius_at(1.0, radius));
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
        let outer1 = layout.radius_at(2.0, radius);
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
        let (inner, outer) = (layout.radius_at(0.0, 100.0), layout.radius_at(1.0, 100.0));
        let mid = (inner + outer) / 2.0;
        let just_right_of_top = (mid * 0.01_f32.sin(), -mid * 0.01_f32.cos());
        let Some(Hit::Arc(arc)) = layout.hit(just_right_of_top.0, just_right_of_top.1, 100.0)
        else {
            panic!("no arc at top");
        };
        assert_eq!(named(&tree, arc), "a");
    }

    fn visible(mut arcs: Vec<DrawArc>) -> Vec<(Target, i32, i32, i32, i32)> {
        let q = |v: f32| (v * 1000.0).round() as i32;
        arcs.retain(|a| a.opacity > 0.0);
        assert!(
            arcs.iter().all(|a| a.opacity == 1.0),
            "an endpoint frame has no half-faded arcs"
        );
        let mut out: Vec<_> = arcs
            .iter()
            .map(|a| (a.target, q(a.inner), q(a.outer), q(a.start), q(a.sweep)))
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
                visible(from.draw_arcs())
            );
            assert_eq!(visible(zoom(&tree, from, to, 1.0)), visible(to.draw_arcs()));
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
            visible(root.draw_arcs())
        );
        assert_eq!(
            visible(zoom(&tree, &root, &deep, 1.0)),
            visible(deep.draw_arcs())
        );
    }

    #[test]
    fn mid_zoom_stays_inside_the_circle() {
        let (_dir, tree) = nested();
        let big = child_named(&tree, tree.root(), "big");
        let from = Layout::new(&tree, tree.root(), Options::default());
        let to = Layout::new(&tree, big, Options::default());
        for i in 0..=10 {
            for a in zoom(&tree, &from, &to, i as f32 / 10.0) {
                assert!(a.start >= 0.0 && a.start + a.sweep <= TAU + 1e-5, "{a:?}");
                assert!(
                    a.inner >= 0.0 && a.outer <= 5.0 && a.inner < a.outer,
                    "{a:?}"
                );
            }
        }
    }

    #[test]
    fn zoomed_in_child_sits_where_its_arc_was() {
        let (_dir, tree) = nested();
        let big = child_named(&tree, tree.root(), "big");
        let layout = Layout::new(&tree, tree.root(), Options::default());
        let arc = layout
            .arcs()
            .find(|a| a.target == Target::Node(big))
            .unwrap();
        let s = span(&tree, tree.root(), big);
        assert_eq!((s.depth, s.start), (0.0, arc.start));
        assert!((s.sweep - arc.sweep).abs() < 1e-5);
    }
}
