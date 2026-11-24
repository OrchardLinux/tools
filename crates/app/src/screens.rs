//! The text column of each screen, as the .pen file designs it. Copy is the
//! approved copy, verbatim.
use crate::theme::*;
use crate::Installer;
use gpui::{div, prelude::*, px, svg, AnyElement, Context, Div, FontWeight, Hsla, SharedString};
use orchard_flow::ScreenKind;
use orchard_flow::{Device, Event, Model, Seen, Sequence, State, Support, UsbMode};

/// Makes a clickable element a keyboard stop too: Tab reaches it, Enter or
/// Space press it, and `ring` styles it while focused.
pub fn pressable(
    el: gpui::Stateful<Div>,
    ring: impl FnOnce(gpui::StyleRefinement) -> gpui::StyleRefinement,
    cx: &mut Context<Installer>,
    act: impl Fn(&mut Installer, &mut gpui::Window, &mut Context<Installer>) + 'static,
) -> gpui::Stateful<Div> {
    // Rings only while navigating by keyboard. gpui turns Enter and Space on
    // a focused element into a click, so on_click covers the keyboard too.
    let el = if crate::keyboard() { el.focus(ring) } else { el };
    // Behind an open dialog, no keyboard stop (the dialog traps focus); in
    // the dialog, after its default choice.
    let el = if !crate::focusable_now() {
        // Still focusable (focus returns here when the dialog closes), but
        // out of the Tab order.
        el.tab_index(0).tab_stop(false)
    } else if crate::in_dialog() {
        el.tab_index(1)
    } else {
        el.tab_index(0)
    };
    el.on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
        // The key that closed a dialog finishes as a press on whatever got
        // focus back; ignore that one.
        if matches!(e, gpui::ClickEvent::Keyboard(_)) && this.dialog_closed_at.is_some_and(|t| t.elapsed().as_millis() < 400) {
            return;
        }
        act(this, window, cx)
    }))
}

/// A link's frame: invisible at rest (the padding is cancelled by a negative
/// margin, so it sits where the text would), a ghost button when focused.
pub fn link(id: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .px(px(6.))
        .mx(px(-6.))
        .cursor_pointer()
        .border_1()
        .border_color(gpui::transparent_black())
        .hover(|s| s.opacity(0.8))
}

/// The ghost button look of a focused link.
pub fn link_ring(s: gpui::StyleRefinement) -> gpui::StyleRefinement {
    s.bg(c(PANEL)).border_color(c(LINE))
}

/// A Lucide icon from the designs, `size` px, in `color`.
pub fn icon(name: &str, size: f32, color: Hsla) -> Div {
    div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size * 0.92)).text_color(color))
}

/// Title and subtitle (gap 12).
fn header(title: &'static str, subtitle: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .w_full()
        .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(title))
        .child(div().text_size(px(14.)).line_height(px(20.)).text_color(c(SUBTITLE)).child(subtitle))
}

pub fn column() -> Div {
    div().w_full().flex().flex_col().gap(px(44.))
}

/// A run of instruction text: plain, or a button name.
pub enum Part {
    Text(&'static str),
    Button(&'static str),
}
use Part::{Button as B, Text as T};

pub struct Step {
    pub parts: &'static [Part],
    pub duration: Option<&'static str>,
}

const CONNECT: Step = Step { parts: &[T("Connect it to this computer with a USB cable")], duration: None };
const LET_GO: Step = Step { parts: &[T("Let go when this window changes")], duration: None };

/// 1.1 and 1.9: Home button.
const HOME_POWER: &[Step] = &[
    CONNECT,
    Step { parts: &[T("Hold "), B("Home"), T(" and "), B("Power"), T(" together")], duration: Some("~10 s") },
    Step { parts: &[T("Let go of "), B("Power"), T(", keep holding "), B("Home")], duration: Some("~10 s") },
    LET_GO,
];

/// 1.11: no Home button.
const VOLUME_SEQUENCE: &[Step] = &[
    CONNECT,
    Step { parts: &[T("Press "), B("Volume Up"), T(", then "), B("Volume Down")], duration: None },
    Step { parts: &[T("Hold "), B("Power"), T(" until the screen goes black")], duration: Some("~10 s") },
    Step { parts: &[T("Keep holding "), B("Power"), T(", also hold "), B("Volume Down")], duration: Some("~5 s") },
    Step { parts: &[T("Let go of "), B("Power"), T(", keep holding "), B("Volume Down")], duration: Some("~10 s") },
    LET_GO,
];

/// iPhone 7 and 7 Plus: the Home button steps with Volume Down and Power.
const VOLUME_DOWN_SIDE: &[Step] = &[
    CONNECT,
    Step { parts: &[T("Hold "), B("Volume Down"), T(" and "), B("Power"), T(" together")], duration: Some("~10 s") },
    Step { parts: &[T("Let go of "), B("Power"), T(", keep holding "), B("Volume Down")], duration: Some("~10 s") },
    LET_GO,
];

/// The table's height for `rows` rows (fractional while it animates): each
/// row 48 px, a 1 px line between rows, the 1 px frame.
fn table_height(rows: f32) -> f32 {
    rows * 48. + (rows - 1.).max(0.) + 2.
}

/// The step table, its height animated by `rows` (fixed row heights).
pub fn steps(steps: &'static [Step], rows: f32) -> Div {
    table(steps, Some(rows), None)
}

/// The step table at its natural height, for steps that wrap.
pub fn steps_natural(steps: &'static [Step]) -> Div {
    table(steps, None, None)
}

/// How much "Guide me" darkens what isn't the moment's (at full fade).
const GUIDE_DARKEN: f32 = 0.72;

/// The countdown circle of a guided step: a faint disc, filled clockwise
/// from the top as `progress` (0…1) goes, in `color`.
fn countdown(progress: f32, color: gpui::Hsla) -> impl IntoElement {
    let size = 16.;
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = gpui::point(bounds.origin.x + px(size / 2.), bounds.origin.y + px(size / 2.));
            let r = size / 2.;
            let disc = |from: f32, sweep: f32| {
                let mut path = gpui::PathBuilder::fill();
                path.move_to(center);
                let steps = (48. * sweep.abs()).ceil().max(2.) as usize;
                for i in 0..=steps {
                    let a = (from + sweep * i as f32 / steps as f32) * std::f32::consts::TAU;
                    path.line_to(gpui::point(center.x + px(r * a.sin()), center.y - px(r * a.cos())));
                }
                path.close();
                path.build().ok()
            };
            if let Some(track) = disc(0., 1.) {
                window.paint_path(track, c(PANEL_LINE));
            }
            if progress > 0.001 {
                if let Some(done) = disc(0., progress.min(1.)) {
                    window.paint_path(done, color);
                }
            }
        },
    )
    .w(px(size))
    .h(px(size))
    .flex_none()
}

