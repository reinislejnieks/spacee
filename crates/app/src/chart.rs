use std::f32::consts::TAU;
use std::time::Instant;

use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::{FileType, Kind, NodeId, Tree};
use sunburst::{DrawArc, Target};

use crate::browser::format_size;
use crate::style::MONO;

/// Space between neighbouring blocks, in pixels.
const GAP: f32 = 3.0;
const ROOT_HEIGHT: f32 = 40.0;

/// Where an icicle drawn inside `bounds` puts things: a bar for the focused folder across
/// the top, then one row per ring of the layout. An arc's angle becomes its horizontal
/// position, so a ring's arcs fill the row from left to right.
#[derive(Clone, Copy)]
pub struct Geometry {
    bounds: Bounds<Pixels>,
    row: f32,
}

/// What a point in the chart is over.
pub enum Spot {
    Root,
    Cell { depth: usize, angle: f32 },
}

impl Geometry {
    pub fn new(bounds: Bounds<Pixels>, levels: usize) -> Self {
        let rows = (bounds.size.height.as_f32() - ROOT_HEIGHT - GAP).max(0.0);
        Self {
            bounds,
            row: rows / levels as f32,
        }
    }

    fn rows_top(&self) -> f32 {
        self.bounds.top().as_f32() + ROOT_HEIGHT + GAP
    }

    pub fn root_bar(&self) -> Bounds<Pixels> {
        Bounds::new(
            self.bounds.origin,
            size(self.bounds.size.width, px(ROOT_HEIGHT)),
        )
    }

    /// The block for `arc`, already shrunk by the gap to its neighbours. Rings may be
    /// fractional while a zoom animates.
    pub fn block(&self, arc: &DrawArc) -> Bounds<Pixels> {
        let width = self.bounds.size.width.as_f32();
        let x = self.bounds.left().as_f32() + arc.start / TAU * width;
        let y = self.rows_top() + arc.inner * self.row;
        Bounds::new(
            point(px(x), px(y)),
            size(
                px(arc.sweep / TAU * width - GAP),
                px((arc.outer - arc.inner) * self.row - GAP),
            ),
        )
    }

    fn first_row(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(self.bounds.left(), px(self.rows_top())),
            size(self.bounds.size.width - px(GAP), px(self.row - GAP)),
        )
    }

    pub fn spot(&self, position: Point<Pixels>) -> Option<Spot> {
        if !self.bounds.contains(&position) || self.row <= 0.0 {
            return None;
        }
        if self.root_bar().contains(&position) {
            return Some(Spot::Root);
        }
        let y = position.y.as_f32() - self.rows_top();
        let x = (position.x - self.bounds.left()).as_f32();
        (y >= 0.0).then(|| Spot::Cell {
            depth: (y / self.row) as usize,
            angle: x / self.bounds.size.width.as_f32() * TAU,
        })
    }

    /// `bounds` relative to the chart's own origin, for positioning an overlay.
    fn local(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds::new(bounds.origin - self.bounds.origin, bounds.size)
    }
}

pub fn paint(
    tree: &Tree,
    arcs: &[DrawArc],
    hovered: Option<Target>,
    dark: bool,
    geometry: Geometry,
    window: &mut Window,
) {
    let start = Instant::now();
    paint_root_bar(dark, geometry, window);
    if arcs.is_empty() {
        // An empty folder still shows where its chart would be.
        paint_block(geometry.first_row(), grey(dark), 0.12, false, window);
    }
    for arc in arcs {
        let bounds = geometry.block(arc);
        if bounds.size.width < px(1.) || bounds.size.height < px(1.) {
            continue;
        }
        let file = matches!(arc.target, Target::Node(id) if matches!(tree.node(id).kind, Kind::File | Kind::Symlink));
        let depth = arc.inner.max(0.0);
        let alpha = if file {
            0.22 - depth * 0.02
        } else {
            0.34 - depth * 0.045
        };
        let color = color(tree, arc.target, dark).opacity(arc.opacity);
        paint_block(bounds, color, alpha, hovered == Some(arc.target), window);
    }
    if std::env::var_os("SPACEE_PERF").is_some() {
        eprintln!("paint: {} arcs in {:.2?}", arcs.len(), start.elapsed());
    }
}

pub fn paint_root_bar(dark: bool, geometry: Geometry, window: &mut Window) {
    let edge = if dark { white() } else { black() };
    window.paint_quad(quad(
        geometry.root_bar(),
        px(9.),
        linear_gradient(
            180.,
            linear_color_stop(edge.opacity(0.05), 0.),
            linear_color_stop(edge.opacity(0.025), 1.),
        ),
        px(1.),
        edge.opacity(0.08),
        BorderStyle::default(),
    ));
}

