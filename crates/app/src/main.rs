mod chart;

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme, IconName, Theme, h_flex, v_flex};
use gpui_kit::*;
use scan::Tree;
use sunburst::{Hit, Layout, Options, Target};

enum Screen {
    Empty,
    Scanning(PathBuf),
    Ready(Scanned),
    Failed(String),
}

struct Scanned {
    tree: Rc<Tree>,
    layout: Rc<Layout>,
    hovered: Option<Target>,
    bounds: Option<Bounds<Pixels>>,
}

struct Spacee {
    screen: Screen,
    _scan: Option<Task<()>>,
    _appearance: Subscription,
}

impl Spacee {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Theme::sync_system_appearance(Some(window), cx);
        let _appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        Self {
            screen: Screen::Empty,
            _scan: None,
            _appearance,
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(path) = paths.pop()
            {
                this.update(cx, |this, cx| this.start_scan(path, cx)).ok();
            }
        })
        .detach();
    }

    fn start_scan(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.screen = Screen::Scanning(path.clone());
        cx.notify();
        let scan = cx.background_spawn(async move {
            let tree = scan::scan(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let layout = Layout::new(&tree, tree.root(), Options::default());
            Ok::<_, String>((tree, layout))
        });
        self._scan = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            this.update(cx, |this, cx| {
                this.screen = match result {
                    Ok((tree, layout)) => Screen::Ready(Scanned {
                        tree: Rc::new(tree),
                        layout: Rc::new(layout),
                        hovered: None,
                        bounds: None,
                    }),
                    Err(message) => Screen::Failed(message),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Screen::Ready(scanned) = &mut self.screen else {
            return;
        };
        let Some(bounds) = scanned.bounds else { return };
        let (center, radius) = chart::geometry(bounds);
        let offset = position - center;
        let hovered = match scanned
            .layout
            .hit(offset.x.as_f32(), offset.y.as_f32(), radius)
        {
            Some(Hit::Arc(arc)) => Some(arc.target),
            _ => None,
        };
        if hovered != scanned.hovered {
            scanned.hovered = hovered;
            cx.notify();
        }
    }

    fn render_chart(&self, scanned: &Scanned, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = scanned.tree.clone();
        let layout = scanned.layout.clone();
        let hovered = scanned.hovered;
        let dark = cx.theme().is_dark();
        let this = cx.entity();
        let (title, size) = describe(
            &tree,
            &layout,
            hovered.unwrap_or(Target::Node(layout.focus)),
        );

        div()
            .id("chart")
            .relative()
            .size_full()
            .on_mouse_move(
                cx.listener(|this, e: &MouseMoveEvent, _, cx| this.hover(e.position, cx)),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, _| {
                            if let Screen::Ready(scanned) = &mut this.screen {
                                scanned.bounds = Some(bounds);
                            }
                        })
                    },
                    move |bounds, _, window, _| {
                        chart::paint(&tree, &layout, hovered, dark, bounds, window)
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
}

fn describe(tree: &Tree, layout: &Layout, target: Target) -> (String, String) {
    match target {
        Target::Node(id) => {
            let node = tree.node(id);
            let name = tree.path(id).file_name().map_or_else(
                || node.name.to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            (name, format_size(node.size))
        }
        Target::Smaller { .. } => {
            let sweep = layout
                .arcs()
                .find(|a| a.target == target)
                .map_or(0.0, |a| a.sweep);
            let focus = tree.node(layout.focus).size;
            let bytes = (sweep / std::f32::consts::TAU) as f64 * focus as f64;
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

impl Render for Spacee {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match &self.screen {
            Screen::Empty => v_flex()
                .gap_3()
                .items_center()
                .child("Choose a folder to scan")
                .child(
                    Button::new("choose")
                        .primary()
                        .label("Choose Folder…")
                        .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                )
                .into_any_element(),
            Screen::Scanning(path) => div()
                .child(format!("Scanning {}…", path.display()))
                .into_any_element(),
            Screen::Failed(message) => div().child(message.clone()).into_any_element(),
            Screen::Ready(scanned) => self.render_chart(scanned, cx).into_any_element(),
        };

        h_flex()
            .size_full()
            .items_start()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                Sidebar::new("sidebar")
                    .header(SidebarHeader::new().child("Spacee"))
                    .child(
                        SidebarGroup::new("Scan").child(
                            SidebarMenu::new().child(
                                SidebarMenuItem::new("Choose Folder…")
                                    .icon(IconName::Folder)
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                            ),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(main),
            )
    }
}

fn main() {
    let initial = std::env::args().nth(1).map(PathBuf::from);
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1100.), px(720.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spacee".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let mut spacee = Spacee::new(window, cx);
                    if let Some(path) = initial {
                        spacee.start_scan(path, cx);
                    }
                    spacee
                })
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}
