//! The screens after the device is found: USB access (1.10), the trouble
//! dialog (1.5), the split (2.x) and the install (3.x), as the .pen file
//! designs them. Copy is the approved copy, verbatim; the few lines the
//! designs don't cover are marked.
use crate::screens::{column, connected_steps, icon, paragraph, pressable, primary, secondary, steps, steps_natural, title, trouble_link, Part, Step};
use crate::theme::*;
use crate::Installer;
use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, Context, Div, FontWeight, MouseButton, SharedString};
use orchard_flow::{Account, Event, Layout, Link, State, MIN_OMARCHY_GB};
use std::time::Instant;

/// The text column's width on the full-width split screens (960 − 2 × 72).
const SPLIT_WIDTH: f32 = 816.;

fn header(title_text: impl Into<SharedString>, subtitle: impl Into<SharedString>, title_color: u32) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .w_full()
        .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(title_color)).child(title_text.into()))
        .child(paragraph(subtitle))
}

/// The system this installer installs (its stamp's name).
fn sys() -> &'static str {
    orchard_flow::system_name()
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(text.into())
}

fn actions() -> Div {
    div().flex().flex_row().items_center().gap(px(12.))
}

/// A primary button that does nothing until `enabled`.
fn gated(id: &'static str, label: &'static str, enabled: bool, event: Event, cx: &mut Context<Installer>) -> gpui::AnyElement {
    if enabled {
        primary(id, label, 18., 9., event, cx).into_any_element()
    } else {
        div()
            .px(px(18.))
            .py(px(9.))
            .bg(c(BLUE))
            .opacity(0.4)
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(ON_BLUE))
            .child(label)
            .into_any_element()
    }
}

/// "I have a backup and approve these changes."
fn approve(approved: bool, cx: &mut Context<Installer>) -> impl IntoElement {
    // The box is the keyboard stop (its ring is a lighter border); the label
    // toggles it too.
    let boxed = div().id("approve").w(px(16.)).h(px(16.)).flex().items_center().justify_center().cursor_pointer().border_1();
    let boxed = if approved {
        boxed.bg(c(BLUE)).border_color(c(BLUE)).child(icon("check", 12., c(ON_BLUE)))
    } else {
        boxed.border_color(c(0x565f89))
    };
    let ring = if approved { c(TITLE) } else { c(BLUE) };
    let boxed = pressable(boxed, move |s| s.border_color(ring), cx, move |this, _, _| this.send(Event::Approve(!approved)));
    let label = div()
        .id("approve-label")
        .cursor_pointer()
        .text_size(px(14.))
        .text_color(c(TEXT))
        .child("I have a backup and approve these changes.")
        .on_click(cx.listener(move |this, _, _, _| this.send(Event::Approve(!approved))));
    div().flex().flex_row().items_center().gap(px(10.)).child(boxed).child(label)
}

/// The address an installed system answers on over the USB cable.
const USB_ADDRESS: &str = "172.16.42.1";

/// "Allow SSH from this computer", with its (i) (3.2).
fn ssh_row(account: &Account, cx: &mut Context<Installer>) -> impl IntoElement {
    let on = account.ssh;
    let toggled = {
        let mut a = account.clone();
        a.ssh = !on;
        a
    };
    let boxed = div().id("ssh").w(px(16.)).h(px(16.)).flex().items_center().justify_center().cursor_pointer().border_1();
    let boxed = if on { boxed.bg(c(BLUE)).border_color(c(BLUE)).child(icon("check", 12., c(ON_BLUE))) } else { boxed.border_color(c(0x565f89)) };
    let ring = if on { c(TITLE) } else { c(BLUE) };
    let send = toggled.clone();
    let boxed = pressable(boxed, move |s| s.border_color(ring), cx, move |this, _, _| this.send(Event::EditAccount(send.clone())));
    let label = div()
        .id("ssh-label")
        .cursor_pointer()
        .text_size(px(14.))
        .text_color(c(TEXT))
        .child("Allow SSH from this computer")
        .on_click(cx.listener(move |this, _, _, _| this.send(Event::EditAccount(toggled.clone()))));
    let info = div().id("ssh-info").cursor_pointer().child(icon("info", 15., c(MUTED))).on_click(cx.listener(|this, _, _, cx| {
        this.ssh_info = true;
        cx.notify();
    }));
    div().flex().flex_row().items_center().gap(px(10.)).child(boxed).child(label).child(info)
}

/// 3.2i: what "Allow SSH from this computer" does.
pub fn ssh_dialog(installer: &Installer, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    crate::screens::dialog(480.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("SSH from this computer"))
        .child(crate::screens::paragraph(format!(
            "Adds this computer's SSH keys to {}, so you can sign in from here without a password, over the USB cable:",
            sys()
        )))
        .child(div().font_family(MONO).text_size(px(13.)).text_color(c(SUBTITLE)).child(format!("ssh {}@{USB_ADDRESS}", installer.username)))
        .child(div().w_full().flex().flex_row().justify_end().pt(px(4.)).child(crate::screens::dialog_button("ssh-ok", "OK", true, Some(focus), cx, |this, cx| {
            this.ssh_info = false;
            cx.notify();
        })))
}