/// A block tinted with `color` at roughly `alpha`, lit a little from the top.
fn paint_block(
    bounds: Bounds<Pixels>,
    color: Hsla,
    alpha: f32,
    hovered: bool,
    window: &mut Window,
) {
    let tint = |a: f32| color.opacity(color.a * a.clamp(0.0, 1.0));
    let (top, bottom, edge) = match hovered {
        true => (alpha + 0.18, alpha + 0.12, 0.85),
        false => (alpha + 0.04, alpha - 0.03, 0.22),
    };
    let radius = bounds.size.width.min(bounds.size.height).min(px(14.)) / 2.0;
    window.paint_quad(quad(
        bounds,
        radius.min(px(7.)),
        linear_gradient(
            180.,
            linear_color_stop(tint(top), 0.),
            linear_color_stop(tint(bottom), 1.),
        ),
        px(1.),
        tint(edge),
        BorderStyle::default(),
    ));
}

/// The labels of the blocks big enough to hold one, positioned over the chart.
/// `focus_size` is the size of the folder the whole width stands for.
pub fn labels(
    tree: &Tree,
    arcs: &[DrawArc],
    focus_size: u64,
    geometry: Geometry,
    cx: &App,
) -> Vec<AnyElement> {
    let dark = cx.theme().is_dark();
    arcs.iter()
        .filter_map(|arc| {
            let bounds = geometry.local(geometry.block(arc));
            let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
            if w <= 44.0 || h <= 20.0 {
                return None;
            }
            let fraction = arc.sweep / TAU;
            let (name, bytes) = match arc.target {
                Target::Node(id) => (tree.node(id).name.to_string(), tree.node(id).size),
                Target::Smaller { .. } => (
                    "Smaller objects".to_string(),
                    (fraction as f64 * focus_size as f64) as u64,
                ),
            };
            let percent = (w > 140.0 && h > 46.0).then(|| format!("{:.1}%", fraction * 100.0));
            let size = (w > 60.0 && h > 40.0).then(|| format_size(bytes));
            let accent = color(tree, arc.target, dark);
            Some(
                div()
                    .absolute()
                    .left(bounds.left())
                    .top(bounds.top())
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .pl(px(9.))
                    .pr(px(8.))
                    .pt(px(6.))
                    .pb(px(5.))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .opacity(arc.opacity)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .when_some(percent, |line, percent| {
                                line.child(
                                    div()
                                        .flex_none()
                                        .font_family(MONO)
                                        .text_size(px(10.5))
                                        .text_color(accent)
                                        .child(percent),
                                )
                            }),
                    )
                    .when_some(size, |label, size| {
                        label.child(
                            div()
                                .w_full()
                                .text_right()
                                .truncate()
                                .font_family(MONO)
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(size),
                        )
                    })
                    .into_any_element(),
            )
        })
        .collect()
}

/// The focused folder's name, a detail such as its path, and its size, over the root bar.
pub fn root_label(
    name: String,
    detail: String,
    size: String,
    geometry: Geometry,
    cx: &App,
) -> AnyElement {
    let bounds = geometry.local(geometry.root_bar());
    let muted = cx.theme().muted_foreground;
    h_flex()
        .absolute()
        .left(bounds.left())
        .top(bounds.top())
        .w(bounds.size.width)
        .h(bounds.size.height)
        .px(px(14.))
        .gap(px(10.))
        .child(Icon::new(IconName::Folder).size_4().text_color(muted))
        .child(
            div()
                .flex_none()
                .max_w(relative(0.6))
                .truncate()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(MONO)
                .text_size(px(12.))
                .text_color(muted)
                .child(detail),
        )
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(size),
        )
        .into_any_element()
}

/// The colour of whatever `target` is, by file type, with greys for space that isn't one.
pub fn color(tree: &Tree, target: Target, dark: bool) -> Hsla {
    match target {
        Target::Smaller { .. } => grey(dark),
        Target::Node(id) => node_color(tree, id, dark),
    }
}

pub fn node_color(tree: &Tree, id: NodeId, dark: bool) -> Hsla {
    let node = tree.node(id);
    match node.kind {
        Kind::Unaccounted => grey(dark),
        _ => swatch(node.file_type, dark),
    }
}

pub fn swatch(file_type: FileType, dark: bool) -> Hsla {
    let (hue, saturation) = palette(file_type);
    hsla(hue, saturation, if dark { 0.64 } else { 0.48 }, 1.0)
}

fn grey(dark: bool) -> Hsla {
    hsla(0.6, 0.14, if dark { 0.40 } else { 0.62 }, 1.0)
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
