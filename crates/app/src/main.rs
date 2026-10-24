mod browser;
mod chart;
mod platform;
mod settings;
mod volumes;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::Selectable;
use gpui_kit::component::button::Button;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::Progress;
use sunburst::{Layout, Options};

use crate::browser::{Browser, format_count, format_size};
use crate::settings::{Appearance, Settings};
use crate::volumes::Volume;

actions!(spacee, [OpenSettings, Quit]);

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

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
    Failed(String),
}

struct Spacee {
    page: Page,
    scan: Option<Scan>,
    volumes: Vec<Volume>,
    full_disk_access: bool,
    settings: Settings,
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
            let (path, name) = (path.clone(), name.clone());
            async move |this, cx| {
                let result = scan.await;
                this.update(cx, |this, cx| {
                    this.scan = Some(match result {
                        Ok(tree) => Scan::Done(cx.new(|cx| Browser::new(tree, name, cx))),
                        Err(e) => Scan::Failed(format!("{}: {e}", path.display())),
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

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                Scan::Failed(_) => "Scan failed".into(),
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
            .header(SidebarHeader::new().child("Spacee"))
            .child(SidebarGroup::new("Scan").child(locations))
            .footer(
                SidebarFooter::new()
                    .selected(self.page == Page::Settings)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.show(Page::Settings, cx)),
                    )
                    .child(h_flex().gap_2().child(IconName::Settings).child("Settings")),
            )
    }

    fn render_volumes(&self, cx: &mut Context<Self>) -> AnyElement {
        let cards = self.volumes.iter().enumerate().map(|(i, volume)| {
            let used = volume.total.saturating_sub(volume.available);
            let fraction = used as f32 / volume.total.max(1) as f32;
            let mount = volume.mount.clone();
            h_flex()
                .gap_4()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .child(IconName::HardDrive)
                .child(
                    v_flex()
                        .flex_1()
                        .gap_2()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(volume.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} available of {}",
                                            format_size(volume.available),
                                            format_size(volume.total)
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .h(px(6.))
                                .w_full()
                                .rounded_full()
                                .bg(cx.theme().muted)
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction))
                                        .rounded_full()
                                        .bg(cx.theme().primary),
                                ),
                        ),
                )
                .child(Button::new(("scan", i)).label("Scan").on_click(
                    cx.listener(move |this, _, _, cx| this.start_scan(mount.clone(), cx)),
                ))
        });

        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Volumes"),
            )
            .children(cards)
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
                                .child("Spacee doesn't have Full Disk Access, so macOS hides some folders from it and their space shows up as hidden. After granting it, quit and reopen Spacee."),
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

    fn render_scan(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.scan {
            None => self.render_volumes(cx),
            Some(Scan::Done(browser)) => browser.clone().into_any_element(),
            Some(Scan::Failed(message)) => centered(div().child(message.clone())),
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
            let layout = Layout::new(&tree, tree.root(), Options::default());
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
        v_flex()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                if let Some((tree, layout)) = &live {
                                    let arcs = layout.draw_arcs();
                                    chart::paint(tree, layout, &arcs, None, dark, bounds, window);
                                }
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
                                    .child(
                                        div()
                                            .text_sm()
                                            .truncate()
                                            .child(format!("Scanning {name}")),
                                    )
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(format_size(progress.bytes())),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} files",
                                                format_count(progress.files())
                                            )),
                                    ),
                            ),
                    ),
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(self.render_sidebar(cx))
            .child(div().size_full().min_w_0().child(page))
    }
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
