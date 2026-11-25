//! The guided run from recovery mode into DFU mode (R.1i–R.5i in
//! installer-app.pen): five steps, Start in the first, the lit step's
//! countdown, Stop until the restart, and the buttons glowing on the device.
//! The flow decides the phases (orchard_flow::recovery); this only shows them.
use crate::screens::{countdown, icon, primary, trouble_link};
use crate::theme::*;
use crate::Installer;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, FontWeight};
use orchard_flow::recovery::{self, Phase, Run};
use orchard_flow::Event;
use std::time::Instant;

/// How much the rest darkens while the run is under way (as "Guide me").
const DIM: f32 = 0.28;

/// Seconds into the run's current phase (the window's own clock: the flow
/// keeps none), started anew whenever the phase changes.
pub fn seconds_in(installer: &mut Installer, phase: &Phase) -> f32 {
    let key = std::mem::discriminant(phase);
    match installer.recovery_since {
        Some((k, at)) if k == key => at.elapsed().as_secs_f32(),
        _ => {
            installer.recovery_since = Some((key, Instant::now()));
            0.
        }
    }
}

/// The header dims with the rest while the run is under way.
pub fn dim_while_running(header: Div, run: &Run) -> Div {
    header.opacity(if run.phase.running() { DIM } else { 1. })
}

/// The note above the steps, if any: the last attempt came back in recovery
/// mode (R.5i), or why the installer couldn't reach the device.
pub fn note(run: &Run) -> Option<Div> {
    let Phase::Ready { again, problem } = &run.phase else { return None };
    let text = match problem {
        Some(why) => {
            let mut text = orchard_flow::for_device(why).to_owned();
            if let Some(first) = text.get(..1) {
                text = first.to_uppercase() + &text[1..];
            }
            text
        }
        None if *again => orchard_flow::for_device("That didn't work. Your iPad is in recovery mode again. Start holding as soon as step 3 lights up.").to_owned(),
        None => return None,
    };
    Some(div().w_full().text_size(px(13.)).line_height(px(19.)).text_color(c(if problem.is_some() { MUTED } else { AMBER })).child(text))
}

/// The steps; Start in the first, the countdown on the lit one.
pub fn steps(run: &Run, seconds: f32, cx: &mut Context<Installer>) -> Div {
    let (hold, power) = recovery::buttons(run.device.sequence);
    let running = run.phase.running();
    let lit = run.phase.row();
    let mut table = div().w_full().flex().flex_col().border_1();
    let (mut bg, mut line) = (c(PANEL), c(PANEL_LINE));
    if running {
        bg.a *= 1. - 0.72;
        line.a *= 1. - 0.72;
    }
    table = table.bg(bg).border_color(line);
    let text = |t: &'static str| div().whitespace_nowrap().child(orchard_flow::for_device(t));
    let button = |b: &'static str| div().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).text_color(c(button_color(b))).child(b);
    let rows: [(Vec<Div>, Option<&str>); 5] = [
        (vec![text("Click Start")], None),
        (vec![text("Get ready")], Some("3 s")),
        (vec![text("Hold "), button(hold), text(" and "), button(power)], Some("4 s")),
        (vec![text("Let go of "), button(power), text(", keep holding "), button(hold)], Some("~10 s")),
        (vec![text("Let go when this window changes")], None),
    ];
    for (i, (parts, duration)) in rows.into_iter().enumerate() {
        let is_lit = lit == Some(i);
        let mut row = div()
            .w_full()
            .h(px(if i == 0 { 48. } else { 49. }))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .flex_none()
            .pl(px(22.))
            .pr(px(18.))
            .child(div().w(px(16.)).font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(format!("{}", i + 1)))
            .child(div().flex().flex_row().items_center().text_size(px(14.)).text_color(c(TEXT)).children(parts))
            .child(div().flex_1());
        if i > 0 {
            row = row.border_t_1().border_color(line);
        }
        if running {
            row = if is_lit { row.bg(c(PANEL)) } else { row.opacity(DIM) };
        }
        // The countdown in place of the duration; Start in the first step.
        let count = match (&run.phase, i) {
            (Phase::GetReady, 1) => Some((recovery::GET_READY.as_secs_f32() - seconds, seconds / recovery::GET_READY.as_secs_f32(), BLUE)),
            (Phase::Hold, 2) => {
                let total = (recovery::HOLD + recovery::RESTARTED).as_secs_f32();
                Some((total - seconds, seconds / total, button_color(power)))
            }
            (Phase::Restarted, 2) => {
                let total = (recovery::HOLD + recovery::RESTARTED).as_secs_f32();
                let done = recovery::HOLD.as_secs_f32() + seconds;
                Some((total - done, done / total, button_color(power)))
            }
            _ => None,
        };
        row = match (count, i, duration) {
            (Some((left, progress, color)), _, _) => row
                .child(div().font_family(MONO).text_size(px(12.)).text_color(c(SUBTITLE)).child(format!("{}", left.max(0.).ceil() as u32)))
                .child(countdown(progress.clamp(0., 1.), c(color))),
            (None, 0, _) => row.child(primary("recovery-start", "Start", 12., 2., Event::Continue, cx)),
            (None, _, Some(d)) => row.child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(d)),
            _ => row,
        };
        table = table.child(row);
    }
    table
}

/// Under the steps: Stop until the restart went out, and the help link.
pub fn actions(run: &Run, cx: &mut Context<Installer>) -> Div {
    let stop = matches!(run.phase, Phase::GetReady | Phase::Hold).then(|| {
        div()
            .id("recovery-stop")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .py(px(4.))
            .cursor_pointer()
            .child(icon("square", 13., c(MUTED)))
            .child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child("Stop"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.send(Event::Cancel);
                cx.notify();
            }))
    });
    div()
        .w(px(440.))
        .mt(px(-20.))
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(20.))
        .children(stop)
        .child(div().opacity(if run.phase.running() { DIM } else { 1. }).child(trouble_link(cx)))
}

/// The note, the steps and the row under them, for a screen's column.
pub fn body(installer: &mut Installer, run: &Run, cx: &mut Context<Installer>) -> Vec<AnyElement> {
    let seconds = seconds_in(installer, &run.phase);
    let mut out = Vec::new();
    if let Some(note) = note(run) {
        out.push(note.into_any_element());
    }
    out.push(steps(run, seconds, cx).into_any_element());
    out.push(actions(run, cx).into_any_element());
    out
}

/// The buttons on the device: dimmed while waiting to start, both lit while
/// held, the one still held lit while letting go.
pub fn glow(run: &Run) -> Option<crate::guide::Look> {
    let (home, power) = match run.phase {
        Phase::Ready { .. } => return None,
        Phase::GetReady => (0., 0.),
        Phase::Hold | Phase::Restarted => (1., 1.),
        Phase::LetGo => (1., 0.),
    };
    // Slots: Home, Power, Volume Up, Volume Down (guide::levels).
    let mut levels = [0.; 4];
    match run.device.sequence {
        orchard_flow::Sequence::HomePower => levels[0] = home,
        _ => levels[3] = home,
    }
    levels[1] = power;
    Some(crate::guide::Look { levels, fade: 1. })
}
