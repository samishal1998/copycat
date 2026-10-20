//! The Tauri app's look, rebuilt for GPUI: its tokens (`main.css` `:root`),
//! its three typefaces, its mark, and the handful of pieces every screen and
//! the dropdown share. Dark only, like the Tauri app.
//!
//! Most of the frame is drawn from plain divs rather than gpui-component
//! widgets: the widgets bring their own paddings and heights, and the point
//! is to match the CSS. The component theme is still set from the tokens, for
//! the widgets that remain (the search input, scrollbars).

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::{Theme, ThemeMode, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

pub const GROUND: u32 = 0x131519;
pub const SURFACE: u32 = 0x1C1F23;
pub const RAISED: u32 = 0x23272E;
pub const RAISED_2: u32 = 0x2A2F37;
pub const LINE: u32 = 0x2E333A;
pub const LINE_SOFT: u32 = 0x23272B;
pub const TEXT: u32 = 0xF2F2F1;
pub const MUTED: u32 = 0x969CA6;
pub const FAINT: u32 = 0x666B74;
pub const ACTIVE: u32 = 0xE4672B;
pub const ACTIVE_2: u32 = 0xF0834E;
pub const GOLD: u32 = 0xD2A84A;
pub const OK: u32 = 0x63C088;
pub const WARN: u32 = 0xD2A84A;
pub const BAD: u32 = 0xE0705E;
/// The rail's own background, between ground and surface.
pub const RAIL: u32 = 0x191C20;
/// Text on an orange button.
pub const ON_ACTIVE: u32 = 0x1B1206;
/// `--active-wash`, rgba(228,103,43,.13), as RGBA for `rgba()`.
pub const WASH: u32 = 0xE4672B21;
/// The orange border of a "next" row: --active at 50%.
pub const ACTIVE_LINE: u32 = 0xE4672B80;

pub const MONO: &str = "JetBrains Mono";
pub const UI: &str = "Inter";
pub const DISPLAY: &str = "Bricolage Grotesque";

/// Register the bundled fonts and set the component theme from the tokens.
/// Once, after `gpui_kit::init` and before any window opens.
pub fn init(cx: &mut App) {
    // Bundled, not looked up: none of the three ships with macOS, and the
    // Tauri app pulls them from Google Fonts, which a native app should not.
    let fonts: [&'static [u8]; 6] = [
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
        include_bytes!("../assets/fonts/BricolageGrotesque-Bold.ttf"),
    ];
    cx.text_system()
        .add_fonts(fonts.into_iter().map(Cow::Borrowed).collect())
        .expect("bundled fonts");

    // Dark first: switching mode reloads the registered theme over any edit.
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = UI.into();
        theme.font_size = px(13.5);
        theme.mono_font_family = MONO.into();
        theme.mono_font_size = px(12.5);
        theme.radius = px(8.);
        theme.radius_lg = px(11.);
        let c = &mut theme.colors;
        c.background = rgb(SURFACE).into();
        c.foreground = rgb(TEXT).into();
        c.border = rgb(LINE).into();
        c.muted = rgb(RAISED).into();
        c.muted_foreground = rgb(MUTED).into();
        c.accent = rgb(RAISED).into();
        c.accent_foreground = rgb(TEXT).into();
        c.primary = rgb(ACTIVE).into();
        c.primary_hover = rgb(ACTIVE_2).into();
        c.primary_active = rgb(ACTIVE).into();
        c.primary_foreground = rgb(ON_ACTIVE).into();
        c.secondary = rgb(RAISED).into();
        c.secondary_hover = rgb(RAISED_2).into();
        c.secondary_active = rgb(RAISED_2).into();
        c.secondary_foreground = rgb(TEXT).into();
        c.success = rgb(OK).into();
        c.warning = rgb(WARN).into();
        c.danger = rgb(BAD).into();
        c.info = rgb(ACTIVE).into();
        c.list = rgb(SURFACE).into();
        c.list_hover = rgb(RAISED).into();
        c.list_active = rgba(WASH).into();
        c.list_active_border = rgb(ACTIVE).into();
        c.input = rgb(LINE).into();
        c.ring = rgb(ACTIVE).into();
        c.caret = rgb(ACTIVE).into();
        c.selection = rgba(0xE4672B4D).into();
        c.popover = rgb(RAISED_2).into();
        c.popover_foreground = rgb(TEXT).into();
        c.sidebar = rgb(RAIL).into();
        c.sidebar_foreground = rgb(MUTED).into();
        c.sidebar_border = rgb(LINE_SOFT).into();
        c.sidebar_accent = rgba(WASH).into();
        c.sidebar_accent_foreground = rgb(TEXT).into();
        c.scrollbar = gpui_kit::transparent_black();
        c.scrollbar_thumb = rgb(LINE).into();
        c.scrollbar_thumb_hover = rgb(FAINT).into();
        c.title_bar = rgb(RAIL).into();
        c.window_border = rgb(LINE).into();
    });
}

/// The brand mark, `mark.svg` drawn in colour. An `img`, not an `svg()`:
/// GPUI paints `svg()` as a one-colour mask. The file declares 22×22, the
/// size it is drawn at, so it is rasterized for that size and stays crisp.
pub fn mark(size: f32) -> Img {
    static MARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(ImageFormat::Svg, include_bytes!("../assets/mark.svg").to_vec()))
    });
    img(MARK.clone()).size(px(size)).flex_none()
}

