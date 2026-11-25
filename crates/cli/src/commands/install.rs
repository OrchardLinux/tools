//! `orchard2 install` and `orchard2 launch`: the graphical installer's flow
//! and runtime (crates/flow, crates/runtime) in the terminal, drawn as the T
//! mockups in installer-app.pen. Resizing iPadOS is opt-in (--split): without
//! it the system goes into the existing split. Every question has a flag;
//! without a terminal nothing is asked and a missing answer ends the run
//! (exit 5) naming the flag. With --json each step is one JSON object per line
//! on stdout; the words for people go to the terminal.
use super::term::{bar, gradient, s, Span, Term, BLUE, FG, GREEN, MUTED, RED, SUB, TITLE, TRACK, YELLOW};
use orchard_flow::{Again, App, Event, Flow, Layout, Link, Sequence, State, MIN_OMARCHY_GB};
use orchard_runtime::driver::{self, Observer, Options};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Outcomes, as the exit status.
#[derive(Clone, Copy)]
enum Exit {
    Done = 0,
    Usage = 2,
    NoDevice = 3,
    Unsupported = 4,
    NeedsInput = 5,
    Failed = 6,
    Network = 7,
    Interrupted = 8,
}

#[derive(clap::Args)]
pub struct InstallArgs {
    /// The distribution: a name (omarchy-a), its release index URL or a local
    /// index.toml; in a distribution checkout the default is its last build
    pub source: Option<String>,
    /// The release index to install from (instead of SOURCE)
    #[arg(long)]
    pub index: Option<String>,
    /// Resize iPadOS first (opt-in): the system's space in GB, or leave the
    /// number out to see the storage and choose. Without --split the system
    /// goes into the existing split.
    #[arg(long, num_args = 0..=1, value_name = "GB")]
    pub split: Option<Option<u32>>,
    /// Approve erasing the partitions after iPadOS
    #[arg(long)]
    pub yes: bool,
    /// Allow SSH from this computer (import its keys)
    #[arg(long)]
    pub ssh: bool,
    /// Don't allow SSH and don't ask
    #[arg(long, conflicts_with = "ssh")]
    pub no_ssh: bool,
    /// The account's password: a file, or - for the first line of stdin; asked when not given
    #[arg(long)]
    pub password_file: Option<PathBuf>,
    /// The lock screen PIN (6 digits): a file, or - for stdin; asked when not given
    #[arg(long)]
    pub pin_file: Option<PathBuf>,
    /// Seconds to wait for each step the person does (DFU mode, a cable); 0 waits for ever
    #[arg(long, default_value = "0")]
    pub wait: u64,
    /// Write the full run log here (also the runtime's own messages)
    #[arg(long)]
    pub log: Option<PathBuf>,
    /// A scripted device instead of USB (development, tests)
    #[arg(long, hide = true)]
    pub demo: bool,
}

#[derive(clap::Args)]
pub struct LaunchArgs {
    /// Only list the installed systems
    #[arg(long, conflicts_with = "entry")]
    pub list: bool,
    /// Start exactly this boot entry
    #[arg(long)]
    pub entry: Option<String>,
    /// Seconds to wait for DFU mode; 0 waits for ever
    #[arg(long, default_value = "0")]
    pub wait: u64,
    /// Write the full run log here (also the runtime's own messages)
    #[arg(long)]
    pub log: Option<PathBuf>,
    /// A scripted device instead of USB (development, tests)
    #[arg(long, hide = true)]
    pub demo: bool,
}

/// The distribution's release index for SOURCE: a URL or file as it is, else
/// a name on OrchardLinux (its latest release).
fn index_for(source: &str) -> String {
    if source.contains("://") {
        return source.to_owned();
    }
    let path = std::path::Path::new(source);
    if path.is_file() {
        let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        return format!("file://{}", absolute.display());
    }
    format!("https://github.com/OrchardLinux/{source}/releases/latest/download/index.toml")
}

/// In a distribution checkout, the release its last `build` made.
fn last_build() -> Option<String> {
    let here = std::env::current_dir().ok()?;
    let (checkout, _) = orchard_build::project::find_release(&here).ok()??;
    let index = orchard_build::distro::latest(&checkout).ok()?;
    Some(format!("file://{}", index.display()))
}

/// Ctrl-C presses since the run started.
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

/// Ctrl-C at a question (reading stdin, which Rust restarts after a signal)
/// or a third Ctrl-C anywhere ends the run at once, terminal restored.
extern "C" fn on_interrupt(_: libc::c_int) {
    let n = INTERRUPTS.fetch_add(1, Ordering::SeqCst) + 1;
    if super::term::ASKING.load(Ordering::SeqCst) || n >= 3 {
        super::term::restore_on_signal();
        // SAFETY: write and _exit are async-signal-safe.
        unsafe {
            libc::write(super::term::TTY_FD.load(Ordering::SeqCst), b"\x1b[0m\n".as_ptr().cast(), 5);
            libc::_exit(Exit::Interrupted as i32);
        }
    }
}

fn catch_interrupts() {
    // SAFETY: installs a handler that only uses async-signal-safe calls.
    unsafe {
        libc::signal(libc::SIGINT, on_interrupt as *const () as libc::sighandler_t);
    }
}

/// The run's words (the terminal) and events (--json on stdout).
struct Out {
    term: Term,
    json: bool,
}

impl Out {
    fn event(&self, value: Value) {
        if self.json {
            println!("{value}");
        }
    }
    fn ok(&mut self, text: impl Into<String>) {
        self.term.line(vec![s("✓ ", GREEN), s(text, FG)]);
    }
    fn ok_note(&mut self, text: impl Into<String>, note: impl Into<String>) {
        self.term.line(vec![s("✓ ", GREEN), s(text, FG), s(format!("  {}", note.into()), MUTED)]);
    }
    fn warn(&mut self, text: impl Into<String>) {
        self.term.line(vec![s("! ", YELLOW), s(text, YELLOW)]);
    }
    fn fail(&mut self, text: impl Into<String>) {
        self.term.line(vec![s("✕ ", RED), s(text, RED)]);
    }
    fn heading(&mut self, text: impl Into<String>, note: Option<String>) {
        let mut l = vec![s(text, TITLE)];
        if let Some(n) = note {
            l.push(s(format!("  {n}"), MUTED));
        }
        self.term.line(l);
    }
    fn sub(&mut self, text: impl Into<String>) {
        self.term.line(vec![s(format!("  {}", text.into()), SUB)]);
    }
    fn muted(&mut self, text: impl Into<String>) {
        self.term.line(vec![s(format!("  {}", text.into()), MUTED)]);
    }
}

