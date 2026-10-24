mod browser;
mod chart;
mod platform;
mod settings;
mod style;
mod volumes;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::{IconName as AppIcon, icon_assets};
use gpui_kit::component::button::Button;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use scan::Progress;
use sunburst::{Layout, Options};

use crate::browser::{Browser, format_count, format_size};
use crate::settings::{Appearance, Settings};
use crate::style::{MONO, Palette, Tone};
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

/// A share of a volume's space, `color` `None` for the free part.
struct Segment {
    label: SharedString,
    bytes: u64,
    color: Option<Hsla>,
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
    fn render_sidebar(&self, collapsed: bool, p: &Palette, cx: &mut Context<Self>) -> Div {
        let item = |id: &'static str, icon: Icon, active: bool| {
            let wash = p.wash;
            let fg = p.fg;
            h_flex()
                .id(id)
                .gap(px(11.))
                .when_else(
                    collapsed,
                    |item| item.justify_center().p(px(10.)),
                    |item| item.px(px(12.)).py(px(9.)),
                )
                .rounded(px(9.))
                .border_1()
                .border_color(transparent_black())
                .text_color(p.dim)
                .cursor_pointer()
                .hover(move |s| s.bg(wash.opacity(0.03)).text_color(fg))
                .when(active, |item| {
                    item.bg(style::gradient(p.raised))
                        .border_color(p.line_strong)
                        .text_color(p.fg)
                })
                .child(icon.size(px(18.)).flex_none())
        };
        let label = |text: SharedString| div().flex_1().min_w_0().truncate().child(text);