/// The colour of a guided step's countdown: Power's when it is held, else
/// the last glowing button's.
fn guide_color(glow: &[crate::guide::Button]) -> gpui::Hsla {
    use crate::guide::Button as G;
    let name = if glow.contains(&G::Power) {
        "Power"
    } else {
        match glow.last() {
            Some(G::VolumeUp) => "Volume Up",
            _ => "",
        }
    };
    c(button_color(name))
}

/// The steps with "Guide me" running (G.2–G.7): the moment's row as is, the
/// cable step reading "Get ready", the countdown in place of the duration,
/// the other rows darkened by `dim` (the fade, 0…1).
pub fn steps_guided(steps: &'static [Step], rows: f32, guide: Option<crate::guide::Moment>, dim: f32) -> Div {
    table(steps, Some(rows), Some((guide, dim)))
}

fn table(steps: &'static [Step], rows: Option<f32>, guide: Option<(Option<crate::guide::Moment>, f32)>) -> Div {
    let mut list = div().w_full();
    if let Some(rows) = rows {
        list = list.h(px(table_height(rows))).overflow_hidden();
    }
    let fixed = rows.is_some();
    let mut list = list
        .flex()
        .flex_col()
        .bg(c(PANEL))
        .border_1()
        .border_color(c(PANEL_LINE));
    for (i, step) in steps.iter().enumerate() {
        let moment = guide.and_then(|(m, _)| m).filter(|m| m.row == i);
        let dim = guide.map(|(_, d)| d).unwrap_or(0.);
        let mut instruction = div().flex().flex_row().items_center().text_size(px(14.)).text_color(c(TEXT));
        // "Get ready" stands in for the cable step while its time runs.
        let get_ready: &'static [Part] = &[T("Get ready")];
        let parts = if moment.is_some() && i == 0 { get_ready } else { step.parts };
        for part in parts {
            let only = parts.len() == 1;
            instruction = match part {
                // A step of one plain run may wrap (2.1); runs with button names stay on one line.
                Part::Text(t) if only => instruction.flex_1().min_w(px(0.)).line_height(px(20.)).child(div().w_full().child(orchard_flow::for_device(t))),
                Part::Text(t) => instruction.child(div().whitespace_nowrap().child(orchard_flow::for_device(t))),
                Part::Button(b) => {
                    instruction.child(div().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).text_color(c(button_color(b))).child(*b))
                }
            };
        }
        let mut row = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .flex_none()
            .pt(px(12.))
            .pb(px(12.))
            .pl(px(22.))
            .pr(px(18.))
            .child(
                div().w(px(16.)).h(px(24.)).flex().items_center().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(format!("{}", i + 1)),
            )
            .child(instruction)
            // A single run of text fills the row itself; the spacer only pushes
            // a duration to the right of a step with button names.
            .when(parts.len() != 1 || !matches!(parts[0], Part::Text(_)), |r| r.child(div().flex_1()));
        match moment {
            // The moment's row: its countdown instead of the duration.
            Some(m) => {
                if let Some(progress) = m.progress {
                    // The seconds left (none while settling), then the circle.
                    if let Some(left) = m.remaining {
                        row = row.child(div().font_family(MONO).text_size(px(12.)).text_color(c(SUBTITLE)).child(format!("{}", left.ceil() as u32)));
                    }
                    row = row.child(countdown(progress, guide_color(m.glow)));
                }
            }
            None => {
                if let Some(d) = step.duration {
                    row = row.child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(d));
                }
                if guide.is_some() {
                    row = row.opacity(1. - GUIDE_DARKEN * dim);
                }
            }
        }
        if fixed {
            row = row.h(px(if i == 0 { 48. } else { 49. }));
        }
        if i > 0 {
            row = row.border_t_1().border_color(c(PANEL_LINE));
        }
        list = list.child(row);
    }
    list
}