/// A secret from FILE, or the next line of stdin for "-".
fn read_secret(path: &PathBuf) -> Result<String, String> {
    let text = if path.as_os_str() == "-" {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
        line
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?
    };
    Ok(text.lines().next().unwrap_or("").to_owned())
}

/// The DFU steps: words with the buttons in blue, and how long each takes.
fn steps(sequence: Sequence) -> Vec<(Vec<Span>, &'static str)> {
    let t = |x: &str| s(x, FG);
    // The buttons in the app's colours: Power pink, Volume Up teal, others blue.
    let k = |x: &str| s(x, match x { "Power" => 0xbb9af7, "Volume Up" => 0x73daca, _ => BLUE });
    let mut out = vec![(vec![t("Connect it to this computer with a USB cable")], "")];
    out.extend(match sequence {
        Sequence::HomePower => vec![
            (vec![t("Hold "), k("Home"), t(" and "), k("Power"), t(" together")], "~10 s"),
            (vec![t("Let go of "), k("Power"), t(", keep holding "), k("Home")], "~10 s"),
        ],
        Sequence::VolumeSequence => vec![
            (vec![t("Press "), k("Volume Up"), t(", then "), k("Volume Down")], ""),
            (vec![t("Hold "), k("Power"), t(" until the screen goes black")], "~10 s"),
            (vec![t("Keep holding "), k("Power"), t(", also hold "), k("Volume Down")], "~5 s"),
            (vec![t("Let go of "), k("Power"), t(", keep holding "), k("Volume Down")], "~10 s"),
        ],
        Sequence::VolumeDownSide => vec![
            (vec![t("Hold "), k("Volume Down"), t(" and "), k("Power"), t(" together")], "~10 s"),
            (vec![t("Let go of "), k("Power"), t(", keep holding "), k("Volume Down")], "~10 s"),
        ],
    });
    out.push((vec![t("Let go when this changes")], ""));
    out
}

const TIPS: [(&str, &str); 4] = [
    ("Try a different cable, port or hub", "A USB-A cable through a hub can work where USB-C doesn't, or the other way round."),
    ("Quit other apps that use USB", "Browsers such as Chrome, Brave and Edge, and apps such as ChatGPT, access USB devices in the background."),
    ("Try another computer", "A Mac instead of Linux, or the other way round, can behave differently over USB."),
    ("Vary the timing", "If the Apple logo appears or nothing changes, hold each step a little shorter or longer."),
];

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|(t, _)| t.as_str()).collect()
}

/// The DFU block (T.1, T.5): heading, the numbered steps, the live wait.
fn dfu_block(out: &mut Out, title: &str, sub: Option<&str>, sequence: Sequence, device: Option<&str>, wait: u64) {
    let list = steps(sequence);
    out.event(json!({"event": "needs", "action": "dfu", "device": device, "steps": list.iter().map(|(p, d)| format!("{}{}", plain(p), if d.is_empty() { String::new() } else { format!(" ({d})") })).collect::<Vec<_>>(), "wait_s": wait}));
    out.term.live(None);
    out.term.blank();
    out.heading(title, None);
    if let Some(sub) = sub {
        out.sub(sub);
    }
    for (i, (parts, duration)) in list.iter().enumerate() {
        let width: usize = parts.iter().map(|(t, _)| t.chars().count()).sum();
        let mut l = vec![s(format!("  {}  ", i + 1), MUTED)];
        l.extend(parts.iter().cloned());
        if !duration.is_empty() {
            l.push(s(format!("{}{duration}", " ".repeat(48usize.saturating_sub(width))), MUTED));
        }
        out.term.line(l);
    }
    out.term.blank();
    if out.term.tty && device.is_none() {
        out.term.line(vec![s("  Other buttons? Press ", MUTED), s("n", BLUE), s(" no Home button · ", MUTED), s("7", BLUE), s(" iPhone 7 · ", MUTED), s("t", BLUE), s(" tips", MUTED)]);
    }
    out.term.live_restart(vec![s("Waiting for a device in DFU mode", SUB)]);
    out.term.keys(true);
}

/// The layout as a row: iPadOS's used and free part, then what follows it.
fn layout_bar(layout: &Layout, ipados_gb: u32, system_gb: Option<u32>, width: usize) -> Vec<Span> {
    let cells = |gb: u32| ((gb as f32 / layout.total_gb.max(1) as f32) * width as f32).round() as usize;
    let used = cells(layout.ipados_used_gb).clamp(1, width);
    let ipados = cells(ipados_gb).clamp(used, width);
    let mut row = vec![s("▓".repeat(used), SUB), s("░".repeat(ipados - used), MUTED)];
    let rest = width - ipados;
    match system_gb {
        Some(_) => {
            let mut g = gradient(&"█".repeat(rest.max(1)));
            g.truncate(rest);
            row.extend(g);
        }
        None => row.push(s("▒".repeat(rest), TRACK)),
    }
    row
}

/// T.3: the storage now and after.
fn split_overview(out: &mut Out, layout: &Layout, gb: u32, name: &str) {
    out.term.blank();
    out.heading("Split the storage", Some(format!("{} GB", layout.total_gb)));
    out.sub("Your apps and data stay where they are.");
    out.term.blank();
    let mut note = orchard_flow::for_device(&format!("iPadOS {} GB, {} GB used", layout.ipados_gb, layout.ipados_used_gb)).to_owned();
    for p in &layout.outside {
        note.push_str(&format!(" · {} {} GB", p.name, p.gb));
    }
    let mut now = vec![s("  Now    ", SUB)];
    now.extend(layout_bar(layout, layout.ipados_gb, None, 40));
    now.push(s(format!("  {note}"), MUTED));
    out.term.line(now);
    after_row(out, layout, gb, name);
    out.term.blank();
}

fn after_row(out: &mut Out, layout: &Layout, gb: u32, name: &str) {
    let ipados = layout.total_gb.saturating_sub(gb);
    let mut after = vec![s("  After  ", SUB)];
    after.extend(layout_bar(layout, ipados, Some(gb), 40));
    after.push(s(orchard_flow::for_device(&format!("  iPadOS {ipados} GB · {name} {gb} GB")).to_owned(), MUTED));
    out.term.line(after);
}

