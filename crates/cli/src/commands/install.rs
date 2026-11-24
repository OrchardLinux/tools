//! `orchard2 install` and `orchard2 launch`: the graphical installer's flow
//! and runtime (crates/flow, crates/runtime) in the terminal. Every question
//! has a flag; without a terminal nothing is asked and a missing answer ends
//! the run (exit 5) naming the flag. With --json each step is one JSON object
//! per line on stdout (the flow's states, progress, what the person has to do,
//! done or the error); the words for people go to stderr.
use orchard_flow::{App, Event, Flow, Sequence, State, MIN_OMARCHY_GB};
use orchard_runtime::driver::{self, Observer, Options};
use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Write};
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
    /// The distribution: a name (omarchy-a), its release index URL or a local index.toml
    pub source: Option<String>,
    /// The release index to install from (instead of SOURCE)
    #[arg(long)]
    pub index: Option<String>,
    /// Space for the system in GB; asked when not given
    #[arg(long)]
    pub split: Option<u32>,
    /// Approve erasing the partitions after iPadOS
    #[arg(long)]
    pub yes: bool,
    /// Install into the existing split without starting iPadOS (the storage
    /// is split already); an unprepared iPad is split as usual
    #[arg(long, conflicts_with = "split")]
    pub keep_split: bool,
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
    /// Write the full run log here
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
    /// Write the full run log here
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

/// Ctrl-C presses since the run started.
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTS.fetch_add(1, Ordering::SeqCst);
}

fn catch_interrupts() {
    // SAFETY: installs a handler that only touches an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_interrupt as *const () as libc::sighandler_t);
    }
}

/// What the person and the agent see.
struct Out {
    json: bool,
    tty: bool,
    log: Option<std::fs::File>,
}

impl Out {
    fn new(json: bool, log: Option<&PathBuf>) -> Out {
        let log = log.and_then(|p| std::fs::File::create(p).map_err(|e| eprintln!("{}: {e}", p.display())).ok());
        Out { json, tty: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(), log }
    }
    fn event(&mut self, value: Value) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "{value}");
        }
        if self.json {
            println!("{value}");
            let _ = std::io::stdout().flush();
        }
    }
    fn say(&mut self, mark: &str, text: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "{mark} {text}");
        }
        let (on, off) = match (self.tty, mark) {
            (true, "✓") => ("\x1b[32m", "\x1b[0m"),
            (true, "!") => ("\x1b[33m", "\x1b[0m"),
            (true, "✕") => ("\x1b[31m", "\x1b[0m"),
            (true, "·") => ("\x1b[2m", "\x1b[0m"),
            _ => ("", ""),
        };
        eprintln!("{on}{mark}{off} {text}");
    }
    fn detail(&mut self, text: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "  {text}");
        }
        let (on, off) = if self.tty { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
        eprintln!("  {on}{text}{off}");
    }
}

/// Reads a line the person types; None without a terminal.
fn ask(out: &Out, question: &str) -> Option<String> {
    if !out.tty {
        return None;
    }
    eprint!("  {question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().to_owned())
}

/// Reads a secret without echoing it; None without a terminal.
fn ask_secret(out: &Out, question: &str) -> Option<String> {
    if !out.tty {
        return None;
    }
    eprint!("  {question}");
    let _ = std::io::stderr().flush();
    // SAFETY: plain termios calls on stdin, restored afterwards.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return None;
        }
        let saved = t;
        t.c_lflag &= !libc::ECHO;
        libc::tcsetattr(0, libc::TCSANOW, &t);
        saved
    };
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, &saved);
    }
    eprintln!();
    read.ok()?;
    Some(line.trim_end_matches(['\r', '\n']).to_owned())
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

