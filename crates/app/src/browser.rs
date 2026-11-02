use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as AppIcon;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use icicle::{DrawBlock, Layout, Options, Target};
use scan::{FileType, Kind, NodeId, Tree};

use crate::chart::{self, Spot};
use crate::platform;
use crate::style::{self, MONO, Palette, Tone};

actions!(browser, [GoUp, QuickLook]);

const ZOOM: Duration = Duration::from_millis(350);
/// A row of the children list, with the gap below it.
pub const ROW_HEIGHT: f32 = 70.0;

/// The icicle, breadcrumb, children list and Trash tray for one finished scan.
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
        let p = Palette::of(cx);
        h_flex()
            .gap(px(9.))
            .px(px(11.))
            .py(px(5.))
            .rounded(px(9.))
            .border_1()
            .border_color(p.line_strong)
            .bg(style::gradient(p.raised))
            .text_color(p.fg)
            .text_size(px(13.))
            .child(self.name.clone())
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(11.5))
                    .text_color(p.mint)
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
    Block(Target),
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

    fn clear_marked(&mut self, cx: &mut Context<Self>) {
        self.marked.clear();
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
            Spot::Cell { depth, x } => Some(Pick::Block(self.layout.block_at(depth, x)?.target)),
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
            Some(Pick::Block(target)) => Some(target),
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
            Some(Pick::Block(Target::Node(id))) => self.focus(id, cx),
            _ => {}
        }
    }

    /// The blocks to paint this frame, advancing the zoom animation if one is running.
    fn frame(&mut self, window: &mut Window) -> Vec<DrawBlock> {
        let Some(zoom) = &self.zoom else {
            return self.layout.draw_blocks();
        };
        let t = zoom.started.elapsed().as_secs_f32() / ZOOM.as_secs_f32();
        if t >= 1.0 {
            self.zoom = None;
            return self.layout.draw_blocks();
        }
        window.request_animation_frame();
        let eased = 1.0 - (1.0 - t).powi(3);
        icicle::zoom(&self.tree, &zoom.from, &self.layout, eased)
    }

    /// A list row's child may be merged into "smaller objects"; highlight that block instead.
    fn hovered_block(&self) -> Option<Target> {
        let hovered = self.hovered?;
        if self.layout.blocks().any(|b| b.target == hovered) {
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

    fn render_chart(&mut self, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let blocks = self.frame(window);
        let hovered = self.hovered_block();
        let dark = p.dark;
        let this = cx.entity();
        let dragged = self
            .hovered
            .and(self.subject())
            .map(|id| Dragged::new(&self.tree, id));
        let focus = self.layout.focus;
        // Labels are placed by the chart's size on the last frame.
        let overlay = self.geometry().map(|geometry| {
            let size = self.tree.node(focus).size;
            let mut labels = chart::labels(&self.tree, &blocks, size, geometry, cx);
            labels.push(chart::root_label(
                self.display_name(focus),
                self.root_detail(),
                format_size(size),
                geometry,
                cx,
            ));
            labels
        });
        let types = file_types(&self.tree, &blocks);
        chart_card(
            "Click to zoom · Esc to go up",
            types,
            p,
            div()
                .id("chart")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .on_mouse_move(cx.listener(Self::mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                .when_some(dragged, |chart, dragged| {
                    chart.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                })
                .child(
                    canvas(
                        {
                            let this = this.clone();
                            move |bounds, window, cx| {
                                let moved = this.update(cx, |this, _| {
                                    this.chart_bounds.replace(bounds) != Some(bounds)
                                });
                                // A notify while painting is lost; render again next
                                // frame so the labels follow.
                                if moved {
                                    window.request_animation_frame();
                                }
                            }
                        },
                        move |bounds, _, window, cx| {
                            let this = this.read(cx);
                            let geometry =
                                chart::Geometry::new(bounds, this.layout.options.max_depth);
                            chart::paint(&this.tree, &blocks, hovered, dark, geometry, window)
                        },
                    )
                    .size_full(),
                )
                .children(overlay.into_iter().flatten()),
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

    /// The bytes `target` takes, working out the merged "smaller objects" from its block.
    fn size_of(&self, target: Target) -> u64 {
        match target {
            Target::Node(id) => self.tree.node(id).size,
            Target::Smaller { .. } => {
                let width = self
                    .layout
                    .blocks()
                    .find(|b| b.target == target)
                    .map_or(0.0, |b| b.width);
                let focus = self.tree.node(self.layout.focus).size;
                (width as f64 * focus as f64) as u64
            }
        }
    }

    fn render_breadcrumb(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let focus = self.layout.focus;
        let mut path = vec![];
        let mut node = Some(focus);
        while let Some(id) = node {
            path.push(id);
            node = self.tree.node(id).parent;
        }
        path.reverse();
        let mut crumbs = vec![];
        for (i, &id) in path.iter().enumerate() {
            if i > 0 {
                crumbs.push(
                    div()
                        .text_size(px(12.))
                        .text_color(p.faint)
                        .child("›")
                        .into_any_element(),
                );
            }
            let current = id == focus;
            let icon: Option<IconName> = match (i, current) {
                (0, _) => Some(IconName::HardDrive),
                (_, true) => Some(IconName::Folder),
                _ => None,
            };
            let (wash, fg) = (p.wash, p.fg);
            crumbs.push(
                h_flex()
                    .id(("crumb", i))
                    .flex_none()
                    .gap(px(7.))
                    .px(px(9.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(transparent_black())
                    .text_color(p.caption)
                    .cursor_pointer()
                    .when_else(
                        current,
                        |crumb| {
                            crumb
                                .bg(p.mint.opacity(0.09))
                                .border_color(p.mint.opacity(0.2))
                                .text_color(p.mint)
                                .font_weight(FontWeight::MEDIUM)
                        },
                        |crumb| crumb.hover(move |s| s.bg(wash.opacity(0.04)).text_color(fg)),
                    )
                    .when_some(icon, |crumb, icon| crumb.child(Icon::new(icon).size_4()))
                    .child(
                        div()
                            .max_w(px(220.))
                            .truncate()
                            .child(self.display_name(id)),
                    )
                    .when(current && i > 0, |crumb| {
                        crumb.child(style::tag(
                            format_size(self.tree.node(id).size),
                            Tone::Mint,
                            p,
                        ))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.focus(id, cx)))
                    .into_any_element(),
            );
        }

        let shown = self.hovered.unwrap_or(Target::Node(focus));
        let (color, name, files) = match shown {
            Target::Node(id) if id == focus => (
                p.mint,
                self.display_name(id),
                Some(self.tree.node(id).file_count),
            ),
            Target::Node(id) => {
                let node = self.tree.node(id);
                let files = (node.kind == Kind::Dir).then_some(node.file_count);
                (
                    chart::node_color(&self.tree, id, p.dark),
                    self.display_name(id),
                    files,
                )
            }
            Target::Smaller { .. } => (
                chart::color(&self.tree, shown, p.dark),
                "Smaller objects".into(),
                None,
            ),
        };
        let bytes = self.size_of(shown);
        let percent = (shown != Target::Node(focus))
            .then(|| bytes as f64 / self.tree.node(focus).size.max(1) as f64 * 100.0);
        let separator = || div().text_color(p.faint).child("·");
        let pill = h_flex()
            .flex_none()
            .max_w(relative(0.6))
            .ml_auto()
            .gap(px(9.))
            .px_3()
            .py(px(6.))
            .rounded_full()
            .border_1()
            .border_color(p.line)
            .bg(p.inset)
            .text_size(px(12.))
            .overflow_hidden()
            .child(style::dot(color, px(7.)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name.to_uppercase()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(9.))
                    .font_family(MONO)
                    .text_color(p.dim)
                    .child(separator())
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.mint)
                            .child(format_size(bytes)),
                    )
                    .when_some(files, |values, files| {
                        values.child(separator()).child(format_files(files))
                    })
                    .when_some(percent, |values, percent| {
                        values.child(separator()).child(format!("{percent:.1}%"))
                    }),
            );

        style::card(p)
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_3()
            .py(px(9.))
            .min_h(px(56.))
            .children(crumbs)
            .child(pill)
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let p = Palette::of(cx);
        let focus = self.tree.node(self.layout.focus);
        range
            .map(|i| {
                let id = focus.children[i];
                let node = self.tree.node(id);
                let target = Target::Node(id);
                let unaccounted = node.kind == Kind::Unaccounted;
                div()
                    .id(("row", i))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .pb(px(6.))
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        if *hovering {
                            this.set_hovered(Some(target), cx);
                        } else if this.hovered == Some(target) {
                            this.set_hovered(None, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.focus(id, cx)))
                    .when(!unaccounted, |row| {
                        row.on_drag(Dragged::new(&self.tree, id), |d, _, _, cx| {
                            cx.new(|_| d.clone())
                        })
                    })
                    .child(
                        row(
                            &self.tree,
                            id,
                            focus.size,
                            self.hovered == Some(target),
                            false,
                            &p,
                        )
                        .cursor_pointer(),
                    )
            })
            .collect()
    }

    fn render_list(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let focus = self.layout.focus;
        let count = self.tree.node(focus).children.len();
        list_card(
            &self.display_name(focus),
            count,
            p,
            match self.empty_message() {
                Some(message) => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_6()
                    .text_size(px(13.))
                    .text_center()
                    .text_color(p.dim)
                    .child(Icon::new(IconName::Inbox).size_6())
                    .child(message)
                    .into_any_element(),
                None => uniform_list("children", count, cx.processor(Self::render_rows))
                    .flex_1()
                    .min_h_0()
                    .into_any_element(),
            },
        )
    }

    fn render_stats(&self, p: &Palette) -> Div {
        let focus = self.tree.node(self.layout.focus);
        let largest = focus
            .children
            .iter()
            .copied()
            .find(|&c| self.tree.node(c).kind != Kind::Unaccounted);
        let marked: u64 = self.marked.iter().map(|&c| self.tree.node(c).size).sum();
        let count = self.marked.len();
        stats([
            stat(
                "This folder",
                format_size(focus.size),
                p.fg,
                format_files(focus.file_count),
                p,
            ),
            match largest {
                Some(id) => {
                    let node = self.tree.node(id);
                    stat(
                        "Largest inside",
                        format_size(node.size),
                        chart::node_color(&self.tree, id, p.dark),
                        format!(
                            "{} · {:.0}%",
                            node.name,
                            node.size as f64 / focus.size.max(1) as f64 * 100.0
                        ),
                        p,
                    )
                }
                None => stat(
                    "Largest inside",
                    "—".into(),
                    p.faint,
                    "Nothing inside".into(),
                    p,
                ),
            },
            stat(
                "Marked for trash",
                format_size(marked),
                if count > 0 { p.mint } else { p.faint },
                format!("{count} {}", if count == 1 { "item" } else { "items" }),
                p,
            ),
        ])
    }

    /// Says how many items the scan couldn't read, and offers Full Disk Access when the
    /// app doesn't have it.
    fn render_unreadable(&self, p: &Palette) -> Option<Div> {
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
        Some(crate::notice(
            format!("{} {items} couldn't be read", format_count(count as u64)).into(),
            format!("macOS keeps some folders private. {consequence}").into(),
            !self.full_disk_access,
            p,
        ))
    }

    fn render_marked(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let chips = self.marked.iter().enumerate().map(|(i, &id)| {
            let node = self.tree.node(id);
            let icon: IconName = match node.kind {
                Kind::Dir => IconName::Folder,
                _ => IconName::File,
            };
            let (wash, fg) = (p.wash, p.fg);
            h_flex()
                .flex_none()
                .gap(px(9.))
                .pl(px(11.))
                .pr(px(6.))
                .py(px(5.))
                .rounded(px(9.))
                .border_1()
                .border_color(p.line_strong)
                .bg(p.wash.opacity(0.03))
                .text_size(px(13.))
                .child(
                    Icon::new(icon)
                        .size_4()
                        .text_color(chart::node_color(&self.tree, id, p.dark)),
                )
                .child(
                    div()
                        .max_w(px(200.))
                        .truncate()
                        .child(node.name.to_string()),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(p.mint)
                        .child(format_size(node.size)),
                )
                .child(
                    div()
                        .id(("unmark", i))
                        .px_1()
                        .rounded(px(4.))
                        .text_color(p.faint)
                        .cursor_pointer()
                        .hover(move |s| s.bg(wash.opacity(0.06)).text_color(fg))
                        .child(Icon::new(IconName::Close).size_3p5())
                        .on_click(cx.listener(move |this, _, _, cx| this.unmark(id, cx))),
                )
        });
        let total: u64 = self.marked.iter().map(|&c| self.tree.node(c).size).sum();
        let count = self.marked.len();
        let mint = p.mint;
        let over = (
            p.card.0.blend(mint.opacity(0.08)),
            p.card.1.blend(mint.opacity(0.05)),
        );

        style::card(p)
            .id("marked")
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .py(px(14.))
            .drag_over::<Dragged>(move |style, _, _, _| {
                style.border_color(mint).bg(style::gradient(over))
            })
            .on_drop(cx.listener(|this, d: &Dragged, _, cx| this.mark(d.id, cx)))
            .child(tray_header(
                match count {
                    0 => "Drag items here, or right-click them and choose Mark for Trash.",
                    _ => "Items go to the macOS Trash, never deleted outright.",
                },
                h_flex()
                    .flex_none()
                    .gap_1()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(p.dim)
                    .map(|sum| match count {
                        0 => sum.child("Nothing marked"),
                        _ => sum
                            .child(format!(
                                "{count} {} ·",
                                if count == 1 { "ITEM" } else { "ITEMS" }
                            ))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.mint)
                                    .child(format_size(total)),
                            ),
                    }),
                p,
            ))
            .when_some(self.trash_error.clone(), |tray, error| {
                tray.child(div().text_size(px(13.)).text_color(p.red).child(error))
            })
            .when(count > 0, |tray| {
                tray.child(
                    h_flex()
                        .flex_wrap()
                        .items_end()
                        .gap_3()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(200.))
                                .flex_wrap()
                                .gap_2()
                                .children(chips),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_2()
                                .ml_auto()
                                .child(
                                    style::button("clear", style::Button::Ghost, p)
                                        .child("Clear")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.clear_marked(cx)),
                                        ),
                                )
                                .child(
                                    style::button("trash", style::Button::Primary, p)
                                        .child(Icon::new(AppIcon::Trash).size_4())
                                        .child(format!("Move to Trash ({})", format_size(total)))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_trash(window, cx)
                                        })),
                                ),
                        ),
                )
            })
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        // Esc and space reach the results as soon as they show, before any hover.
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        let narrow = is_narrow(window);
        let this = cx.entity();
        let chart = self.render_chart(&p, window, cx);
        let stats = self.render_stats(&p);
        let list = self.render_list(&p, cx);
        let content = content(chart, stats, list, narrow);

        page("browser", narrow)
            .key_context("Browser")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::quick_look))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.focus_parent(cx)))
            // Hovering is enough to aim the keyboard, so take focus without a click.
            .on_mouse_move(cx.listener(|this, _, window, cx| {
                if !this.focus_handle.is_focused(window) {
                    window.focus(&this.focus_handle, cx);
                }
            }))
            .child(self.render_breadcrumb(&p, cx))
            .children(self.render_unreadable(&p))
            .child(
                content.context_menu(move |menu, _, cx| {
                    this.read(cx).context_menu(menu, this.clone())
                }),
            )
            .child(self.render_marked(&p, cx))
    }
}

