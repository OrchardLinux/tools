//! The window around the flow, for both programs: the installer and Orchard,
//! the launcher (src/bin). The driver (runtime) polls USB, steps the flow and
//! does the work on its own thread; this draws the flow's view and sends what
//! the user does back as events.
mod assets;
mod details;
mod device;
mod input;
mod install_screens;
mod screens;
mod theme;

use gpui::{
    div, point, prelude::*, px, size, App, Application, Bounds, Context, MouseButton, SharedString, TitlebarOptions,
    Window, WindowBounds, WindowOptions,
};
use orchard_flow::{view, DeviceView, Event, Flow, ScreenKind};
use orchard_runtime::driver::{self, Observer, Options};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use theme::*;

gpui::actions!(installer, [FocusNext, FocusPrevious, CloseDialog, Hide, HideOthers, ShowAll, Quit]);

/// While a dialog is open, only its own controls are keyboard stops; the
/// screen behind sets none (focus trap). `IN_DIALOG` marks the dialog's own.
pub static DIALOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static IN_DIALOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a control being drawn now belongs to the open dialog.
pub fn in_dialog() -> bool {
    IN_DIALOG.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a control being drawn now may take keyboard focus.
pub fn focusable_now() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    !DIALOG.load(Relaxed) || IN_DIALOG.load(Relaxed)
}

/// Whether the user is moving through the window with the keyboard: focus
/// rings show only then (like a browser's :focus-visible). Tab turns it on,
/// any click turns it off.
pub static KEYBOARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn keyboard() -> bool {
    KEYBOARD.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_keyboard(on: bool) {
    KEYBOARD.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The newest flow from the driver, picked up by the window.
#[derive(Clone, Default)]
struct Latest(Arc<Mutex<Option<Flow>>>);

struct Bridge {
    latest: Latest,
    /// The last state's name, to log each change once.
    last: String,
}

/// The executor, for putting the launcher on this computer after an install.
static EXECUTOR: std::sync::OnceLock<orchard_runtime::exec::Executor> = std::sync::OnceLock::new();
/// The launcher's setup after an install: where it is now, or what went wrong.
pub static LAUNCHER_SETUP: Mutex<Option<Result<String, String>>> = Mutex::new(None);
static LAUNCHER_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Once the system is installed: download, check and put the launcher in
/// place, from the asset the stamp pins.
fn set_up_launcher() {
    if LAUNCHER_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let pin = STAMP.get().and_then(|s| s.as_ref()).and_then(|s| s.launcher_here().cloned());
    let (Some(pin), Some(executor)) = (pin, EXECUTOR.get().cloned()) else {
        eprintln!("launcher: no launcher pinned for {} (development build)", orchard_runtime::stamp::host());
        return;
    };
    std::thread::spawn(move || {
        let result = orchard_runtime::setup_launcher::install(&executor, &pin);
        eprintln!("{} launcher: {result:?}", orchard_runtime::install::stamp());
        *LAUNCHER_SETUP.lock().unwrap() = Some(result);
    });
}

impl Observer for Bridge {
    fn changed(&mut self, flow: &Flow) {
        // STAMP is set only in the installer.
        if STAMP.get().is_some() && matches!(flow.state, orchard_flow::State::Starting { .. }) {
            set_up_launcher();
        }
        // One line per change of state (its name only: states can hold the account).
        let name: String = format!("{:?}", flow.state).chars().take_while(|c| c.is_alphanumeric()).collect();
        if name != self.last {
            eprintln!("{} state: {} -> {name}", orchard_runtime::install::stamp(), self.last);
            self.last = name;
        }
        *self.latest.0.lock().unwrap() = Some(flow.clone());
    }
    fn quit(&mut self) {
        std::process::exit(0);
    }
    fn effect(&mut self, effect: &orchard_flow::Effect) {
        eprint!("{} ", orchard_runtime::install::stamp());
        // Not the account (it holds the password).
        if !matches!(effect, orchard_flow::Effect::StartInstall(_)) {
            eprintln!("effect: {effect:?}");
        } else {
            eprintln!("effect: StartInstall");
        }
    }
    fn usb(&mut self, event: &Event) {
        eprintln!("{} usb: {event:?}", orchard_runtime::install::stamp());
    }
}

/// A value moving from one number to another over `millis`, eased out.
#[derive(Clone, Copy)]
pub struct Tween {
    from: f32,
    to: f32,
    at: Instant,
    millis: f32,
    /// When it was last set (every frame that draws it): a tween not drawn
    /// for a while (window in the background) jumps instead of animating
    /// from a stale value.
    touched: Instant,
}

impl Tween {
    fn new(value: f32, millis: f32) -> Tween {
        Tween { from: value, to: value, at: Instant::now(), millis, touched: Instant::now() }
    }

    /// 0…1 through the move, eased out (cubic).
    pub fn progress(&self) -> f32 {
        let t = (self.at.elapsed().as_secs_f32() * 1000. / self.millis).clamp(0., 1.);
        1. - (1. - t).powi(3)
    }

    pub fn value(&self) -> f32 {
        self.from + (self.to - self.from) * self.progress()
    }

    /// Moves to `to` from wherever it is now.
    fn set(&mut self, to: f32) {
        let stale = self.touched.elapsed() > Duration::from_millis(300);
        self.touched = Instant::now();
        if stale {
            // Not drawn for a while: where it should be now, no animation.
            self.from = to;
            self.to = to;
        } else if to != self.to {
            *self = Tween { from: self.value(), to, at: Instant::now(), millis: self.millis, touched: self.touched };
        }
    }
}

/// The two dialogs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Skip,
    Trouble,
    Erase,
    /// palera1n waits for the cable to be replugged at the device.
    Replug,
    /// The split erases partitions: confirm (2.3).
    SplitErase,
    /// ⌘Q / close: "Quit the installer?"
    Quit,
    /// ⌘Q / close while the split or the install runs: not now.
    QuitBlocked,
    /// What "Allow SSH from this computer" does (3.2i).
    SshInfo,
    /// The start's Details while the install system starts (3.2e).
    Details,
    /// A download failed for good (retries didn't help): why, and try again.
    Download,
}

/// What a quit request (⌘Q, the close button) shows in the installer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QuitAsk {
    Confirm,
    Blocked,
}