const TABS: [(Sequence, &str); 3] = [
    (Sequence::HomePower, "Home button"),
    (Sequence::VolumeSequence, "No Home button"),
    (Sequence::VolumeDownSide, "iPhone 7 and 7 Plus"),
];

fn tabs(selected: Sequence, cx: &mut Context<Installer>) -> Div {
    let mut row = div().w(px(440.)).flex().flex_row().gap(px(28.)).border_b_1().border_color(c(LINE));
    for (sequence, label) in TABS {
        let on = sequence == selected;
        let tab = div()
            .id(label)
            .pb(px(10.))
            .font_family(MONO)
            .text_size(px(12.))
            .cursor_pointer()
            .text_color(c(if on { TEXT } else { MUTED }))
            .child(label);
        let mut tab = pressable(tab, |s| s.text_color(c(TITLE)), cx, move |this, _, _| this.send(Event::SelectTab(sequence)));
        if on {
            tab = tab.font_weight(FontWeight::SEMIBOLD).border_b_2().border_color(c(BLUE));
        } else {
            tab = tab.hover(|s| s.text_color(c(SUBTITLE)));
        }
        row = row.child(tab);
    }
    row
}

pub fn trouble_link(cx: &mut Context<Installer>) -> impl IntoElement {
    // In a row, so the link is as wide as its text, not the column; 24 px
    // below what it helps with instead of the column's 44.
    div().flex().flex_row().mt(px(-20.)).child(trouble(cx))
}

fn trouble(cx: &mut Context<Installer>) -> gpui::Stateful<Div> {
    pressable(link("trouble"), link_ring, cx, |this, _, cx| {
        this.trouble = true;
        cx.notify();
    })
    .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .py(px(4.))
        .child(icon("life-buoy", 15., c(BLUE)))
        .child(div().font_family(MONO).text_size(px(12.)).text_color(c(BLUE)).child("Having trouble?"))
}

/// 1.1 / 1.11 (and 1.9 on Linux): waiting for DFU, with the steps of the
/// selected button layout.
/// The note under the Home button steps (1.1): its gap and line height.
const NOTE_GAP: f32 = 16.;
const NOTE_LINE: f32 = 19.;

fn waiting_for_dfu(tab: Sequence, rows: f32, note: f32, guide: (bool, Option<crate::guide::Moment>, f32), unresponsive: bool, launcher: Option<Option<String>>, cx: &mut Context<Installer>) -> Div {
    let (running, moment, dim) = guide;
    let darken = 1. - GUIDE_DARKEN * dim;
    let list = match tab {
        Sequence::HomePower => HOME_POWER,
        Sequence::VolumeSequence => VOLUME_SEQUENCE,
        Sequence::VolumeDownSide => VOLUME_DOWN_SIDE,
    };
    // L.1 (the launcher) words it as starting the device.
    let top = match &launcher {
        Some(_) => header("Start your device", "This puts it in a special startup mode (DFU) so Orchard can start it."),
        None => header("Let's connect your device", "This puts it in a special startup mode (DFU) so the installer can reach it."),
    };
    let problem = launcher.flatten();
    column()
        .child(top.opacity(darken))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(16.))
                .child(div().w_full().opacity(darken).child(tabs(tab, cx)))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(steps_guided(list, rows, moment, dim))
                        // The iPhone 8 has a Home button but the no-Home-button
                        // steps. Its height and opacity animate with the table's
                        // (`note`, 0…1), so the two morph together.
                        // The space grows and shrinks; the text sits at its
                        // final place on top of it and only fades (no clipping).
                        .child(
                            div().relative().w_full().h(px((NOTE_GAP + NOTE_LINE) * note)).child(
                                div()
                                    .absolute()
                                    .top(px(NOTE_GAP))
                                    .left(px(0.))
                                    .whitespace_nowrap()
                                    .opacity(note.powi(3) * darken)
                                    .text_size(px(13.))
                                    .line_height(px(NOTE_LINE))
                                    .text_color(c(MUTED))
                                    .child("iPhone 8 and later use the No Home button steps."),
                            ),
                        ),
                ),
        )
        // The launcher's last start failed: why (draft copy).
        .children(problem.map(|p| {
            div().w_full().text_size(px(13.)).line_height(px(19.)).text_color(c(RED)).child(orchard_flow::for_device(&format!("Orchard couldn't start your iPad: {p}")).to_owned())
        }))
        // A device running iPadOS is on USB but doesn't answer, so it can't
        // be shown as found (draft copy).
        .children(unresponsive.then(|| {
            div().w_full().text_size(px(13.)).line_height(px(19.)).text_color(c(AMBER)).child(
                orchard_flow::for_device("An iPad is connected but isn't responding. Unplug the cable at your iPad and plug it back in."),
            )
        }))
        // "Having trouble?" and, right-aligned, "Guide me" (or "Stop" while it
        // runs): the row is the text column's width (440 px beside the device).
        .child(
            div()
                .w(px(440.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .mt(px(-20.))
                .child(div().opacity(darken).child(trouble(cx)))
                .child(guide_button(tab, running, cx)),
        )
}

/// G.1: "Guide me" starts the run of `tab`'s steps; "Stop" ends it.
fn guide_button(tab: Sequence, running: bool, cx: &mut Context<Installer>) -> impl IntoElement {
    let color = if running { c(MUTED) } else { c(BLUE) };
    div()
        .id("guide")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .py(px(4.))
        .cursor_pointer()
        .child(icon(if running { "square" } else { "play" }, 13., color))
        .child(div().font_family(MONO).text_size(px(12.)).text_color(color).child(if running { "Stop" } else { "Guide me" }))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.guide = if this.guide.is_some() { None } else { Some((std::time::Instant::now(), tab)) };
            cx.notify();
        }))
}