/// The red list of what the install erases (2.3, 3.6).
fn erase_alert(layout: &Layout) -> Div {
    let total: u32 = layout.outside.iter().map(|p| p.gb).sum();
    let count = layout.outside.len();
    let mut rows = div().w_full().flex().flex_col().gap(px(3.)).pl(px(23.));
    for p in &layout.outside {
        rows = rows.child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .gap(px(12.))
                .font_family(MONO)
                .text_size(px(12.))
                .child(div().w(px(120.)).flex_none().whitespace_nowrap().text_color(c(TEXT)).child(p.name.clone()))
                .child(div().w(px(110.)).flex_none().whitespace_nowrap().text_color(c(SUBTITLE)).child(p.filesystem.clone()))
                .child(div().whitespace_nowrap().text_color(c(SUBTITLE)).child(format!("{} GB", p.gb))),
        );
    }
    let noun = if count == 1 { "partition" } else { "partitions" };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .py(px(12.))
        .bg(ca(0xf7768e0f))
        .border_1()
        .border_color(ca(0xf7768e59))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(icon("triangle-alert", 15., c(RED)))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(RED)).child(format!("{count} {noun} ({total} GB) will be erased"))),
        )
        .child(rows)
}

/// A yellow notice: heading and body.
fn notice(head: impl Into<SharedString>, body: impl Into<SharedString>, with_icon: bool) -> Div {
    let mut top = div().flex().flex_row().items_center().gap(px(8.));
    if with_icon {
        top = top.child(icon("triangle-alert", 15., c(AMBER)));
    }
    top = top.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(AMBER)).child(head.into()));
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(16.))
        .py(px(12.))
        .bg(ca(0xe0af680f))
        .border_1()
        .border_color(ca(0xe0af6852))
        .child(top)
        .child(div().w_full().text_size(px(13.)).line_height(px(19.)).text_color(c(SUBTITLE)).child(body.into()))
}

/// The two values above the split bar.
fn split_labels(ipados_gb: u32, omarchy_gb: u32) -> Div {
    let side = |name: &'static str, swatch: gpui::Hsla, value: u32, right: bool| {
        let mut col = div().flex().flex_col().gap(px(4.));
        if right {
            col = col.items_end();
        }
        col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(8.)).h(px(8.)).bg(swatch))
                .child(div().text_size(px(13.)).text_color(c(SUBTITLE)).child(name)),
        )
        .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child(format!("{value} GB")))
    };
    div()
        .w_full()
        .flex()
        .flex_row()
        .justify_between()
        .items_end()
        .child(side("iPadOS", c(MUTED), ipados_gb, false))
        .child(side(sys(), c(BLUE), omarchy_gb, true))
}

/// What the bar says inside the Omarchy part.
enum OmarchyNote {
    None,
    Replaces,
    Needs,
}

/// The split bar: iPadOS (with its used part), the divider, Omarchy A.
/// `omarchy_gb` may be fractional while the divider is dragged.
fn split_bar(layout: &Layout, omarchy_gb: f32, note: OmarchyNote, draggable: Option<(&gpui::FocusHandle, bool)>, cx: &mut Context<Installer>) -> impl IntoElement {
    let total = layout.total_gb.max(1) as f32;
    let ipados_w = SPLIT_WIDTH * (total - omarchy_gb) / total;
    // The used part keeps its true size; the label is clipped when the
    // divider comes close (the flow never lets iPadOS go below what it uses).
    let used_w = (SPLIT_WIDTH * layout.ipados_used_gb as f32 / total - 24.).max(0.);
    let used_label = if layout.ipados_used_gb as f32 / total > 0.85 { c(FAINT) } else { c(MUTED) };
    let ipados = div()
        .h_full()
        .w(px(ipados_w))
        .flex_none()
        .overflow_hidden()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .bg(c(0x1f2335))
        .border_1()
        .border_color(c(PANEL_LINE))
        .child(div().w(px(used_w)).h(px(32.)).flex_none().bg(c(LINE)))
        .child(div().flex_none().font_family(MONO).text_size(px(11.)).text_color(used_label).whitespace_nowrap().child(format!("{} GB used", layout.ipados_used_gb)));
    let mut omarchy = div().flex_1().min_w(px(0.)).h_full().overflow_hidden().flex().flex_row().items_center().justify_end().px(px(12.));
    if omarchy_gb > 0. {
        omarchy = omarchy.bg(linear_gradient(270., linear_color_stop(c(BLUE), 0.), linear_color_stop(c(0xbb9af7), 1.)));
    }
    omarchy = match note {
        OmarchyNote::None => omarchy,
        OmarchyNote::Replaces => omarchy.child(div().whitespace_nowrap().font_family(MONO).text_size(px(11.)).text_color(ca(0x1a1b26cc)).child("replaces existing Linux")),
        OmarchyNote::Needs => omarchy.child(div().font_family(MONO).text_size(px(11.)).text_color(c(RED)).whitespace_nowrap().child(format!("needs {MIN_OMARCHY_GB} GB"))),
    };
    // The divider, and its focus ring drawn last (above the gradient),
    // centred on the 4 px handle.
    let ring = draggable.is_some_and(|(_, ring)| ring);
    let handle = div().w(px(4.)).h(px(72.)).flex_none().bg(c(TEXT));
    let mut bar = div().id("split-bar").relative().w_full().h(px(56.)).flex().flex_row().items_center().child(ipados).child(handle).child(omarchy);
    if ring {
        bar = bar.child(div().absolute().left(px(ipados_w - 4.)).top(px(-12.)).w(px(12.)).h(px(80.)).border_1().border_color(c(BLUE)));
    }
    if draggable.is_some() {
        // The handle's grab area, wider than the 4 px line: the resize cursor.
        bar = bar.child(
            div().absolute().left(px(ipados_w - 6.)).top(px(-8.)).w(px(16.)).h(px(72.)).cursor(gpui::CursorStyle::ResizeLeftRight),
        );
    }
    if let Some((focus, _)) = draggable {
        let max = layout.max_omarchy_gb() as f32;
        let total_gb = layout.total_gb as f32;
        // Pressing on the bar puts the divider there and starts a drag; the
        // window follows the pointer from then on (Installer::drag_to).
        let start = omarchy_gb;
        bar = bar
            .track_focus(focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    this.drag = Some(crate::Drag { total_gb, max_gb: max, value: start, active: true });
                    this.drag_to(f32::from(e.position.x));
                    cx.notify();
                }),
            )
            // ← gives Omarchy A more, → less: 1 GB, 10 GB with Shift.
            .on_key_down(cx.listener(move |this, e: &gpui::KeyDownEvent, _, cx| {
                let step = if e.keystroke.modifiers.shift { 10. } else { 1. };
                let delta = match e.keystroke.key.as_str() {
                    "left" => step,
                    "right" => -step,
                    _ => return,
                };
                let current = this.drag.as_ref().map(|d| d.value.round()).unwrap_or(start.round());
                this.drag = Some(crate::Drag { total_gb, max_gb: max, value: current, active: false });
                this.set_split(current + delta);
                cx.stop_propagation();
                cx.notify();
            }));
    }
    bar
}