/// The steps into DFU mode for the device's buttons.
fn dfu_steps(sequence: Sequence) -> Vec<&'static str> {
    let mut steps = vec!["Connect it to this computer with a USB cable"];
    steps.extend(match sequence {
        Sequence::HomePower => vec!["Hold Home and Power together (~10 s)", "Let go of Power, keep holding Home (~10 s)"],
        Sequence::VolumeSequence => vec![
            "Press Volume Up, then Volume Down",
            "Hold Top until the screen goes black (~10 s)",
            "Keep holding Top, also hold Volume Down (~5 s)",
            "Let go of Top, keep holding Volume Down (~10 s)",
        ],
        Sequence::VolumeDownSide => vec!["Hold Volume Down and Side together (~10 s)", "Let go of Side, keep holding Volume Down (~10 s)"],
    });
    steps.push("Let go when this changes");
    steps
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
        eprintln!("! {message}");
    }
    fn quit(&mut self) {}
}

fn start(app: App, index: Option<String>, demo: bool, out: &mut Out) -> Result<(driver::Handle, Receiver<Flow>), ExitCode> {
    let (tx, rx) = mpsc::channel();
    let mut options = Options::new(app);
    options.index = index;
    options.live = true;
    let started = if demo { driver::start_demo(options, Relay(tx)) } else { driver::start(options, Relay(tx)) };
    match started {
        Ok(handle) => Ok((handle, rx)),
        Err(e) => {
            out.event(json!({"event": "error", "code": "start", "message": e.to_string()}));
            out.say("✕", &format!("Couldn't start: {e}"));
            Err(ExitCode::from(Exit::Failed as u8))
        }
    }
}