/// The steps of a sequence once the device is already connected.
pub fn connected_steps(sequence: Sequence) -> &'static [Step] {
    let all = match sequence {
        Sequence::HomePower => HOME_POWER,
        Sequence::VolumeSequence => VOLUME_SEQUENCE,
        Sequence::VolumeDownSide => VOLUME_DOWN_SIDE,
    };
    &all[1..]
}

/// 1.2: a device is connected, but in another mode than DFU.
fn wrong_mode(seen: &Seen, rows: f32, cx: &mut Context<Installer>) -> Div {
    let kind = match seen.device.model {
        Model::IpadHome | Model::IpadNoHome => "iPad",
        Model::IphoneHome | Model::IphoneNoHome => "iPhone",
    };
    let mode = match seen.mode {
        UsbMode::Recovery => "recovery mode",
        UsbMode::Normal => "normal mode",
        UsbMode::Install => "install mode",
        _ => "another mode",
    };
    let badge = div()
        .px(px(7.))
        .py(px(2.))
        .border_1()
        .border_color(ca(0xe0af6866))
        .font_family(MONO)
        .text_size(px(11.))
        .text_color(c(AMBER))
        .child(mode);
    let header = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .w_full()
        .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(format!("Put your {kind} in DFU mode")))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .child(div().text_size(px(15.)).text_color(c(SUBTITLE)).child(seen.device.name.clone()))
                .child(badge),
        );
    column().child(header).child(steps(connected_steps(seen.device.sequence), rows)).child(trouble_link(cx))
}

/// A filled blue button: the way forward.
pub fn primary(id: &'static str, label: &'static str, padding_x: f32, padding_y: f32, event: Event, cx: &mut Context<Installer>) -> impl IntoElement {
    let button = div()
        .id(id)
        .px(px(padding_x - 1.))
        .py(px(padding_y - 1.))
        .border_1()
        .border_color(c(BLUE))
        .bg(c(BLUE))
        .hover(|s| s.bg(c(0x93b4f9)).border_color(c(0x93b4f9)))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(ON_BLUE))
        .cursor_pointer()
        .child(label);
    pressable(button, |s| s.border_color(c(TITLE)), cx, move |this, _, _| this.send(event.clone()))
}

/// A dialog's default choice: a primary button that has focus when the
/// dialog opens and comes first in its Tab order, so Enter takes it.
pub fn default_button(id: &'static str, label: &'static str, event: Event, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> impl IntoElement {
    let button = div()
        .id(id)
        .track_focus(focus)
        .px(px(15.))
        .py(px(7.))
        .border_1()
        .border_color(c(BLUE))
        .bg(c(BLUE))
        .hover(|s| s.bg(c(0x93b4f9)).border_color(c(0x93b4f9)))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(ON_BLUE))
        .cursor_pointer()
        .child(label);
    let button = if crate::keyboard() { button.focus(|s| s.border_color(c(TITLE))) } else { button };
    button.on_click(cx.listener(move |this, _, _, _| this.send(event.clone())))
}

/// An outlined button: another way.
pub fn secondary(id: &'static str, label: &'static str, padding_x: f32, padding_y: f32, event: Event, cx: &mut Context<Installer>) -> impl IntoElement {
    let button = div()
        .id(id)
        .px(px(padding_x))
        .py(px(padding_y))
        .border_1()
        .border_color(c(0x3b4261))
        .hover(|s| s.border_color(c(0x565f89)).bg(c(PANEL)))
        .text_size(px(14.))
        .text_color(c(TEXT))
        .cursor_pointer()
        .child(label);
    pressable(button, |s| s.border_color(c(BLUE)), cx, move |this, _, _| this.send(event.clone()))
}

/// A small mono label in a 1 px frame of its color.
pub fn badge(label: impl Into<SharedString>, color: u32) -> Div {
    div()
        .px(px(7.))
        .py(px(2.))
        .border_1()
        .border_color(ca((color << 8) | 0x66))
        .font_family(MONO)
        .text_size(px(11.))
        .text_color(c(color))
        .child(label.into())
}

pub fn title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(text.into())
}

fn model_line(device: &Device, label: &'static str, color: u32) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .child(div().text_size(px(15.)).text_color(c(SUBTITLE)).child(device.name.clone()))
        .child(badge(label, color))
}

pub fn paragraph(text: impl Into<SharedString>) -> Div {
    div().w_full().text_size(px(14.)).line_height(px(21.)).text_color(c(SUBTITLE)).child(text.into())
}

fn kind(device: &Device) -> (&'static str, &'static str) {
    match device.model {
        Model::IpadHome | Model::IpadNoHome => ("iPad", "iPadOS"),
        Model::IphoneHome | Model::IphoneNoHome => ("iPhone", "iOS"),
    }
}