fn split_subtitle() -> String {
    format!("Drag the divider to choose how much space {} gets. Your apps and data stay where they are.", sys())
}

/// 2.2 / 2.2b / 2.3: choose the split.
#[allow(clippy::too_many_arguments)]
fn choose_split(layout: &Layout, shown_gb: f32, approved: bool, connected: bool, stuck: bool, problem: Option<&str>, focus: &gpui::FocusHandle, ring: bool, cx: &mut Context<Installer>) -> Div {
    let max = layout.max_omarchy_gb();
    let replaces = !layout.outside.is_empty();
    let little = max < orchard_flow::RECOMMENDED_OMARCHY_GB;
    let slider = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(split_labels(layout.total_gb - shown_gb.round() as u32, shown_gb.round() as u32))
        .child(split_bar(layout, shown_gb, if replaces { OmarchyNote::Replaces } else { OmarchyNote::None }, Some((focus, ring)), cx));
    let mut col = div().w_full().flex().flex_col().gap(px(22.)).child(header("Split the storage", split_subtitle(), TITLE)).child(slider);
    if little {
        col = col.child(notice(
            format!("iPadOS can only spare {max} GB"),
            format!("That's enough for {}, but tight. For more room, delete some apps or files on your iPad and check again.", sys()),
            true,
        ));
    }
    // With partitions to erase, Continue opens the confirmation dialog (the
    // red list and the backup approval live there, as on install).
    let label = if replaces { "Continue" } else { "Split storage" };
    let ready = if replaces { connected } else { approved && connected };
    let mut buttons = actions().w_full().child(gated("split", label, ready, Event::Continue, cx));
    if little && connected {
        buttons = buttons.child(secondary("check-again", "Check again", 16., 9., Event::Retry, cx));
    }
    // The companion's connection dropped: it comes back by itself.
    if !connected {
        // Not back after 20 s: the USB link is stuck; a replug clears it.
        let (line, color) = if stuck { ("Unplug the cable from your iPad and plug it back in.", AMBER) } else { ("Reconnecting…", MUTED) };
        buttons = buttons.child(div().ml(px(16.)).text_size(px(13.)).text_color(c(color)).child(line));
    } else if let Some(problem) = problem {
        // Why the last split didn't happen (the iPad's own words).
        let line = format!("Your iPad didn't split the storage: {problem}");
        buttons = buttons.child(div().ml(px(16.)).flex_1().min_w(px(0.)).text_size(px(13.)).line_height(px(19.)).text_color(c(RED)).child(line));
    }
    if replaces {
        col.child(buttons)
    } else {
        col.child(approve(approved, cx)).child(buttons)
    }
}

/// 2.3's confirmation, opened by Continue: what the split erases and the
/// backup approval (draft copy). Cancel is the safe default.
pub fn split_erase_dialog(layout: &Layout, approved: bool, connected: bool, stuck: bool, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    let cancel = div()
        .id("cancel-split")
        .track_focus(focus)
        .px(px(16.))
        .py(px(8.))
        .border_1()
        .border_color(c(0x3b4261))
        .hover(|s| s.border_color(c(0x565f89)).bg(c(PANEL)))
        .text_size(px(14.))
        .text_color(c(TEXT))
        .cursor_pointer()
        .child("Cancel");
    let cancel = if crate::keyboard() { cancel.focus(|s| s.border_color(c(BLUE))) } else { cancel };
    let cancel = cancel.on_click(cx.listener(|this, _, _, _| this.send(Event::Cancel)));
    crate::screens::dialog(480.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Before splitting"))
        .child(crate::screens::paragraph(format!("The partitions after iPadOS are erased, and {} gets their space.", sys())))
        .child(erase_alert(layout))
        .child(approve(approved, cx))
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .pt(px(4.))
                // The companion's connection dropped: Split storage waits for it.
                .child(div().flex_1().min_w(px(0.)).text_size(px(13.)).line_height(px(19.)).when(!connected, |d| {
                    let (line, color) = if stuck { ("Unplug the cable from your iPad and plug it back in.", AMBER) } else { ("Reconnecting…", MUTED) };
                    d.text_color(c(color)).child(line)
                }))
                .child(cancel)
                .child(gated("confirm-split", "Split storage", approved && connected, Event::Continue, cx)),
        )
}