/// The split divider under the pointer: continuous while dragging, sent to
/// the flow in whole gigabytes.
pub struct Drag {
    pub total_gb: f32,
    pub max_gb: f32,
    /// Omarchy A's share in GB, fractional; stays where the user leaves it.
    pub value: f32,
    /// The pointer is holding the divider.
    pub active: bool,
}

/// The split bar's place in the window (its text column, 960 − 2 × 72).
const SPLIT_LEFT: f32 = 72.;
const SPLIT_WIDTH: f32 = 816.;

/// What the window showed last, to animate what changes (spec: Transitions).
struct Shown {
    device: DeviceView,
    /// The device before the last change, while it animates away.
    previous: Option<(DeviceView, Instant)>,
    /// The text column's key: the screen. A tab switch only animates the
    /// step table's height.
    content: ScreenKind,
    content_at: Instant,
    /// +1: the new screen comes from the right; -1: from the left.
    content_direction: f32,
}

const DEVICE_MILLIS: f32 = 420.;
const CONTENT_MILLIS: f32 = 260.;

pub struct Installer {
    app: orchard_flow::App,
    flow: Flow,
    events: Sender<Event>,
    started: Instant,
    /// The distribution the installer installs, for the title.
    title: SharedString,
    shown: Option<Shown>,
    /// The waiting screen's step table, in rows, so its height animates.
    pub rows: Tween,
    /// The start's Details panel (2.1p/2.1q): open or not, its height, its
    /// log's scroll, how many log lines were shown, when Copy was pressed.
    pub details_open: bool,
    pub details: Tween,
    pub details_scroll: gpui::ScrollHandle,
    pub details_seen: std::cell::Cell<usize>,
    /// 3.2e: the Details dialog, opened by the (i) beside 3.2's subtitle.
    pub details_info: bool,
    pub copied_at: Option<Instant>,
    /// The account form's fields (3.2): password, repeat, PIN, repeat.
    pub inputs: Option<[gpui::Entity<input::TextInput>; 4]>,
    /// The account user the release creates.
    pub username: SharedString,
    /// The storage layout last shown, for the screens after the split.
    pub layout: Option<orchard_flow::Layout>,
    /// When the current screen appeared (elapsed times, estimates).
    pub since: Instant,
    /// The split divider while it is dragged.
    pub drag: Option<Drag>,
    /// The trouble dialog (1.5) is open.
    pub trouble: bool,
    /// The SSH explanation (3.2i) is open.
    pub ssh_info: bool,
    /// SSH was allowed for this install: 3.5 shows how to sign in.
    pub ssh_chosen: bool,
    /// The install's progress, eased between the receiver's reports.
    pub progress: Tween,
    /// The window's own focus, so the first Tab has somewhere to start from.
    root_focus: gpui::FocusHandle,
    /// The split divider as a keyboard stop.
    pub slider_focus: gpui::FocusHandle,
    /// Whether to draw the divider's focus ring this frame.
    pub slider_ring: bool,
    /// A new screen opened: focus goes back to the window, so Tab starts at
    /// its first control.
    refocus: bool,
    /// The account form just opened: its password field is to take focus.
    focus_password: bool,
    /// Whether a dialog showed last frame, to move focus into a new one.
    dialog_was: bool,
    /// The open dialog's default choice.
    pub dialog_focus: gpui::FocusHandle,
    /// What had focus when the dialog opened, to return to on close.
    focus_before_dialog: Option<gpui::FocusHandle>,
    /// The open dialog itself (not a Tab stop).
    pub dialog_root: gpui::FocusHandle,
    /// When a dialog last closed (see screens::pressable).
    pub dialog_closed_at: Option<Instant>,
    /// The dialog showing or fading out, and how far it is shown (0…1).
    pub dialog_kind: Option<DialogKind>,
    pub dialog_fade: Tween,
    /// The erase dialog's content, kept to draw it while it fades out.
    pub erase_shown: Option<(orchard_flow::Layout, bool)>,
    /// The split's erase dialog's content, likewise.
    pub split_erase_shown: Option<(orchard_flow::Layout, bool)>,
    /// When the last frame was drawn (ORCHARD_TIMING).
    last_frame: Instant,
    /// The install's stopwatch: from Install to the device's "done".
    install_started: Option<Instant>,
    /// After "done", 3.4 stays until its bar has filled to 100 % (then 3.5).
    pub finish_hold: Option<Instant>,
    pub install_took: Option<Duration>,
    /// A quit request waiting for an answer (installer only).
    pub quitting: Option<QuitAsk>,
    /// Since when the companion is unreachable while the split screen or the
    /// split watches it (after 20 s: tell the user to replug).
    pub companion_lost_at: Option<Instant>,
}