/// Text in the mono face at `size`.
pub fn mono(size: f32) -> Div {
    div().font_family(MONO).text_size(px(size))
}

/// CSS `letter-spacing`, which GPUI lacks: one element per character, with the
/// spacing as the gap between them.
pub fn spaced(text: &str, spacing: f32) -> Div {
    h_flex().gap(px(spacing)).children(text.chars().map(|c| c.to_string()))
}

/// A screen's title and the line under it (`.screen h1` and `.sub`).
pub fn heading(title: impl Into<SharedString>, sub: impl Into<SharedString>) -> Div {
    div()
        .child(
            div()
                .font_family(DISPLAY)
                .font_weight(FontWeight::BOLD)
                .text_size(px(24.))
                .line_height(px(36.))
                .mb(px(4.))
                .child(title.into()),
        )
        .child(div().text_color(rgb(MUTED)).mb(px(22.)).child(sub.into()))
}

/// `.card` with its `h2`.
pub fn card(title: &str) -> Div {
    card_in(title, FAINT)
}

/// `.card` with its `h2` in `title_color`.
pub fn card_in(title: &str, title_color: u32) -> Div {
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(rgb(LINE))
        .rounded(px(11.))
        .bg(rgb(GROUND))
        .px(px(18.))
        .py(px(16.))
        .mb(px(16.))
        .child(
            mono(11.)
                .text_color(rgb(title_color))
                .mb(px(12.))
                .child(spaced(&title.to_uppercase(), 1.65)),
        )
}

/// A box with the CSS's heavy orange left edge (`border-left:3px solid
/// var(--active)` next to a 1px `border` of another colour). GPUI borders take
/// one colour, so the edge is the orange box showing past the inner one.
pub fn accent_edge(inner: impl Styled + IntoElement, border: Rgba, radius: f32) -> Div {
    div().rounded(px(radius)).bg(rgb(ACTIVE)).pl(px(3.)).child(
        inner
            .rounded_r(px(radius))
            .rounded_l(px(radius - 3.))
            .border_1()
            .border_l_0()
            .border_color(border),
    )
}

/// The connection dot, with the dropdown's soft ring when `ring`.
pub fn dot(connected: bool, ring: bool) -> Div {
    let core = div().size(px(7.)).rounded_full().bg(rgb(if connected { OK } else { FAINT }));
    if ring && connected {
        div().size(px(13.)).rounded_full().bg(rgba(0x63C08829)).flex().items_center().justify_center().child(core)
    } else {
        core
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Primary,
    Ghost,
    Danger,
    GhostDanger,
}

/// `.btn` from main.css, `.btn.small` when `small`. The caller adds the click.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind, small: bool) -> Stateful<Div> {
    let ghost = matches!(kind, Kind::Ghost | Kind::GhostDanger);
    let (bg, border, fg) = match kind {
        Kind::Primary => (rgb(ACTIVE), rgb(ACTIVE), rgb(ON_ACTIVE)),
        _ if ghost => (rgba(0), rgba(0), rgb(MUTED)),
        _ => (rgb(RAISED), rgb(LINE), rgb(TEXT)),
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(7.))
        .rounded(px(8.))
        .border_1()
        .bg(bg)
        .border_color(border)
        .text_color(fg)
        .font_weight(if kind == Kind::Primary { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
        // A `<button>` does not inherit the body's 1.5 line height.
        .line_height(relative(1.2))
        .map(|this| {
            if small {
                this.px(px(8.)).py(px(4.)).text_size(px(11.5))
            } else {
                this.px(px(12.)).py(px(7.)).text_size(px(12.5))
            }
        })
        .cursor_pointer()
        .hover(move |style| match kind {
            Kind::Primary => style.bg(rgb(ACTIVE_2)),
            Kind::Plain => style.border_color(rgb(FAINT)),
            Kind::Ghost => style.bg(rgb(RAISED)).text_color(rgb(TEXT)),
            Kind::Danger => style.border_color(rgb(BAD)).text_color(rgb(BAD)),
            Kind::GhostDanger => style.bg(rgb(RAISED)).border_color(rgb(BAD)).text_color(rgb(BAD)),
        })
        .child(label.into())
}

/// A small square bordered button (the dropdown's `.icon-btn`, `.step`,
/// `.page-btn`, the session list's `.sl-btn`): a glyph, dimmed and inert when
/// disabled. The caller adds the click, and only when enabled.
pub fn glyph_button(id: impl Into<ElementId>, glyph: &'static str, w: f32, h: f32, disabled: bool) -> Stateful<Div> {
    glyph_button_hover(id, glyph, w, h, disabled, |style| style.text_color(rgb(TEXT)).border_color(rgb(FAINT)))
}

/// [`glyph_button`] with its own hover style. GPUI allows one hover per
/// element (a second one panics in debug builds), so it is a parameter.
pub fn glyph_button_hover(
    id: impl Into<ElementId>,
    glyph: &'static str,
    w: f32,
    h: f32,
    disabled: bool,
    hover: fn(StyleRefinement) -> StyleRefinement,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(w))
        .h(px(h))
        .rounded(px(if h >= 22. { 6. } else { 5. }))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(RAISED))
        .text_color(rgb(MUTED))
        .when(disabled, |this| this.opacity(0.35))
        .when(!disabled, |this| {
            this.cursor_pointer().hover(hover)
        })
        .child(glyph)
}
