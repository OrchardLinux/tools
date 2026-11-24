//! The persistent device drawing (spec: "Device states"): one drawing per
//! button layout, with the screen off or lit, a glyph, the error tint, the
//! buttons the user has to press highlighted, and rings while waiting.
//! Geometry is the .pen file's, in a 290×400 frame.
use crate::theme::*;
use gpui::{div, img, prelude::*, px, svg, Div, Hsla, Pixels};
use orchard_flow::{DeviceView, Glyph, Screen, Tint};
use orchard_flow::{Model, Sequence};

pub const WIDTH: f32 = 290.;
pub const HEIGHT: f32 = 400.;

fn at(x: f32, y: f32, w: f32, h: f32) -> Div {
    div().absolute().left(px(x)).top(px(y)).w(px(w)).h(px(h))
}

/// The colors a drawing is in, from its state.
struct Paint {
    body_line: Hsla,
    screen: Hsla,
    screen_line: Hsla,
    idle_button: Hsla,
    pressed_button: Hsla,
}

fn paint(view: &DeviceView) -> Paint {
    let error = view.tint == Tint::Error;
    let (screen, screen_line) = match (error, view.screen) {
        (true, _) => (c(0x1c1720), ca(0xf7768e40)),
        (false, Screen::Lit) => (c(0x22263a), ca(0x7aa2f740)),
        (false, Screen::Off) => (c(0x16161e), c(LINE)),
    };
    Paint {
        body_line: if error { ca(0xf7768eb3) } else { c(0x414868) },
        screen,
        screen_line,
        idle_button: if error { ca(0xf7768eb3) } else { c(0x414868) },
        pressed_button: c(BLUE),
    }
}

/// Which physical buttons a sequence asks for.
#[derive(Clone, Copy, PartialEq)]
enum Button {
    Home,
    /// Power on iPads with a Home button, Top on those without.
    Top,
    Side,
    VolumeUp,
    VolumeDown,
}

fn pressed(sequence: Option<Sequence>, model: Model) -> &'static [Button] {
    match (sequence, model) {
        (None, _) => &[],
        (Some(Sequence::HomePower), Model::IpadHome) => &[Button::Home, Button::Top],
        (Some(Sequence::HomePower), _) => &[Button::Home, Button::Side],
        (Some(Sequence::VolumeDownSide), _) => &[Button::VolumeDown, Button::Side],
        (Some(Sequence::VolumeSequence), Model::IpadNoHome) => &[Button::VolumeUp, Button::VolumeDown, Button::Top],
        (Some(Sequence::VolumeSequence), _) => &[Button::VolumeUp, Button::VolumeDown, Button::Side],
    }
}

/// A small ring spinner for lists, distinct from the device's spokes: a
/// faint circle with a blue arc turning once a second.
pub fn ring(size: f32, seconds: f32) -> Div {
    let turn = gpui::radians(seconds.rem_euclid(1.0) * std::f32::consts::TAU);
    div()
        .relative()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .child(at(0., 0., size, size).child(svg().path("device/ring-track.svg").size_full().text_color(c(PANEL_LINE))))
        .child(
            at(0., 0., size, size)
                .child(svg().path("device/ring-arc.svg").size_full().text_color(c(BLUE)).with_transformation(gpui::Transformation::rotate(turn))),
        )
}

