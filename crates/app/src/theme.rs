//! The designs' palette (Tokyo Night) and type, as the .pen file states them.
use gpui::{rgb, rgba, Hsla};

pub const SANS: &str = "Source Sans 3";
pub const MONO: &str = "JetBrains Mono";

/// An opaque color, 0xRRGGBB.
pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// A color with alpha, 0xRRGGBBAA.
pub fn ca(hex: u32) -> Hsla {
    rgba(hex).into()
}

pub const WINDOW: u32 = 0x1a1b26;
pub const TITLE: u32 = 0xe6e9fa;
pub const SUBTITLE: u32 = 0xa9b1d6;
pub const TEXT: u32 = 0xc0caf5;
pub const MUTED: u32 = 0x565f89;
pub const FAINT: u32 = 0x737aa2;
pub const LINE: u32 = 0x2f334d;
pub const PANEL: u32 = 0x1d1f2b;
pub const PANEL_LINE: u32 = 0x292e42;
pub const BLUE: u32 = 0x7aa2f7;
pub const RED: u32 = 0xf7768e;
pub const GREEN: u32 = 0x9ece6a;
pub const AMBER: u32 = 0xe0af68;
/// Dark text on a blue button.
pub const ON_BLUE: u32 = 0x16161e;

/// The DFU buttons' colours, the same in the drawing and the steps: Power
/// (Top, Side) pink, Volume Up teal, Home and Volume Down blue.
pub const BUTTON_POWER: u32 = 0xbb9af7;
pub const BUTTON_VOLUME_UP: u32 = 0x73daca;
pub const BUTTON_OTHER: u32 = BLUE;

/// The colour of a button named in the steps.
pub fn button_color(name: &str) -> u32 {
    match name {
        "Power" => BUTTON_POWER,
        "Volume Up" => BUTTON_VOLUME_UP,
        _ => BUTTON_OTHER,
    }
}
