mod browser;
mod chart;
mod platform;
mod settings;
mod style;
mod volumes;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::Selectable;
use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::Progress;
use sunburst::{Layout, Options};

use crate::browser::{Browser, format_count, format_size};
use crate::settings::{Appearance, Settings};
use crate::volumes::Volume;

actions!(spacee, [OpenSettings, Quit]);

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Rows in the chart that grows while a scan runs.
const LIVE_LEVELS: usize = 4;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Volumes,
    Scan,
    Settings,
}

enum Scan {
    Running {
        name: String,
        progress: Arc<Progress>,
        _task: Task<()>,
        _redraw: Task<()>,
    },
    Done(Entity<Browser>),
    Failed {
        name: String,
        error: String,
    },
}

struct Spacee {
    page: Page,
    scan: Option<Scan>,
    volumes: Vec<Volume>,
    full_disk_access: bool,
    settings: Settings,
    /// Where the running scan's chart was drawn last frame, for placing its labels.
    live_bounds: Option<Bounds<Pixels>>,
    _appearance: Subscription,
}

impl Spacee {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Theme::sync_system_appearance(Some(window), cx);
        let _appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        Self {
            page: Page::Volumes,
            scan: None,
            volumes: volumes::list(),
            full_disk_access: platform::has_full_disk_access(),
            settings,
            live_bounds: None,
            _appearance,
        }
    }

    fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        if page == Page::Volumes {
            self.volumes = volumes::list();
            self.full_disk_access = platform::has_full_disk_access();
        }
        self.page = page;
        cx.notify();
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = appearance;
        self.settings.save();
        appearance.apply(cx);
        cx.notify();
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

    fn drop_paths(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(dir) = paths.paths().iter().find(|p| p.is_dir()) {
            self.start_scan(dir.clone(), cx);
        }
    }

    fn start_scan(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.cancel_scan();
        let progress = Arc::new(Progress::default());
        let scan = cx.background_spawn({
            let (path, progress) = (path.clone(), progress.clone());
            async move { scan::scan(&path, &progress) }
        });
        let name = display_name(&path);
        let _task = cx.spawn({
            let name = name.clone();
            async move |this, cx| {
                let result = scan.await;
                this.update(cx, |this, cx| {
                    this.scan = Some(match result {
                        Ok(tree) => Scan::Done(cx.new(|cx| Browser::new(tree, name, cx))),
                        Err(e) => Scan::Failed {
                            name,
                            error: describe_error(&e),
                        },
                    });
                    cx.notify();
                })
                .ok();
            }
        });
        let _redraw = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_INTERVAL).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        self.scan = Some(Scan::Running {
            name,
            progress,
            _task,
            _redraw,
        });
        self.show(Page::Scan, cx);
    }

    fn cancel_scan(&mut self) {
        if let Some(Scan::Running { progress, .. }) = &self.scan {
            progress.cancel();
            self.scan = None;
        }
    }

    /// `collapsed` shrinks it to icons, leaving the chart room in a narrow window.
    fn render_sidebar(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let mut locations = SidebarMenu::new().child(
            SidebarMenuItem::new("Volumes")
                .icon(IconName::HardDrive)
                .active(self.page == Page::Volumes)
                .on_click(cx.listener(|this, _, _, cx| this.show(Page::Volumes, cx))),
        );
        if let Some(scan) = &self.scan {
            let name = match scan {
                Scan::Running { name, .. } => name.clone(),
                Scan::Done(browser) => browser.read(cx).root_name(),
                Scan::Failed { name, .. } => name.clone(),
            };
            locations = locations.child(
                SidebarMenuItem::new(name)
                    .icon(IconName::ChartPie)
                    .active(self.page == Page::Scan)
                    .on_click(cx.listener(|this, _, _, cx| this.show(Page::Scan, cx))),
            );
        }
        locations = locations.child(
            SidebarMenuItem::new("Choose Folder…")
                .icon(IconName::Folder)
                .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
        );

        Sidebar::new("sidebar")
            .collapsed(collapsed)
            .when(!collapsed, |sidebar| {
                sidebar.header(SidebarHeader::new().child("Spacee"))
            })
            .child(SidebarGroup::new("Scan").child(locations))
            .footer(
                SidebarFooter::new()
                    .selected(self.page == Page::Settings)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.show(Page::Settings, cx)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(IconName::Settings)
                            .when(!collapsed, |footer| footer.child("Settings")),
                    ),
            )
    }

    fn render_volumes(&self, cx: &mut Context<Self>) -> AnyElement {
        let startup = self
            .volumes
            .iter()
            .position(|v| v.kind == volumes::Kind::Startup)
            .map(|i| self.render_startup(i, cx));
        let mut sections = vec![];
        for group in volumes::Group::ALL {
            let mut rows = vec![];
            for (i, volume) in self.volumes.iter().enumerate() {
                if volume.kind.group() == Some(group) {
                    rows.push(self.render_volume_row(i, rows.is_empty(), cx));
                }
            }
            if rows.is_empty() {
                continue;
            }
            let muted = cx.theme().muted_foreground;
            sections.push(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(group.title()),
                            )
                            .when_some(group.note(), |heading, note| {
                                heading.child(div().min_w_0().truncate().child(note))
                            }),
                    )
                    .child(
                        v_flex()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .overflow_hidden()
                            .children(rows),
                    ),
            );
        }

        v_flex()
            .id("volumes")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_5()
            .children(startup)
            .children(sections)
            .when(!self.full_disk_access, |page| {
                page.child(
                    h_flex()
                        .gap_4()
                        .p_4()
                        .rounded_lg()
                        .bg(cx.theme().secondary)
                        .child(IconName::Info)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .child("Spacee doesn't have Full Disk Access, so macOS hides some folders from it and their space shows up as unaccounted. After granting it, quit and reopen Spacee."),
                        )
                        .child(
                            Button::new("full-disk-access")
                                .label("Grant Full Disk Access…")
                                .on_click(|_, _, _| platform::open_full_disk_access_settings()),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("choose")
                            .label("Choose Folder…")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("or drop a folder onto the window"),
                    ),
            )
            .into_any_element()
    }

    /// The startup disk, given most of the screen since it's the one that fills up.
    fn render_startup(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let volume = &self.volumes[i];
        let fraction = volume.used_fraction();
        let status = volumes::Status::of(fraction);
        let color = status_color(status, cx);
        let mount = volume.mount.clone();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .flex_wrap()
            .gap_6()
            .p_6()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(260.))
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("STARTUP DISK"),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(volume.name.clone()),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1p5()
                                    .px_2p5()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(color.opacity(0.15))
                                    .text_color(color)
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(div().size_2().rounded_full().bg(color))
                                    .child(status.label()),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(40.))
                                    .line_height(px(44.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format_size(volume.available)),
                            )
                            .child(
                                div()
                                    .pb_1()
                                    .text_lg()
                                    .text_color(muted)
                                    .child(format!("free of {}", format_size(volume.total))),
                            ),
                    )
                    .child(usage_bar(fraction, color, px(10.), cx))
                    .child(div().text_sm().text_color(muted).child(format!(
                        "{}% full, {} used",
                        (fraction * 100.0).round(),
                        format_size(volume.total.saturating_sub(volume.available))
                    ))),
            )
            .child(
                v_flex()
                    .w(px(200.))
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new(("scan", i))
                            .primary()
                            .label(format!("Scan {}", volume.name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_scan(mount.clone(), cx)
                            })),
                    )
                    .when_some(std::env::var_os("HOME"), |buttons, home| {
                        buttons.child(Button::new("scan-home").label("Scan Home Folder").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.start_scan(PathBuf::from(&home), cx)
                            }),
                        ))
                    }),
            )
            .into_any_element()
    }

    /// `first` leaves out the divider above the row.
    fn render_volume_row(&self, i: usize, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let volume = &self.volumes[i];
        let fraction = volume.used_fraction();
        let color = status_color(volumes::Status::of(fraction), cx);
        let mount = volume.mount.clone();
        let (icon, host) = match &volume.kind {
            volumes::Kind::Network { host } => (IconName::Globe, Some(host.clone())),
            _ => (IconName::HardDrive, None),
        };
        h_flex()
            .gap_3()
            .px_4()
            .py_2p5()
            .when(!first, |row| row.border_t_1())
            .border_color(cx.theme().border)
            .child(Icon::new(icon).flex_none())
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(180.))
                            .truncate()
                            .child(volume.name.clone()),
                    )
                    .when_some(host, |name, host| {
                        name.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("on {host}")),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(110.))
                    .flex_none()
                    .child(usage_bar(fraction, color, px(6.), cx)),
            )
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} free of {}",
                        format_size(volume.available),
                        format_size(volume.total)
                    )),
            )
            .child(
                Button::new(("scan", i)).small().label("Scan").on_click(
                    cx.listener(move |this, _, _, cx| this.start_scan(mount.clone(), cx)),
                ),
            )
            .into_any_element()
    }

    fn render_scan(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.scan {
            None => self.render_volumes(cx),
            Some(Scan::Done(browser)) => browser.clone().into_any_element(),
            Some(Scan::Failed { name, error }) => centered(
                v_flex()
                    .items_center()
                    .gap_3()
                    .max_w(px(420.))
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size_8()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Couldn't scan {name}")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(error.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .mt_2()
                            .child(Button::new("back").label("Volumes").on_click(
                                cx.listener(|this, _, _, cx| this.show(Page::Volumes, cx)),
                            ))
                            .child(
                                Button::new("choose")
                                    .primary()
                                    .label("Choose Folder…")
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                            ),
                    ),
            ),
            Some(Scan::Running { name, progress, .. }) => self.render_running(name, progress, cx),
        }
    }

    /// The folders found so far as a sunburst that grows while the scan runs.
    fn render_running(
        &self,
        name: &str,
        progress: &Progress,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let start = Instant::now();
        let live = progress.snapshot().map(|tree| {
            let options = Options {
                max_depth: LIVE_LEVELS,
                ..Options::default()
            };
            let layout = Layout::new(&tree, tree.root(), options);
            (tree, layout)
        });
        if std::env::var_os("SPACEE_PERF").is_some()
            && let Some((tree, _)) = &live
        {
            eprintln!(
                "snapshot: {} folders in {:.2?}",
                tree.node_count(),
                start.elapsed()
            );
        }
        let dark = cx.theme().is_dark();
        let levels = LIVE_LEVELS;
        let overlay = self.live_bounds.map(|bounds| {
            let geometry = chart::Geometry::new(bounds, levels);
            let mut labels = match &live {
                Some((tree, layout)) => {
                    let size = tree.node(tree.root()).size;
                    chart::labels(tree, &layout.draw_arcs(), size, geometry, cx)
                }
                None => vec![],
            };
            labels.push(chart::root_label(
                name.to_string(),
                format!("{} files", format_count(progress.files())),
                format_size(progress.bytes()),
                geometry,
                cx,
            ));
            labels
        });
        let this = cx.entity();
        v_flex()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .m_3()
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                this.update(cx, |this, cx| {
                                    if this.live_bounds != Some(bounds) {
                                        this.live_bounds = Some(bounds);
                                        cx.notify();
                                    }
                                })
                            },
                            move |bounds, _, window, _| {
                                let geometry = chart::Geometry::new(bounds, levels);
                                if let Some((tree, layout)) = &live {
                                    let arcs = layout.draw_arcs();
                                    chart::paint(tree, &arcs, None, dark, geometry, window);
                                }
                            },
                        )
                        .size_full(),
                    )
                    .children(overlay.into_iter().flatten()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .gap_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(progress.current().display().to_string()),
                    )
                    .child(
                        Button::new("cancel")
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_scan();
                                this.show(Page::Volumes, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = Appearance::ALL
            .iter()
            .position(|&a| a == self.settings.appearance);
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
            .child(
                h_flex()
                    .gap_6()
                    .child(div().w(px(120.)).child("Appearance"))
                    .child(
                        RadioGroup::horizontal("appearance")
                            .children(Appearance::ALL.map(|a| a.label()))
                            .selected_index(selected)
                            .on_change(cx.listener(|this, &i: &usize, _, cx| {
                                this.set_appearance(Appearance::ALL[i], cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Spacee {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = window.viewport_size().width < px(880.);
        let page = match self.page {
            Page::Volumes => self.render_volumes(cx),
            Page::Scan => self.render_scan(cx),
            Page::Settings => self.render_settings(cx),
        };
        h_flex()
            .size_full()
            .items_start()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_drop(cx.listener(Self::drop_paths))
            .child(self.render_sidebar(collapsed, cx))
            .child(div().size_full().min_w_0().child(page))
    }
}

/// The error's message without Rust's " (os error N)" suffix, such as "No such file or
/// directory".
fn describe_error(error: &std::io::Error) -> String {
    let message = error.to_string();
    match message.rfind(" (os error ") {
        Some(at) => message[..at].to_string(),
        None => message,
    }
}

fn status_color(status: volumes::Status, cx: &App) -> Hsla {
    match status {
        volumes::Status::Fine => cx.theme().success,
        volumes::Status::FillingUp => cx.theme().warning,
        volumes::Status::AlmostFull => cx.theme().danger,
    }
}

fn usage_bar(fraction: f32, color: Hsla, height: Pixels, cx: &App) -> impl IntoElement {
    div()
        .h(height)
        .w_full()
        .rounded_full()
        .bg(cx.theme().muted)
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(color),
        )
}

fn centered(content: impl IntoElement) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(content)
        .into_any_element()
}

/// The volume's name, such as "Macintosh HD", when `path` is where one is mounted, else
/// the folder's name.
fn display_name(path: &Path) -> String {
    if let Some(volume) = volumes::list().into_iter().find(|v| v.mount == path) {
        return volume.name;
    }
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn main() {
    let initial = std::env::args().nth(1).map(PathBuf::from);
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let settings = Settings::load();
            // Before the window opens, so its first frame already has the chosen appearance.
            settings.appearance.apply(cx);

            cx.bind_keys([
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("space", browser::QuickLook, Some("Browser")),
            ]);
            cx.set_menus(vec![Menu {
                name: "Spacee".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Spacee", Quit),
                ],
                disabled: false,
            }]);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1100.), px(720.)), cx)),
                window_min_size: Some(size(px(640.), px(460.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spacee".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (_, spacee) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let mut spacee = Spacee::new(settings, window, cx);
                    if let Some(path) = initial {
                        spacee.start_scan(path, cx);
                    }
                    spacee
                })
            })
            .expect("failed to open window");
            cx.on_action(move |_: &OpenSettings, cx| {
                spacee.update(cx, |this, cx| this.show(Page::Settings, cx));
            });
            cx.activate(true);
        });
}