/// 1.3 / 1.6: found in DFU; next, make room (or skip that).
fn found(device: &Device, cx: &mut Context<Installer>) -> Div {
    let (name, os) = kind(device);
    let (label, color) = match device.support {
        Support::Experimental => ("experimental", AMBER),
        _ => ("supported", BLUE),
    };
    let header = div().flex().flex_col().gap(px(12.)).w_full().child(title(format!("Found your {name}"))).child(model_line(device, label, color));
    let next = div()
        .w(px(342.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Next, make room"))
        .child(paragraph(format!("The installer starts your {name} to split the storage between {os} and {}.", orchard_flow::system_name())))
        .child(paragraph("If you have done this before, you can skip this step."));
    let actions = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .child(primary("continue", "Continue", 18., 9., Event::Continue, cx))
        .child(secondary("skip", "Skip this step", 16., 9., Event::Skip, cx));
    column().child(header).child(next).child(actions)
}

/// 1.7: in DFU, but not supported.
fn unsupported(device: &Device, cx: &mut Context<Installer>) -> Div {
    let (name, _) = kind(device);
    let leave = match device.sequence {
        Sequence::HomePower => "To leave this mode, hold Home and Power until the Apple logo appears.",
        Sequence::VolumeDownSide => "To leave this mode, hold Volume Down and Power until the Apple logo appears.",
        Sequence::VolumeSequence => "To leave this mode, press Volume Up, then Volume Down, then hold Power until the Apple logo appears.",
    };
    let header = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .w_full()
        .child(title(format!("This {name} isn't supported yet")))
        .child(model_line(device, "not supported", RED));
    let supported = link("supported-devices")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .py(px(2.))
        .child(div().text_size(px(14.)).text_color(c(BLUE)).child("See supported devices on GitHub"))
        .child(icon("arrow-up-right", 14., c(BLUE)));
    let link = div().pt(px(4.)).flex().flex_row().child(pressable(supported, link_ring, cx, |_, _, cx| cx.open_url(SUPPORTED_DEVICES)));
    column().child(header).child(div().w(px(342.)).flex().flex_col().gap(px(10.)).child(paragraph(leave)).child(link))
}

/// Where the supported devices are listed.
const SUPPORTED_DEVICES: &str = "https://github.com/OrchardLinux/orchard/blob/main/docs/compatibility.md";

/// 1.8: several devices in DFU.
fn several(devices: &[Device]) -> Div {
    let mut list = div().w_full().flex().flex_col().bg(c(PANEL)).border_1().border_color(c(PANEL_LINE));
    for (i, device) in devices.iter().enumerate() {
        let mut row = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(11.))
            .child(div().text_size(px(14.)).text_color(c(TEXT)).child(device.name.clone()))
            .child(badge("dfu", BLUE));
        if i > 0 {
            row = row.border_t_1().border_color(c(PANEL_LINE));
        }
        list = list.child(row);
    }
    column().child(title("More than one device in DFU mode")).child(
        div()
            .w(px(342.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(list)
            .child(div().text_size(px(14.)).text_color(c(SUBTITLE)).child("Disconnect all but the one you want to install on.")),
    )
}

/// What covers the whole window, above the screen: 1.4's dialog.
pub fn overlay(installer: &Installer, cx: &mut Context<Installer>) -> Option<AnyElement> {
    // Open, or still fading out after closing: scrim fades, dialog rises 8 px.
    let shown = installer.dialog_fade.value();
    let kind = installer.dialog_kind?;
    if shown <= 0.001 {
        return None;
    }
    let open = dialog_open(installer);
    let root = installer.dialog_root.clone();
    let focus = installer.dialog_focus.clone();
    let dialog = match kind {
        crate::DialogKind::Trouble => crate::install_screens::trouble_dialog(&focus, cx),
        crate::DialogKind::SshInfo => crate::install_screens::ssh_dialog(installer, &focus, cx),
        crate::DialogKind::Details => crate::details::dialog(installer, &focus, cx),
        crate::DialogKind::Skip => skip_dialog(&focus, cx),
        crate::DialogKind::Replug => crate::install_screens::replug_dialog(),
        crate::DialogKind::SplitErase => {
            let (layout, approved) = installer.split_erase_shown.clone()?;
            let (connected, stuck) = (installer.flow.companion, installer.link_stuck());
            crate::install_screens::split_erase_dialog(&layout, approved, connected, stuck, &focus, cx)
        }
        crate::DialogKind::Erase => {
            let (layout, approved) = installer.erase_shown.clone()?;
            crate::install_screens::erase_dialog(&layout, approved, &focus, cx)
        }
        crate::DialogKind::Quit => quit_dialog(&installer.flow.state, &focus, cx),
        crate::DialogKind::QuitBlocked => quit_blocked_dialog(&installer.flow.state, &focus, cx),
        crate::DialogKind::Download => download_dialog(installer, &focus, cx),
    };
    let dialog = with_default_on_enter(dialog.track_focus(&root), cx).relative().top(px((1. - shown) * 8.));
    let mut scrim = scrim().opacity(shown).child(dialog);
    if !open {
        // Fading out: nothing in it can be pressed any more.
        scrim = scrim.child(div().absolute().left(px(0.)).top(px(0.)).size_full().occlude());
    }
    Some(scrim.into_any_element())
}

/// A dialog's frame, `width` wide.
pub fn dialog(width: f32) -> Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(20.))
        .p(px(28.))
        .bg(c(WINDOW))
        .border_1()
        .border_color(c(LINE))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0x00000080),
            offset: gpui::point(px(0.), px(20.)),
            blur_radius: px(50.),
            spread_radius: px(0.),
        }])
}

