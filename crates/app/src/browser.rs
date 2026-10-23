use std::f32::consts::TAU;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::{Kind, NodeId, Tree};
use sunburst::{DrawArc, Hit, Layout, Options, Target};

use crate::chart;

const ZOOM: Duration = Duration::from_millis(350);
const ROW_HEIGHT: f32 = 44.0;

/// The sunburst, breadcrumb and children list for one finished scan.
pub struct Browser {
    tree: Rc<Tree>,
    layout: Rc<Layout>,
    zoom: Option<Zoom>,
    hovered: Option<Target>,
    chart_bounds: Option<Bounds<Pixels>>,
}

struct Zoom {
    from: Rc<Layout>,
    started: Instant,
}

/// What the pointer is over in the chart.
enum Pick {
    Center,
    Arc(Target),
}

impl Browser {
    pub fn new(tree: Tree) -> Self {
        let layout = Layout::new(&tree, tree.root(), Options::default());
        Self {
            tree: Rc::new(tree),
            layout: Rc::new(layout),
            zoom: None,
            hovered: None,
            chart_bounds: None,
        }
    }

    fn focus(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if id == self.layout.focus || self.tree.node(id).kind != Kind::Dir {
            return;
        }
        let to = Layout::new(&self.tree, id, Options::default());
        let from = std::mem::replace(&mut self.layout, Rc::new(to));
        self.zoom = Some(Zoom {
            from,
            started: Instant::now(),
        });
        self.hovered = None;
        cx.notify();
    }

    fn pick(&self, position: Point<Pixels>) -> Option<Pick> {
        if self.zoom.is_some() {
            return None;
        }
        let (center, radius) = chart::geometry(self.chart_bounds?);
        let offset = position - center;
        match self
            .layout
            .hit(offset.x.as_f32(), offset.y.as_f32(), radius)?
        {
            Hit::Center => Some(Pick::Center),
            Hit::Arc(arc) => Some(Pick::Arc(arc.target)),
        }
    }

    fn set_hovered(&mut self, hovered: Option<Target>, cx: &mut Context<Self>) {
        if hovered != self.hovered {
            self.hovered = hovered;
            cx.notify();
        }
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let hovered = match self.pick(e.position) {
            Some(Pick::Arc(target)) => Some(target),
            _ => None,
        };
        self.set_hovered(hovered, cx);
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.pick(e.position) {
            Some(Pick::Center) => {
                if let Some(parent) = self.tree.node(self.layout.focus).parent {
                    self.focus(parent, cx);
                }
            }
            Some(Pick::Arc(Target::Node(id))) => self.focus(id, cx),
            _ => {}
        }
    }

    /// The arcs to paint this frame, advancing the zoom animation if one is running.
    fn frame(&mut self, window: &mut Window) -> Vec<DrawArc> {
        let Some(zoom) = &self.zoom else {
            return self.layout.draw_arcs();
        };
        let t = zoom.started.elapsed().as_secs_f32() / ZOOM.as_secs_f32();
        if t >= 1.0 {
            self.zoom = None;
            return self.layout.draw_arcs();
        }
        window.request_animation_frame();
        let eased = 1.0 - (1.0 - t).powi(3);
        sunburst::zoom(&self.tree, &zoom.from, &self.layout, eased)
    }

    /// A list row's child may be merged into "smaller objects"; highlight that arc instead.
    fn hovered_arc(&self) -> Option<Target> {
        let hovered = self.hovered?;
        if self.layout.arcs().any(|a| a.target == hovered) {
            return Some(hovered);
        }
        match hovered {
            Target::Node(id) if self.tree.node(id).parent == Some(self.layout.focus) => {
                Some(Target::Smaller {
                    parent: self.layout.focus,
                })
            }
            _ => None,
        }
    }