/// 2.2c: not enough free space.
fn no_room(layout: &Layout, cx: &mut Context<Installer>) -> Div {
    let free = layout.max_omarchy_gb();
    let subtitle = format!(
        "{} needs at least {MIN_OMARCHY_GB} GB, and iPadOS only has {free} GB free right now. Delete some apps or files on your iPad, then check again.",
        sys()
    );
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(header("Not enough free space on your iPad", subtitle, TITLE))
        .child(div().w_full().flex().flex_col().gap(px(12.)).child(split_labels(layout.total_gb, 0)).child(split_bar(layout, 0., OmarchyNote::Needs, None, cx)))
        .child(actions().child(primary("check-again", "Check again", 18., 9., Event::Retry, cx)))
}

/// The split's three steps (2.4, 2.4b, 2.5): done, now, later.
fn split_steps(lost: bool, grow: Option<bool>, seconds: f32) -> Div {
    let row = |label: &'static str, state: u8, first: bool| {
        let (color, weight) = match state {
            0 => (c(SUBTITLE), FontWeight::NORMAL),
            1 if lost => (c(RED), FontWeight::SEMIBOLD),
            1 => (c(TEXT), FontWeight::SEMIBOLD),
            _ => (c(MUTED), FontWeight::NORMAL),
        };
        let mark = match state {
            0 => icon("check", 14., c(GREEN)),
            1 if lost => icon("circle-alert", 14., c(RED)),
            1 => div().w(px(14.)).h(px(14.)).flex().items_center().justify_center().child(crate::device::ring(14., seconds)),
            _ => icon("circle", 14., c(MUTED)),
        };
        let mut r = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(12.))
            .child(mark)
            .child(div().text_size(px(14.)).font_weight(weight).text_color(color).child(label));
        if !first {
            r = r.border_t_1().border_color(c(PANEL_LINE));
        }
        r
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .bg(c(PANEL))
        .border_1()
        .border_color(c(PANEL_LINE))
        .child(row("Check storage", 0, true))
        .child(row(match grow { Some(true) => "Grow iPadOS", Some(false) => "Shrink iPadOS", None => "Remove the old partitions" }, 1, false))
        .child(row("Check the result", 2, false))
}

fn minutes(since: Instant) -> u64 {
    since.elapsed().as_secs() / 60
}

/// 2.4 / 2.4b / 2.5: splitting, after a relaunch, or the link lost.
/// `ipados`: iPadOS's size (GB) after and before the split, when known.
fn splitting(link: Link, reopened: bool, ipados: Option<(u32, u32)>, stuck: bool, since: Instant, seconds: f32) -> Div {
    let lost = link == Link::Lost;
    // Growing into removed partitions is quick; shrinking APFS takes long.
    // None: the same size, only the old partitions go.
    let grow = match ipados {
        Some((after, before)) if after == before => None,
        Some((after, before)) => Some(after > before),
        None => Some(false),
    };
    let progress = div().w_full().flex().flex_col().gap(px(10.)).child(split_steps(lost, grow, seconds));
    if lost {
        return column()
            .gap(px(24.))
            .child(header("Reconnect your iPad", "The connection to your iPad was lost. Plug it back in to continue.", RED))
            .child(progress);
    }
    let subtitle = if reopened {
        "The installer was closed while the storage was being split. Checking on your iPad…".to_owned()
    } else {
        match (ipados, grow) {
            (Some((gb, _)), Some(true)) => format!("iPadOS is growing to {gb} GB. This takes a moment."),
            (Some((gb, _)), Some(false)) => format!("iPadOS is shrinking to {gb} GB. This can take around 30 minutes."),
            (Some(_), None) => format!("Removing the old {} partitions. This takes a moment.", sys()),
            (None, _) => "This can take around 30 minutes.".to_owned(),
        }
    };
    // Not designed beyond the example line: only what the window knows.
    let status = format!("still working · {} min so far", minutes(since));
    // Only after a minute (fading in), its place kept from the start so
    // nothing moves (after a relaunch too).
    let after = since.elapsed().as_secs_f32();
    let visible = ((after - 60.) / 0.4).clamp(0., 1.);
    let liveness = div()
        .opacity(visible)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(c(GREEN)))
        .child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(status));
    // The window lost sight of the split (stuck USB link): a replug clears it.
    let liveness = if stuck {
        div().text_size(px(13.)).line_height(px(19.)).text_color(c(AMBER)).child("The connection stopped. Unplug the cable from your iPad and plug it back in. This doesn't affect the split.")
    } else {
        liveness
    };
    column()
        .gap(px(24.))
        .child(header("Splitting the storage", subtitle, TITLE))
        .child(progress.child(liveness))
        .child(notice(
            "Keep your iPad connected and leave it alone",
            "It may freeze or look idle for a long time. That doesn't mean it's finished or failed. Don't restart or unplug it. Closing this window won't stop it.",
            true,
        ))
}

/// 2.6: split done.
fn split_done(layout: Option<&Layout>, omarchy_gb: u32, cx: &mut Context<Installer>) -> Div {
    let mut col = div().w_full().flex().flex_col().gap(px(22.)).child(header(
        "Your iPad is ready",
        format!("iPadOS and {} now each have their own space. Your apps and data are where they were.", sys()),
        TITLE,
    ));
    if let Some(layout) = layout {
        col = col.child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(split_labels(layout.total_gb - omarchy_gb, omarchy_gb))
                .child(split_bar(layout, omarchy_gb as f32, OmarchyNote::None, None, cx)),
        );
    }
    col.child(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(heading(format!("Next, install {}", sys())))
            .child(paragraph("Put your iPad into DFU mode once more, the same way as before.")),
    )
    .child(actions().child(primary("continue", "Continue", 18., 9., Event::Continue, cx)))
}

