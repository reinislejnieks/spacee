use std::f32::consts::{PI, TAU};
use std::time::Instant;

use gpui_kit::*;
use scan::{FileType, Kind, Tree};
use sunburst::{DrawArc, Layout, Target};

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
    arcs: &[DrawArc],
    hovered: Option<Target>,
    dark: bool,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let start = Instant::now();
    let (center, radius) = geometry(bounds);
    if arcs.is_empty() {
        // An empty folder still shows where its chart would be.
        let (inner, outer) = (layout.radius_at(0.0, radius), layout.radius_at(1.0, radius));
        let color = hsla(0.0, 0.0, if dark { 0.18 } else { 0.9 }, 1.0);
        for (a0, a1) in [(0.0, PI), (PI, TAU)] {
            if let Some(path) = sector(center, inner + GAP, outer - GAP, a0, a1) {
                window.paint_path(path, color);
            }
        }
    }
    for arc in arcs {
        let inner = layout.radius_at(arc.inner, radius);
        let outer = layout.radius_at(arc.outer, radius);
        let color = color(tree, arc, hovered == Some(arc.target), dark);
        for (a0, a1) in halves(arc) {
            if let Some(path) = sector(center, inner + GAP, outer - GAP, a0, a1) {
                window.paint_path(path, color);
            }
        }
    }
    if std::env::var_os("SPACEE_PERF").is_some() {
        eprintln!("paint: {} arcs in {:.2?}", arcs.len(), start.elapsed());
    }
}

pub fn color(tree: &Tree, arc: &DrawArc, hovered: bool, dark: bool) -> Hsla {
    let depth = arc.inner;
    let grey = (0.0, 0.0, if dark { 0.35 } else { 0.72 });
    let (hue, saturation, lightness) = match arc.target {
        Target::Smaller { .. } => grey,
        Target::Node(id) => {
            let node = tree.node(id);
            let (hue, saturation) = palette(node.file_type);
            let s = saturation * (1.0 - depth * 0.1);
            let l = if dark {
                0.52 + depth * 0.05
            } else {
                0.50 + depth * 0.07
            };
            match node.kind {
                Kind::Dir => (hue, s, l),
                Kind::Unaccounted => grey,
                Kind::File | Kind::Symlink => (hue, s * 0.55, l),
            }
        }
    };
    let lightness = if hovered { lightness + 0.15 } else { lightness };
    hsla(hue, saturation, lightness.min(0.92), arc.opacity)
}

/// The colour of a top-level folder of this type, for the legend.
pub fn swatch(file_type: FileType, dark: bool) -> Hsla {
    let (hue, saturation) = palette(file_type);
    hsla(hue, saturation, if dark { 0.52 } else { 0.50 }, 1.0)
}

/// Hue and saturation for each type, spaced so neighbours on the wheel stay apart.
fn palette(file_type: FileType) -> (f32, f32) {
    match file_type {
        FileType::Video => (0.92, 0.72),
        FileType::Audio => (0.04, 0.78),
        FileType::Image => (0.12, 0.80),
        FileType::Code => (0.36, 0.55),
        FileType::App => (0.50, 0.65),
        FileType::Document => (0.60, 0.72),
        FileType::Archive => (0.76, 0.55),
        FileType::Other => (0.62, 0.14),
    }
}

/// SVG-style arcs can't span a full circle, so arcs wider than half a turn are drawn in two pieces.
fn halves(arc: &DrawArc) -> Vec<(f32, f32)> {
    let end = arc.start + arc.sweep;
    if arc.sweep > PI {
        let mid = arc.start + arc.sweep / 2.0;
        vec![(arc.start, mid), (mid, end)]
    } else {
        vec![(arc.start, end)]
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
