//! The Tauri app's look, rebuilt for GPUI: its tokens (`main.css` `:root`),
//! its three typefaces, its mark, and the handful of pieces every screen and
//! the dropdown share. Dark, as the Tauri app, plus a light twin (Settings →
//! Appearance).
//!
//! The look lives in the component theme (`init`): stock gpui-component
//! widgets (Button, Switch, Sidebar, TitleBar, Alert, GroupBox, ...) pick the
//! Copycat colours, fonts and radii from it. The helpers below return those
//! widgets, preconfigured; screens add behaviour, not styling.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Theme, ThemeMode, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
pub use gpui_kit::assets::IconName;

use crate::settings::Settings;

/// One look's surfaces, lines and text; the orange accents are shared.
pub struct Palette {
    pub ground: u32,
    pub surface: u32,
    pub raised: u32,
    pub raised_2: u32,
    pub line: u32,
    pub line_soft: u32,
    pub text: u32,
    pub muted: u32,
    pub faint: u32,
    /// The rail's own background, between ground and surface.
    pub rail: u32,
    pub popover: u32,
    /// Drop shadows, as RGBA.
    pub shadow: u32,
}

/// The Tauri app's tokens.
pub const DARK: Palette = Palette {
    ground: 0x131519,
    surface: 0x1C1F23,
    raised: 0x23272E,
    raised_2: 0x2A2F37,
    line: 0x2E333A,
    line_soft: 0x23272B,
    text: 0xF2F2F1,
    muted: 0x969CA6,
    faint: 0x666B74,
    rail: 0x191C20,
    popover: 0x2A2F37,
    shadow: 0x000000D9,
};

/// Warm paper, as the DMG background: white rows on a tinted ground.
pub const LIGHT: Palette = Palette {
    ground: 0xF3F1ED,
    surface: 0xFAF9F7,
    raised: 0xFFFFFF,
    raised_2: 0xEFECE7,
    line: 0xE0DCD5,
    line_soft: 0xE9E6E1,
    text: 0x1E2125,
    muted: 0x5C616A,
    faint: 0x8A8F98,
    rail: 0xF1EFEB,
    popover: 0xFFFFFF,
    shadow: 0x00000033,
};

thread_local! {
    /// Which palette the window drawing now uses; set with the theme.
    static DARK_NOW: Cell<bool> = const { Cell::new(true) };
}

/// The palette of the window being drawn.
pub fn pal() -> &'static Palette {
    if is_dark() { &DARK } else { &LIGHT }
}

/// Whether the window being drawn is in the dark look.
pub fn is_dark() -> bool {
    DARK_NOW.get()
}

pub const ACTIVE: u32 = 0xE4672B;
pub const ACTIVE_2: u32 = 0xF0834E;
pub const GOLD: u32 = 0xD2A84A;
pub const OK: u32 = 0x63C088;
pub const WARN: u32 = 0xD2A84A;
pub const BAD: u32 = 0xE0705E;
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

    // Switching mode reloads the registered theme over any edit, so each
    // look is built once and kept whole, then swapped in per window (`themed`).
    let light = build(ThemeMode::Light, &LIGHT, cx);
    let dark = build(ThemeMode::Dark, &DARK, cx);
    cx.set_global(Looks { light, dark });
}

/// Both looks, each the component theme and the base theme under it.
struct Looks {
    light: (Theme, gpui_kit::base::Theme),
    dark: (Theme, gpui_kit::base::Theme),
}

impl Global for Looks {}

