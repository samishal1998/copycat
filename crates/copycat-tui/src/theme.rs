//! Colours, from `docs/BRAND.md`.
//!
//! The brand guidance is explicit that orange marks active semantic state and
//! that not every control should be orange, so accent here means "this is the
//! thing currently in effect" — the selected clip, the live session, a failing
//! check — and everything else is charcoal, slate, and white.

use ratatui::style::{Color, Modifier, Style};

/// Burnt orange. Active state only.
pub const ACCENT: Color = Color::Rgb(0xE4, 0x67, 0x2B);
/// Charcoal, the dark surface.
pub const SURFACE: Color = Color::Rgb(0x1C, 0x1F, 0x23);
/// Slate, for secondary text and inactive elements.
pub const MUTED: Color = Color::Rgb(0x6B, 0x6F, 0x76);
/// Off-white, primary text.
pub const TEXT: Color = Color::Rgb(0xF7, 0xF7, 0xF7);
/// Soft gold, used sparingly for things worth noticing but not wrong.
pub const NOTICE: Color = Color::Rgb(0xD2, 0xA8, 0x4A);

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn label() -> Style {
    Style::default().fg(MUTED)
}

pub fn body() -> Style {
    Style::default().fg(TEXT)
}

pub fn selected() -> Style {
    Style::default().fg(SURFACE).bg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn notice() -> Style {
    Style::default().fg(NOTICE)
}

pub fn tab_active() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn tab_inactive() -> Style {
    Style::default().fg(MUTED)
}
