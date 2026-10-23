use std::f32::consts::PI;
use std::time::Instant;

use gpui_kit::*;
use scan::{Kind, Tree};
use sunburst::{Arc, Layout, Target};

/// Gap between neighbouring arcs, in pixels.
const GAP: f32 = 0.75;
const PADDING: f32 = 24.0;

/// Center and outer radius of the sunburst drawn inside `bounds`.
pub fn geometry(bounds: Bounds<Pixels>) -> (Point<Pixels>, f32) {
    let radius = (bounds.size.width.min(bounds.size.height) / 2.0).as_f32() - PADDING;
    (bounds.center(), radius.max(0.0))
}

pub fn paint(
    tree: &Tree,
    layout: &Layout,
    hovered: Option<Target>,
    dark: bool,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let start = Instant::now();
    let (center, radius) = geometry(bounds);
    for arc in layout.arcs() {
        let (inner, outer) = layout.ring_radii(arc.depth, radius);
        let color = color(tree, arc, hovered == Some(arc.target), dark);
        for (a0, a1) in halves(arc) {
            if let Some(path) = sector(center, inner + GAP, outer - GAP, a0, a1) {
                window.paint_path(path, color);
            }
        }
    }
    if std::env::var_os("SPACEE_PERF").is_some() {
        eprintln!(
            "paint: {} arcs in {:.2?}",
            layout.arcs().count(),
            start.elapsed()
        );
    }
}

fn color(tree: &Tree, arc: &Arc, hovered: bool, dark: bool) -> Hsla {
    let depth = arc.depth as f32;
    let (saturation, lightness) = match arc.target {
        Target::Smaller { .. } => (0.0, if dark { 0.35 } else { 0.72 }),
        Target::Node(id) => {
            let s = 0.72 - depth * 0.07;
            let l = if dark {
                0.52 + depth * 0.05
            } else {
                0.50 + depth * 0.07
            };
            match tree.node(id).kind {
                Kind::Dir => (s, l),
                _ => (s * 0.45, l),
            }
        }
    };
    let lightness = if hovered { lightness + 0.15 } else { lightness };
    hsla(arc.hue, saturation, lightness.min(0.92), 1.0)
}

/// SVG-style arcs can't span a full circle, so arcs wider than half a turn are drawn in two pieces.
fn halves(arc: &Arc) -> Vec<(f32, f32)> {
    if arc.sweep > PI {
        let mid = arc.start + arc.sweep / 2.0;
        vec![(arc.start, mid), (mid, arc.end())]
    } else {
        vec![(arc.start, arc.end())]
    }
}

fn sector(center: Point<Pixels>, inner: f32, outer: f32, a0: f32, a1: f32) -> Option<Path<Pixels>> {
    let gap_in = GAP / inner;
    let gap_out = GAP / outer;
    if a1 - a0 <= 2.0 * gap_in || outer <= inner {
        return None;
    }
    let at = |r: f32, a: f32| point(center.x + px(r * a.sin()), center.y - px(r * a.cos()));
    let large = a1 - a0 > PI;
    let mut path = PathBuilder::fill();
    path.move_to(at(outer, a0 + gap_out));
    path.arc_to(
        point(px(outer), px(outer)),
        px(0.),
        large,
        true,
        at(outer, a1 - gap_out),
    );
    path.line_to(at(inner, a1 - gap_in));
    path.arc_to(
        point(px(inner), px(inner)),
        px(0.),
        large,
        false,
        at(inner, a0 + gap_in),
    );
    path.close();
    path.build().ok()
}
