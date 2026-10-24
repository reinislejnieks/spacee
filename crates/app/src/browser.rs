use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::{FileType, Kind, NodeId, Tree};
use sunburst::{DrawArc, Layout, Options, Target};

use crate::chart::{self, Spot};
use crate::platform;

actions!(browser, [QuickLook]);

const ZOOM: Duration = Duration::from_millis(350);
const ROW_HEIGHT: f32 = 44.0;

/// The sunburst, breadcrumb, children list and Trash tray for one finished scan.
pub struct Browser {
    tree: Tree,
    /// What to call the root: the volume's name, such as "Macintosh HD", or the folder's.
    root_name: String,
    layout: Rc<Layout>,
    zoom: Option<Zoom>,
    hovered: Option<Target>,
    chart_bounds: Option<Bounds<Pixels>>,
    /// Items waiting to be moved to the Trash. None is inside another.
    marked: Vec<NodeId>,
    trash_error: Option<String>,
    full_disk_access: bool,
    focus_handle: FocusHandle,
}

/// An item dragged from the chart or the list toward the Trash tray.
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
    /// The bar for the focused folder, which leads back up to its parent.
    Root,
    Arc(Target),
}

impl Browser {
    pub fn new(tree: Tree, root_name: String, cx: &mut Context<Self>) -> Self {
        let layout = Layout::new(&tree, tree.root(), Options::default());
        Self {
            tree,
            root_name,
            layout: Rc::new(layout),
            zoom: None,
            hovered: None,
            chart_bounds: None,
            marked: vec![],
            trash_error: None,
            full_disk_access: platform::has_full_disk_access(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn root_name(&self) -> String {
        self.root_name.clone()
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    fn display_name(&self, id: NodeId) -> String {
        match id == self.tree.root() {
            true => self.root_name.clone(),
            false => self.tree.node(id).name.to_string(),
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

    fn focus_parent(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.tree.node(self.layout.focus).parent {
            self.focus(parent, cx);
        }
    }

    /// The item on disk that the context menu and Quick Look act on: the hovered item,
    /// else the focused folder.
    fn subject(&self) -> Option<NodeId> {
        match self.hovered.unwrap_or(Target::Node(self.layout.focus)) {
            Target::Node(id) if self.tree.node(id).kind != Kind::Unaccounted => Some(id),
            _ => None,
        }
    }

    fn mark(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tree = &self.tree;
        if id == tree.root()
            || tree.node(id).kind == Kind::Unaccounted
            || self.marked.iter().any(|&c| is_within(tree, id, c))
        {
            return;
        }
        self.marked.retain(|&c| !is_within(tree, c, id));
        self.marked.push(id);
        self.trash_error = None;
        cx.notify();
    }

    fn unmark(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.marked.retain(|&c| c != id);
        self.trash_error = None;
        cx.notify();
    }

    fn confirm_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = match &self.marked[..] {
            [id] => format!("Move “{}” to the Trash?", self.tree.node(*id).name),
            items => format!("Move {} items to the Trash?", items.len()),
        };
        let size = self.marked.iter().map(|&c| self.tree.node(c).size).sum();
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
                this.update(cx, |this, cx| this.trash_marked(cx)).ok();
            }
        })
        .detach();
    }

    /// Trashes the marked items and takes them out of the tree. Items that fail stay
    /// marked, and the first error is shown.
    fn trash_marked(&mut self, cx: &mut Context<Self>) {
        let mut focus = self.layout.focus;
        let mut failed = vec![];
        let mut errors = vec![];
        for id in std::mem::take(&mut self.marked) {
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
        self.marked = failed;
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
            return match self.hovered {
                Some(Target::Node(_)) => menu
                    .label("Unaccounted space")
                    .label("Snapshots, purgeable space, system volumes,")
                    .label("and folders Spacee couldn't read"),
                _ => menu.label("Smaller objects"),
            };
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
            PopupMenuItem::new("Mark for Trash")
                .icon(IconName::Delete)
                .disabled(id == self.tree.root())
                .on_click(move |_, _, cx| this.update(cx, |this, cx| this.mark(id, cx))),
        )
    }

    fn geometry(&self) -> Option<chart::Geometry> {
        let bounds = self.chart_bounds?;
        Some(chart::Geometry::new(bounds, self.layout.options.max_depth))
    }

    fn pick(&self, position: Point<Pixels>) -> Option<Pick> {
        if self.zoom.is_some() {
            return None;
        }
        match self.geometry()?.spot(position)? {
            Spot::Root => Some(Pick::Root),
            Spot::Cell { depth, angle } => {
                Some(Pick::Arc(self.layout.arc_at(depth, angle)?.target))
            }
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
            Some(Pick::Root) => self.focus_parent(cx),
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
        let dragged = self
            .hovered
            .and(self.subject())
            .map(|id| Dragged::new(&self.tree, id));
        let focus = self.layout.focus;
        // Labels are placed by the chart's size on the last frame.
        let overlay = self.geometry().map(|geometry| {
            let size = self.tree.node(focus).size;
            let mut labels = chart::labels(&self.tree, &arcs, size, geometry, cx);
            labels.push(chart::root_label(
                self.display_name(focus),
                self.root_detail(),
                format_size(size),
                geometry,
                cx,
            ));
            labels
        });
        let types: HashSet<FileType> = arcs
            .iter()
            .filter_map(|a| match a.target {
                Target::Node(id) if self.tree.node(id).kind != Kind::Unaccounted => {
                    Some(self.tree.node(id).file_type)
                }
                _ => None,
            })
            .collect();
        let legend = FileType::ALL
            .into_iter()
            .filter(|t| types.contains(t))
            .map(|t| {
                h_flex()
                    .gap_1p5()
                    .child(div().size_2().rounded_full().bg(chart::swatch(t, dark)))
                    .child(t.label())
            });

        v_flex()
            .id("chart")
            .flex_1()
            .min_w(px(240.))
            .h_full()
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .when_some(dragged, |chart, dragged| {
                chart.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        canvas(
                            {
                                let this = this.clone();
                                move |bounds, _, cx| {
                                    this.update(cx, |this, cx| {
                                        if this.chart_bounds != Some(bounds) {
                                            this.chart_bounds = Some(bounds);
                                            cx.notify();
                                        }
                                    })
                                }
                            },
                            move |bounds, _, window, cx| {
                                let this = this.read(cx);
                                let geometry =
                                    chart::Geometry::new(bounds, this.layout.options.max_depth);
                                chart::paint(&this.tree, &arcs, hovered, dark, geometry, window)
                            },
                        )
                        .size_full(),
                    )
                    .children(overlay.into_iter().flatten()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .pt_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .children(legend),
            )
    }

    /// Where the focused folder sits inside the scan, or, for the scan's top folder and
    /// those right inside it, how many files it holds.
    fn root_detail(&self) -> String {
        let focus = self.layout.focus;
        let mut names = vec![];
        let mut node = self.tree.node(focus).parent;
        while let Some(id) = node
            && id != self.tree.root()
        {
            names.push(self.tree.node(id).name.to_string());
            node = self.tree.node(id).parent;
        }
        if names.is_empty() {
            return format_files(self.tree.node(focus).file_count);
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    /// Why the focused folder has nothing to show, if it doesn't.
    fn empty_message(&self) -> Option<&'static str> {
        let focus = self.layout.focus;
        if !self.tree.node(focus).children.is_empty() {
            return None;
        }
        let path = self.tree.path(focus);
        Some(match self.tree.errors.iter().any(|e| e.path == path) {
            true => "Spacee wasn't allowed to read this folder.",
            false => "This folder is empty.",
        })
    }

    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut path = vec![];
        let mut node = Some(self.layout.focus);
        while let Some(id) = node {
            path.push(id);
            node = self.tree.node(id).parent;
        }
        let items = path.into_iter().rev().map(|id| {
            BreadcrumbItem::new(self.display_name(id))
                .on_click(cx.listener(move |this, _, _, cx| this.focus(id, cx)))
        });
        h_flex()
            .px_4()
            .py_2()
            .overflow_x_hidden()
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
        range
            .map(|i| {
                let id = focus.children[i];
                let node = self.tree.node(id);
                let target = Target::Node(id);
                let swatch = chart::node_color(&self.tree, id, dark);
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
                    .when(node.kind != Kind::Unaccounted, |row| {
                        row.on_drag(Dragged::new(&self.tree, id), |d, _, _, cx| {
                            cx.new(|_| d.clone())
                        })
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

    /// Says how many items the scan couldn't read, and offers Full Disk Access when the
    /// app doesn't have it.
    fn render_unreadable(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let count = self.tree.errors.len();
        if count == 0 {
            return None;
        }
        let root = self.tree.node(self.tree.root());
        let unaccounted = root
            .children
            .iter()
            .any(|&c| self.tree.node(c).kind == Kind::Unaccounted);
        let (items, consequence) = match (count == 1, unaccounted) {
            (true, true) => ("item", "It counts toward Unaccounted space."),
            (true, false) => ("item", "It's left out of the totals."),
            (false, true) => ("items", "They count toward Unaccounted space."),
            (false, false) => ("items", "They're left out of the totals."),
        };
        Some(
            h_flex()
                .px_4()
                .py_1p5()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(div().flex_1().min_w_0().child(format!(
                    "{} {items} couldn't be read. {consequence}",
                    format_count(count as u64)
                )))
                .when(!self.full_disk_access, |bar| {
                    bar.child(
                        Button::new("full-disk-access")
                            .small()
                            .label("Grant Full Disk Access…")
                            .on_click(|_, _, _| platform::open_full_disk_access_settings()),
                    )
                }),
        )
    }

    fn render_marked(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.marked.iter().enumerate().map(|(i, &id)| {
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
                    Button::new(("unmark", i))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(move |this, _, _, cx| this.unmark(id, cx))),
                )
        });
        let total = self.marked.iter().map(|&c| self.tree.node(c).size).sum();
        let hint = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Drag items here, or right-click them and choose Mark for Trash");

        h_flex()
            .id("marked")
            .flex_none()
            .h(px(52.))
            .px_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .drag_over::<Dragged>(|style, _, _, cx| style.bg(cx.theme().accent))
            .on_drop(cx.listener(|this, d: &Dragged, _, cx| this.mark(d.id, cx)))
            .when(
                self.marked.is_empty() && self.trash_error.is_none(),
                |tray| tray.child(hint),
            )
            .when(!self.marked.is_empty(), |tray| {
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
            .when(!self.marked.is_empty(), |tray| {
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
            .when_some(self.render_unreadable(cx), |this, notice| {
                this.child(notice)
            })
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
                            .w(relative(0.38))
                            .min_w(px(220.))
                            .max_w(px(320.))
                            .h_full()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .py_1()
                            .map(|list| match self.empty_message() {
                                Some(message) => list.child(
                                    v_flex()
                                        .size_full()
                                        .items_center()
                                        .justify_center()
                                        .gap_2()
                                        .px_6()
                                        .text_sm()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(Icon::new(IconName::Inbox).size_6())
                                        .child(message),
                                ),
                                None => list.child(
                                    uniform_list(
                                        "children",
                                        count,
                                        cx.processor(Self::render_rows),
                                    )
                                    .size_full(),
                                ),
                            }),
                    )
                    .context_menu(move |menu, _, cx| {
                        this.read(cx).context_menu(menu, this.clone())
                    }),
            )
            .child(self.render_marked(cx))
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

pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Rounded, such as "412K files", where the exact count doesn't matter.
pub fn format_files(n: u32) -> String {
    let count = match n {
        0..10_000 => format_count(n.into()),
        10_000..1_000_000 => format!("{:.0}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    };
    format!("{count} {}", if n == 1 { "file" } else { "files" })
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