/// Whether the results page stacks the chart, stats and list and scrolls.
pub fn is_narrow(window: &Window) -> bool {
    window.viewport_size().width < px(980.)
}

/// The results page: a header, the chart, stats and list, and the Trash tray, in that
/// order. The running scan fills the same page, so nothing moves when it finishes.
pub fn page(id: &'static str, narrow: bool) -> Stateful<Div> {
    v_flex()
        .id(id)
        .size_full()
        .px_5()
        .pt(px(18.))
        .pb_5()
        .gap(px(14.))
        .when(narrow, |page| page.overflow_y_scroll())
}

pub fn content(chart: Div, stats: Div, list: Div, narrow: bool) -> Stateful<Div> {
    match narrow {
        // The page scrolls, so the chart and the list each get a fixed height.
        true => v_flex()
            .id("content")
            .flex_none()
            .gap(px(14.))
            .child(chart.h(px(460.)))
            .child(stats)
            .child(list.h(px(360.))),
        false => h_flex()
            .id("content")
            .flex_1()
            .min_h_0()
            .items_stretch()
            .gap(px(14.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(14.))
                    .child(chart.flex_1().min_h_0())
                    .child(stats),
            )
            .child(list.w(px(340.)).flex_none()),
    }
}

/// The file types the drawn blocks show, for the legend.
pub fn file_types(tree: &Tree, blocks: &[DrawBlock]) -> HashSet<FileType> {
    blocks
        .iter()
        .filter_map(|b| match b.target {
            Target::Node(id) if tree.node(id).kind != Kind::Unaccounted => {
                Some(tree.node(id).file_type)
            }
            _ => None,
        })
        .collect()
}

