use gpui_kit::component::{ActiveTheme, Icon, h_flex};
use gpui_kit::*;

pub const MONO: &str = "Menlo";

/// The app's colours. Dark is slate navy with a mint accent; light takes its neutrals
/// from the theme and darkens the accents so they hold up on white.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub window: Hsla,
    pub sidebar: (Hsla, Hsla),
    pub card: (Hsla, Hsla),
    /// The selected sidebar item.
    pub raised: (Hsla, Hsla),
    pub line: Hsla,
    pub line_strong: Hsla,
    pub fg: Hsla,
    pub dim: Hsla,
    pub faint: Hsla,
    pub caption: Hsla,
    pub mint: Hsla,
    pub mint_ink: Hsla,
    pub red: Hsla,
    pub amber: Hsla,
    pub blue: Hsla,
    pub violet: Hsla,
    /// The empty part of a bar.
    pub well: Hsla,
    /// Sunken strips such as the reading line and the hover pill.
    pub inset: Hsla,
    /// Faint highlights are this colour at a low opacity.
    pub wash: Hsla,
}

impl Palette {
    pub fn of(cx: &App) -> Self {
        let theme = cx.theme();
        if theme.is_dark() {
            let c = |hex| Hsla::from(rgb(hex));
            Self {
                dark: true,
                window: c(0x121821),
                sidebar: (c(0x181f2a), c(0x141a23)),
                card: (c(0x1c2430), c(0x161c26)),
                raised: (c(0x252e3c), c(0x1f2733)),
                line: c(0x262f3d),
                line_strong: c(0x313b4c),
                fg: c(0xe6eaf2),
                dim: c(0x9aa4b6),
                faint: c(0x6b7487),
                caption: c(0xc3cbd8),
                mint: c(0x4ade9b),
                mint_ink: c(0x052e1c),
                red: c(0xff6b6b),
                amber: c(0xfbbf24),
                blue: c(0x60a5fa),
                violet: c(0xa78bfa),
                well: c(0x0b0f15),
                inset: c(0x0d1219),
                wash: white(),
            }
        } else {
            let c = |hex| Hsla::from(rgb(hex));
            let bg = theme.background;
            Self {
                dark: false,
                window: theme.secondary,
                sidebar: (bg, theme.secondary),
                card: (bg, bg.blend(theme.secondary.opacity(0.5))),
                raised: (bg, theme.secondary),
                line: theme.border,
                line_strong: theme.border.blend(theme.muted_foreground.opacity(0.15)),
                fg: theme.foreground,
                dim: theme.muted_foreground,
                faint: theme.muted_foreground.opacity(0.75),
                caption: theme.foreground.opacity(0.75),
                mint: c(0x149a62),
                mint_ink: white(),
                red: c(0xd93c3c),
                amber: c(0xb7790a),
                blue: c(0x2f6fd0),
                violet: c(0x7856d6),
                well: theme.secondary.blend(theme.border.opacity(0.6)),
                inset: theme.secondary,
                wash: black(),
            }
        }
    }

    pub fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Neutral => self.dim,
            Tone::Mint => self.mint,
            Tone::Red => self.red,
            Tone::Amber => self.amber,
            Tone::Blue => self.blue,
            Tone::Violet => self.violet,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Neutral,
    Mint,
    Red,
    Amber,
    Blue,
    Violet,
}

pub fn gradient(colors: (Hsla, Hsla)) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(colors.0, 0.),
        linear_color_stop(colors.1, 1.),
    )
}

pub fn card(p: &Palette) -> Div {
    div()
        .rounded(px(14.))
        .border_1()
        .border_color(p.line)
        .bg(gradient(p.card))
}

/// A card for what the screen is about, its top lit with a trace of the accent.
pub fn glow_card(p: &Palette) -> Div {
    let top = p
        .card
        .0
        .blend(p.mint.opacity(if p.dark { 0.07 } else { 0.05 }));
    card(p).bg(gradient((top, p.card.1)))
}

pub fn caption(text: impl Into<SharedString>, p: &Palette) -> Div {
    let text: SharedString = text.into();
    div()
        .flex_none()
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.caption)
        .child(text.to_uppercase())
}

pub fn tag(text: impl Into<SharedString>, tone: Tone, p: &Palette) -> Div {
    tag_base(tone, p).child(text.into())
}

/// A tag led by a dot of its colour, for a state such as "Live".
pub fn dot_tag(text: impl Into<SharedString>, tone: Tone, p: &Palette) -> Div {
    tag_base(tone, p)
        .child(dot(p.tone(tone), px(6.)))
        .child(text.into())
}

pub fn tag_base(tone: Tone, p: &Palette) -> Div {
    let color = p.tone(tone);
    let bg = match tone {
        Tone::Neutral => p.wash.opacity(0.05),
        _ => color.opacity(0.12),
    };
    h_flex()
        .flex_none()
        .gap_1p5()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(6.))
        .bg(bg)
        .text_color(color)
        .font_family(MONO)
        .text_size(px(11.))
}

pub fn dot(color: Hsla, size: Pixels) -> Div {
    div().flex_none().size(size).rounded_full().bg(color)
}

/// A rounded square holding an icon, tinted with `color`.
pub fn tile(icon: impl Into<Icon>, color: Hsla, size: Pixels, p: &Palette) -> Div {
    let (bg, border) = match color == p.dim {
        true => (p.wash.opacity(0.03), p.line_strong),
        false => (color.opacity(0.1), color.opacity(0.3)),
    };
    div()
        .flex_none()
        .size(size)
        .rounded(size / 4.)
        .border_1()
        .border_color(border)
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).size(size * 0.45).text_color(color))
}

pub fn kbd(text: &'static str, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(5.))
        .border_1()
        .border_color(p.line_strong)
        .bg(p.wash.opacity(0.04))
        .font_family(MONO)
        .text_size(px(11.))
        .text_color(p.faint)
        .child(text)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Normal,
    Primary,
    Ghost,
}

/// A button to fill with an icon and a label. Make it smaller with padding and text size.
pub fn button(id: impl Into<ElementId>, kind: Button, p: &Palette) -> Stateful<Div> {
    let base = h_flex()
        .id(id)
        .flex_none()
        .gap_2()
        .px(px(14.))
        .py(px(7.))
        .rounded(px(9.))
        .border_1()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer();
    let wash = p.wash;
    match kind {
        Button::Normal => base
            .border_color(p.line_strong)
            .bg(wash.opacity(0.04))
            .text_color(p.fg)
            .hover(move |s| s.bg(wash.opacity(0.08))),
        Button::Primary => {
            let mint = p.mint;
            base.border_color(mint)
                .bg(mint)
                .text_color(p.mint_ink)
                .font_weight(FontWeight::SEMIBOLD)
                .shadow(vec![BoxShadow {
                    color: mint.opacity(0.35),
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(18.),
                    spread_radius: px(-8.),
                    inset: false,
                }])
                .hover(move |s| s.bg(mint.blend(white().opacity(0.08))))
        }
        Button::Ghost => {
            let fg = p.fg;
            base.border_color(transparent_black())
                .text_color(p.dim)
                .hover(move |s| s.bg(wash.opacity(0.05)).text_color(fg))
        }
    }
}

/// A rounded bar filled to `fraction` with `color`.
pub fn bar(fraction: f32, color: Hsla, height: Pixels, p: &Palette) -> Div {
    div()
        .h(height)
        .w_full()
        .rounded_full()
        .bg(p.well)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .rounded_full()
                .bg(color),
        )
}