/// 2.1b / 3.2r: palera1n sits after checkm8 until the cable is replugged at
/// the device. No buttons: it closes by itself once the device is back.
pub fn replug_dialog() -> Div {
    crate::screens::dialog(420.)
        .items_center()
        .child(icon("plug", 28., c(BLUE)))
        .child(div().w_full().text_center().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Reconnect the cable at your iPad"))
        .child(
            div().w_full().text_center().text_size(px(14.)).line_height(px(21.)).text_color(c(SUBTITLE)).child(
                "Starting your iPad has stalled. Unplugging the cable at your iPad and plugging it back in usually fixes this. It continues by itself.",
            ),
        )
}

/// 2.1a: palera1n is starting the iPad.
fn starting_ipad() -> Div {
    column().child(header("Starting your iPad", "This takes about a minute. The window moves on when iPadOS is coming up.", TITLE))
}

/// 2.1: waiting for iPadOS with the companion; `slow` once it is well past
/// the usual time (draft copy).
fn waiting_for_ipados(slow: bool, cx: &mut Context<Installer>) -> Div {
    const STEPS: &[Step] = &[
        Step { parts: &[Part::Text("Unlock your iPad when the Lock Screen appears")], duration: None },
        Step {
            parts: &[Part::Text("Leave it connected. It can take a minute to be ready, even if nothing seems to happen.")],
            duration: None,
        },
    ];
    column()
        .child(header("Waiting for your iPad", "It should now be starting iPadOS. This window moves on as soon as it's ready.", TITLE))
        .child(steps_natural(STEPS))
        .children(slow.then(|| {
            actions()
                .child(secondary("start-over", "Start over", 16., 9., Event::Retry, cx))
                .child(div().ml(px(16.)).flex_1().min_w(px(0.)).text_size(px(13.)).line_height(px(19.)).text_color(c(AMBER)).child(
                    "Your iPad hasn't answered yet. If it shows the Lock Screen or Home Screen and nothing happens, start over in DFU mode.",
                ))
        }))
}

/// 3.1: DFU mode again.
fn dfu_again(sequence: orchard_flow::Sequence, rows: f32, cx: &mut Context<Installer>) -> Div {
    column()
        .child(header("Put your iPad in DFU mode again", format!("{} installs from DFU mode. Same buttons as before.", sys()), TITLE))
        .child(steps(connected_steps(sequence), rows))
        .child(trouble_link(cx))
}

/// 3.2: create the account while the install system starts.
fn create_account(installer: &Installer, account: &Account, ready: bool, cx: &mut Context<Installer>) -> Div {
    let inputs = installer.inputs.as_ref().expect("account inputs");
    let username = installer.username.clone();
    let row = |label: &'static str, value: gpui::AnyElement, first: bool| {
        let mut r = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(12.))
            .child(div().w(px(130.)).flex_none().text_size(px(14.)).text_color(c(TEXT)).child(label))
            .child(div().flex_1().child(value));
        if !first {
            r = r.border_t_1().border_color(c(PANEL_LINE));
        }
        r
    };
    let form = div()
        .w_full()
        .flex()
        .flex_col()
        .bg(c(PANEL))
        .border_1()
        .border_color(c(PANEL_LINE))
        // Set by the release: shown, not editable.
        .child(row("Username", div().font_family(MONO).text_size(px(13.)).text_color(c(MUTED)).child(username).into_any_element(), true))
        .child(row("Password", secret(inputs, 0, cx), false))
        .child(row("Repeat password", secret(inputs, 1, cx), false))
        .child(row("Lock screen PIN", secret(inputs, 2, cx), false))
        .child(row("Repeat PIN", secret(inputs, 3, cx), false));
    let valid = account.valid();
    // What still keeps "Install" off, once the user has typed something.
    // Well past the usual time without the install system: the safety net
    // for anything that didn't report itself (draft copy).
    let slow = matches!(installer.flow.state, State::CreateAccount { env: None, slow: true, .. });
    let problem = account_problem(account).or(if slow {
        Some("Your iPad is taking longer than usual. If nothing happens, start over in DFU mode.")
    } else if valid && !ready {
        Some("Your iPad is still getting ready.")
    } else {
        None
    });
    let hint = problem.map(|p| div().text_size(px(13.)).text_color(c(if valid { MUTED } else { AMBER })).child(p));
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(title("Create your account"))
                .child(div().text_size(px(14.)).text_color(c(SUBTITLE)).child(if installer.flow.resuming {
                    "The install continues where it stopped once you've entered your account again."
                } else {
                    "Your iPad is getting ready in the meantime."
                })),
        )
        .child(form)
        .child(ssh_row(account, cx))
        // The hint beside the button, well apart from it.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(28.))
                .child(gated("install", "Install", valid && ready, Event::Continue, cx))
                .children(slow.then(|| secondary("start-over", "Start over", 16., 9., Event::Retry, cx)))
                .children(hint),
        )
}