/// The chart's card: `hint` above the icicle `area`, and a legend of `types` below it.
pub fn chart_card(
    hint: &'static str,
    types: HashSet<FileType>,
    p: &Palette,
    area: impl IntoElement,
) -> Div {
    let legend = FileType::ALL
        .into_iter()
        .filter(|t| types.contains(t))
        .map(|t| {
            h_flex()
                .gap(px(7.))
                .child(style::dot(chart::swatch(t, p.dark), px(9.)))
                .child(t.label())
        });
    style::card(p)
        .flex()
        .flex_col()
        .p(px(14.))
        .pb_3()
        .child(
            h_flex().pb_3().justify_end().child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.5))
                    .text_color(p.faint)
                    .child(hint),
            ),
        )
        .child(area)
        .child(
            h_flex()
                .flex_none()
                .flex_wrap()
                .gap_x(px(18.))
                .gap_y_1p5()
                .pt_3()
                .text_size(px(12.5))
                .text_color(p.caption)
                .children(legend),
        )
}

/// The card listing the children of the folder `name`, `body` holding the rows.
pub fn list_card(name: &str, count: usize, p: &Palette, body: impl IntoElement) -> Div {
    style::card(p)
        .flex()
        .flex_col()
        .px_3()
        .pt(px(14.))
        .pb(px(10.))
        .child(
            h_flex()
                .px_1()
                .pb_3()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Children of {name}")),
                )
                .child(style::tag(format_count(count as u64), Tone::Neutral, p))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(p.faint)
                        .child("By size ↓"),
                ),
        )
        .child(body)
}