/// The glyph, centred on the body of `width`×367 at `left`.
fn glyph(glyph: Glyph, left: f32, width: f32, step: usize) -> Option<Div> {
    let centre = |w: f32, h: f32| at(left + (width - w) / 2., 16. + (367. - h) / 2., w, h);
    Some(match glyph {
        Glyph::None => return None,
        Glyph::Check => centre(44., 44.).child(svg().path("device/check.svg").size_full().text_color(ca(0xc0caf599))),
        Glyph::Spinner => {
            // The design's eight spokes, brightest first, turning one step at a time.
            const ALPHA: [f32; 8] = [1.0, 0.8863, 0.7725, 0.6588, 0.5412, 0.4275, 0.3137, 0.2];
            let mut spinner = centre(32., 32.).opacity(0.6);
            for i in 0..8 {
                let alpha = ALPHA[(i + 8 - step % 8) % 8];
                let color = Hsla { a: alpha, ..c(SUBTITLE) };
                spinner = spinner.child(at(0., 0., 32., 32.).child(svg().path(format!("device/spoke-{}.svg", i + 1)).size_full().text_color(color)));
            }
            spinner
        }
        // The stamp's wordmark; without one, the system's name.
        Glyph::Wordmark if crate::assets::WORDMARK.get().is_some() => {
            centre(120., 28.15).child(svg().path("device/wordmark.svg").size_full().text_color(ca(0xc0caf566)))
        }
        Glyph::Wordmark => centre(160., 28.)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(18.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(ca(0xc0caf566))
            .child(orchard_flow::system_name()),
        Glyph::Sad => centre(52., 52.)
            .flex()
            .items_center()
            .justify_center()
            .font_family(MONO)
            .text_size(px(40.))
            .text_color(c(MUTED))
            .child(":("),
        Glyph::Unplugged => centre(36., 36.).child(svg().path("device/unplugged.svg").size_full().text_color(c(RED))),
        Glyph::Alert => centre(36., 36.).child(svg().path("device/alert.svg").size_full().text_color(c(RED))),
        Glyph::Usb => centre(36., 36.).child(svg().path("icons/plug.svg").size_full().text_color(c(MUTED))),
    })
}

/// The body's drop shadow (design: 0 16 40 #00000033), baked with dithering
/// by tools/shadows.py: drawn by the GPU it bands on the dark window.
fn shadow(model: Model, left: f32, width: f32) -> Div {
    const MARGIN: f32 = 80.;
    let name = match model {
        Model::IpadHome => "ipad-home",
        Model::IpadNoHome => "ipad-no-home",
        Model::IphoneHome => "iphone-home",
        Model::IphoneNoHome => "iphone-no-home",
    };
    at(left - MARGIN, 16. - MARGIN, width + 2. * MARGIN, 367. + 2. * MARGIN)
        .child(img(format!("device/shadow-{name}.png")).size_full())
}

/// While waiting, waves leave the device and travel outward, fading as
/// they grow, like a radio ping: one every PERIOD / WAVES seconds.
pub fn waves(view: &DeviceView, seconds: f32) -> Option<Div> {
    if !view.rings {
        return None;
    }
    const PERIOD: f32 = 6.0;
    const WAVES: usize = 3;
    const FROM: f32 = 340.;
    const TO: f32 = 540.;
    let error = view.tint == Tint::Error;
    // The design's innermost ring, the brightest, is where a wave starts.
    let base = if error { ca(0xf7768e21) } else { ca(0x7aa2f716) };
    let mut layer = div().absolute().left(px(0.)).top(px(0.)).w(px(WIDTH)).h(px(HEIGHT));
    for i in 0..WAVES {
        let t = (seconds / PERIOD + i as f32 / WAVES as f32).rem_euclid(1.0);
        let size = FROM + (TO - FROM) * (1. - (1. - t).powi(2));
        // Fades in over the first tenth, then out as it grows.
        let alpha = base.a * 2.2 * (t * 10.).min(1.) * (1. - t).powf(1.6);
        let x = (WIDTH - size) / 2.;
        let y = 16. + 367. / 2. - size / 2.;
        layer = layer.child(at(x, y, size, size).rounded_full().border_1().border_color(Hsla { a: alpha, ..base }));
    }
    Some(layer)
}

fn bar(x: f32, y: f32, w: f32, h: f32, color: Hsla, radius: [f32; 4]) -> Div {
    let r = |v: f32| -> Pixels { px(v) };
    at(x, y, w, h)
        .bg(color)
        .rounded_tl(r(radius[0]))
        .rounded_tr(r(radius[1]))
        .rounded_br(r(radius[2]))
        .rounded_bl(r(radius[3]))
}

/// `view`'s device, or for `view.pair` (1.11) the iPhone with a Home button
/// in front on the left and the iPad without one behind on the right, every
/// button of both in sight.
pub fn drawing(view: &DeviceView, seconds: f32, guide: Option<[f32; 4]>) -> Div {
    if !view.pair {
        return device(view, seconds, guide);
    }
    let iphone = DeviceView { model: Model::IphoneHome, rings: false, pair: false, ..*view };
    let ipad = DeviceView { pair: false, ..*view };
    div()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex_none()
        .child(at(35., -10., WIDTH, HEIGHT).child(device(&ipad, seconds, guide)))
        .child(at(-85., 20., WIDTH, HEIGHT).child(device(&iphone, seconds, guide)))
}

/// The device as `view` describes it, `seconds` into the app's life (for
/// the spinner). The waves are a separate layer ([`waves`]).
pub fn device(view: &DeviceView, seconds: f32, guide: Option<[f32; 4]>) -> Div {
    let step = (seconds * 10.) as usize;
    let p = paint(view);
    let pressed = pressed(view.buttons, view.model);
    // Each pressed button in its own colour (theme::button_color's). While
    // "Guide me" runs, the ones of the moment glow and the rest are muted.
    let as_guide = |b: Button| match b {
        Button::Top | Button::Side => crate::guide::Button::Power,
        Button::Home => crate::guide::Button::Home,
        Button::VolumeUp => crate::guide::Button::VolumeUp,
        Button::VolumeDown => crate::guide::Button::VolumeDown,
    };
    // How lit the button is, 0…1 (fading in and out with the guide's timeline).
    let level = |b: Button| if pressed.contains(&b) { guide.map(|g| g[as_guide(b).slot()]).unwrap_or(0.) } else { 0. };
    let button = |b: Button| {
        if !pressed.contains(&b) {
            return p.idle_button;
        }
        let mut color = match b {
            Button::Top | Button::Side => c(BUTTON_POWER),
            Button::VolumeUp => c(BUTTON_VOLUME_UP),
            Button::Home | Button::VolumeDown => c(BUTTON_OTHER),
        };
        if guide.is_some() {
            color.a = 0.35 + 0.65 * level(b);
        }
        color
    };
    // The glow: a soft halo in the button's colour.
    let shine = |d: Div, b: Button| {
        let lit = level(b);
        if lit < 0.01 {
            return d;
        }
        let mut halo = button(b);
        halo.a = 0.8 * lit;
        d.shadow(vec![gpui::BoxShadow { color: halo, offset: gpui::point(px(0.), px(0.)), blur_radius: px(10.), spread_radius: px(1.) }])
    };
    let mut frame = div().relative().w(px(WIDTH)).h(px(HEIGHT)).flex_none();
    let body = |left: f32, width: f32, radius: f32| {
        at(left, 16., width, 367.).bg(c(0x1f2230)).rounded(px(radius)).border_1().border_color(p.body_line)
    };
    let screen = |x: f32, y: f32, w: f32, h: f32, r: f32| at(x, y, w, h).bg(p.screen).rounded(px(r)).border_1().border_color(p.screen_line);
    let (body_left, body_width) = match view.model {
        Model::IpadHome => {
            frame = frame.child(shadow(Model::IpadHome, 15., 260.));
            frame = frame
                .child(
                    body(15., 260., 22.)
                        .child(screen(17., 33., 226., 301., 1.))
                        .child(at(127., 14., 5., 5.).rounded_full().bg(c(0x414868)))
                        .child(shine(at(121., 342., 18., 18.).rounded_full().border_2().border_color(button(Button::Home)), Button::Home)),
                )
                .child(shine(bar(241., 12., 17., 4., button(Button::Top), [2., 2., 0., 0.]), Button::Top));
            (15., 260.)
        }
        Model::IpadNoHome => {
            frame = frame.child(shadow(Model::IpadNoHome, 12.5, 265.));
            frame = frame
                .child(body(12.5, 265., 27.).child(screen(15., 15., 235., 337., 18.)))
                .child(shine(bar(220., 12., 18., 4., button(Button::Top), [2., 2., 0., 0.]), Button::Top))
                .child(shine(bar(277.5, 53., 4., 26., button(Button::VolumeUp), [0., 2., 2., 0.]), Button::VolumeUp))
                .child(shine(bar(277.5, 86., 4., 26., button(Button::VolumeDown), [0., 2., 2., 0.]), Button::VolumeDown));
            (12.5, 265.)
        }
        Model::IphoneHome => {
            frame = frame.child(shadow(Model::IphoneHome, 56., 178.));
            frame = frame
                .child(
                    body(56., 178., 26.)
                        .child(screen(11.5, 45.5, 155., 276., 1.))
                        .child(at(69., 21., 40., 4.).rounded(px(2.)).bg(c(0x414868)))
                        .child(at(52., 19., 7., 7.).rounded_full().bg(c(0x414868)))
                        .child(shine(at(75.5, 330., 27., 27.).rounded_full().border_2().border_color(button(Button::Home)), Button::Home)),
                )
                .child(shine(bar(234., 114., 3., 34., button(Button::Side), [0., 1.5, 1.5, 0.]), Button::Side))
                .child(bar(53., 69., 3., 15., p.idle_button, [1.5, 0., 0., 1.5]))
                .child(shine(bar(53., 109., 3., 24., button(Button::VolumeUp), [1.5, 0., 0., 1.5]), Button::VolumeUp))
                .child(shine(bar(53., 141., 3., 24., button(Button::VolumeDown), [1.5, 0., 0., 1.5]), Button::VolumeDown));
            (56., 178.)
        }
        Model::IphoneNoHome => {
            frame = frame.child(shadow(Model::IphoneNoHome, 54.5, 181.));
            // The notched screen is a path: fill and outline drawn separately.
            frame = frame
                .child(
                    body(54.5, 181., 30.)
                        .child(at(10., 10., 161., 347.).child(svg().path("device/notch-screen-fill.svg").size_full().text_color(p.screen)))
                        .child(at(10., 10., 161., 347.).child(svg().path("device/notch-screen-line.svg").size_full().text_color(p.screen_line))),
                )
                .child(shine(bar(235.5, 113., 3., 30., button(Button::Side), [0., 1.5, 1.5, 0.]), Button::Side))
                .child(bar(51.5, 93., 3., 13., p.idle_button, [1.5, 0., 0., 1.5]))
                .child(shine(bar(51.5, 134., 3., 24., button(Button::VolumeUp), [1.5, 0., 0., 1.5]), Button::VolumeUp))
                .child(shine(bar(51.5, 169., 3., 24., button(Button::VolumeDown), [1.5, 0., 0., 1.5]), Button::VolumeDown));
            (54.5, 181.)
        }
    };
    if let Some(g) = glyph(view.glyph, body_left, body_width, step) {
        frame = frame.child(g);
    }
    frame
}
