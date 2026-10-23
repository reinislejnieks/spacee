use std::f32::consts::TAU;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::{Kind, NodeId, Tree};
use sunburst::{DrawArc, Hit, Layout, Options, Target};

use crate::{chart, platform};

actions!(browser, [QuickLook]);

const ZOOM: Duration = Duration::from_millis(350);
const ROW_HEIGHT: f32 = 44.0;

/// The sunburst, breadcrumb, children list and collector for one finished scan.
pub struct Browser {
    tree: Tree,
    layout: Rc<Layout>,
    zoom: Option<Zoom>,
    hovered: Option<Target>,
    chart_bounds: Option<Bounds<Pixels>>,
    /// Items waiting to be moved to the Trash. None is inside another.
    collector: Vec<NodeId>,
    trash_error: Option<String>,
    focus_handle: FocusHandle,
}

/// An item dragged from the chart or the list toward the collector.
#[derive(Clone)]
struct Dragged {
    id: NodeId,
    name: SharedString,
    size: u64,
}

impl Dragged {
    fn new(tree: &Tree, id: NodeId) -> Self {
        Self {
            id,
            name: tree.node(id).name.to_string().into(),
            size: tree.node(id).size,
        }
    }
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_sm()
            .child(self.name.clone())
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format_size(self.size)),
            )
    }
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
    pub fn new(tree: Tree, cx: &mut Context<Self>) -> Self {
        let layout = Layout::new(&tree, tree.root(), Options::default());
        Self {
            tree,
            layout: Rc::new(layout),
            zoom: None,
            hovered: None,
            chart_bounds: None,
            collector: vec![],
            trash_error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn root_name(&self) -> String {
        display_name(&self.tree, self.tree.root())
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

    /// What the context menu and Quick Look act on: the hovered item, else the focused folder.
    fn subject(&self) -> Option<NodeId> {
        match self.hovered.unwrap_or(Target::Node(self.layout.focus)) {
            Target::Node(id) => Some(id),
            Target::Smaller { .. } => None,
        }
    }

    fn collect(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tree = &self.tree;
        if id == tree.root() || self.collector.iter().any(|&c| is_within(tree, id, c)) {
            return;
        }
        self.collector.retain(|&c| !is_within(tree, c, id));
        self.collector.push(id);
        self.trash_error = None;
        cx.notify();
    }

    fn uncollect(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.collector.retain(|&c| c != id);
        self.trash_error = None;
        cx.notify();
    }

    fn confirm_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = match &self.collector[..] {
            [id] => format!("Move “{}” to the Trash?", self.tree.node(*id).name),
            items => format!("Move {} items to the Trash?", items.len()),
        };
        let size = self.collector.iter().map(|&c| self.tree.node(c).size).sum();
        let detail = format!("They take {} on disk.", format_size(size));
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.trash_collected(cx)).ok();
            }
        })
        .detach();
    }

    /// Trashes the collected items and takes them out of the tree. Items that fail stay
    /// collected, and the first error is shown.
    fn trash_collected(&mut self, cx: &mut Context<Self>) {
        let mut focus = self.layout.focus;
        let mut failed = vec![];
        let mut errors = vec![];
        for id in std::mem::take(&mut self.collector) {
            match platform::trash(&self.tree.path(id)) {
                Ok(()) => {
                    if is_within(&self.tree, focus, id) {
                        focus = self.tree.node(id).parent.unwrap();
                    }
                    self.tree.remove(id);
                }
                Err(e) => {
                    errors.push(format!("{}: {e}", self.tree.node(id).name));
                    failed.push(id);
                }
            }
        }
        self.collector = failed;
        self.trash_error = match &errors[..] {
            [] => None,
            [e] => Some(format!("Couldn't move to the Trash. {e}")),
            [e, ..] => Some(format!(
                "Couldn't move {} items to the Trash. {e}",
                errors.len()
            )),
        };
        self.layout = Rc::new(Layout::new(&self.tree, focus, Options::default()));
        self.zoom = None;
        self.hovered = None;
        cx.notify();
    }

    fn quick_look(&mut self, _: &QuickLook, _: &mut Window, _: &mut Context<Self>) {
        if let Some(id) = self.subject() {
            platform::quick_look(&self.tree.path(id));
        }
    }

    fn context_menu(&self, menu: PopupMenu, this: Entity<Self>) -> PopupMenu {
        let Some(id) = self.subject() else {
            return menu.label("Smaller objects");
        };
        let path = self.tree.path(id);
        menu.item(
            PopupMenuItem::new("Reveal in Finder")
                .icon(IconName::FolderOpen)
                .on_click({
                    let path = path.clone();
                    move |_, _, _| platform::reveal(&path)
                }),
        )
        .item(
            PopupMenuItem::new("Quick Look")
                .icon(IconName::Eye)
                .on_click({
                    let path = path.clone();
                    move |_, _, _| platform::quick_look(&path)
                }),
        )
        .item(
            PopupMenuItem::new("Copy Path")
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ))
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new("Add to Collector")
                .icon(IconName::Delete)
                .disabled(id == self.tree.root())
                .on_click(move |_, _, cx| this.update(cx, |this, cx| this.collect(id, cx))),
        )
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
        if cx.has_active_drag() {
            return;
        }
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
        let hovered = self.hovered_arc();
        let dark = cx.theme().is_dark();
        let this = cx.entity();
        let (title, size) = describe(
            &self.tree,
            &self.layout,
            self.hovered.unwrap_or(Target::Node(self.layout.focus)),
        );
        let dragged = match self.hovered {
            Some(Target::Node(id)) => Some(Dragged::new(&self.tree, id)),
            _ => None,
        };

        div()
            .id("chart")
            .relative()
            .flex_1()
            .h_full()
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .when_some(dragged, |chart, dragged| {
                chart.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
            })
            .child(
                canvas(
                    {
                        let this = this.clone();
                        move |bounds, _, cx| {
                            this.update(cx, |this, _| this.chart_bounds = Some(bounds))
                        }
                    },
                    move |bounds, _, window, cx| {
                        let this = this.read(cx);
                        let (tree, layout) = (&this.tree, &this.layout);
                        chart::paint(tree, layout, &arcs, hovered, dark, bounds, window)
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
                    .on_drag(Dragged::new(&self.tree, id), |d, _, _, cx| {
                        cx.new(|_| d.clone())
                    })
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

    fn render_collector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.collector.iter().enumerate().map(|(i, &id)| {
            let node = self.tree.node(id);
            h_flex()
                .flex_none()
                .gap_1()
                .pl_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .text_sm()
                .child(
                    div()
                        .max_w(px(160.))
                        .truncate()
                        .child(node.name.to_string()),
                )
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(format_size(node.size)),
                )
                .child(
                    Button::new(("uncollect", i))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(move |this, _, _, cx| this.uncollect(id, cx))),
                )
        });
        let total = self.collector.iter().map(|&c| self.tree.node(c).size).sum();
        let hint = div()
            .flex_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Drag items here, or right-click them, to collect them for the Trash");

        h_flex()
            .id("collector")
            .flex_none()
            .h(px(52.))
            .px_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .drag_over::<Dragged>(|style, _, _, cx| style.bg(cx.theme().accent))
            .on_drop(cx.listener(|this, d: &Dragged, _, cx| this.collect(d.id, cx)))
            .when(
                self.collector.is_empty() && self.trash_error.is_none(),
                |tray| tray.child(hint),
            )
            .when(!self.collector.is_empty(), |tray| {
                tray.child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .overflow_x_hidden()
                        .children(chips),
                )
            })
            .when_some(self.trash_error.clone(), |tray, error| {
                tray.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(!self.collector.is_empty(), |tray| {
                tray.child(div().flex_none().text_sm().child(format_size(total)))
                    .child(
                        Button::new("trash")
                            .danger()
                            .small()
                            .icon(IconName::Delete)
                            .label("Move to Trash")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_trash(window, cx)),
                            ),
                    )
            })
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.tree.node(self.layout.focus).children.len();
        let this = cx.entity();
        v_flex()
            .size_full()
            .key_context("Browser")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::quick_look))
            // Hovering is enough to aim the space bar, so take focus without a click.
            .on_mouse_move(cx.listener(|this, _, window, cx| {
                if !this.focus_handle.is_focused(window) {
                    window.focus(&this.focus_handle, cx);
                }
            }))
            .child(self.render_breadcrumb(cx))
            .child(
                h_flex()
                    .id("content")
                    .flex_1()
                    .min_h_0()
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
                    )
                    .context_menu(move |menu, _, cx| {
                        this.read(cx).context_menu(menu, this.clone())
                    }),
            )
            .child(self.render_collector(cx))
    }
}

/// Whether `id` is `ancestor` or lies inside it.
fn is_within(tree: &Tree, id: NodeId, ancestor: NodeId) -> bool {
    let mut cur = Some(id);
    while let Some(id) = cur {
        if id == ancestor {
            return true;
        }
        cur = tree.node(id).parent;
    }
    false
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
pub fn format_size(bytes: u64) -> String {
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