/// The dark scrim a dialog sits on, over the whole window; it takes the
/// clicks meant for the screen behind.
pub fn scrim() -> Div {
    div().absolute().left(px(0.)).top(px(0.)).size_full().bg(ca(0x0d0e14b3)).occlude().flex().flex_col().items_center().justify_center()
}

/// Enter with nothing in the dialog focused takes its default choice; a
/// focused button handles Enter itself.
fn with_default_on_enter(dialog: Div, cx: &mut Context<Installer>) -> Div {
    dialog.on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
        if e.keystroke.key != "enter" || !this.dialog_root.is_focused(window) {
            return;
        }
        if this.quitting.is_some() {
            this.quitting = None;
            cx.notify();
        } else if this.trouble {
            this.trouble = false;
            cx.notify();
        } else if matches!(this.flow.state, State::Found { confirm_skip: true, .. }) {
            this.send(Event::CancelSkip);
        } else if matches!(this.flow.state, State::ConfirmErase { .. }) || this.flow.confirming_split {
            this.send(Event::Cancel);
        }
        cx.stop_propagation();
    }))
}

/// Whether a dialog covers the screen.
pub fn dialog_open(installer: &Installer) -> bool {
    open_dialog(installer).is_some()
}

/// Which dialog is open, if any.
pub fn open_dialog(installer: &Installer) -> Option<crate::DialogKind> {
    if let Some(ask) = installer.quitting {
        Some(match ask {
            crate::QuitAsk::Confirm => crate::DialogKind::Quit,
            crate::QuitAsk::Blocked => crate::DialogKind::QuitBlocked,
        })
    } else if failed_download(installer).is_some() {
        Some(crate::DialogKind::Download)
    } else if installer.trouble {
        Some(crate::DialogKind::Trouble)
    } else if installer.ssh_info {
        Some(crate::DialogKind::SshInfo)
    } else if installer.details_info {
        Some(crate::DialogKind::Details)
    } else if matches!(installer.flow.state, State::Found { confirm_skip: true, .. }) {
        Some(crate::DialogKind::Skip)
    } else if matches!(installer.flow.state, State::ConfirmErase { .. }) {
        Some(crate::DialogKind::Erase)
    } else if installer.flow.confirming_split {
        Some(crate::DialogKind::SplitErase)
    } else if installer.flow.boot_stalled {
        Some(crate::DialogKind::Replug)
    } else {
        None
    }
}

/// 1.4: are you sure you want to skip making room?
fn skip_dialog(focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    
    dialog(440.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Skip this step?"))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(paragraph(format!("{} can only be installed into storage that has been split off for it. That happens once, during the first install.", orchard_flow::system_name())))
                .child(paragraph(orchard_flow::for_device("Only skip it if this iPad has been set up before."))),
        )
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(10.))
                .pt(px(4.))
                .child(secondary("confirm-skip", "Skip", 16., 8., Event::ConfirmSkip, cx))
                .child(default_button("cancel-skip", "Don't skip", Event::CancelSkip, focus, cx)),
        )
}

pub fn text_column(installer: &mut Installer, kind: ScreenKind, cx: &mut Context<Installer>) -> AnyElement {
    if installer.app == orchard_flow::App::Launcher {
        if let Some(screen) = launcher_column(installer, cx) {
            return screen;
        }
    }
    if let Some(screen) = crate::install_screens::text_column(installer, cx) {
        return screen;
    }
    match (&installer.flow.state, kind) {
        (State::WaitingForDfu { seen, leaving, .. }, ScreenKind::WrongMode) if !seen.is_empty() || leaving.is_some() => {
            // While it drops off for DFU, the screen stays as it was.
            let seen = seen.last().or(leaving.as_ref()).unwrap().clone();
            installer.rows.set(connected_steps(seen.device.sequence).len() as f32);
            wrong_mode(&seen, installer.rows.value(), cx).into_any_element()
        }
        (State::WaitingForDfu { tab, .. }, _) => {
            let tab = *tab;
            let count = match tab {
                Sequence::HomePower => HOME_POWER.len(),
                Sequence::VolumeSequence => VOLUME_SEQUENCE.len(),
                Sequence::VolumeDownSide => VOLUME_DOWN_SIDE.len(),
            };
            installer.rows.set(count as f32);
            installer.note.set(if tab == Sequence::HomePower { 1. } else { 0. });
            // "Guide me": over, or for another tab, ends the run.
            if installer.guide.is_some_and(|(_, s)| s != tab) || (installer.guide.is_some() && installer.guide_moment().is_none()) {
                installer.guide = None;
            }
            installer.guide_fade.set(if installer.guide.is_some() { 1. } else { 0. });
            let fade = installer.guide_fade.value();
            // While the darkening fades out after the run, its last row stays
            // bright (without its countdown); once faded, nothing is held.
            let moment = match installer.guide_moment() {
                Some(m) => {
                    installer.guide_last = Some(m);
                    Some(m)
                }
                None if fade > 0.001 => installer.guide_last.map(|m| crate::guide::Moment { progress: None, remaining: None, ..m }),
                None => {
                    installer.guide_last = None;
                    None
                }
            };
            let guide = (installer.guide.is_some(), moment, fade);
            let unresponsive = installer.flow.unresponsive;
            let launcher = (installer.app == orchard_flow::App::Launcher).then(|| installer.flow.launch_problem.clone());
            waiting_for_dfu(tab, installer.rows.value(), installer.note.value(), guide, unresponsive, launcher, cx).into_any_element()
        }
        (State::Found { device, .. }, _) => found(device, cx).into_any_element(),
        (State::Unsupported { device }, _) => unsupported(device, cx).into_any_element(),
        (State::SeveralInDfu { devices }, _) => several(devices).into_any_element(),
        // Not drawn yet: the screen's name, so the flow can be followed.
        (_, kind) => column().child(div().text_size(px(24.)).text_color(c(TITLE)).child(format!("{kind:?}"))).into_any_element(),
    }
}

