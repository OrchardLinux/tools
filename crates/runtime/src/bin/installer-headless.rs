//! The installer's flow without a window: prints the screen and the device
//! view on every change, the USB events and the effects it carries out.
//!
//! Usage: installer-headless [--index <url>] [--data <orchard checkout>] [--local <release dir>] [--launcher] [--no-fetch] [--release]
//! --index is the release index to install from (a distribution's index.toml).
//! --data uses that board data instead of the release's own.
//! --companion <dir> uses a companion built into that directory instead of
//! the one built into the installer (development).
//! --local installs a local release build (its directory with index.toml)
//! instead of the published one.
//! --no-fetch only prints asset effects instead of downloading and building.
//! --live acts on the device (palera1n boots, splitting, installing); without
//! it those steps are printed and skipped.
//! --release prints the current release and exits.
//! Type `c` (Continue), `s` (Skip), `y` (confirm skip), `n` (cancel),
//! `e` (approve the erase) or `a PASSWORD PIN` (the account), then Enter.
use orchard_flow::{view, Account, App, Effect, Event, Flow};
use orchard_runtime::driver::{self, Observer, Options};
use std::path::PathBuf;
use std::sync::mpsc;

struct Printer {
    app: App,
    last: String,
}

impl Observer for Printer {
    fn changed(&mut self, flow: &Flow) {
        let (device, screen) = view(self.app, &flow.state);
        let now = format!("{screen:?} · {device:?}");
        if now != self.last {
            println!("{now}");
            self.last = now;
        }
    }
    fn effect(&mut self, effect: &Effect) {
        println!("  → {effect:?}");
    }
    fn usb(&mut self, event: &Event) {
        println!("usb: {event:?}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let app = if args.iter().any(|a| a == "--launcher") { App::Launcher } else { App::Installer };
    let mut options = Options::new(app);
    options.data = value("--data");
    options.local = value("--local");
    options.index = args.iter().position(|a| a == "--index").and_then(|i| args.get(i + 1)).cloned();
    options.companion = value("--companion");
    options.fetch = !args.iter().any(|a| a == "--no-fetch");
    options.live = args.iter().any(|a| a == "--live");
    let fail = |e: orchard_cache::Error| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };

    if args.iter().any(|a| a == "--release") {
        let executor = options.executor(mpsc::channel().0).unwrap_or_else(|e| fail(e));
        let rel = executor.release().unwrap_or_else(|e| fail(e));
        println!("{} {} (user {:?})", rel.name, rel.version, rel.account_user);
        println!("root {} MB · platforms {:?}", rel.root.bytes >> 20, rel.platforms.iter().map(|p| &p.name).collect::<Vec<_>>());
        return;
    }

    let handle = driver::start(options, Printer { app, last: String::new() }).unwrap_or_else(|e| fail(e));
    // The account user the release creates.
    let username = handle.executor.release().ok().and_then(|r| r.account_user).unwrap_or_else(|| "user".into());
    for line in std::io::stdin().lines().map_while(Result::ok) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let event = match words.as_slice() {
            ["a", password, pin] => Event::EditAccount(Account {
                username: username.clone(),
                password: (*password).into(),
                password_again: (*password).into(),
                pin: (*pin).into(),
                pin_again: (*pin).into(),
                ssh: false,
            }),
            ["e"] => Event::Approve(true),
            ["c"] => Event::Continue,
            ["s"] => Event::Skip,
            ["y"] => Event::ConfirmSkip,
            ["n"] => Event::CancelSkip,
            _ => continue,
        };
        if handle.events.send(event).is_err() {
            break;
        }
    }
    // Input closed: keep running until the flow quits or the process is stopped.
    loop {
        std::thread::park();
    }
}