impl Installer {
    /// The companion stayed unreachable for 20 s: a stuck USB link, which a
    /// replug at the iPad clears (seen once, 2026-11-22).
    pub fn link_stuck(&self) -> bool {
        self.companion_lost_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(20))
    }

    /// ⌘Q or the window's close button. The launcher quits; the installer
    /// asks first, and refuses while the iPad is splitting or installing.
    pub fn request_quit(&mut self, cx: &mut Context<Self>) {
        if self.app == orchard_flow::App::Launcher {
            // Not while m1n1 still loads the entry over USB: that cuts the start off.
            if matches!(self.flow.state, orchard_flow::State::Starting { .. }) && !self.flow.entry_started {
                self.quitting = Some(QuitAsk::Blocked);
                cx.notify();
            } else {
                cx.quit();
            }
            return;
        }
        let busy = matches!(self.flow.state, orchard_flow::State::Splitting { .. } | orchard_flow::State::Installing { .. });
        self.quitting = Some(if busy { QuitAsk::Blocked } else { QuitAsk::Confirm });
        cx.notify();
    }

    pub fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Seconds since the window opened, for the device's animations.
    fn seconds(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    /// Moves the dragged divider to the pointer's x, and tells the flow
    /// when the whole-GB value changes.
    pub fn drag_to(&mut self, x: f32) {
        let Some(drag) = self.drag.as_ref() else { return };
        let ipados = (x - SPLIT_LEFT) / SPLIT_WIDTH * drag.total_gb;
        self.set_split(drag.total_gb - ipados);
    }

    /// Puts the divider at `omarchy_gb` (kept as is, fractional) and tells
    /// the flow when the whole-GB value changes.
    pub fn set_split(&mut self, omarchy_gb: f32) {
        let Some(drag) = self.drag.as_mut() else { return };
        drag.value = omarchy_gb.clamp(orchard_flow::MIN_OMARCHY_GB as f32, drag.max_gb);
        let gb = drag.value.round() as u32;
        if !matches!(self.flow.state, orchard_flow::State::ChooseSplit { omarchy_gb, .. } if omarchy_gb == gb) {
            self.send(Event::SetOmarchySize(gb));
        }
    }

    /// The account form (3.2): its fields exist while it shows, and what is
    /// typed goes to the flow as the account.
    fn sync_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account = match &self.flow.state {
            orchard_flow::State::CreateAccount { account, .. } => account,
            // The erase confirmation sits between the form's visits: what was
            // typed stays.
            orchard_flow::State::ConfirmErase { .. } => return,
            _ => {
                self.inputs = None;
                return;
            }
        };
        if self.inputs.is_none() {
            let field = |cx: &mut Context<Self>, placeholder, mask, digits| cx.new(|cx| input::TextInput::new(cx, placeholder, mask, digits));
            let inputs = [
                field(cx, "", true, None),
                field(cx, "", true, None),
                field(cx, "", true, Some(6)),
                field(cx, "", true, Some(6)),
            ];
            // The eye buttons show the fields' state: redraw when a field changes (⌘⇧H).
            for input in &inputs {
                cx.observe(input, |_, _, cx| cx.notify()).detach();
            }
            self.inputs = Some(inputs);
            self.focus_password = true;
        }
        // The password field takes focus as the form opens. gpui drops focus
        // on a field not drawn yet, so ask again until it holds.
        if self.focus_password && !screens::dialog_open(self) {
            let focus = gpui::Focusable::focus_handle(self.inputs.as_ref().unwrap()[0].read(cx), cx);
            if focus.is_focused(window) {
                self.focus_password = false;
            } else {
                window.focus(&focus);
                window.request_animation_frame();
            }
        }
        let texts: Vec<String> = self.inputs.as_ref().unwrap().iter().map(|i| i.read(cx).text().to_string()).collect();
        let typed = orchard_flow::Account {
            username: self.username.to_string(),
            password: texts[0].clone(),
            password_again: texts[1].clone(),
            pin: texts[2].clone(),
            pin_again: texts[3].clone(),
            ssh: account.ssh,
        };
        self.ssh_chosen = account.ssh;
        if typed != *account {
            self.send(Event::EditAccount(typed));
        }
    }

    /// The install's stopwatch (3.5 shows it): it runs from Install until
    /// the install system reports done.
    fn time_install(&mut self) {
        match &self.flow.state {
            orchard_flow::State::Installing { .. } => {
                self.install_started.get_or_insert_with(Instant::now);
            }
            orchard_flow::State::Starting { .. } => {
                if let (Some(start), None) = (self.install_started, self.install_took) {
                    let took = start.elapsed();
                    eprintln!("{} timing: installed in {:.1}s (Install pressed to done)", orchard_runtime::install::stamp(), took.as_secs_f64());
                    self.install_took = Some(took);
                }
            }
            _ => {
                self.install_started = None;
                self.install_took = None;
            }
        }
    }

    /// Notes what changed since the last frame.
    fn track(&mut self, device: DeviceView, kind: ScreenKind) {
        // A dialog belongs to the screen beneath it: opening or closing it
        // doesn't replay the screen's slide-in.
        let content = match kind {
            ScreenKind::SkipConfirmation => ScreenKind::Found,
            ScreenKind::ConfirmErase => ScreenKind::CreateAccount,
            kind => kind,
        };
        let now = Instant::now();
        let Some(shown) = &mut self.shown else {
            self.shown = Some(Shown { device, previous: None, content, content_at: now - Duration::from_secs(1), content_direction: 1. });
            return;
        };
        if shown.device != device {
            shown.previous = Some((shown.device, now));
            shown.device = device;
        }
        if shown.content != content {
            self.since = now;
            self.refocus = true;
            // A new screen closes the trouble dialog; focus starts afresh.
            self.trouble = false;
            self.ssh_info = false;
            self.details_info = false;
            self.focus_before_dialog = None;
            // Back when the screen goes back in the flow.
            let back = (content as u8) < (shown.content as u8);
            shown.content = content;
            shown.content_at = now;
            shown.content_direction = if back { -1. } else { 1. };
        }
    }

    /// The device column: waves, and the device morphing from its previous
    /// look (a carousel slide for a new model, a crossfade otherwise).
    fn device_layer(&mut self) -> gpui::Div {
        let seconds = self.seconds();
        let shown = self.shown.as_mut().expect("tracked");
        let current = shown.device;
        let mut layer = div().relative().w(px(device::WIDTH)).h(px(device::HEIGHT)).flex_none();
        if let Some(waves) = device::waves(&current, seconds) {
            layer = layer.child(waves);
        }
        // Ease out (cubic): the slide starts moving at once and settles gently.
        let ease = |at: Instant| {
            let t = (at.elapsed().as_secs_f32() * 1000. / DEVICE_MILLIS).clamp(0., 1.);
            1. - (1. - t).powi(3)
        };
        // The devices sit in a strip clipped a little wider than the column
        // (room for the shadows), so a slide never runs into the text.
        const CLIP_X: f32 = 75.;
        const CLIP_Y: f32 = 70.;
        const SPACING: f32 = 200.;
        let place = |view: &DeviceView, dx: f32, opacity: f32| {
            div().absolute().left(px(CLIP_X + dx)).top(px(CLIP_Y)).opacity(opacity).child(device::drawing(view, seconds))
        };
        let mut strip = div()
            .absolute()
            .left(px(-CLIP_X))
            .top(px(-CLIP_Y))
            .w(px(device::WIDTH + 2. * CLIP_X))
            .h(px(device::HEIGHT + 2. * CLIP_Y))
            .overflow_hidden();
        match shown.previous {
            Some((old, at)) if at.elapsed().as_secs_f32() * 1000. < DEVICE_MILLIS => {
                let e = ease(at);
                if old.model != current.model {
                    // One strip: both move by the same amount, a device apart.
                    // Fades follow the position: out while leaving, in while arriving.
                    let dir = if model_order(current.model) > model_order(old.model) { 1. } else { -1. };
                    let shift = -dir * SPACING * e;
                    let leaving = (1. - e / 0.5).max(0.);
                    let arriving = ((e - 0.3) / 0.7).clamp(0., 1.);
                    strip = strip.child(place(&old, shift, leaving)).child(place(&current, shift + dir * SPACING, arriving));
                } else {
                    strip = strip.child(place(&old, 0., 1. - e)).child(place(&current, 0., e));
                }
            }
            _ => {
                shown.previous = None;
                strip = strip.child(place(&current, 0., 1.));
            }
        }
        layer = layer.child(strip);
        layer
    }

    /// Whether anything on screen moves: waves, the spinner, a transition.
    fn animating(&self, device: &DeviceView) -> bool {
        let busy = |at: Instant, millis: f32| at.elapsed().as_secs_f32() * 1000. < millis;
        let shown = self.shown.as_ref().expect("tracked");
        device.visible && (device.rings || device.glyph == orchard_flow::Glyph::Spinner)
            || shown.previous.is_some_and(|(_, at)| busy(at, DEVICE_MILLIS))
            || busy(shown.content_at, CONTENT_MILLIS)
            || self.rows.progress() < 1.
            || self.progress.progress() < 1.
            || self.dialog_fade.progress() < 1.
            || matches!(self.flow.state, orchard_flow::State::Splitting { .. })
            // Waiting to show the replug hint.
            || (self.companion_lost_at.is_some() && !self.link_stuck())
    }

    /// The text column's slide and fade after a change of screen.
    fn content_motion(&self) -> (f32, f32) {
        let shown = self.shown.as_ref().expect("tracked");
        let t = (shown.content_at.elapsed().as_secs_f32() * 1000. / CONTENT_MILLIS).clamp(0., 1.);
        let e = 1. - (1. - t).powi(3);
        (shown.content_direction * 24. * (1. - e), e)
    }
}