/// A small tooltip (the eye buttons' shortcut).
pub struct Tip(pub SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .bg(c(PANEL))
            .border_1()
            .border_color(c(PANEL_LINE))
            .text_size(px(12.))
            .text_color(c(TEXT))
            .child(self.0.clone())
    }
}

/// The launcher's own screens (L.2–L.5); L.1 is the DFU screen above.
fn launcher_column(installer: &mut Installer, cx: &mut Context<Installer>) -> Option<AnyElement> {
    let state = installer.flow.state.clone();
    Some(match &state {
        // palera1n, Pongo and m1n1 start it and read what's installed (draft
        // copy, as the installer's 2.1a): the replug dialog can come up here.
        State::Found { device, .. } => column()
            .child(title(format!("Starting your {}", kind(device).0)))
            .children(crate::install_screens::attempt_line(installer.flow.start_attempt))
            .child(crate::details::details(installer, cx))
            .into_any_element(),
        State::ChooseSystem { device, entries, selected } => choose_system(device, entries, *selected, cx).into_any_element(),
        // Until m1n1 took the entry, the window stays connected (quitting
        // then cuts off the start): no Done yet.
        State::Starting { title: name, .. } if !installer.flow.entry_started => column()
            .child(title(format!("Starting {name}")))
            .into_any_element(),
        State::Starting { title: name, .. } => column()
            .child(header_owned(format!("{name} is starting"), format!("It's starting up on your {}. Carry on there.", installer.flow.state.device().map(|d| kind(d).0).unwrap_or("device"))))
            .child(div().flex().flex_row().child(primary("done", "Done", 18., 9., Event::Continue, cx)))
            .into_any_element(),
        // Neutral: the launcher doesn't know or advertise any system.
        State::NothingInstalled { device } => column()
            .child(div().flex().flex_col().gap(px(12.)).w_full().child(title(format!("Nothing to start on this {}", kind(device).0))).child(model_line(device, "supported", BLUE)))
            .child(div().w(px(342.)).child(paragraph("No installed system was found on it. Install one first.")))
            .into_any_element(),
        _ => return None,
    })
}

fn header_owned(title_text: String, subtitle: String) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .w_full()
        .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(title_text))
        .child(div().text_size(px(14.)).line_height(px(20.)).text_color(c(SUBTITLE)).child(subtitle))
}