/// A password or PIN field with its eye button (3.2) to show what's typed.
fn secret(inputs: &[gpui::Entity<crate::input::TextInput>; 4], i: usize, cx: &mut Context<Installer>) -> gpui::AnyElement {
    let input = inputs[i].clone();
    let shown = !input.read(cx).masked();
    // Not a Tab stop (Tab goes field to field): the mouse, or ⌘⇧H in the field.
    let shortcut = if cfg!(target_os = "macos") { "⌘⇧H" } else { "Ctrl+Shift+H" };
    let tip = format!("{} ({shortcut})", if shown { "Hide" } else { "Show" });
    let eye = div()
        .id(("eye", i))
        .w(px(18.))
        .h(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tooltip(move |_, cx| cx.new(|_| crate::screens::Tip(tip.clone().into())).into())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(inputs) = this.inputs.as_ref() {
                inputs[i].update(cx, |field, cx| {
                    let masked = field.masked();
                    field.set_masked(!masked, cx)
                });
            }
        }))
        .child(icon("eye", 14., c(if shown { BLUE } else { MUTED })));
    div().w_full().flex().flex_row().items_center().gap(px(8.)).child(div().flex_1().child(input)).child(eye).into_any_element()
}

/// Why the account isn't ready yet, in the order a user fills the form;
/// nothing for fields not started. Not in the designs: a line under the form.
fn account_problem(a: &Account) -> Option<&'static str> {
    if !a.password.is_empty() && a.password.chars().count() < 8 {
        return Some("The password needs at least 8 characters.");
    }
    if !a.password_again.is_empty() && a.password != a.password_again {
        return Some("The passwords don't match.");
    }
    if !a.pin.is_empty() && a.pin.len() != 6 {
        return Some("The PIN has 6 digits.");
    }
    if !a.pin_again.is_empty() && a.pin != a.pin_again {
        return Some("The PINs don't match.");
    }
    None
}

/// 3.6 as a dialog over the account form, opened by Install: what the
/// install erases, and the backup approval. Cancel is the safe default.
pub fn erase_dialog(layout: &Layout, approved: bool, focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    let cancel = div()
        .id("cancel-erase")
        .track_focus(focus)
        .px(px(16.))
        .py(px(8.))
        .border_1()
        .border_color(c(0x3b4261))
        .hover(|s| s.border_color(c(0x565f89)).bg(c(PANEL)))
        .text_size(px(14.))
        .text_color(c(TEXT))
        .cursor_pointer()
        .child("Cancel");
    let cancel = if crate::keyboard() { cancel.focus(|s| s.border_color(c(BLUE))) } else { cancel };
    let cancel = cancel.on_click(cx.listener(|this, _, _, _| this.send(Event::Cancel)));
    crate::screens::dialog(480.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Before installing"))
        .child(crate::screens::paragraph(format!("{} goes into its own space, next to iPadOS.", sys())))
        .child(erase_alert(layout))
        .child(approve(approved, cx))
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(10.))
                .pt(px(4.))
                .child(cancel)
                .child(gated("confirm-erase", "Install", approved, Event::Continue, cx)),
        )
}

/// 3.4: installing.
fn installing(percent: u8, shown: f32, streamed: bool, since: Instant) -> Div {
    let p = percent.min(100) as f32 / 100.;
    let bar = (shown / 100.).clamp(0., 1.);
    // The time left from the rate so far; none until there is a rate.
    let elapsed = since.elapsed().as_secs_f32();
    let subtitle = if !streamed && p > 0.05 {
        let left = (elapsed / p * (1. - p) / 60.).ceil().max(1.) as u32;
        let unit = if left == 1 { "minute" } else { "minutes" };
        format!("About {left} {unit} left. Keep your iPad connected.")
    } else {
        "Keep your iPad connected.".to_owned()
    };
    let track = div()
        .w_full()
        .h(px(6.))
        .relative()
        .bg(c(PANEL))
        .border_1()
        .border_color(c(PANEL_LINE))
        .child(
            div()
                .absolute()
                .left(px(0.))
                .top(px(0.))
                .h(px(4.))
                .w(gpui::relative(bar))
                .bg(linear_gradient(270., linear_color_stop(c(BLUE), 0.), linear_color_stop(c(0xbb9af7), 1.))),
        );
    // "Finishing up" after the image is written is not in the designs.
    let stage = if streamed { "Finishing up" } else { "Writing the system" };
    column()
        .child(header(format!("Installing {}", sys()), subtitle, TITLE))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(track)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(div().text_size(px(13.)).text_color(c(MUTED)).child(stage))
                        .child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(format!("{percent}%"))),
                ),
        )
}