fn model_order(model: orchard_flow::Model) -> u8 {
    match model {
        orchard_flow::Model::IpadHome => 0,
        orchard_flow::Model::IpadNoHome => 1,
        orchard_flow::Model::IphoneHome => 2,
        orchard_flow::Model::IphoneNoHome => 3,
    }
}

impl Render for Installer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_start = Instant::now();
        // Freeze hunting (ORCHARD_TIMING=1): slow frames and long gaps.
        if timing() {
            let gap = self.last_frame.elapsed().as_millis();
            if gap > 250 {
                eprintln!("timing: {gap} ms since the last frame (dialog {:?}, screen {:?})", screens::open_dialog(self).is_some(), view(self.app, &self.flow.state).1);
            }
        }
        let watching = matches!(self.flow.state, orchard_flow::State::ChooseSplit { .. } | orchard_flow::State::Splitting { link: orchard_flow::Link::Up, .. });
        if watching && !self.flow.companion {
            self.companion_lost_at.get_or_insert_with(Instant::now);
        } else {
            self.companion_lost_at = None;
        }
        let result = self.render_inner(window, cx);
        // Draw every display frame while something moves. Asked last, after
        // this frame has updated everything (a dialog that just started to
        // fade, a tween that just got a new target), so no motion stalls.
        let device_view = view(self.app, &self.flow.state).0;
        if self.animating(&device_view) {
            window.request_animation_frame();
        }
        if timing() {
            let took = frame_start.elapsed().as_millis();
            if took > 100 {
                eprintln!("timing: frame took {took} ms");
            }
        }
        self.last_frame = Instant::now();
        result
    }
}

fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ORCHARD_TIMING").is_some())
}

impl Installer {
    fn render_inner(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        self.time_install();
        // Just installed: 3.4 with its bar filling to 100 % first, then 3.5.
        let held = self.hold_finish(window);
        let real = held.map(|installing| std::mem::replace(&mut self.flow.state, installing));
        let element = self.render_screen(window, cx);
        if let Some(real) = real {
            self.flow.state = real;
        }
        element
    }

    /// While the finished install's bar fills to 100 %: the state to draw
    /// instead of Starting (3.4 at 100 %).
    fn hold_finish(&mut self, window: &mut Window) -> Option<orchard_flow::State> {
        let orchard_flow::State::Starting { device, .. } = &self.flow.state else {
            self.finish_hold = None;
            return None;
        };
        // Only right after an install this window watched (the bar is on).
        if self.install_started.is_none() && self.finish_hold.is_none() {
            return None;
        }
        let until = *self.finish_hold.get_or_insert_with(|| Instant::now() + Duration::from_millis(900));
        if Instant::now() >= until {
            return None;
        }
        window.request_animation_frame();
        Some(orchard_flow::State::Installing { device: device.clone(), percent: 100, link: orchard_flow::Link::Up, streamed: true, step: None })
    }

    fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let (device_view, kind) = view(self.app, &self.flow.state);
        self.track(device_view, kind);
        if std::mem::take(&mut self.refocus) {
            window.focus(&self.root_focus);
        }
        self.sync_account(window, cx);
        self.slider_ring = keyboard() && self.slider_focus.is_focused(window);
        match &self.flow.state {
            orchard_flow::State::ChooseSplit { omarchy_gb, .. } => {
                if self.drag.as_ref().is_some_and(|d| !d.active && d.value.round() as u32 != *omarchy_gb) {
                    self.drag = None;
                }
            }
            _ => self.drag = None,
        }
        let titlebar = div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .font_family(MONO)
            .text_size(px(12.))
            .text_color(c(MUTED))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            // Where the window manager draws its own title bar (Linux with
            // server-side decorations), the title isn't shown a second time.
            .when(cfg!(target_os = "macos") || !matches!(window.window_decorations(), gpui::Decorations::Server), |d| d.child(self.title.clone()));
        let visual = div()
            .w(px(360.))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(self.device_layer());
        let padding = if device_view.visible { 56. } else { 72. };
        let mut body = div().flex_1().flex().flex_row().items_center().gap(px(48.)).px(px(padding));
        if device_view.visible {
            body = body.child(visual);
        }
        let (dx, opacity) = self.content_motion();
        // A dialog traps focus: the screen behind draws without keyboard stops.
        let dialog = screens::dialog_open(self);
        if dialog != self.dialog_was {
            self.dialog_was = dialog;
            // The text fields draw themselves: redraw them so they join (or
            // leave) the Tab order with the dialog.
            if let Some(inputs) = &self.inputs {
                for input in inputs {
                    input.update(cx, |_, cx| cx.notify());
                }
            }
            if dialog {
                // Opening: remember what had focus; the dialog itself takes it,
                // so the first Tab reaches its default choice.
                self.focus_before_dialog = window.focused(cx);
                window.focus(&self.dialog_root);
            } else {
                // Closing: back to what had focus, else the screen's start.
                self.dialog_closed_at = Some(Instant::now());
                match self.focus_before_dialog.take() {
                    Some(previous) => window.focus(&previous),
                    None => window.focus(&self.root_focus),
                }
            }
        }
        DIALOG.store(dialog, std::sync::atomic::Ordering::Relaxed);
        // Fade the dialog in, or out keeping the one that was open.
        match screens::open_dialog(self) {
            Some(kind) => {
                self.dialog_kind = Some(kind);
                self.dialog_fade.set(1.);
                if let orchard_flow::State::ConfirmErase { layout, approved, .. } = &self.flow.state {
                    self.erase_shown = Some((layout.clone(), *approved));
                }
                if let orchard_flow::State::ChooseSplit { layout, approved, .. } = &self.flow.state {
                    self.split_erase_shown = Some((layout.clone(), *approved));
                }
            }
            None => self.dialog_fade.set(0.),
        }
        let column = screens::text_column(self, kind, cx);
        // The design's text column: 440 px beside the device, 816 px without it.
        let width = if device_view.visible { 440. } else { 816. };
        body = body.child(div().w(px(width)).flex_none().relative().left(px(dx)).opacity(opacity).child(column));
        IN_DIALOG.store(true, std::sync::atomic::Ordering::Relaxed);
        let overlay = screens::overlay(self, cx);
        IN_DIALOG.store(false, std::sync::atomic::Ordering::Relaxed);
        if self.drag.as_ref().is_some_and(|d| d.active) {
            window.request_animation_frame();
        }
        let dragging = self.drag.as_ref().is_some_and(|d| d.active);
        let mut window = div()
            .id("window")
            .track_focus(&self.root_focus)
            // While the divider is dragged, the resize cursor everywhere.
            .when(dragging, |w| w.cursor(gpui::CursorStyle::ResizeLeftRight))
            // ⌘Q and the menu's Quit arrive here while the window has focus:
            // the app-level handler can't reach the window then (gpui holds it
            // during dispatch), so it would do nothing.
            .on_action(cx.listener(|this, _: &Quit, _, cx| this.request_quit(cx)))
            .on_action(cx.listener(|this, _: &CloseDialog, _, cx| {
                // Escape: the dialog's safe way out.
                if this.quitting.is_some() {
                    this.quitting = None;
                    cx.notify();
                } else if this.trouble {
                    this.trouble = false;
                    cx.notify();
                } else if this.ssh_info {
                    this.ssh_info = false;
                    cx.notify();
                } else if this.details_info {
                    this.details_info = false;
                    cx.notify();
                } else if matches!(this.flow.state, orchard_flow::State::Found { confirm_skip: true, .. }) {
                    this.send(Event::CancelSkip);
                } else if matches!(this.flow.state, orchard_flow::State::ConfirmErase { .. }) || this.flow.confirming_split {
                    this.send(Event::Cancel);
                }
            }))
            .on_action(|_: &FocusNext, window, _| {
                set_keyboard(true);
                window.focus_next()
            })
            .on_action(|_: &FocusPrevious, window, _| {
                set_keyboard(true);
                window.focus_prev()
            })
            .capture_any_mouse_down(|_, window, _| {
                set_keyboard(false);
                window.refresh();
            })
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                if this.drag.as_ref().is_some_and(|d| d.active) {
                    if e.pressed_button == Some(MouseButton::Left) {
                        this.drag_to(f32::from(e.position.x));
                    } else if let Some(d) = this.drag.as_mut() {
                        d.active = false;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                // Let go: the divider stays where it was put.
                if let Some(d) = this.drag.as_mut() {
                    d.active = false;
                }
                cx.notify();
            }))
            .relative()
            .size_full().flex().flex_col().bg(c(WINDOW)).text_color(c(TEXT)).font_family(SANS).child(titlebar).child(body);
        if let Some(overlay) = overlay {
            window = window.child(overlay);
        }
        window
    }
}