    fn render_chart(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let arcs = self.frame(window);
        let tree = self.tree.clone();
        let layout = self.layout.clone();
        let hovered = self.hovered_arc();
        let dark = cx.theme().is_dark();
        let this = cx.entity();
        let (title, size) = describe(
            &tree,
            &layout,
            self.hovered.unwrap_or(Target::Node(layout.focus)),
        );

        div()
            .id("chart")
            .relative()
            .flex_1()
            .h_full()
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, _| this.chart_bounds = Some(bounds))
                    },
                    move |bounds, _, window, _| {
                        chart::paint(&tree, &layout, &arcs, hovered, dark, bounds, window)
                    },
                )
                .size_full(),
            )
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .items_center()
                            .max_w(px(180.))
                            .child(div().text_sm().truncate().child(title))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(size),
                            ),
                    ),
            )
    }

    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut path = vec![];
        let mut node = Some(self.layout.focus);
        while let Some(id) = node {
            path.push(id);
            node = self.tree.node(id).parent;
        }
        let items = path.into_iter().rev().map(|id| {
            BreadcrumbItem::new(display_name(&self.tree, id))
                .on_click(cx.listener(move |this, _, _, cx| this.focus(id, cx)))
        });
        h_flex()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Breadcrumb::new().children(items))
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let focus = self.tree.node(self.layout.focus);
        let dark = cx.theme().is_dark();
        let ring = &self.layout.rings[0];
        let smaller = Target::Smaller {
            parent: self.layout.focus,
        };
        range
            .map(|i| {
                let id = focus.children[i];
                let node = self.tree.node(id);
                let target = Target::Node(id);
                let arc = ring
                    .iter()
                    .find(|a| a.target == target)
                    .or_else(|| ring.iter().find(|a| a.target == smaller))
                    .map(|a| DrawArc {
                        target: a.target,
                        inner: 0.0,
                        outer: 1.0,
                        start: a.start,
                        sweep: a.sweep,
                        opacity: 1.0,
                    });
                let swatch = arc.map_or(cx.theme().muted, |a| {
                    chart::color(&self.tree, &a, false, dark)
                });
                let fraction = node.size as f32 / focus.size.max(1) as f32;
                let hovered = self.hovered == Some(target);

                div()
                    .id(("row", i))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1()
                    .when(hovered, |row| row.bg(cx.theme().accent))
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        if *hovering {
                            this.set_hovered(Some(target), cx);
                        } else if this.hovered == Some(target) {
                            this.set_hovered(None, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.focus(id, cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(div().size_2p5().rounded_full().flex_none().bg(swatch))
                            .child(div().flex_1().truncate().child(node.name.to_string()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format_size(node.size)),
                            ),
                    )
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded_full()
                            .bg(cx.theme().muted)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded_full()
                                    .bg(swatch),
                            ),
                    )
            })
            .collect()
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.tree.node(self.layout.focus).children.len();
        v_flex()
            .size_full()
            .child(self.render_breadcrumb(cx))
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .items_start()
                    .child(self.render_chart(window, cx))
                    .child(
                        div()
                            .w(px(320.))
                            .h_full()
                            .flex_none()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .py_1()
                            .child(
                                uniform_list("children", count, cx.processor(Self::render_rows))
                                    .size_full(),
                            ),
                    ),
            )
    }
}

fn display_name(tree: &Tree, id: NodeId) -> String {
    let node = tree.node(id);
    match node.parent {
        Some(_) => node.name.to_string(),
        None => tree.path(id).file_name().map_or_else(
            || node.name.to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

fn describe(tree: &Tree, layout: &Layout, target: Target) -> (String, String) {
    match target {
        Target::Node(id) => (display_name(tree, id), format_size(tree.node(id).size)),
        Target::Smaller { .. } => {
            let sweep = layout
                .arcs()
                .find(|a| a.target == target)
                .map_or(0.0, |a| a.sweep);
            let focus = tree.node(layout.focus).size;
            let bytes = (sweep / TAU) as f64 * focus as f64;
            ("Smaller objects".into(), format_size(bytes as u64))
        }
    }
}

/// Decimal units, as Finder reports sizes.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} bytes"),
        _ if value < 10.0 => format!("{value:.1} {}", UNITS[unit]),
        _ => format!("{value:.0} {}", UNITS[unit]),
    }
}