/// The red list of what gets erased, and the consent.
fn erase_consent(out: &mut Out, layout: &Layout, yes: bool, title: &str, intro: &str) -> bool {
    out.term.blank();
    out.heading(title, None);
    out.sub(intro);
    for p in &layout.outside {
        out.term.line(vec![s("  ✕ ", RED), s(format!("{:<16}", p.name), FG), s(format!("{:<12}", p.filesystem), SUB), s(format!("{} GB", p.gb), SUB)]);
    }
    if yes {
        out.event(json!({"event": "approved", "by": "--yes"}));
        out.muted("Approved with --yes.");
        return true;
    }
    match out.term.ask(vec![s("  I have a backup and approve these changes. Type ", FG), s("yes", BLUE), s(": ", FG)], false) {
        Some(t) if t == "yes" => true,
        Some(_) => {
            out.fail("Not approved; nothing was changed");
            false
        }
        None => {
            out.event(json!({"event": "needs", "action": "input", "flag": "--yes"}));
            out.fail("Erasing needs approval: run again with --yes");
            false
        }
    }
}

/// The flow's state name, as the GUI's log and --json say it.
fn state_name(state: &State) -> String {
    let name: String = format!("{state:?}").chars().take_while(|c| c.is_alphanumeric()).collect();
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// Passes the driver's flow to the command's thread.
struct Relay(mpsc::Sender<Flow>);

impl Observer for Relay {
    fn changed(&mut self, flow: &Flow) {
        let _ = self.0.send(flow.clone());
    }
    fn problem(&mut self, message: &str) {
        // stderr is the log now (Term::new).
        eprintln!("problem: {message}");
    }
    fn quit(&mut self) {}
}

fn start(app: App, index: Option<String>, demo: bool) -> Result<(driver::Handle, Receiver<Flow>), String> {
    let (tx, rx) = mpsc::channel();
    let mut options = Options::new(app);
    options.index = index;
    options.live = true;
    let started = if demo { driver::start_demo(options, Relay(tx)) } else { driver::start(options, Relay(tx)) };
    started.map(|h| (h, rx)).map_err(|e| e.to_string())
}

/// Ends the run: the last event and the exit status.
fn finish(out: &mut Out, exit: Exit, code: &str, message: &str) -> ExitCode {
    out.term.live(None);
    out.term.keys(false);
    if !matches!(exit, Exit::Done) {
        let log = out.term.log_path.as_ref().map(|p| p.display().to_string());
        out.event(json!({"event": "error", "code": code, "message": message, "exit": exit as u8, "log": log}));
        if let Some(log) = log {
            out.muted(format!("Log: {log}"));
        }
    }
    ExitCode::from(exit as u8)
}

/// A failed download (after the runtime's own retries), if any.
fn failed_asset(flow: &Flow) -> Option<String> {
    orchard_flow::assets::ALL.iter().find_map(|&a| match flow.assets.get(a) {
        orchard_flow::assets::AssetState::Failed(why) => Some(format!("{a:?}: {why}")),
        _ => None,
    })
}

/// Waiting for DFU mode. The steps show once USB has had a moment to report
/// a device that is in DFU mode already.
struct Waiting {
    since: Instant,
    shown: bool,
    sequence: Option<Sequence>,
}

impl Waiting {
    fn new() -> Waiting {
        Waiting { since: Instant::now(), shown: false, sequence: None }
    }
}

/// Keys while waiting (T.1): n and 7 show other buttons' steps, t the tips.
fn waiting_keys(out: &mut Out, waiting: &mut Waiting, wait: u64) {
    while let Some(k) = out.term.key() {
        let sequence = match k {
            'n' | 'N' => Some(Sequence::VolumeSequence),
            '7' => Some(Sequence::VolumeDownSide),
            'h' | 'H' => Some(Sequence::HomePower),
            't' | 'T' => {
                out.term.blank();
                out.heading("Having trouble?", None);
                for (i, (head, body)) in TIPS.iter().enumerate() {
                    out.term.line(vec![s(format!("  {}  ", i + 1), MUTED), s(*head, FG)]);
                    out.muted(format!("   {body}"));
                }
                None
            }
            _ => None,
        };
        if let Some(seq) = sequence.filter(|q| Some(*q) != waiting.sequence) {
            waiting.sequence = Some(seq);
            let title = match seq {
                Sequence::VolumeSequence => "Put your device in DFU mode · no Home button",
                Sequence::VolumeDownSide => "Put your device in DFU mode · iPhone 7 and 7 Plus",
                Sequence::HomePower => "Put your device in DFU mode · Home button",
            };
            dfu_block(out, title, None, seq, None, wait);
        }
    }
}

/// While nothing changes: the steps after a moment, the --wait limit.
fn idle(out: &mut Out, last: &str, waiting: &mut Waiting, wait: u64) -> Option<ExitCode> {
    if last == "waiting-for-dfu" && !waiting.shown && waiting.since.elapsed() > Duration::from_millis(1500) {
        waiting.shown = true;
        waiting.sequence = Some(Sequence::HomePower);
        dfu_block(out, "Put your device in DFU mode", None, Sequence::HomePower, None, wait);
    }
    if wait > 0 && waiting.shown && waiting.since.elapsed() > Duration::from_secs(wait) && (last == "waiting-for-dfu" || last == "dfu-again") {
        out.fail("No device showed up in DFU mode in time");
        return Some(finish(out, Exit::NoDevice, "timeout", "no device in DFU mode"));
    }
    None
}

/// What the installer's and the launcher's loops share for each new flow: a
/// failed download, other devices on USB, the step's first sighting, the
/// stalled start, several devices in DFU and waiting for DFU mode.
struct Shared {
    app: App,
    /// The step shown last (its state name).
    last: String,
    waiting: Waiting,
    stalled: bool,
    /// The devices last listed for "More than one device connected".
    others_said: Vec<orchard_flow::Other>,
    /// The exploit's attempt and palera1n's stage last shown.
    attempt_said: u32,
    stage_said: Option<&'static str>,
}

enum Next {
    /// Nothing else to do for this flow (the other devices are listed).
    Skip,
    Exit(ExitCode),
    /// Go on with the step; `first` when it shows for the first time.
    Go { first: bool },
}

impl Shared {
    fn new(app: App) -> Shared {
        Shared { app, last: String::new(), waiting: Waiting::new(), stalled: false, others_said: Vec::new(), attempt_said: 1, stage_said: None }
    }

    /// "Unplug the ones you don't want to install on." and the launcher's own.
    fn unplug_hint(&self) -> &'static str {
        match self.app {
            App::Installer => "Unplug the ones you don't want to install on.",
            App::Launcher => "Unplug the ones you don't want to start.",
        }
    }

    /// "Starting your iPad", with the exploit's attempt and palera1n's stage,
    /// while the device is being started (both apps).
    fn starting(&mut self, out: &mut Out, flow: &Flow, first: bool) {
        if first {
            out.term.live_restart(vec![s(orchard_flow::for_device("Starting your iPad"), FG), s("  about a minute", MUTED)]);
        }
        if flow.start_attempt > self.attempt_said {
            self.attempt_said = flow.start_attempt;
            out.event(json!({"event": "retry", "reason": "exploit", "attempt": flow.start_attempt}));
        }
        // The stage from palera1n's log, beside the attempt.
        let stage = orchard_flow::start_stage(&flow.start_log);
        if stage != self.stage_said || first {
            if stage != self.stage_said {
                if let Some(st) = stage {
                    out.event(json!({"event": "stage", "stage": st, "attempt": flow.start_attempt.max(1)}));
                }
            }
            self.stage_said = stage;
            let mut l = vec![s(orchard_flow::for_device("Starting your iPad"), FG)];
            if flow.start_attempt > 1 {
                l.push(s(format!("  attempt {}", flow.start_attempt), MUTED));
            }
            l.push(s(stage.map(|st| format!("  {}", orchard_flow::for_device(st))).unwrap_or_else(|| "  about a minute".into()), MUTED));
            out.term.live(Some(l));
        }
    }

    /// DFU mode again (3.1), saying why; `name` is the system's name.
    fn dfu_again(&mut self, out: &mut Out, flow: &Flow, device: &orchard_flow::Device, why: Again, name: &str, wait: u64) {
        out.term.live(None);
        if matches!(why, Again::Retry | Again::RetryPrepare) {
            out.event(json!({"event": "retry", "reason": "start-failed"}));
        }
        let (title, sub): (String, String) = match why {
            Again::Install => (format!("Next, install {name}"), orchard_flow::for_device("Put your iPad in DFU mode again, the same way as before.").to_owned()),
            Again::Retry | Again::RetryPrepare => (
                orchard_flow::for_device("Starting your iPad didn't work").to_owned(),
                format!("Put it in DFU mode again, the same way as before. {} then tries again.", orchard_flow::app_name(self.app)),
            ),
            Again::Split => (
                orchard_flow::for_device("Put your iPad in DFU mode again").to_owned(),
                orchard_flow::for_device(&format!("The installer then starts your iPad to split the storage between iPadOS and {name}.")).to_owned(),
            ),
            Again::CheckSplit => (
                orchard_flow::for_device("Put your iPad in DFU mode again").to_owned(),
                orchard_flow::for_device("Your iPad restarted while the storage was being split. In DFU mode the installer can check it.").to_owned(),
            ),
        };
        if let Some(p) = flow.start_problem.as_deref().filter(|_| matches!(why, Again::Retry | Again::RetryPrepare)) {
            out.muted(orchard_flow::for_device(p));
        }
        self.attempt_said = 1;
        self.stage_said = None;
        dfu_block(out, &title, Some(&sub), device.sequence, Some(&device.name), wait);
        self.waiting = Waiting { since: Instant::now(), shown: true, sequence: Some(device.sequence) };
    }

    fn see(&mut self, out: &mut Out, flow: &Flow) -> Next {
        if let Some(why) = failed_asset(flow) {
            out.term.live(None);
            out.fail("A download didn't work. Check the internet connection, then try again.");
            out.muted(&why);
            return Next::Exit(finish(out, Exit::Network, "download", &why));
        }
        // Other Apple devices on USB: nothing goes on until they're gone, then
        // the current step is shown again.
        if flow.refusing() {
            if flow.others != self.others_said {
                self.others_said = flow.others.clone();
                out.term.live(None);
                out.event(json!({"event": "needs", "action": "disconnect-others", "devices": flow.others.iter().map(|o| json!({"name": o.name, "mode": o.mode})).collect::<Vec<_>>()}));
                out.warn(orchard_flow::SEVERAL_TITLE);
                for other in &flow.others {
                    out.sub(format!("{} · {}", other.name, other.mode));
                }
                out.sub(self.unplug_hint());
                out.term.live_restart(vec![s("Waiting for one device", SUB)]);
            }
            return Next::Skip;
        }
        if !self.others_said.is_empty() {
            self.others_said.clear();
            self.last.clear();
        }
        let name_now = state_name(&flow.state);
        let first = name_now != self.last;
        if first {
            out.event(json!({"event": "state", "state": name_now}));
            if name_now != "waiting-for-dfu" && name_now != "dfu-again" {
                out.term.keys(false);
            }
            self.last = name_now;
        }
        if flow.boot_stalled && !self.stalled {
            self.stalled = true;
            out.event(json!({"event": "needs", "action": "replug"}));
            out.warn(orchard_flow::for_device("Reconnect the cable at your iPad"));
            out.sub(orchard_flow::for_device("Starting it has stalled. Unplugging the cable at your iPad and plugging it back in usually fixes this. It continues by itself."));
        } else if !flow.boot_stalled {
            self.stalled = false;
        }
        match &flow.state {
            State::WaitingForDfu { .. } if first => self.waiting = Waiting::new(),
            State::Unsupported { device } => {
                out.term.live(None);
                out.fail(format!("This {} isn't supported yet", device.name));
                out.muted("Supported devices: https://github.com/OrchardLinux/orchard/blob/main/docs/compatibility.md");
                return Next::Exit(finish(out, Exit::Unsupported, "unsupported", &device.name));
            }
            State::SeveralInDfu { .. } if first => {
                out.term.live(None);
                out.warn(orchard_flow::SEVERAL_TITLE);
                out.sub(self.unplug_hint());
                out.term.live_restart(vec![s("Waiting for one device in DFU mode", SUB)]);
            }
            _ => {}
        }
        Next::Go { first }
    }
}