/// Which program this is.
pub use orchard_flow::App as Kind;

fn options(app: Kind) -> Options {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(Into::into);
    // --launcher: the Orchard launcher (start an installed system) instead.
    let mut options = Options::new(app);
    // The installer's distro: its stamp (name, wordmark, release index). A debug
    // build also takes --index URL, --name NAME and --wordmark SVG, for development.
    if app == Kind::Installer {
        let stamp = orchard_runtime::stamp::read();
        if let Some(stamp) = &stamp {
            orchard_flow::set_system_name(&stamp.name);
            if let Some(wordmark) = &stamp.wordmark {
                let _ = assets::WORDMARK.set(wordmark.clone().into_bytes());
            }
            options.index = Some(stamp.index.clone());
        } else if cfg!(debug_assertions) {
            options.index = args.iter().position(|a| a == "--index").and_then(|i| args.get(i + 1)).cloned();
            if let Some(name) = args.iter().position(|a| a == "--name").and_then(|i| args.get(i + 1)) {
                orchard_flow::set_system_name(name);
            }
            if let Some(file) = args.iter().position(|a| a == "--wordmark").and_then(|i| args.get(i + 1)) {
                if let Ok(svg) = std::fs::read(file) {
                    let _ = assets::WORDMARK.set(svg);
                }
            }
        } else if !args.iter().any(|a| a == "--local") {
            eprintln!("This installer isn't stamped with a system to install.");
            std::process::exit(1);
        }
        let _ = STAMP.set(stamp);
    }
    options.data = value("--data");
    options.local = value("--local");
    options.companion = value("--companion");
    options.pongo = value("--pongo");
    // Device actions (palera1n, splitting, installing): always in a release
    // build; a debug build only with --live.
    options.live = args.iter().any(|a| a == "--live") || !cfg!(debug_assertions);
    options
}

