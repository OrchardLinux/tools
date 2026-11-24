//! The start's Details (2.1p closed, 2.1q open): a quiet toggle; open, the
//! attempt, the stage and palera1n's log, five lines high, the log scrolling
//! and following its end, with Copy (diagnostics for a bug report) beside
//! the toggle. The panel's height animates open and closed.
use crate::theme::*;
use crate::Installer;
use gpui::{div, prelude::*, px, ClipboardItem, Context, Div, SharedString};
use std::time::{Duration, Instant};

const LINE: f32 = 18.;
const LOG_LINES: f32 = 3.;
/// Padding 12 + attempt + 6 + stage + 6 + three log lines + padding 12.
const PANEL: f32 = 12. + LINE + 6. + LINE + 6. + LOG_LINES * LINE + 12.;

pub fn details(installer: &mut Installer, cx: &mut Context<Installer>) -> Div {
    installer.details.set(if installer.details_open { 1. } else { 0. });
    let t = installer.details.value();
    let log = installer.flow.start_log.clone();
    if log.len() != installer.details_seen {
        installer.details_seen = log.len();
        installer.details_scroll.scroll_to_bottom();
    }
    let open = installer.details_open;
    let toggle = div()
        .id("details-toggle")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .py(px(4.))
        .cursor_pointer()
        .text_color(c(MUTED))
        .hover(|s| s.text_color(c(SUBTITLE)))
        .child(crate::screens::icon(if open { "chevron-down" } else { "chevron-right" }, 14., c(MUTED)))
        .child(div().font_family(MONO).text_size(px(12.)).child("Details"))
        .on_click(cx.listener(|this, _, _, cx| {
            this.details_open = !this.details_open;
            cx.notify();
        }));
    let copied = installer.copied_at.is_some_and(|at| at.elapsed() < Duration::from_millis(1500));
    let copy = div()
        .id("details-copy")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .py(px(4.))
        .cursor_pointer()
        .opacity(t)
        .text_color(c(MUTED))
        .hover(|s| s.text_color(c(SUBTITLE)))
        .child(crate::screens::icon(if copied { "check" } else { "copy" }, 14., c(MUTED)))
        .child(div().font_family(MONO).text_size(px(12.)).child(if copied { "Copied" } else { "Copy" }))
        .on_click(cx.listener(|this, _, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(diagnostics(this)));
            this.copied_at = Some(Instant::now());
            cx.notify();
        }));
    let bar = div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(toggle)
        .when(t > 0.01, |d| d.child(copy));
    let row = |key: &'static str, value: String| {
        div()
            .h(px(LINE))
            .flex()
            .flex_row()
            .gap(px(12.))
            .child(div().w(px(64.)).flex_none().text_color(c(MUTED)).child(key))
            .child(div().flex_1().min_w(px(0.)).truncate().text_color(c(SUBTITLE)).child(value))
    };
    let stage = orchard_flow::start_stage(&log).map(|s| orchard_flow::for_device(s).to_owned()).unwrap_or_else(|| "Waiting".to_owned());
    let lines = div()
        .id("details-log")
        .h(px(LOG_LINES * LINE))
        .overflow_y_scroll()
        .track_scroll(&installer.details_scroll)
        .flex()
        .flex_col()
        .children(log.iter().map(|l| div().h(px(LINE)).flex_none().truncate().text_size(px(11.)).text_color(c(MUTED)).child(SharedString::from(l.clone()))));
    let panel = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(14.))
        .py(px(12.))
        .bg(c(0x16161e))
        .border_1()
        .border_color(c(PANEL_LINE))
        .font_family(MONO)
        .text_size(px(12.))
        .child(row("Attempt", installer.flow.start_attempt.max(1).to_string()))
        .child(row("Stage", stage))
        .child(lines);
    div()
        .w_full()
        .flex()
        .flex_col()
        .child(bar)
        // Opening and closing: the height and the panel's opacity follow t.
        .child(div().w_full().h(px((PANEL + 10.) * t)).overflow_hidden().opacity(t).child(div().pt(px(10.)).child(panel)))
}

/// What Copy puts on the clipboard: enough for a bug report in one paste.
fn diagnostics(installer: &Installer) -> String {
    let flow = &installer.flow;
    let device = flow.state.device().map(|d| format!("{} ({}, {:?})", d.name, d.board, d.model)).unwrap_or_else(|| "none".into());
    let flags: Vec<String> = std::env::vars().filter(|(k, _)| k.starts_with("ORCHARD_")).map(|(k, v)| format!("{k}={v}")).collect();
    let mut out = format!(
        "Orchard Installer {} · {} · {} {}\nDevice: {device}\nFlags: {}\nAttempt: {}\nStage: {}\n",
        env!("CARGO_PKG_VERSION"),
        orchard_flow::system_name(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if flags.is_empty() { "none".into() } else { flags.join(" ") },
        flow.start_attempt.max(1),
        orchard_flow::start_stage(&flow.start_log).unwrap_or("waiting"),
    );
    if let Some(problem) = &flow.start_problem {
        out.push_str(&format!("Last start: {problem}\n"));
    }
    out.push_str("\npalera1n:\n");
    for line in &flow.start_log {
        out.push_str(line);
        out.push('\n');
    }
    out
}