pub fn install(json: bool, args: &InstallArgs) -> ExitCode {
    let mut out = Out { term: Term::new(args.log.as_deref()), json };
    let given = args.index.as_deref().or(args.source.as_deref()).map(index_for);
    let Some(index) = given.or_else(last_build) else {
        out.fail("Which system? orchard2 install omarchy-a (or --index URL)");
        return finish(&mut out, Exit::Usage, "usage", "no distribution given");
    };
    if let Some(Some(gb)) = args.split {
        if gb < MIN_OMARCHY_GB {
            out.fail(format!("--split needs at least {MIN_OMARCHY_GB} GB"));
            return finish(&mut out, Exit::Usage, "usage", "split too small");
        }
    }
    let secret = |p: &Option<PathBuf>| p.as_ref().map(read_secret).transpose();
    let (password, pin) = match (secret(&args.password_file), secret(&args.pin_file)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            out.fail(&e);
            return finish(&mut out, Exit::Usage, "usage", &e);
        }
    };
    if password.as_ref().is_some_and(|p| p.chars().count() < 8) {
        out.fail("The password needs 8 or more characters");
        return finish(&mut out, Exit::Usage, "usage", "password too short");
    }
    if pin.as_ref().is_some_and(|p| p.len() != 6 || !p.bytes().all(|b| b.is_ascii_digit())) {
        out.fail("The lock screen PIN needs exactly 6 digits");
        return finish(&mut out, Exit::Usage, "usage", "pin not 6 digits");
    }
    catch_interrupts();
    out.term.live_restart(vec![s("Reading the release", SUB)]);
    let (handle, rx) = match start(App::Installer, Some(index), args.demo) {
        Ok(h) => h,
        Err(e) => {
            out.fail(format!("Couldn't start: {e}"));
            return finish(&mut out, Exit::Failed, "start", &e);
        }
    };
    let release = match handle.executor.release() {
        Ok(r) => r,
        Err(e) => {
            out.fail(format!("Couldn't read the release: {e}"));
            return finish(&mut out, Exit::Network, "release", &e.to_string());
        }
    };
    orchard_flow::set_system_name(&release.name);
    let name = release.name.clone();
    let user = release.account_user.clone().unwrap_or_else(|| "user".into());
    out.term.live(None);
    out.event(json!({"event": "release", "name": name, "version": release.version}));
    out.term.line(vec![s("Orchard Installer", TITLE), s(" · ", MUTED), s(format!("{name} {}", release.version), BLUE)]);
    let send = |e: Event| {
        let _ = handle.events.send(e);
    };
    let mut shared = Shared::new(App::Installer);
    let mut found = false;
    let mut split_done = false;
    let mut erase_done = false;
    let mut account_sent = false;
    let mut account_continued = false;
    let mut split_layout: Option<(Layout, u32)> = None;
    let mut installed_at: Option<Instant> = None;
    let mut reported = 0u8;
    let mut step_said: Option<String> = None;
    let mut ssh_chosen = false;
    let mut slow_said = false;
    let mut lost_said = false;
    let mut ctrl_c_said = false;
    loop {
        out.term.tick();
        waiting_keys(&mut out, &mut shared.waiting, args.wait);
        let flow = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(flow) => Some(flow),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return finish(&mut out, Exit::Failed, "driver", "the installer stopped"),
        };
        let interrupts = INTERRUPTS.load(Ordering::SeqCst);
        let busy = shared.last == "splitting" || shared.last == "installing";
        if interrupts > 0 {
            if busy && interrupts == 1 {
                if !ctrl_c_said {
                    ctrl_c_said = true;
                    out.warn(orchard_flow::for_device("Wait until it's done: your iPad is working on it. Ctrl-C again quits anyway."));
                }
            } else {
                // Leaves a temporary iPadOS by restarting the iPad, as the window does.
                send(Event::Quit);
                std::thread::sleep(Duration::from_secs(2));
                return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
            }
        }
        let Some(flow) = flow else {
            if let Some(code) = idle(&mut out, &shared.last, &mut shared.waiting, args.wait) {
                return code;
            }
            continue;
        };
        let first = match shared.see(&mut out, &flow) {
            Next::Skip => continue,
            Next::Exit(code) => return code,
            Next::Go { first } => first,
        };
        match &flow.state {
            State::WaitingForDfu { .. } | State::SeveralInDfu { .. } | State::Unsupported { .. } => {}
            State::UsbAccessNeeded => {
                if first {
                    out.warn("This computer needs to allow USB access for the device. Approving it asks for your password.");
                    send(Event::Continue);
                }
            }
            State::Found { device, .. } => {
                if !found {
                    found = true;
                    shared.waiting.shown = false;
                    out.term.live(None);
                    out.ok(format!("{} in DFU mode", device.name));
                    // Resizing is opt-in: --split, or yes to the question.
                    let resize = args.split.is_some() || {
                        let q = vec![s(orchard_flow::for_device("  Resize iPadOS first? "), FG), s(format!("Without resizing, {name} goes into the existing split. "), MUTED), s("[y/N] ", MUTED)];
                        matches!(out.term.ask(q, false).as_deref().map(str::trim), Some("y" | "Y" | "yes"))
                    };
                    if resize {
                        send(Event::Continue);
                    } else {
                        // Into the existing split (the window's Skip).
                        send(Event::Skip);
                        send(Event::ConfirmSkip);
                    }
                }
            }
            State::WaitingForIpados { slow, .. } => {
                shared.starting(&mut out, &flow, first);
                if *slow && !slow_said {
                    slow_said = true;
                    out.warn(orchard_flow::for_device("Your iPad hasn't answered yet. If it shows the Lock Screen, unlock it."));
                }
            }
            State::ChooseSplit { layout, omarchy_gb, .. } => {
                if !split_done {
                    split_done = true;
                    out.term.live(None);
                    out.ok(orchard_flow::for_device("Your iPad is running iPadOS with the installer's app"));
                    let max = layout.max_omarchy_gb();
                    out.event(json!({"event": "split", "total_gb": layout.total_gb, "ipados_gb": layout.ipados_gb, "ipados_used_gb": layout.ipados_used_gb, "suggested_gb": omarchy_gb, "min_gb": MIN_OMARCHY_GB, "max_gb": max, "after_ipados": layout.outside.iter().map(|p| json!({"name": p.name, "filesystem": p.filesystem, "gb": p.gb})).collect::<Vec<_>>()}));
                    let shown = args.split.flatten().map(|g| g.min(max)).unwrap_or(*omarchy_gb);
                    split_overview(&mut out, layout, shown, &name);
                    if max < MIN_OMARCHY_GB {
                        out.fail(orchard_flow::for_device(&format!("{name} needs at least {MIN_OMARCHY_GB} GB, and iPadOS can spare {max} GB right now")).to_owned());
                        out.sub(orchard_flow::for_device("Delete some apps or files on your iPad, then try again."));
                        send(Event::Quit);
                        std::thread::sleep(Duration::from_secs(1));
                        return finish(&mut out, Exit::Failed, "no-room", "not enough free space");
                    }
                    let gb = match args.split.flatten() {
                        Some(_) => shown,
                        None => match out.term.ask(vec![s(format!("  Space for {name} in GB "), FG), s(format!("[{omarchy_gb}]"), BLUE), s(format!(" {MIN_OMARCHY_GB}–{max}: "), MUTED)], false) {
                            Some(t) if t.trim().is_empty() => *omarchy_gb,
                            Some(t) => match t.trim().parse::<u32>() {
                                Ok(gb) if (MIN_OMARCHY_GB..=max).contains(&gb) => {
                                    after_row(&mut out, layout, gb, &name);
                                    gb
                                }
                                _ => {
                                    out.fail(format!("Give a size from {MIN_OMARCHY_GB} to {max} GB"));
                                    send(Event::Quit);
                                    std::thread::sleep(Duration::from_secs(1));
                                    return finish(&mut out, Exit::Usage, "usage", "split out of range");
                                }
                            },
                            None => {
                                out.event(json!({"event": "needs", "action": "input", "flag": "--split GB", "suggested_gb": omarchy_gb}));
                                out.fail(format!("How much space for {name}? Run again with --split GB (suggested: {omarchy_gb})"));
                                send(Event::Quit);
                                std::thread::sleep(Duration::from_secs(1));
                                return finish(&mut out, Exit::NeedsInput, "needs-input", "--split GB");
                            }
                        },
                    };
                    if !layout.outside.is_empty() && !erase_consent(&mut out, layout, args.yes, "Before splitting", &orchard_flow::for_device(&format!("The partitions after iPadOS are erased, and {name} gets their space.")).to_owned()) {
                        send(Event::Quit);
                        std::thread::sleep(Duration::from_secs(1));
                        return finish(&mut out, Exit::NeedsInput, "needs-input", "--yes");
                    }
                    split_layout = Some((layout.clone(), gb));
                    send(Event::SetOmarchySize(gb));
                    send(Event::Approve(true));
                    send(Event::Continue);
                    if !layout.outside.is_empty() {
                        send(Event::Continue);
                    }
                }
            }
            State::Splitting { omarchy_gb, link, .. } => {
                if first {
                    let (grow, ipados) = match &split_layout {
                        Some((l, gb)) => (l.total_gb.saturating_sub(*gb) > l.ipados_gb, l.total_gb.saturating_sub(*gb)),
                        None => (false, 0),
                    };
                    out.term.blank();
                    out.heading("Splitting the storage", None);
                    out.sub(if grow { orchard_flow::for_device(&format!("iPadOS is growing to {ipados} GB. This takes a moment.")).to_owned() } else { orchard_flow::for_device(&format!("iPadOS is shrinking to {ipados} GB. This can take around 30 minutes.")).to_owned() });
                    out.term.blank();
                    out.warn(orchard_flow::for_device("Keep your iPad connected and leave it alone."));
                    out.sub("It may freeze or look idle for a long time. That doesn't mean it's finished or failed.");
                    out.term.blank();
                    out.term.line(vec![s("  ✓ ", GREEN), s("Check storage", MUTED)]);
                    out.term.live_restart(vec![s(if grow { orchard_flow::for_device("Grow iPadOS") } else { orchard_flow::for_device("Shrink iPadOS") }, FG), s(format!("  {name} gets {omarchy_gb} GB"), MUTED)]);
                }
                if matches!(link, Link::Lost) && !lost_said {
                    lost_said = true;
                    out.warn(orchard_flow::for_device("The connection to your iPad dropped. It keeps splitting; reconnecting…"));
                } else if matches!(link, Link::Up) {
                    lost_said = false;
                }
            }
            State::SplitDone { omarchy_gb, .. } => {
                if first {
                    let took = out.term.live_secs();
                    out.term.live(None);
                    out.term.line(vec![s("  ✓ ", GREEN), s(orchard_flow::for_device("Resize iPadOS"), MUTED), s(format!("  {}:{:02}", took / 60, took % 60), MUTED)]);
                    out.term.line(vec![s("  ✓ ", GREEN), s("Check the result", MUTED)]);
                    let ipados = split_layout.as_ref().map(|(l, _)| l.total_gb.saturating_sub(*omarchy_gb)).unwrap_or(0);
                    out.term.blank();
                    out.ok_note("The storage is split", orchard_flow::for_device(&format!("iPadOS {ipados} GB · {name} {omarchy_gb} GB")).to_owned());
                    send(Event::Continue);
                }
            }
            State::DfuAgain { device, why, .. } => {
                if first {
                    shared.dfu_again(&mut out, &flow, device, *why, &name, args.wait);
                }
            }
            State::NotPrepared { .. } => {
                out.term.live(None);
                out.fail(orchard_flow::for_device("Your iPad's storage isn't split yet"));
                out.sub(orchard_flow::for_device(&format!("{name} goes into storage split off from iPadOS. Run again and resize iPadOS first (answer y, or --split).")).to_owned());
                out.event(json!({"event": "needs", "action": "input", "flag": "--split"}));
                send(Event::Quit);
                std::thread::sleep(Duration::from_secs(1));
                return finish(&mut out, Exit::NeedsInput, "not-split", "--split");
            }
            State::CreateAccount { account, env, .. } => {
                if !account_sent {
                    account_sent = true;
                    out.term.live(None);
                    // In DFU mode again (or the first time, into the existing split).
                    if shared.waiting.shown || !found {
                        found = true;
                        out.ok(format!("{} in DFU mode", flow.state.device().map(|d| d.name.as_str()).unwrap_or("Your device")));
                        shared.waiting.shown = false;
                    }
                    let Some(account) = make_account(&mut out, &user, password.clone(), pin.clone(), args) else {
                        send(Event::Quit);
                        std::thread::sleep(Duration::from_secs(1));
                        return finish(&mut out, Exit::NeedsInput, "needs-input", "--password-file/--pin-file");
                    };
                    ssh_chosen = account.ssh;
                    send(Event::EditAccount(account));
                    if env.is_none() {
                        out.term.live_restart(vec![s(orchard_flow::for_device("Your iPad is getting ready"), SUB)]);
                    }
                }
                if env.is_some() && account.valid() && !account_continued {
                    account_continued = true;
                    send(Event::Continue);
                }
            }
            State::ConfirmErase { layout, .. } => {
                if !erase_done {
                    erase_done = true;
                    out.term.live(None);
                    if !erase_consent(&mut out, layout, args.yes, "Before installing", orchard_flow::for_device("Installing erases these partitions. iPadOS and its data stay as they are.")) {
                        send(Event::Quit);
                        std::thread::sleep(Duration::from_secs(1));
                        return finish(&mut out, Exit::NeedsInput, "needs-input", "--yes");
                    }
                    send(Event::Approve(true));
                    send(Event::Continue);
                }
            }
            State::Installing { percent, streamed, step, link, .. } => {
                if first {
                    installed_at.get_or_insert_with(Instant::now);
                    out.term.live(None);
                    out.term.blank();
                    out.heading(format!("Installing {name}"), Some(orchard_flow::for_device("Keep your iPad connected.").into()));
                    out.term.blank();
                }
                if matches!(link, Link::Lost) && !lost_said {
                    lost_said = true;
                    out.warn(orchard_flow::for_device("The connection to your iPad dropped. Reconnect the cable; it continues where it was."));
                } else if matches!(link, Link::Up) {
                    lost_said = false;
                }
                if !*streamed {
                    let secs = installed_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0).max(0.1);
                    let mb = release.root.bytes as f64 * *percent as f64 / 100.0 / 1e6;
                    let mut l = bar(*percent as usize * 32 / 100, 32);
                    l.push(s(format!("  {percent:>3}%"), FG));
                    l.push(s("  Writing the system", SUB));
                    if *percent > 0 {
                        l.push(s(format!("  {:.1} MB/s", mb / secs), MUTED));
                    }
                    out.term.live(Some(l));
                    if *percent >= reported.saturating_add(10) {
                        reported = *percent;
                        out.event(json!({"event": "progress", "percent": percent}));
                        if !out.term.tty {
                            out.sub(format!("Writing the system {percent}%"));
                        }
                    }
                } else if step != &step_said {
                    step_said = step.clone();
                    if let Some(st) = step {
                        out.event(json!({"event": "progress", "step": st}));
                        let mut l = bar(32, 32);
                        l.push(s("  100%", FG));
                        l.push(s("  Finishing up", SUB));
                        l.push(s(format!("  {st}"), MUTED));
                        out.term.live(Some(l));
                    }
                }
            }
            State::InstallFailed { .. } => {
                let why = flow.install_problem.clone().unwrap_or_else(|| "the install system stopped".into());
                out.term.live(None);
                out.term.blank();
                out.fail(format!("{name} couldn't be installed"));
                out.sub(orchard_flow::for_device("Something went wrong while writing it. iPadOS and your data weren't touched."));
                out.muted(&why);
                return finish(&mut out, Exit::Failed, "install-failed", &why);
            }
            State::Starting { .. } => {
                let took = installed_at.map(|t| t.elapsed().as_secs_f64());
                out.term.live(None);
                out.event(json!({"event": "done", "installed_in": took, "ssh": ssh_chosen.then(|| format!("{user}@172.16.42.1"))}));
                if installed_at.is_some() {
                    let mut l = bar(32, 32);
                    l.push(s("  100%", FG));
                    l.push(s("  Written and checked", SUB));
                    out.term.line(l);
                }
                out.term.blank();
                out.term.line(vec![s("✓ ", GREEN), s(format!("{name} is starting"), TITLE)]);
                if let Some(t) = took {
                    let mut l = vec![s("  ", FG)];
                    l.extend(gradient(&format!("Installed in {t:.1}s")));
                    out.term.line(l);
                }
                out.term.blank();
                out.heading("Starting it again later", None);
                out.sub(orchard_flow::for_device(&format!("After a restart your iPad starts iPadOS as usual. To start {name},")).to_owned());
                out.term.line(vec![s("  run ", SUB), s("orchard2 launch", BLUE), s(orchard_flow::for_device(" and put your iPad in DFU mode."), SUB)]);
                if ssh_chosen {
                    out.term.blank();
                    out.heading("Signing in from this computer", None);
                    out.term.line(vec![s(format!("  ssh {user}@172.16.42.1"), BLUE)]);
                }
                return finish(&mut out, Exit::Done, "done", "");
            }
            State::ChooseSystem { .. } | State::NothingInstalled { .. } => {}
        }
    }
}