/// 3.5: the system is starting.
fn starting(took: Option<std::time::Duration>, ssh_user: Option<&str>, cx: &mut Context<Installer>) -> Div {
    column()
        .child(title(format!("{} is starting", sys())))
        // The install time (Omarchy's own installer shows it too), in the
        // progress bar's gradient, one shade per character.
        .children(took.map(|t| div().font_family(MONO).text_size(px(13.)).child(gradient_text(&format!("Installed {} in {:.1}s", sys(), t.as_secs_f64())))))
        .child(
            div().w(px(342.)).flex().flex_col().gap(px(10.)).child(heading("Starting it again later")).child({
                // "Orchard" in the accent: the app to open.
                let text_35 = format!("After a restart your iPad starts iPadOS as usual. To start {}, open the Orchard app on this computer and put your iPad in DFU mode.", sys());
                let at = text_35.find("Orchard").unwrap();
                let blue = gpui::HighlightStyle { color: Some(c(BLUE)), ..Default::default() };
                div()
                    .w_full()
                    .text_size(px(14.))
                    .line_height(px(21.))
                    .text_color(c(SUBTITLE))
                    .child(gpui::StyledText::new(text_35).with_highlights(vec![(at..at + "Orchard".len(), blue)]))
            }),
        )
        .children(launcher_line())
        .children(ssh_user.map(|user| {
            div()
                .w(px(342.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(heading("Signing in from this computer"))
                .child(div().font_family(MONO).text_size(px(13.)).text_color(c(SUBTITLE)).child(format!("ssh {user}@{USB_ADDRESS}")))
        }))
        .child(actions().child(primary("done", "Done", 18., 9., Event::Continue, cx)))
}

/// Monospace text in the progress bar's blue-to-violet gradient.
fn gradient_text(text: &str) -> gpui::StyledText {
    let (from, to) = ((0x7a, 0xa2, 0xf7), (0xbb, 0x9a, 0xf7));
    let n = text.chars().count().max(2) - 1;
    let mut runs = Vec::new();
    for (i, (at, ch)) in text.char_indices().enumerate() {
        let mix = |a: u32, b: u32| (a as f32 + (b as f32 - a as f32) * i as f32 / n as f32).round() as u32;
        let rgb = (mix(from.0, to.0) << 16) | (mix(from.1, to.1) << 8) | mix(from.2, to.2);
        runs.push((at..at + ch.len_utf8(), gpui::HighlightStyle { color: Some(c(rgb)), ..Default::default() }));
    }
    gpui::StyledText::new(text.to_owned()).with_highlights(runs)
}

/// Where the installer put Orchard, or why it couldn't (every error shows).
fn launcher_line() -> Option<Div> {
    let line = |text: String, color: u32| div().w(px(342.)).font_family(MONO).text_size(px(13.)).line_height(px(19.)).text_color(c(color)).child(text);
    match crate::LAUNCHER_SETUP.lock().unwrap().clone() {
        None if crate::STAMP.get().and_then(|s| s.as_ref()).and_then(|s| s.launcher_here()).is_some() => Some(line("Setting up Orchard…".into(), SUBTITLE)),
        None => None,
        Some(Ok(at)) => Some(line(format!("Orchard is in {at}"), SUBTITLE)),
        Some(Err(e)) => Some(line(format!("Orchard couldn't be set up: {e}"), RED)),
    }
}

/// 3.7: storage not prepared.
fn not_prepared(cx: &mut Context<Installer>) -> Div {
    column()
        .child(header(
            "Your iPad's storage is not prepared yet",
            format!("Put your iPad in DFU mode again. The installer then starts your iPad to split the storage between iPadOS and {}.", sys()),
            RED,
        ))
        .child(actions().child(primary("continue", "Continue", 18., 9., Event::Continue, cx)))
}

/// 3.9: the install failed.
fn install_failed(reason: Option<&str>, cx: &mut Context<Installer>) -> Div {
    column()
        .child(header(
            format!("{} couldn't be installed", sys()),
            "Something went wrong while writing it. iPadOS and your data weren't touched. Put your iPad in DFU mode again to retry.",
            RED,
        ))
        // What went wrong, in the device's or the installer's own words.
        .children(reason.map(|r| div().w_full().font_family(MONO).text_size(px(12.)).line_height(px(18.)).text_color(c(SUBTITLE)).child(r.to_owned())))
        .child(actions().child(primary("retry", "Try again", 18., 9., Event::Retry, cx)).child(save_log(cx)))
}

/// Opens the installer's logs folder (the design's "Save log").
fn save_log(cx: &mut Context<Installer>) -> impl IntoElement {
    let button = div()
        .id("save-log")
        .px(px(16.))
        .py(px(9.))
        .border_1()
        .border_color(c(0x3b4261))
        .hover(|s| s.border_color(c(0x565f89)).bg(c(PANEL)))
        .text_size(px(14.))
        .text_color(c(TEXT))
        .cursor_pointer()
        .child("Save log");
    pressable(button, |s| s.border_color(c(BLUE)), cx, |_, _, cx| {
        let logs = std::env::temp_dir().join("orchard-installer-cache").join("logs");
        cx.open_url(&format!("file://{}", logs.display()));
    })
}

/// 1.10: USB access on Linux.
fn usb_access(cx: &mut Context<Installer>) -> Div {
    column()
        .child(header(
            "Allow access to USB devices",
            "The installer needs permission to talk to your device over USB. This is set up once and stays in place for later installs.",
            TITLE,
        ))
        .child(
            actions()
                .child(primary("allow", "Allow access", 18., 9., Event::Continue, cx))
                .child(div().text_size(px(13.)).text_color(c(MUTED)).child("You'll be asked for your password.")),
        )
}

/// 1.5: tips for getting into DFU mode.
pub fn trouble_dialog(focus: &gpui::FocusHandle, cx: &mut Context<Installer>) -> Div {
    const TIPS: [(&str, &str); 4] = [
        ("Try a different cable, port or hub", "A USB-A cable through a hub can work where USB-C doesn't, or the other way round."),
        ("Quit other apps that use USB", "Browsers such as Chrome, Brave and Edge, and apps such as ChatGPT, access USB devices in the background, even with WebUSB turned off."),
        ("Try another computer", "A Mac instead of Linux, or the other way round, can behave differently over USB."),
        ("Vary the timing", "If the Apple logo appears or nothing changes, hold each step a little shorter or longer."),
    ];
    let mut tips = div().w_full().flex().flex_col().bg(c(PANEL)).border_1().border_color(c(PANEL_LINE));
    for (i, (head, body)) in TIPS.iter().enumerate() {
        let mut row = div()
            .w_full()
            .flex()
            .flex_row()
            .gap(px(12.))
            .px(px(16.))
            .py(px(12.))
            .child(div().font_family(MONO).text_size(px(12.)).text_color(c(MUTED)).child(format!("{}", i + 1)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TEXT)).child(*head))
                    .child(div().w_full().text_size(px(13.)).line_height(px(19.)).text_color(c(SUBTITLE)).child(*body)),
            );
        if i > 0 {
            row = row.border_t_1().border_color(c(PANEL_LINE));
        }
        tips = tips.child(row);
    }
    let footer = div()
        .w_full()
        .flex()
        .flex_row()
        .justify_between()
        .items_center()
        .child({
            let report = crate::screens::link("report").py(px(2.)).text_size(px(13.)).text_color(c(BLUE)).child("Something else fixed it for you? Tell us");
            pressable(report, crate::screens::link_ring, cx, |_, _, cx| cx.open_url(REPORT))
        })
        .child(
            div()
                .id("trouble-done")
                .track_focus(focus)
                .px(px(17.))
                .py(px(7.))
                .border_1()
                .border_color(c(BLUE))
                .bg(c(BLUE))
                .hover(|s| s.bg(c(0x93b4f9)).border_color(c(0x93b4f9)))
                .when(crate::keyboard(), |b| b.focus(|s| s.border_color(c(TITLE))))
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(ON_BLUE))
                .cursor_pointer()
                .child("Done")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.trouble = false;
                    cx.notify();
                })),
        );
    
    crate::screens::dialog(520.)
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(TITLE)).child("Having trouble?"))
        .child(paragraph("Getting into DFU mode can be tricky, and what gets in the way differs from setup to setup. These have fixed it for some people:"))
        .child(tips)
        .child(footer)
}

