mod browser;
mod chart;

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme, IconName, Theme, h_flex, v_flex};
use gpui_kit::*;

use crate::browser::Browser;

enum Screen {
    Empty,
    Scanning(PathBuf),
    Ready(Entity<Browser>),
    Failed(String),
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
            scan::scan(&path).map_err(|e| format!("{}: {e}", path.display()))
        });
        self._scan = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            this.update(cx, |this, cx| {
                this.screen = match result {
                    Ok(tree) => Screen::Ready(cx.new(|_| Browser::new(tree))),
                    Err(message) => Screen::Failed(message),
                };
                cx.notify();
            })
            .ok();
        }));
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
            Screen::Ready(browser) => browser.clone().into_any_element(),
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
            .child(match &self.screen {
                Screen::Ready(_) => div().size_full().child(main),
                _ => v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(main),
            })
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
