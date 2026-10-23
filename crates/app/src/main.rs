use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu};
use gpui_kit::component::{ActiveTheme, Theme, h_flex, v_flex};
use gpui_kit::*;

struct Spacee {
    _appearance: Subscription,
}

impl Spacee {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Theme::sync_system_appearance(Some(window), cx);
        let _appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        Self { _appearance }
    }
}

impl Render for Spacee {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .items_start()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                Sidebar::<SidebarGroup<SidebarMenu>>::new("sidebar")
                    .header(SidebarHeader::new().child("Spacee")),
            )
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child("Choose a disk to scan"),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
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
                cx.new(|cx| Spacee::new(window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
}