/// The account from the flags or the prompts (T.6), checked as the window checks it.
fn make_account(out: &mut Out, user: &str, password: Option<String>, pin: Option<String>, args: &InstallArgs) -> Option<orchard_flow::Account> {
    out.term.blank();
    out.heading("Create your account", Some(orchard_flow::for_device("Your iPad is getting ready in the meantime.").into()));
    out.term.blank();
    out.term.line(vec![s(format!("  {:<19}", "Username"), SUB), s(user, FG)]);
    let password = secret_pair(out, "Password", "Repeat password", "--password-file", password, "password", |p| (p.chars().count() >= 8).then_some(()).ok_or("The password needs 8 or more characters"))?;
    let pin = secret_pair(out, "Lock screen PIN", "Repeat PIN", "--pin-file", pin, "PIN", |p| (p.len() == 6 && p.bytes().all(|b| b.is_ascii_digit())).then_some(()).ok_or("The PIN needs exactly 6 digits"))?;
    let ssh = if args.ssh {
        true
    } else if args.no_ssh || !out.term.tty {
        false
    } else {
        out.term.blank();
        let answer = out.term.ask(vec![s("  Allow SSH from this computer? ", FG), s("[y/N] ", MUTED)], false);
        let yes = matches!(answer.as_deref().map(str::trim), Some("y" | "Y" | "yes"));
        if yes {
            out.muted(format!("Adds this computer's keys: ssh {user}@172.16.42.1 over the USB cable, no password."));
        }
        yes
    };
    Some(orchard_flow::Account { username: user.to_owned(), password: password.clone(), password_again: password, pin: pin.clone(), pin_again: pin, ssh })
}