/// Ends the run: the last event and the exit status.
fn finish(out: &mut Out, exit: Exit, code: &str, message: &str) -> ExitCode {
    if !matches!(exit, Exit::Done) {
        out.event(json!({"event": "error", "code": code, "message": message, "exit": exit as u8}));
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

/// What happened since the last flow, said once each.
struct Said {
    state: String,
    device_steps: bool,
    stalled: bool,
    percent: u8,
    step: Option<String>,
    split_started: Option<Instant>,
}

impl Said {
    fn new() -> Said {
        Said { state: String::new(), device_steps: false, stalled: false, percent: 0, step: None, split_started: None }
    }
}

fn needs_dfu(out: &mut Out, said: &mut Said, sequence: Sequence, device: Option<&str>, wait: u64, again: bool) {
    if said.device_steps {
        return;
    }
    said.device_steps = true;
    let steps = dfu_steps(sequence);
    out.event(json!({"event": "needs", "action": "dfu", "device": device, "steps": steps, "wait_s": wait}));
    out.say("!", if again { "Put your device in DFU mode again" } else { "Put your device in DFU mode" });
    for (i, s) in steps.iter().enumerate() {
        out.detail(&format!("{}  {s}", i + 1));
    }
}

pub fn install(json: bool, args: &InstallArgs) -> ExitCode {
    let mut out = Out::new(json, args.log.as_ref());
    let Some(index) = args.index.clone().or_else(|| args.source.as_deref().map(index_for)) else {
        out.say("✕", "Which system? orchard2 install omarchy-a (or --index URL)");
        return finish(&mut out, Exit::Usage, "usage", "no distribution given");
    };
    if args.split.is_some_and(|gb| gb < MIN_OMARCHY_GB) {
        out.say("✕", &format!("--split needs at least {MIN_OMARCHY_GB} GB"));
        return finish(&mut out, Exit::Usage, "usage", "split too small");
    }
    // The secrets first: stdin may carry them (--password-file -).
    let password = match &args.password_file {
        Some(p) => match read_secret(p) {
            Ok(s) => Some(s),
            Err(e) => return finish(&mut out, Exit::Usage, "usage", &e),
        },
        None => None,
    };
    let pin = match &args.pin_file {
        Some(p) => match read_secret(p) {
            Ok(s) => Some(s),
            Err(e) => return finish(&mut out, Exit::Usage, "usage", &e),
        },
        None => None,
    };
    // Wrong secrets fail now, not after the device is in DFU mode.
    if password.as_ref().is_some_and(|p| p.chars().count() < 8) {
        out.say("✕", "The password needs 8 or more characters");
        return finish(&mut out, Exit::Usage, "usage", "password too short");
    }
    if pin.as_ref().is_some_and(|p| p.len() != 6 || !p.bytes().all(|b| b.is_ascii_digit())) {
        out.say("✕", "The lock screen PIN needs exactly 6 digits");
        return finish(&mut out, Exit::Usage, "usage", "pin not 6 digits");
    }
    catch_interrupts();
    let (handle, rx) = match start(App::Installer, Some(index), args.demo, &mut out) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let release = handle.executor.release();
    let (name, user) = match &release {
        Ok(r) => {
            orchard_flow::set_system_name(&r.name);
            out.event(json!({"event": "release", "name": r.name, "version": r.version}));
            out.say("·", &format!("Orchard Installer · {} {}", r.name, r.version));
            (r.name.clone(), r.account_user.clone().unwrap_or_else(|| "user".into()))
        }
        Err(e) => {
            out.say("✕", &format!("Couldn't read the release: {e}"));
            return finish(&mut out, Exit::Network, "release", &e.to_string());
        }
    };
    let send = |e: Event| {
        let _ = handle.events.send(e);
    };
    let mut said = Said::new();
    let mut account_sent = false;
    let mut split_answered = false;
    let mut erase_answered = false;
    let mut continued_from: Option<String> = None;
    let mut installed_at: Option<Instant> = None;
    let mut ssh_chosen = false;
    let mut waiting_since = Instant::now();
    loop {
        let flow = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(flow) => flow,
            Err(RecvTimeoutError::Timeout) => {
                if INTERRUPTS.load(Ordering::SeqCst) > 0 {
                    return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
                }
                if args.wait > 0 && said.device_steps && waiting_since.elapsed() > Duration::from_secs(args.wait) {
                    out.say("✕", "No device showed up in DFU mode in time");
                    return finish(&mut out, Exit::NoDevice, "timeout", "no device in DFU mode");
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return finish(&mut out, Exit::Failed, "driver", "the installer stopped"),
        };
        // Ctrl-C: while the iPad works on its own, the first one only warns.
        let interrupts = INTERRUPTS.load(Ordering::SeqCst);
        if interrupts > 0 {
            let busy = matches!(flow.state, State::Splitting { .. } | State::Installing { .. });
            if busy && interrupts == 1 {
                if said.state != "ctrl-c" {
                    out.say("!", "Wait until it's done: your iPad is working on it. Press Ctrl-C again to quit anyway.");
                    said.state = "ctrl-c".into();
                }
            } else {
                // Leaves a temporary iPadOS by restarting the iPad, as the window does.
                send(Event::Quit);
                std::thread::sleep(Duration::from_secs(3));
                return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
            }
        }
        if let Some(why) = failed_asset(&flow) {
            out.say("✕", "A download didn't work. Check the internet connection, then try again.");
            out.detail(&why);
            return finish(&mut out, Exit::Network, "download", &why);
        }
        let name_now = state_name(&flow.state);
        if name_now != said.state && said.state != "ctrl-c" {
            out.event(json!({"event": "state", "state": name_now}));
        }
        if flow.boot_stalled && !said.stalled {
            said.stalled = true;
            out.event(json!({"event": "needs", "action": "replug"}));
            out.say("!", "Reconnect the cable at your iPad. Starting it has stalled; it continues by itself.");
        } else if !flow.boot_stalled {
            said.stalled = false;
        }
        let first = name_now != said.state;
        if first {
            said.state = name_now.clone();
        }
        match &flow.state {
            State::WaitingForDfu { tab, .. } => {
                if first {
                    waiting_since = Instant::now();
                }
                needs_dfu(&mut out, &mut said, *tab, None, args.wait, false);
            }
            State::UsbAccessNeeded => {
                if first {
                    out.say("!", "This computer needs to allow USB access for the device. Approving it asks for your password.");
                    send(Event::Continue);
                }
            }
            State::SeveralInDfu { .. } => {
                if first {
                    out.say("!", "Two devices are in DFU mode. Unplug the ones you don't want to install on.");
                }
            }
            State::Unsupported { device } => {
                out.say("✕", &format!("This {} isn't supported yet", device.name));
                out.detail("Supported devices: https://github.com/OrchardLinux/orchard/blob/main/docs/compatibility.md");
                return finish(&mut out, Exit::Unsupported, "unsupported", &device.name);
            }
            State::Found { device, .. } => {
                said.device_steps = false;
                if continued_from.as_deref() != Some("found") {
                    continued_from = Some("found".into());
                    out.say("✓", &format!("{} in DFU mode", device.name));
                    if args.keep_split {
                        // Straight to the install system (the window's "Skip").
                        send(Event::Skip);
                        send(Event::ConfirmSkip);
                    } else {
                        send(Event::Continue);
                    }
                }
            }
            State::WaitingForIpados { slow, .. } => {
                if first {
                    out.say("·", "Starting your iPad (about a minute)");
                }
                if *slow && continued_from.as_deref() != Some("slow") {
                    continued_from = Some("slow".into());
                    out.say("!", "Your iPad hasn't answered yet. If it shows the Lock Screen and nothing happens, unlock it.");
                }
            }
            State::ChooseSplit { layout, omarchy_gb, .. } => {
                if !split_answered {
                    split_answered = true;
                    out.say("✓", "Your iPad is running iPadOS with the installer's app");
                    let max = layout.max_omarchy_gb();
                    out.event(json!({"event": "split", "total_gb": layout.total_gb, "ipados_used_gb": layout.ipados_used_gb, "suggested_gb": omarchy_gb, "min_gb": MIN_OMARCHY_GB, "max_gb": max, "erases": layout.outside.iter().map(|p| json!({"name": p.name, "gb": p.gb})).collect::<Vec<_>>()}));
                    out.say("·", &format!("Split the storage · {} GB. Your apps and data stay where they are.", layout.total_gb));
                    if max < MIN_OMARCHY_GB {
                        out.say("✕", &format!("{name} needs at least {MIN_OMARCHY_GB} GB, and iPadOS can only spare {max} GB. Delete some apps or files on your iPad, then try again."));
                        return finish(&mut out, Exit::Failed, "no-room", "not enough free space");
                    }
                    let gb = match args.split {
                        Some(gb) => gb.min(max),
                        None => match ask(&out, &format!("Space for {name} in GB [{omarchy_gb}] ({MIN_OMARCHY_GB}–{max}): ")) {
                            Some(t) if t.is_empty() => *omarchy_gb,
                            Some(t) => match t.parse::<u32>() {
                                Ok(gb) if (MIN_OMARCHY_GB..=max).contains(&gb) => gb,
                                _ => {
                                    out.say("✕", &format!("Give a size from {MIN_OMARCHY_GB} to {max} GB"));
                                    return finish(&mut out, Exit::Usage, "usage", "split out of range");
                                }
                            },
                            None => {
                                out.event(json!({"event": "needs", "action": "input", "flag": "--split", "suggested_gb": omarchy_gb}));
                                out.say("✕", &format!("How much space for {name}? Run again with --split GB (suggested {omarchy_gb})"));
                                return finish(&mut out, Exit::NeedsInput, "needs-input", "--split");
                            }
                        },
                    };
                    if !layout.outside.is_empty() {
                        out.say("·", &format!("The partitions after iPadOS are erased, and {name} gets their space:"));
                        for p in &layout.outside {
                            out.detail(&format!("✕ {:<16} {:<10} {} GB", p.name, p.filesystem, p.gb));
                        }
                        if !consent(&mut out, args.yes) {
                            return finish(&mut out, Exit::NeedsInput, "needs-input", "--yes");
                        }
                    }
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
                    said.split_started = Some(Instant::now());
                    out.say("·", &format!("Splitting the storage: {omarchy_gb} GB for {name}. Keep your iPad connected and leave it alone; shrinking iPadOS can take around 30 minutes."));
                }
                if matches!(link, orchard_flow::Link::Lost) && continued_from.as_deref() != Some("split-lost") {
                    continued_from = Some("split-lost".into());
                    out.say("!", "The connection to your iPad dropped. It keeps splitting; reconnecting…");
                }
            }
            State::SplitDone { omarchy_gb, .. } => {
                if first {
                    let took = said.split_started.map(|t| t.elapsed().as_secs() / 60).unwrap_or(0);
                    out.say("✓", &format!("The storage is split · {name} {omarchy_gb} GB ({took} min)"));
                    send(Event::Continue);
                }
            }
            State::DfuAgain { device, .. } => {
                if first {
                    waiting_since = Instant::now();
                    said.device_steps = false;
                }
                needs_dfu(&mut out, &mut said, device.sequence, Some(&device.name), args.wait, true);
            }
            State::CreateAccount { account, env, .. } => {
                said.device_steps = false;
                if !account_sent {
                    account_sent = true;
                    if first {
                        out.say("✓", "In DFU mode; the install system is starting");
                    }
                    let Some(account) = make_account(&mut out, &user, password.clone(), pin.clone(), args) else {
                        return finish(&mut out, Exit::NeedsInput, "needs-input", "--password-file/--pin-file");
                    };
                    ssh_chosen = account.ssh;
                    send(Event::EditAccount(account));
                }
                if account_sent && env.is_some() && account.valid() && continued_from.as_deref() != Some("account") {
                    continued_from = Some("account".into());
                    send(Event::Continue);
                }
            }
            State::ConfirmErase { layout, .. } => {
                if !erase_answered {
                    erase_answered = true;
                    out.say("·", &format!("Before installing: {name} goes into its own space, next to iPadOS. These partitions are erased:"));
                    for p in &layout.outside {
                        out.detail(&format!("✕ {:<16} {:<10} {} GB", p.name, p.filesystem, p.gb));
                    }
                    if !consent(&mut out, args.yes) {
                        return finish(&mut out, Exit::NeedsInput, "needs-input", "--yes");
                    }
                    send(Event::Approve(true));
                    send(Event::Continue);
                }
            }
            State::NotPrepared { .. } => {
                if first {
                    out.say("!", &format!("Your iPad's storage is not prepared yet. The installer starts your iPad to split the storage between iPadOS and {name}."));
                    split_answered = false;
                    send(Event::Continue);
                }
            }
            State::Installing { percent, streamed, step, .. } => {
                if first {
                    installed_at.get_or_insert_with(Instant::now);
                    out.say("·", &format!("Installing {name}. Keep your iPad connected."));
                }
                if *percent >= said.percent.saturating_add(10) || (*percent == 100 && said.percent < 100) {
                    said.percent = *percent;
                    out.event(json!({"event": "progress", "percent": percent}));
                    out.detail(&format!("Writing the system {percent}%"));
                }
                if *streamed && step != &said.step {
                    said.step = step.clone();
                    if let Some(s) = step {
                        out.event(json!({"event": "progress", "step": s}));
                        out.detail(&format!("Finishing: {s}"));
                    }
                }
            }
            State::InstallFailed { .. } => {
                let why = flow.install_problem.clone().unwrap_or_else(|| "the install system stopped".into());
                out.say("✕", &format!("{name} couldn't be installed. iPadOS and your data weren't touched."));
                out.detail(&why);
                return finish(&mut out, Exit::Failed, "install-failed", &why);
            }
            State::Starting { .. } => {
                let took = installed_at.map(|t| t.elapsed().as_secs_f64());
                out.event(json!({"event": "done", "installed_in": took, "ssh": ssh_chosen.then(|| format!("{user}@172.16.42.1"))}));
                out.say("✓", &format!("{name} is starting"));
                if let Some(t) = took {
                    out.detail(&format!("Installed {name} in {t:.1}s"));
                }
                out.detail("To start it again later: orchard2 launch, with your iPad in DFU mode");
                if ssh_chosen {
                    out.detail(&format!("ssh {user}@172.16.42.1"));
                }
                return finish(&mut out, Exit::Done, "done", "");
            }
            State::ChooseSystem { .. } | State::NothingInstalled { .. } => {}
        }
    }
}

/// The erase approval: --yes, or typing yes.
fn consent(out: &mut Out, yes: bool) -> bool {
    if yes {
        out.event(json!({"event": "approved", "by": "--yes"}));
        return true;
    }
    match ask(out, "I have a backup and approve these changes. Type yes to continue: ") {
        Some(t) if t == "yes" => true,
        Some(_) => {
            out.say("✕", "Not approved; nothing was changed");
            false
        }
        None => {
            out.event(json!({"event": "needs", "action": "input", "flag": "--yes"}));
            out.say("✕", "Erasing needs approval: run again with --yes");
            false
        }
    }
}

/// The account from the flags or the prompts, checked as the window checks it.
fn make_account(out: &mut Out, user: &str, password: Option<String>, pin: Option<String>, args: &InstallArgs) -> Option<orchard_flow::Account> {
    out.say("·", "Create your account. Your iPad is getting ready in the meantime.");
    out.detail(&format!("Username: {user}"));
    let password = match password {
        Some(p) => p,
        None => {
            let a = ask_secret(out, "Password (8 or more characters): ");
            let b = a.as_ref().and_then(|_| ask_secret(out, "Repeat password: "));
            match (a, b) {
                (Some(a), Some(b)) if a == b => a,
                (Some(_), Some(_)) => {
                    out.say("✕", "The passwords don't match");
                    return None;
                }
                _ => {
                    out.event(json!({"event": "needs", "action": "input", "flag": "--password-file"}));
                    out.say("✕", "The account needs a password: run again with --password-file FILE (or - for stdin)");
                    return None;
                }
            }
        }
    };
    let pin = match pin {
        Some(p) => p,
        None => {
            let a = ask_secret(out, "Lock screen PIN (6 digits): ");
            let b = a.as_ref().and_then(|_| ask_secret(out, "Repeat PIN: "));
            match (a, b) {
                (Some(a), Some(b)) if a == b => a,
                (Some(_), Some(_)) => {
                    out.say("✕", "The PINs don't match");
                    return None;
                }
                _ => {
                    out.event(json!({"event": "needs", "action": "input", "flag": "--pin-file"}));
                    out.say("✕", "The account needs a lock screen PIN: run again with --pin-file FILE");
                    return None;
                }
            }
        }
    };
    let ssh = if args.ssh {
        true
    } else if args.no_ssh {
        false
    } else {
        out.detail(&format!("Adds this computer's SSH keys, so you can sign in from here without a password: ssh {user}@172.16.42.1"));
        matches!(ask(out, "Allow SSH from this computer? [y/N] ").as_deref(), Some("y" | "Y" | "yes"))
    };
    let account = orchard_flow::Account { username: user.to_owned(), password: password.clone(), password_again: password, pin: pin.clone(), pin_again: pin, ssh };
    if !account.valid() {
        out.say("✕", "The password needs 8 or more characters and the PIN exactly 6 digits");
        return None;
    }
    Some(account)
}

pub fn launch(json: bool, args: &LaunchArgs) -> ExitCode {
    let mut out = Out::new(json, args.log.as_ref());
    catch_interrupts();
    let (handle, rx) = match start(App::Launcher, None, args.demo, &mut out) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let send = |e: Event| {
        let _ = handle.events.send(e);
    };
    let mut said = Said::new();
    let mut chosen = false;
    let mut waiting_since = Instant::now();
    loop {
        let flow = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(flow) => flow,
            Err(RecvTimeoutError::Timeout) => {
                if INTERRUPTS.load(Ordering::SeqCst) > 0 {
                    return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
                }
                if args.wait > 0 && said.device_steps && waiting_since.elapsed() > Duration::from_secs(args.wait) {
                    return finish(&mut out, Exit::NoDevice, "timeout", "no device in DFU mode");
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return finish(&mut out, Exit::Failed, "driver", "the launcher stopped"),
        };
        if INTERRUPTS.load(Ordering::SeqCst) > 0 && !(matches!(flow.state, State::Starting { .. }) && !flow.entry_started) {
            return finish(&mut out, Exit::Interrupted, "interrupted", "Ctrl-C");
        }
        if let Some(why) = failed_asset(&flow) {
            out.say("✕", "A download didn't work. Check the internet connection, then try again.");
            out.detail(&why);
            return finish(&mut out, Exit::Network, "download", &why);
        }
        if let Some(problem) = &flow.launch_problem {
            out.say("✕", &format!("Orchard couldn't start your iPad: {problem}"));
            return finish(&mut out, Exit::Failed, "start-failed", problem);
        }
        let name_now = state_name(&flow.state);
        let first = name_now != said.state;
        if first {
            out.event(json!({"event": "state", "state": name_now}));
            said.state = name_now;
        }
        if flow.boot_stalled && !said.stalled {
            said.stalled = true;
            out.event(json!({"event": "needs", "action": "replug"}));
            out.say("!", "Reconnect the cable at your iPad. Starting it has stalled; it continues by itself.");
        }
        match &flow.state {
            State::WaitingForDfu { tab, .. } => {
                if first {
                    waiting_since = Instant::now();
                }
                needs_dfu(&mut out, &mut said, *tab, None, args.wait, false);
            }
            State::Unsupported { device } => {
                out.say("✕", &format!("This {} isn't supported yet", device.name));
                return finish(&mut out, Exit::Unsupported, "unsupported", &device.name);
            }
            State::Found { device, .. } if first => {
                out.say("✓", &format!("{} in DFU mode", device.name));
                out.say("·", "Looking for installed systems…");
            }
            State::ChooseSystem { entries, selected, .. } if !chosen => {
                out.event(json!({"event": "systems", "entries": entries.iter().map(|e| json!({"id": e.id, "title": e.title, "kernel": e.kernel, "default": e.default})).collect::<Vec<_>>()}));
                if args.list {
                    for e in entries {
                        eprintln!("{:<24} {:<16} {:<10}{}", e.id, e.title, e.kernel, if e.default { " default" } else { "" });
                    }
                    return finish(&mut out, Exit::Done, "done", "");
                }
                let index = match &args.entry {
                    Some(id) => match entries.iter().position(|e| &e.id == id) {
                        Some(i) => i,
                        None => {
                            out.say("✕", &format!("No boot entry {id}; orchard2 launch --list shows them"));
                            return finish(&mut out, Exit::Usage, "usage", "unknown entry");
                        }
                    },
                    None if entries.len() == 1 => 0,
                    None => {
                        out.say("·", &format!("Found {} installed systems:", entries.len()));
                        for (i, e) in entries.iter().enumerate() {
                            out.detail(&format!("{}  {:<16} {}{}", i + 1, e.title, e.kernel, if e.default { "  default" } else { "" }));
                        }
                        match ask(&out, &format!("Start which? [{}] ", selected + 1)) {
                            Some(t) if t.is_empty() => *selected,
                            Some(t) => match t.parse::<usize>() {
                                Ok(n) if (1..=entries.len()).contains(&n) => n - 1,
                                _ => return finish(&mut out, Exit::Usage, "usage", "no such system"),
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
                out.say("✕", "Nothing to start on this iPad. No installed system was found on it. Install one first.");
                return finish(&mut out, Exit::Failed, "nothing-installed", "no installed system");
            }
            State::Starting { title, .. } => {
                if first {
                    out.say("·", &format!("Starting {title}"));
                }
                if flow.entry_started {
                    out.event(json!({"event": "done", "started": title}));
                    out.say("✓", &format!("{title} is starting on your iPad"));
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
        assert_eq!(dfu_steps(Sequence::HomePower).len(), 4);
        assert!(dfu_steps(Sequence::VolumeSequence).iter().any(|s| s.contains("Volume Up")));
    }
}