/// The Trash tray's icon, title and `message`, with `summary` on the right.
pub fn tray_header(message: &'static str, summary: impl IntoElement, p: &Palette) -> Div {
    h_flex()
        .gap_4()
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_3()
                .child(style::tile(AppIcon::Trash, p.red, px(36.), p))
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(style::caption("Trash tray", p))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.))
                                .text_color(p.dim)
                                .child(message),
                        ),
                ),
        )
        .child(summary)
}

/// A stat card under the chart.
pub fn stat(label: &'static str, value: String, color: Hsla, detail: String, p: &Palette) -> Div {
    style::card(p)
        .min_w_0()
        .px(px(14.))
        .py_3()
        .child(
            div()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(p.dim)
                .child(label),
        )
        .child(
            div()
                .mt_1()
                .truncate()
                .text_size(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .child(value),
        )
        .child(
            div()
                .truncate()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(p.faint)
                .child(detail),
        )
}

/// The row of three stat cards under the chart.
pub fn stats(cards: [Div; 3]) -> Div {
    div()
        .flex_none()
        .grid()
        .grid_cols(3)
        .gap_3()
        .children(cards)
}

/// A row of the children list: `id`'s name, size and share of `parent_size`. A `live`
/// row's folder is still being scanned, so its file count and type aren't known yet.
pub fn row(
    tree: &Tree,
    id: NodeId,
    parent_size: u64,
    hovered: bool,
    live: bool,
    p: &Palette,
) -> Div {
    let node = tree.node(id);
    let color = chart::node_color(tree, id, p.dark);
    let fraction = node.size as f32 / parent_size.max(1) as f32;
    let (icon, detail): (Icon, String) = match node.kind {
        Kind::Dir if live => (IconName::Folder.into(), "scanning…".into()),
        Kind::Dir => (
            IconName::Folder.into(),
            format!(
                "{} · {}",
                format_files(node.file_count),
                node.file_type.label().to_lowercase()
            ),
        ),
        Kind::Unaccounted => (
            AppIcon::CircleDashed.into(),
            "not visible to the scan".into(),
        ),
        Kind::File | Kind::Symlink => {
            (IconName::File.into(), node.file_type.label().to_lowercase())
        }
    };
    let unaccounted = node.kind == Kind::Unaccounted;
    let (wash, line_strong) = (p.wash, p.line_strong);

    h_flex()
        .size_full()
        .gap_3()
        .px(px(11.))
        .rounded(px(11.))
        .border_1()
        .border_color(transparent_black())
        .bg(wash.opacity(0.016))
        .when(hovered, |row| {
            row.bg(wash.opacity(0.035)).border_color(line_strong)
        })
        .child(style::tile(icon, color, px(34.), p))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .when(unaccounted, |name| name.text_color(p.dim))
                                .child(node.name.to_string()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format_size(node.size)),
                        ),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(p.faint)
                                .child(detail),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(color)
                                .child(format!("{:.1}%", fraction * 100.0)),
                        ),
                )
                .child(div().pt_1().child(style::bar(fraction, color, px(4.), p))),
        )
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