/// A password or PIN from its flag, or asked twice until both match and CHECK passes.
fn secret_pair(out: &mut Out, label: &str, again: &str, flag: &str, given: Option<String>, what: &str, check: impl Fn(&str) -> Result<(), &'static str>) -> Option<String> {
    if let Some(v) = given {
        out.term.line(vec![s(format!("  {label:<19}"), SUB), s(format!("from {flag}"), MUTED)]);
        return Some(v);
    }
    loop {
        let a = out.term.ask(vec![s(format!("  {label:<19}"), SUB)], true);
        let b = a.as_ref().and_then(|_| out.term.ask(vec![s(format!("  {again:<19}"), SUB)], true));
        match (a, b) {
            (Some(a), Some(b)) if a != b => out.warn(format!("The {what}s don't match. Once more:")),
            (Some(a), Some(_)) => match check(&a) {
                Ok(()) => return Some(a),
                Err(why) => out.warn(format!("{why}. Once more:")),
            },
            _ => {
                out.event(json!({"event": "needs", "action": "input", "flag": flag}));
                out.fail(format!("Run again with {flag} FILE (or - for stdin)"));
                return None;
            }
        }
    }
}

pub fn launch(json: bool, args: &LaunchArgs) -> ExitCode {
    let mut out = Out { term: Term::new(args.log.as_deref()), json };
    catch_interrupts();
    let (handle, rx) = match start(App::Launcher, None, args.demo) {
        Ok(h) => h,
        Err(e) => {
            out.fail(format!("Couldn't start: {e}"));
            return finish(&mut out, Exit::Failed, "start", &e);
        }
    };
    out.term.line(vec![s("Orchard", TITLE)]);
    let send = |e: Event| {
        let _ = handle.events.send(e);
    };
    if args.list || args.entry.is_some() {
        // Also with one system installed: --list starts nothing, and --entry
        // starts only the entry it names.
        send(Event::AskEntry);
    }
    let mut shared = Shared::new(App::Launcher);
    let mut chosen = false;
    loop {
        out.term.tick();
        waiting_keys(&mut out, &mut shared.waiting, args.wait);
        let flow = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(flow) => Some(flow),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return finish(&mut out, Exit::Failed, "driver", "the launcher stopped"),
        };
        if INTERRUPTS.load(Ordering::SeqCst) > 0 {
            return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
        }
        let Some(flow) = flow else {
            if let Some(code) = idle(&mut out, &shared.last, &mut shared.waiting, args.wait) {
                return code;
            }
            continue;
        };
        let first = match shared.see(&mut out, &flow) {
            Next::Skip => continue,
            Next::Exit(code) => return code,
            Next::Go { first } => first,
        };
        match &flow.state {
            State::Found { device, .. } => {
                if first {
                    out.term.live(None);
                    out.ok(format!("{} in DFU mode", device.name));
                }
                shared.starting(&mut out, &flow, first);
            }
            State::DfuAgain { device, why, .. } if first => shared.dfu_again(&mut out, &flow, device, *why, "", args.wait),
            State::ChooseSystem { entries, selected, .. } if !chosen => {
                out.term.live(None);
                out.event(json!({"event": "systems", "entries": entries.iter().map(|e| json!({"id": e.id, "title": e.title, "kernel": e.kernel, "default": e.default})).collect::<Vec<_>>()}));
                let count = entries.len();
                if args.list {
                    out.term.blank();
                    for e in entries {
                        out.term.line(vec![s(format!("  {:<24} ", e.id), BLUE), s(format!("{:<16} {:<10}", e.title, e.kernel), FG), s(if e.default { " default" } else { "" }, GREEN)]);
                    }
                    return finish(&mut out, Exit::Done, "done", "");
                }
                let index = match &args.entry {
                    Some(id) => match entries.iter().position(|e| &e.id == id) {
                        Some(i) => i,
                        None => {
                            out.fail(format!("No boot entry {id}; orchard2 launch --list shows them"));
                            return finish(&mut out, Exit::Usage, "usage", "unknown entry");
                        }
                    },
                    None if count == 1 => 0,
                    None => {
                        out.term.blank();
                        out.heading("Choose a system", None);
                        for (i, e) in entries.iter().enumerate() {
                            out.term.line(vec![s(format!("  {}  ", i + 1), BLUE), s(format!("{:<16}", e.title), FG), s(format!("{:<9}", e.kernel), MUTED), s(if e.default { "default" } else { "" }, GREEN)]);
                        }
                        out.term.blank();
                        match out.term.ask(vec![s("  Start which? ", FG), s(format!("[{}] ", selected + 1), MUTED)], false) {
                            Some(t) if t.trim().is_empty() => *selected,
                            Some(t) => match t.trim().parse::<usize>() {
                                Ok(n) if (1..=count).contains(&n) => n - 1,
                                _ => {
                                    out.fail(format!("Choose 1 to {count}"));
                                    return finish(&mut out, Exit::Usage, "usage", "no such system");
                                }
                            },
                            None => *selected,
                        }
                    }
                };
                chosen = true;
                send(Event::SelectEntry(index));
                send(Event::Continue);
            }
            State::NothingInstalled { .. } => {
                out.term.live(None);
                out.term.blank();
                out.fail(orchard_flow::for_device("Nothing to start on this iPad"));
                out.sub("No installed system was found on it. Install one first.");
                return finish(&mut out, Exit::Failed, "nothing-installed", "no installed system");
            }
            State::Starting { title, .. } => {
                if first {
                    out.term.live_restart(vec![s(format!("Starting {title}"), FG)]);
                }
                if flow.entry_started {
                    out.term.live(None);
                    out.event(json!({"event": "done", "started": title}));
                    out.ok_note(format!("Starting {title}"), orchard_flow::for_device("it continues on your iPad"));
                    return finish(&mut out, Exit::Done, "done", "");
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_become_index_urls() {
        assert_eq!(index_for("omarchy-a"), "https://github.com/OrchardLinux/omarchy-a/releases/latest/download/index.toml");
        assert_eq!(index_for("https://x/index.toml"), "https://x/index.toml");
    }

    #[test]
    fn state_names_are_kebab_case() {
        assert_eq!(state_name(&State::start()), "waiting-for-dfu");
    }

    #[test]
    fn dfu_steps_follow_the_buttons() {
        assert_eq!(steps(Sequence::HomePower).len(), 4);
        assert!(steps(Sequence::VolumeSequence).iter().any(|(p, _)| plain(p).contains("Volume Up")));
    }

    #[test]
    fn the_layout_bars_fill_their_width() {
        let layout = Layout { total_gb: 128, ipados_gb: 128, ipados_used_gb: 41, outside: vec![] };
        let width = |row: Vec<Span>| row.iter().map(|(t, _)| t.chars().count()).sum::<usize>();
        assert_eq!(width(layout_bar(&layout, 128, None, 40)), 40);
        assert_eq!(width(layout_bar(&layout, 58, Some(70), 40)), 40);
    }
}