/// L.2 / L.3: the installed systems, one to pick (the default first selected);
/// the list scrolls past five.
fn choose_system(device: &Device, entries: &[orchard_flow::Entry], selected: usize, cx: &mut Context<Installer>) -> Div {
    let mut list = div().id("entries").w_full().max_h(px(5. * 49.)).overflow_y_scroll().flex().flex_col().bg(c(PANEL)).border_1().border_color(c(PANEL_LINE));
    for (i, entry) in entries.iter().enumerate() {
        let chosen = i == selected;
        let radio = div()
            .w(px(14.))
            .h(px(14.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(c(if chosen { BLUE } else { 0x565f89 }))
            .when(chosen, |r| r.child(div().w(px(6.)).h(px(6.)).bg(c(BLUE))));
        let mut row = div()
            .id(("entry", i))
            .w_full()
            .h(px(48.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .cursor_pointer()
            .when(chosen, |r| r.bg(c(0x24283b)))
            .hover(|s| s.bg(c(0x24283b)))
            .child(radio)
            .child(div().text_size(px(14.)).text_color(c(TEXT)).child(entry.title.clone()))
            .child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(entry.kernel.clone()))
            .child(div().flex_1());
        if entry.default {
            row = row.child(badge("default", MUTED));
        }
        if i > 0 {
            row = row.border_t_1().border_color(c(PANEL_LINE));
        }
        let row = pressable(row, |s| s.border_color(c(BLUE)), cx, move |this, _, _| this.send(Event::SelectEntry(i)));
        list = list.child(row);
    }
    column()
        .child(div().flex().flex_col().gap(px(12.)).w_full().child(title(format!("Found your {}", kind(device).0))).child(model_line(device, "supported", BLUE)))
        .child(div().w_full().flex().flex_col().gap(px(10.)).child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Choose a system")).child(list))
        .child(div().flex().flex_row().child(primary("start", "Start", 18., 9., Event::Continue, cx)))
}

/// A dialog button that runs `act` (not a flow event).
pub(crate) fn dialog_button(id: &'static str, label: &'static str, primary: bool, focus: Option<&gpui::FocusHandle>, cx: &mut Context<Installer>, act: impl Fn(&mut Installer, &mut Context<Installer>) + 'static) -> impl IntoElement {
    let mut button = div().id(id).px(px(if primary { 15. } else { 16. })).py(px(if primary { 7. } else { 8. })).border_1().text_size(px(14.)).cursor_pointer().child(label);
    if let Some(focus) = focus {
        button = button.track_focus(focus);
    }
    button = if primary {
        button
            .border_color(c(BLUE))
            .bg(c(BLUE))
            .hover(|s| s.bg(c(0x93b4f9)).border_color(c(0x93b4f9)))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(ON_BLUE))
            .when(crate::keyboard(), |b| b.focus(|s| s.border_color(c(TITLE))))
    } else {
        button.border_color(c(0x3b4261)).hover(|s| s.border_color(c(0x565f89)).bg(c(PANEL))).text_color(c(TEXT))
    };
    button.on_click(cx.listener(move |this, _, _, cx| act(this, cx)))
}

/// How to force-restart this device, as a sentence ending in "appears".
fn restart_steps(device: &orchard_flow::Device) -> &'static str {
    use orchard_flow::Sequence;
    match device.sequence {
        Sequence::HomePower => "hold the Home and power buttons until the Apple logo appears",
        Sequence::VolumeDownSide => "hold the volume down and power buttons until the Apple logo appears",
        Sequence::VolumeSequence => "press volume up, then volume down, then hold the power button until the Apple logo appears",
    }
}

/// What the quit dialog says, by where the iPad is (draft copy).
pub fn quit_copy(state: &State) -> (&'static str, String) {
    let kind = |d: &orchard_flow::Device| if matches!(d.model, orchard_flow::Model::IpadHome | orchard_flow::Model::IpadNoHome) { "iPad" } else { "iPhone" };
    match state {
        // The temporary iPadOS: quitting restarts the iPad into normal iPadOS.
        State::WaitingForIpados { device, .. } | State::ChooseSplit { device, .. } => {
            ("Quit the installer?", orchard_flow::for_device(&format!("Your {} restarts into iPadOS. Nothing on it has been changed.", kind(device))).to_owned())
        }
        State::SplitDone { device, .. } => (
            "Quit the installer?",
            orchard_flow::for_device(&format!("Your {} restarts into iPadOS. Its storage stays split, so you can install {} later.", kind(device), orchard_flow::system_name())).to_owned(),
        ),
        // In DFU mode or the install system.
        State::DfuAgain { device, .. } | State::CreateAccount { device, .. } | State::ConfirmErase { device, .. } | State::NotPrepared { device } | State::InstallFailed { device } => (
            "Quit the installer?",
            format!("Your {k} is in a special startup mode for the installer. To use it normally again, restart it: {s}.", k = kind(device), s = restart_steps(device)),
        ),
        _ => ("Quit the installer?", "You can start it again any time.".to_owned()),
    }
}

/// ⌘Q / close: really quit? Says how to get the device back to normal.
fn quit_dialog(state: &State, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    let (title, body) = quit_copy(state);
    dialog(440.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(title))
        .child(paragraph(body))
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(10.))
                .pt(px(4.))
                // The flow quits (restarting the iPad first where it should).
                .child(dialog_button("quit-confirm", "Quit", false, None, cx, |this, _| this.send(Event::Quit)))
                .child(dialog_button("quit-cancel", "Cancel", true, Some(focus), cx, |this, cx| {
                    this.quitting = None;
                    cx.notify();
                })),
        )
}

/// The first download that failed for good, with the reason.
fn failed_download(installer: &Installer) -> Option<(orchard_flow::assets::Asset, String)> {
    orchard_flow::assets::ALL.iter().find_map(|&a| match installer.flow.assets.get(a) {
        orchard_flow::assets::AssetState::Failed(why) => Some((a, why.clone())),
        _ => None,
    })
}

/// A download failed for good (draft copy): what, why, and try again.
fn download_dialog(installer: &Installer, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    let (asset, why) = failed_download(installer).expect("a failed download");
    let what = match asset {
        orchard_flow::assets::Asset::RootImage => format!("{} itself", orchard_flow::system_name()),
        orchard_flow::assets::Asset::Palera1n => orchard_flow::for_device("palera1n, which starts your iPad").to_owned(),
        _ => format!("part of {}", orchard_flow::system_name()),
    };
    dialog(480.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("A download didn't work"))
        .child(paragraph(format!("The installer couldn't download {what}. Check the internet connection, then try again.")))
        .child(div().w_full().font_family(MONO).text_size(px(12.)).line_height(px(18.)).text_color(c(SUBTITLE)).child(why))
        .child(div().w_full().flex().flex_row().justify_end().pt(px(4.)).child(dialog_button(
            "download-retry",
            "Try again",
            true,
            Some(focus),
            cx,
            |this, _| this.send(Event::RetryAssets),
        )))
}

/// ⌘Q / close while the iPad splits or installs: not now (draft copy).
fn quit_blocked_dialog(state: &State, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    let (title, body) = match state {
        State::Splitting { .. } => ("Wait until the split is done".to_owned(), orchard_flow::for_device("Your iPad is splitting its storage. Quit once it's done.").to_owned()),
        State::Starting { title: name, .. } => (format!("Wait until {name} has started"), orchard_flow::for_device("Your iPad is still loading it. Quit once it's starting.").to_owned()),
        _ => {
            let name = orchard_flow::system_name();
            (format!("Wait until {name} is installed"), orchard_flow::for_device(&format!("{name} is being installed on your iPad. Quit once it's done.")).to_owned())
        }
    };
    dialog(440.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(title))
        .child(paragraph(body))
        .child(div().w_full().flex().flex_row().justify_end().pt(px(4.)).child(dialog_button("quit-ok", "OK", true, Some(focus), cx, |this, cx| {
            this.quitting = None;
            cx.notify();
        })))
}