fn build(mode: ThemeMode, p: &Palette, cx: &mut App) -> (Theme, gpui_kit::base::Theme) {
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = UI.into();
        theme.font_size = px(13.5);
        theme.mono_font_family = MONO.into();
        theme.mono_font_size = px(12.5);
        theme.radius = px(8.);
        theme.radius_lg = px(11.);
        let c = &mut theme.colors;
        c.background = rgb(p.surface).into();
        c.foreground = rgb(p.text).into();
        c.border = rgb(p.line).into();
        c.muted = rgb(p.raised).into();
        c.muted_foreground = rgb(p.muted).into();
        c.accent = rgb(p.raised).into();
        c.accent_foreground = rgb(p.text).into();
        c.primary = rgb(ACTIVE).into();
        c.primary_hover = rgb(ACTIVE_2).into();
        c.primary_active = rgb(ACTIVE).into();
        c.primary_foreground = rgb(ON_ACTIVE).into();
        c.secondary = rgb(p.raised).into();
        c.secondary_hover = rgb(p.raised_2).into();
        c.secondary_active = rgb(p.raised_2).into();
        c.secondary_foreground = rgb(p.text).into();
        c.success = rgb(OK).into();
        c.warning = rgb(WARN).into();
        c.danger = rgb(BAD).into();
        c.info = rgb(ACTIVE).into();
        c.list = rgb(p.surface).into();
        c.list_hover = rgb(p.raised).into();
        c.list_active = rgba(WASH).into();
        c.list_active_border = rgb(ACTIVE).into();
        c.input = rgb(p.line).into();
        c.ring = rgb(ACTIVE).into();
        c.caret = rgb(ACTIVE).into();
        c.selection = rgba(0xE4672B4D).into();
        c.popover = rgb(p.popover).into();
        c.popover_foreground = rgb(p.text).into();
        c.sidebar = rgb(p.rail).into();
        c.sidebar_foreground = rgb(p.muted).into();
        c.sidebar_border = rgb(p.line_soft).into();
        c.sidebar_accent = rgba(WASH).into();
        c.sidebar_accent_foreground = rgb(p.text).into();
        c.scrollbar = gpui_kit::transparent_black();
        c.scrollbar_thumb = rgb(p.line).into();
        c.scrollbar_thumb_hover = rgb(p.faint).into();
        c.title_bar = rgb(p.rail).into();
        c.title_bar_border = rgb(p.line_soft).into();
        c.window_border = rgb(p.line).into();
        c.sidebar_primary = rgb(ACTIVE).into();
        c.sidebar_primary_foreground = rgb(ON_ACTIVE).into();
        c.group_box = rgb(p.ground).into();
        c.group_box_foreground = rgb(p.faint).into();
        c.tab_bar = rgb(p.rail).into();
        c.tab = rgba(0).into();
        c.tab_active = rgb(p.raised).into();
        c.tab_foreground = rgb(p.muted).into();
        c.tab_active_foreground = rgb(p.text).into();
        c.accordion = rgb(p.ground).into();
        c.danger_hover = rgb(BAD).into();
        c.danger_active = rgb(BAD).into();
        c.danger_foreground = rgb(ON_ACTIVE).into();
        c.success_foreground = rgb(ON_ACTIVE).into();
        c.warning_foreground = rgb(ON_ACTIVE).into();
        c.info_foreground = rgb(ON_ACTIVE).into();
        c.overlay = rgba(0x00000080).into();
    });
    (cx.global::<Theme>().clone(), cx.global::<gpui_kit::base::Theme>().clone())
}

/// Put this window's look in place before it draws: the theme is one global
/// shared by every window, so each window's root view sets its own, from
/// Settings → Appearance (`floating` for the floating button's own switch)
/// and the window's system appearance. Only swaps on a change, without a
/// refresh, so windows in different looks do not redraw each other.
pub fn themed(floating: bool, window: &Window, cx: &mut App) {
    let settings = Settings::get(cx).read(cx);
    let appearance = if floating { settings.floating_appearance } else { settings.appearance };
    let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let dark = appearance.dark(system_dark);
    DARK_NOW.set(dark);
    if cx.theme().is_dark() != dark {
        let looks = cx.global::<Looks>();
        let (theme, base) = if dark { looks.dark.clone() } else { looks.light.clone() };
        cx.set_global(theme);
        cx.set_global(base);
    }
}

/// Redraw `window` when the system appearance changes: "system" looks follow it.
pub fn follow_system(window: &Window) {
    window.observe_window_appearance(|window, _| window.refresh()).detach();
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
        .child(div().text_color(rgb(pal().muted)).mb(px(22.)).child(sub.into()))
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
    let core = div().size(px(7.)).rounded_full().bg(rgb(if connected { OK } else { pal().faint }));
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

/// A stock [`Button`]. The caller adds the click.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind, small: bool) -> Button {
    let b = Button::new(id).label(label);
    let b = match kind {
        Kind::Plain => b,
        Kind::Primary => b.primary(),
        Kind::Ghost => b.ghost(),
        Kind::Danger => b.danger().outline(),
        Kind::GhostDanger => b.ghost().text_color(rgb(BAD)),
    };
    if small { b.small() } else { b }
}

/// Orange call-to-action.
pub fn primary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    button(id, label, Kind::Primary, false)
}

/// Borderless button.
pub fn ghost(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    button(id, label, Kind::Ghost, false)
}

/// Small icon-only button (replaces [`glyph_button`]); `.tooltip(..)` it.
pub fn icon_button(id: impl Into<ElementId>, icon: IconName) -> Button {
    Button::new(id).icon(icon).ghost().small()
}

/// `.card`: a stock [`GroupBox`] titled in the mono caps style. Add children
/// with `.child(..)`.
pub fn section(title: &str) -> GroupBox {
    GroupBox::new().fill().title(mono(11.).text_color(rgb(pal().faint)).child(spaced(&title.to_uppercase(), 1.65)))
}
