//! Everything the window draws that isn't code: the designs' Lucide icons,
//! the device glyphs and the fonts, built into the binary.
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

macro_rules! embed {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $path)) as &[u8])),*]
    };
}

static FILES: &[(&str, &[u8])] = embed!(
    "icons/app-window.svg", "icons/arrow-up-right.svg", "icons/check.svg", "icons/chevron-right.svg",
    "icons/circle-alert.svg", "icons/circle.svg", "icons/eye.svg", "icons/laptop.svg", "icons/life-buoy.svg",
    "icons/plug-zap.svg", "icons/plug.svg", "icons/power-off.svg", "icons/tablet.svg", "icons/triangle-alert.svg",
    "device/alert.svg", "device/check.svg", "device/notch-screen-fill.svg", "device/notch-screen-line.svg",
    "device/spoke-1.svg", "device/spoke-2.svg", "device/spoke-3.svg", "device/spoke-4.svg",
    "device/spoke-5.svg", "device/spoke-6.svg", "device/spoke-7.svg", "device/spoke-8.svg",
    "device/unplugged.svg", "device/ring-track.svg", "device/ring-arc.svg",
    "device/shadow-ipad-home.png", "device/shadow-ipad-no-home.png",
    "device/shadow-iphone-home.png", "device/shadow-iphone-no-home.png",
);

pub static FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/SourceSans3-Regular.ttf"),
    include_bytes!("../assets/fonts/SourceSans3-Semibold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
];

/// The system's wordmark, from the installer's stamp (none in the launcher).
pub static WORDMARK: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "device/wordmark.svg" {
            return Ok(WORDMARK.get().map(|w| Cow::Borrowed(w.as_slice())));
        }
        Ok(FILES.iter().find(|(p, _)| *p == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}