/// The installer's stamp, for installing the launcher afterwards.
pub static STAMP: std::sync::OnceLock<Option<orchard_runtime::stamp::Stamp>> = std::sync::OnceLock::new();

/// Runs the window for this program until it quits.
pub fn run(app: Kind) {
    if std::env::args().any(|a| a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let latest = Latest::default();
    // --demo: a scripted device to click through every screen; no hardware.
    let demo = std::env::args().any(|a| a == "--demo");
    // Our own messages also in logs/installer-<time>.log (launcher-… for
    // Orchard): there's no terminal when started from the app menu.
    if !demo {
        orchard_runtime::session_log::start(if app == Kind::Launcher { "launcher" } else { "installer" });
    }
    let started = if demo {
        driver::start_demo(options(app), Bridge { latest: latest.clone(), last: String::new() })
    } else {
        driver::start(options(app), Bridge { latest: latest.clone(), last: String::new() })
    };
    let handle = match started {
        Ok(handle) => handle,
        Err(e) => {
            let name = if app == Kind::Launcher { "Orchard" } else { "The installer" };
            let text = format!("{name} can't start: {e}");
            eprintln!("{text}");
            // Started from the Dock or a menu there is no terminal: say it on screen.
            if cfg!(target_os = "macos") {
                let quoted = text.replace('\\', "\\\\").replace('"', "\\\"");
                let _ = std::process::Command::new("osascript")
                    .args(["-e", &format!("display alert \"{name} can't start\" message \"{quoted}\" as critical")])
                    .status();
            } else {
                let _ = std::process::Command::new("notify-send").args(["-u", "critical", &format!("{name} can't start"), &text]).status();
            }
            std::process::exit(1);
        }
    };
    let _ = EXECUTOR.set(handle.executor.clone());
    let launcher = app == Kind::Launcher;
    let release = if launcher { None } else { handle.executor.release().ok() };
    // Development without a stamp: the name from the release.
    if let Some(rel) = &release {
        orchard_flow::set_system_name(&rel.name);
    }
    let title: SharedString = if launcher { "Orchard".into() } else { format!("Orchard Installer — {}", orchard_flow::system_name()).into() };
    let username: SharedString = release.and_then(|r| r.account_user).unwrap_or_else(|| "user".into()).into();
    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        let fonts = assets::FONTS.iter().map(|f| std::borrow::Cow::Borrowed(*f)).collect();
        cx.text_system().add_fonts(fonts).expect("fonts");
        input::bind_keys(cx);
        cx.bind_keys([
            gpui::KeyBinding::new("tab", FocusNext, None),
            gpui::KeyBinding::new("shift-tab", FocusPrevious, None),
            gpui::KeyBinding::new("escape", CloseDialog, None),
        ]);
        // The app's name in its menu: "Install <system>" or "Orchard".
        let name: SharedString = if launcher { "Orchard".into() } else { title.split(" — ").nth(1).map(|d| format!("Install {d}")).unwrap_or_else(|| "Orchard Installer".into()).into() };
        if cfg!(target_os = "macos") {
            cx.bind_keys([
                gpui::KeyBinding::new("cmd-h", Hide, None),
                gpui::KeyBinding::new("cmd-alt-h", HideOthers, None),
                gpui::KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.set_menus(vec![gpui::Menu {
                name: name.clone(),
                items: vec![
                    gpui::MenuItem::action(format!("Hide {name}"), Hide),
                    gpui::MenuItem::action("Hide Others", HideOthers),
                    gpui::MenuItem::action("Show All", ShowAll),
                    gpui::MenuItem::separator(),
                    gpui::MenuItem::action(format!("Quit {name}"), Quit),
                ],
            }]);
        } else {
            // Hiding is the window manager's on Linux (e.g. Super+H in GNOME).
            cx.bind_keys([gpui::KeyBinding::new("ctrl-q", Quit, None)]);
        }
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        let window = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(16.))),
            }),
            // The designs are 960 × 640; the window stays that size.
            is_resizable: false,
            ..Default::default()
        };
        let handle_window = cx.open_window(window, |window, cx| {
            let view = cx.new(|cx| {
                // Pick up the driver's newest flow; redraw for the spinner.
                let mut setup_shown = false;
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(Duration::from_millis(33)).await;
                    let next = latest.0.lock().unwrap().take();
                    // The launcher's setup finished: show it on 3.5.
                    let setup = !setup_shown && LAUNCHER_SETUP.lock().unwrap().is_some();
                    setup_shown |= setup;
                    let alive = this.update(cx, |installer: &mut Installer, cx| {
                        if let Some(flow) = next {
                            installer.flow = flow;
                            cx.notify();
                        } else if setup {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                })
                .detach();
                Installer {
                    app: if launcher { orchard_flow::App::Launcher } else { orchard_flow::App::Installer },
                    flow: Flow::new(),
                    events: handle.events.clone(),
                    started: Instant::now(),
                    title: title.clone(),
                    shown: None,
                    rows: Tween::new(4., 280.),
                    details_open: false,
                    details: Tween::new(0., 220.),
                    details_scroll: gpui::ScrollHandle::new(),
                    details_seen: std::cell::Cell::new(0),
                    details_info: false,
                    copied_at: None,
                    inputs: None,
                    username: username.clone(),
                    layout: None,
                    since: Instant::now(),
                    drag: None,
                    trouble: false,
                    ssh_info: false,
                    ssh_chosen: false,
                    progress: Tween::new(0., 900.),
                    root_focus: cx.focus_handle(),
                    slider_focus: cx.focus_handle().tab_stop(true).tab_index(0),
                    slider_ring: false,
                    refocus: false,
                    focus_password: false,
                    dialog_was: false,
                    dialog_focus: cx.focus_handle().tab_stop(true).tab_index(0),
                    focus_before_dialog: None,
                    dialog_root: cx.focus_handle(),
                    dialog_closed_at: None,
                    dialog_kind: None,
                    dialog_fade: Tween::new(0., 150.),
                    last_frame: Instant::now(),
                    install_started: None,
                    finish_hold: None,
                    install_took: None,
                    erase_shown: None,
                    split_erase_shown: None,
                    quitting: None,
                    companion_lost_at: None,
                }
            });
            let root = view.read(cx).root_focus.clone();
            window.focus(&root);
            // The close button is a quit request (one window, one app).
            let closing = view.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                let _ = closing.update(cx, |installer, cx| installer.request_quit(cx));
                false
            });
            view
        })
        .expect("window");
        cx.on_action(move |_: &Quit, cx| {
            let _ = handle_window.update(cx, |installer, _, cx| installer.request_quit(cx));
        });
        cx.activate(true);
    });
}