/// Where "Tell us" goes.
const REPORT: &str = "https://github.com/OrchardLinux/orchard/issues";

/// The screens of this module, if the state is one of them.
pub fn text_column(installer: &mut Installer, cx: &mut Context<Installer>) -> Option<gpui::AnyElement> {
    let state = installer.flow.state.clone();
    Some(match &state {
        State::UsbAccessNeeded => usb_access(cx).into_any_element(),
        // 2.1a until palera1n boots iPadOS, then 2.1.
        State::WaitingForIpados { .. } if !installer.flow.booting => starting_ipad().into_any_element(),
        State::WaitingForIpados { slow, .. } => waiting_for_ipados(*slow, cx).into_any_element(),
        State::ChooseSplit { layout, omarchy_gb, approved, .. } => {
            installer.layout = Some(layout.clone());
            if layout.max_omarchy_gb() < MIN_OMARCHY_GB {
                no_room(layout, cx).into_any_element()
            } else {
                // While dragging, the window's own position; else the flow's.
                let shown = installer.drag.as_ref().map(|d| d.value).unwrap_or(*omarchy_gb as f32);
                let (focus, ring) = (installer.slider_focus.clone(), installer.slider_ring);
                let connected = installer.flow.companion;
                let problem = installer.flow.split_problem.clone();
                let stuck = installer.link_stuck();
                choose_split(layout, shown, *approved, connected, stuck, problem.as_deref(), &focus, ring, cx).into_any_element()
            }
        }
        State::Splitting { link, reopened, omarchy_gb, .. } => {
            let ipados = installer.layout.as_ref().map(|l| (l.total_gb - omarchy_gb, l.ipados_gb));
            let seconds = installer.started.elapsed().as_secs_f32();
            splitting(*link, *reopened, ipados, installer.link_stuck(), installer.since, seconds).into_any_element()
        }
        State::SplitDone { omarchy_gb, .. } => {
            let layout = installer.layout.clone();
            split_done(layout.as_ref(), *omarchy_gb, cx).into_any_element()
        }
        State::DfuAgain { device, .. } => {
            let steps = connected_steps(device.sequence);
            installer.rows.set(steps.len() as f32);
            dfu_again(device.sequence, installer.rows.value(), cx).into_any_element()
        }
        State::CreateAccount { account, env, .. } => create_account(installer, account, env.is_some(), cx).into_any_element(),
        // The erase dialog (3.6) sits over the form: what was typed stays.
        State::ConfirmErase { account, .. } => create_account(installer, account, true, cx).into_any_element(),
        State::NotPrepared { .. } => not_prepared(cx).into_any_element(),
        State::Installing { percent, link: Link::Lost, streamed: false, .. } => {
            let _ = percent;
            column()
                .gap(px(24.))
                .child(header("Reconnect your iPad", "The connection to your iPad was lost. Plug it back in to continue.", RED))
                .into_any_element()
        }
        // 99 % at most until the device reports done: the image being
        // written isn't the end (finishing up follows); 100 % is 3.5.
        State::Installing { percent, streamed, .. } => {
            // (100 only in the moment after done, before 3.5: see hold_finish.)
            let percent = if installer.finish_hold.is_some() { 100 } else { (*percent).min(99) };
            installer.progress.set(percent as f32);
            installing(percent, installer.progress.value(), *streamed, installer.since).into_any_element()
        }
        State::InstallFailed { .. } => {
            let reason = installer.flow.install_problem.clone();
            install_failed(reason.as_deref(), cx).into_any_element()
        }
        State::Starting { .. } => {
            let took = installer.install_took;
            let user = installer.username.clone();
            starting(took, installer.ssh_chosen.then_some(user.as_ref()), cx).into_any_element()
        }
        _ => return None,
    })
}