        let scan = self.scan.as_ref().map(|scan| {
            let (name, detail) = match scan {
                Scan::Running { name, .. } => (name.clone(), "scanning…".to_string()),
                Scan::Done(browser) => {
                    let tree = browser.read(cx).tree();
                    (
                        browser.read(cx).root_name(),
                        format_size(tree.node(tree.root()).size),
                    )
                }
                Scan::Failed { name, .. } => (name.clone(), "failed".to_string()),
            };
            let running = matches!(scan, Scan::Running { .. });
            item(
                "nav-scan",
                Icon::new(AppIcon::TextAlignStart),
                self.page == Page::Scan,
            )
            .on_click(cx.listener(|this, _, _, cx| this.show(Page::Scan, cx)))
            .when(!collapsed, |item| {
                item.child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(name))
                        .child(
                            div()
                                .truncate()
                                .font_family(MONO)
                                .text_size(px(11.5))
                                .text_color(p.faint)
                                .child(detail),
                        ),
                )
                .when(running, |item| item.child(style::dot(p.mint, px(7.))))
            })
        });

        v_flex()
            .flex_none()
            .h_full()
            .w(px(if collapsed { 68. } else { 236. }))
            .px(px(if collapsed { 10. } else { 12. }))
            .pb_4()
            .gap(px(3.))
            .bg(style::gradient(p.sidebar))
            .border_r_1()
            .border_color(p.line)
            .child(
                // Room for the traffic lights, and a handle to move the window by.
                div().id("titlebar").flex_none().h(px(44.)).on_mouse_down(
                    MouseButton::Left,
                    |e, window, _| {
                        if e.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    },
                ),
            )
            .child(
                h_flex()
                    .gap(px(11.))
                    .when_else(
                        collapsed,
                        |brand| brand.justify_center().pb_4(),
                        |brand| brand.px(px(6.)).pb(px(22.)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(30.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.line_strong)
                            .bg(style::gradient(p.raised))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(p.mint)
                            .font_weight(FontWeight::BOLD)
                            .child("S"),
                    )
                    .when(!collapsed, |brand| {
                        brand.child(
                            div()
                                .text_size(px(15.))
                                .font_weight(FontWeight::BOLD)
                                .child("SPACEE"),
                        )
                    }),
            )
            .when(!collapsed, |side| {
                side.child(div().px_2().pb_2().child(style::caption("Storage", p)))
            })
            .child(
                item(
                    "nav-volumes",
                    Icon::new(IconName::HardDrive),
                    self.page == Page::Volumes,
                )
                .on_click(cx.listener(|this, _, _, cx| this.show(Page::Volumes, cx)))
                .when(!collapsed, |item| item.child(label("Volumes".into()))),
            )
            .children(scan)
            .child(
                item("nav-choose", Icon::new(IconName::Folder), false)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx)))
                    .when(!collapsed, |item| {
                        item.child(label("Choose Folder…".into()))
                    }),
            )
            .child(div().flex_1())
            .child(
                item(
                    "nav-settings",
                    Icon::new(IconName::Settings),
                    self.page == Page::Settings,
                )
                .when(self.page != Page::Settings, |item| {
                    item.border_color(p.line).bg(p.wash.opacity(0.02))
                })
                .on_click(cx.listener(|this, _, _, cx| this.show(Page::Settings, cx)))
                .when(!collapsed, |item| {
                    item.child(label("Settings".into()))
                        .child(style::kbd("⌘,", p))
                }),
            )
    }

    fn render_volumes(&self, width: Pixels, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let startup = self
            .volumes
            .iter()
            .position(|v| v.kind == volumes::Kind::Startup)
            .map(|i| self.render_startup(i, p, cx));
        // Cards at least 300px wide, as many to a row as fit.
        let columns = ((width.as_f32() - 40.0 + 12.0) / (300.0 + 12.0)).max(1.0) as u16;
        let mut sections = vec![];
        for group in volumes::Group::ALL {
            let cards: Vec<_> = (0..self.volumes.len())
                .filter(|&i| self.volumes[i].kind.group() == Some(group))
                .map(|i| self.render_volume_card(i, p, cx))
                .collect();
            if cards.is_empty() {
                continue;
            }
            sections.push(
                v_flex()
                    .gap_2p5()
                    .child(
                        h_flex()
                            .gap_2p5()
                            .child(style::caption(group.title(), p))
                            .when_some(group.note(), |heading, note| {
                                heading.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_right()
                                        .text_size(px(12.))
                                        .text_color(p.faint)
                                        .child(note),
                                )
                            }),
                    )
                    .child(div().grid().grid_cols(columns).gap_3().children(cards)),
            );
        }

        v_flex()
            .id("volumes")
            .size_full()
            .overflow_y_scroll()
            .px_5()
            .pt(px(18.))
            .pb_5()
            .gap(px(14.))
            .child(
                h_flex()
                    .justify_between()
                    .child(style::caption("Volumes", p))
                    .child(
                        style::tag_base(Tone::Neutral, p)
                            .child(style::dot(p.mint, px(6.)))
                            .child(format!("{} mounted", self.volumes.len())),
                    ),
            )
            .children(startup)
            .when(!self.full_disk_access, |page| {
                page.child(full_disk_access_notice(
                    "Spacee doesn't have Full Disk Access".into(),
                    "macOS hides some folders from it, so their space shows up as unaccounted. After granting it, quit and reopen Spacee.",
                    p,
                ))
            })
            .children(sections)
            .child(
                v_flex()
                    .id("dropzone")
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .p(px(26.))
                    .rounded(px(14.))
                    .border_1()
                    .border_dashed()
                    .border_color(p.line_strong)
                    .bg(p.wash.opacity(0.012))
                    .drag_over::<ExternalPaths>({
                        let mint = p.mint;
                        move |style, _, _, _| style.border_color(mint).bg(mint.opacity(0.05))
                    })
                    .child(
                        style::tile(IconName::Folder, p.dim, px(48.), p).rounded_full(),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Drop a folder to scan"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(p.dim)
                            .child("Scan just that folder instead of a whole drive."),
                    )
                    .child(
                        style::button("choose", style::Button::Normal, p)
                            .child("Choose Folder…")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The startup disk, given the most room since it's the one that fills up.
    fn render_startup(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let volume = &self.volumes[i];
        let fraction = volume.used_fraction();
        let status = volumes::Status::of(fraction);
        let tone = status_tone(status);
        let mount = volume.mount.clone();
        let segments = self.breakdown(volume, p, cx);
        let bar = match &segments {
            Some(segments) => {
                let total = segments.iter().map(|s| s.bytes).sum::<u64>().max(1) as f32;
                h_flex()
                    .h(px(12.))
                    .gap(px(3.))
                    .children(segments.iter().map(|s| {
                        let segment = div()
                            .h_full()
                            .min_w(px(3.))
                            .w(relative(s.bytes as f32 / total))
                            .rounded(px(4.));
                        match s.color {
                            Some(color) => segment.bg(color),
                            None => segment
                                .border_1()
                                .border_color(p.wash.opacity(0.1))
                                .bg(pattern_slash(p.wash.opacity(0.14), 3.0, 4.0)),
                        }
                    }))
                    .into_any_element()
            }
            None => style::bar(fraction, p.tone(tone), px(12.), p).into_any_element(),
        };
        let legend = segments.map(|segments| {
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(segments.into_iter().map(|s| {
                    let swatch = div().flex_none().size(px(8.)).rounded(px(2.));
                    v_flex()
                        .flex_1()
                        .min_w(px(110.))
                        .px(px(11.))
                        .py_2()
                        .rounded(px(9.))
                        .border_1()
                        .border_color(p.line)
                        .bg(p.wash.opacity(0.02))
                        .child(
                            h_flex()
                                .gap(px(7.))
                                .text_size(px(12.))
                                .text_color(p.dim)
                                .child(match s.color {
                                    Some(color) => swatch.bg(color),
                                    None => swatch.border_1().border_color(p.wash.opacity(0.3)),
                                })
                                .child(div().min_w_0().truncate().child(s.label)),
                        )
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format_size(s.bytes)),
                        )
                }))
        });

        style::glow_card(p)
            .flex_none()
            .px(px(26.))
            .py_6()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_x_6()
                    .gap_y_4()
                    .child(
                        h_flex()
                            .gap_4()
                            .min_w_0()
                            .child(
                                div()
                                    .relative()
                                    .child(style::tile(IconName::HardDrive, p.mint, px(52.), p))
                                    .child(
                                        style::dot(p.mint, px(11.))
                                            .absolute()
                                            .right(px(-3.))
                                            .bottom(px(-3.))
                                            .border_2()
                                            .border_color(p.card.0),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .gap_2p5()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(26.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(volume.name.clone()),
                                            )
                                            .child(style::tag("Startup disk", Tone::Mint, p)),
                                    )
                                    .child(
                                        div()
                                            .font_family(MONO)
                                            .text_size(px(12.))
                                            .text_color(p.faint)
                                            .child(format!(
                                                "{} · {} used",
                                                volume.mount.display(),
                                                format_size(volume.used())
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .items_end()
                            .gap_2()
                            .child(
                                h_flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(40.))
                                            .line_height(px(42.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(format_size(volume.available)),
                                    )
                                    .child(
                                        div().text_size(px(15.)).text_color(p.dim).child("free"),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2p5()
                                    .child(style::dot_tag(
                                        format!("{} ({:.0}%)", status.label(), fraction * 100.0),
                                        tone,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .font_family(MONO)
                                            .text_size(px(12.))
                                            .text_color(p.faint)
                                            .child(format!("of {}", format_size(volume.total))),
                                    ),
                            ),
                    ),
            )
            .child(bar)
            .children(legend)
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2p5()
                    .when_some(std::env::var_os("HOME"), |buttons, home| {
                        buttons.child(
                            style::button("scan-home", style::Button::Normal, p)
                                .child(Icon::new(AppIcon::House).size_4())
                                .child("Scan Home Folder")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_scan(PathBuf::from(&home), cx)
                                })),
                        )
                    })
                    .child(
                        style::button(("scan", i), style::Button::Primary, p)
                            .child(Icon::new(AppIcon::Disc).size_4())
                            .child(format!("Scan {}", volume.name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_scan(mount.clone(), cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// What fills `volume`, by the largest folders at its top, when this session has
    /// finished scanning it.
    fn breakdown(&self, volume: &Volume, p: &Palette, cx: &App) -> Option<Vec<Segment>> {
        let Some(Scan::Done(browser)) = &self.scan else {
            return None;
        };
        let tree = browser.read(cx).tree();
        if tree.path(tree.root()) != volume.mount {
            return None;
        }
        const SHOWN: usize = 5;
        let children = &tree.node(tree.root()).children;
        let mut segments: Vec<Segment> = children
            .iter()
            .take(SHOWN)
            .map(|&id| Segment {
                label: tree.node(id).name.to_string().into(),
                bytes: tree.node(id).size,
                color: Some(chart::node_color(tree, id, p.dark)),
            })
            .collect();
        let rest: u64 = children
            .iter()
            .skip(SHOWN)
            .map(|&id| tree.node(id).size)
            .sum();
        if rest > 0 {
            segments.push(Segment {
                label: "Everything else".into(),
                bytes: rest,
                color: Some(p.faint.opacity(0.6)),
            });
        }
        segments.push(Segment {
            label: "Free".into(),
            bytes: volume.available,
            color: None,
        });
        Some(segments)
    }

    fn render_volume_card(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let volume = &self.volumes[i];
        let fraction = volume.used_fraction();
        let status = volumes::Status::of(fraction);
        let tone = status_tone(status);
        let mount = volume.mount.clone();
        let (icon, color, detail): (Icon, _, _) = match &volume.kind {
            volumes::Kind::Network { host } => (
                IconName::Network.into(),
                p.blue,
                format!("{host} · {}", volume.mount.display()),
            ),
            volumes::Kind::DiskImage => (
                AppIcon::Disc.into(),
                p.violet,
                volume.mount.display().to_string(),
            ),
            _ => (
                IconName::HardDrive.into(),
                p.dim,
                volume.mount.display().to_string(),
            ),
        };
        style::card(p)
            .min_w_0()
            .px(px(18.))
            .py_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                h_flex()
                    .gap(px(13.))
                    .child(style::tile(icon, color, px(36.), p))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(volume.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(MONO)
                                    .text_size(px(11.5))
                                    .text_color(p.faint)
                                    .child(detail),
                            ),
                    )
                    .child(style::tag(status.label(), tone, p)),
            )
            .child(style::bar(fraction, p.tone(tone), px(6.), p))
            .child(
                h_flex()
                    .gap_2p5()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .text_color(p.dim)
                            .child(format!(
                                "{} free of {} · {:.0}% used",
                                format_size(volume.available),
                                format_size(volume.total),
                                fraction * 100.0
                            )),
                    )
                    .child(
                        style::button(("scan", i), style::Button::Normal, p)
                            .px(px(11.))
                            .py(px(4.))
                            .text_size(px(12.5))
                            .child("Scan")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_scan(mount.clone(), cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_scan(&self, width: Pixels, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        match &self.scan {
            None => self.render_volumes(width, p, cx),
            Some(Scan::Done(browser)) => browser.clone().into_any_element(),
            Some(Scan::Failed { name, error }) => centered(
                style::card(p)
                    .p_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .max_w(px(440.))
                    .child(style::tile(IconName::TriangleAlert, p.red, px(44.), p))
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Couldn't scan {name}")),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_center()
                            .text_color(p.dim)
                            .child(error.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .mt_2()
                            .child(
                                style::button("back", style::Button::Normal, p)
                                    .child("Volumes")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.show(Page::Volumes, cx)),
                                    ),
                            )
                            .child(
                                style::button("choose", style::Button::Primary, p)
                                    .child("Choose Folder…")
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

    fn render_settings(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let selected = Appearance::ALL
            .iter()
            .position(|&a| a == self.settings.appearance);
        v_flex()
            .size_full()
            .px_5()
            .pt(px(18.))
            .pb_5()
            .gap(px(14.))
            .child(style::caption("Settings", p))
            .child(
                style::card(p)
                    .p_5()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_6()
                    .child(
                        v_flex()
                            .w(px(200.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Appearance"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(p.dim)
                                    .child("Light, dark, or as macOS is set"),
                            ),
                    )
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
        let p = Palette::of(cx);
        let viewport = window.viewport_size().width;
        let collapsed = viewport < px(880.);
        let width = viewport - px(if collapsed { 68. } else { 236. });
        let page = match self.page {
            Page::Volumes => self.render_volumes(width, &p, cx),
            Page::Scan => self.render_scan(width, &p, cx),
            Page::Settings => self.render_settings(&p, cx),
        };
        h_flex()
            .size_full()
            .items_start()
            .bg(p.window)
            .text_color(p.fg)
            .text_size(px(14.))
            .on_drop(cx.listener(Self::drop_paths))
            .child(self.render_sidebar(collapsed, &p, cx))
            .child(
                div().relative().size_full().min_w_0().child(page).child(
                    // The window has no title bar; the strip above the page moves it.
                    div()
                        .id("drag")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(12.))
                        .on_mouse_down(MouseButton::Left, |e, window, _| {
                            if e.click_count == 2 {
                                window.titlebar_double_click();
                            } else {
                                window.start_window_move();
                            }
                        }),
                ),
            )
    }
}

/// Says why space is missing and offers to open the Full Disk Access settings.
pub fn full_disk_access_notice(title: SharedString, text: &'static str, p: &Palette) -> Div {
    style::card(p)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(14.))
        .py_3()
        .child(style::tile(AppIcon::Lock, p.violet, px(36.), p))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(px(13.)).text_color(p.dim).child(text)),
        )
        .child(
            style::button("full-disk-access", style::Button::Normal, p)
                .child("Grant Full Disk Access…")
                .on_click(|_, _, _| platform::open_full_disk_access_settings()),
        )
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

fn status_tone(status: volumes::Status) -> Tone {
    match status {
        volumes::Status::Fine => Tone::Mint,
        volumes::Status::FillingUp => Tone::Amber,
        volumes::Status::AlmostFull => Tone::Red,
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

/// Icons beyond the ones the components bring, embedded on their own.
struct AppAssets;

icon_assets!(
    ExtraIcons,
    [CircleDashed, Disc, House, Lock, TextAlignStart, Trash]
);

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

fn main() {
    let initial = std::env::args().nth(1).map(PathBuf::from);
    gpui_kit::application()
        .with_assets(AppAssets)
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
                window_bounds: Some(WindowBounds::centered(size(px(1240.), px(820.)), cx)),
                window_min_size: Some(size(px(640.), px(460.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spacee".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
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
