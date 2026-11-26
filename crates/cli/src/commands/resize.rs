//! `resize`: gets Resize APFS, the app that resizes iPadOS, onto the device
//! and running, by an access route. The person does the rest in the app.
use crate::args::Global;
use crate::output::{self, Output, Report};
use orchard_boards::catalog::Catalog;
use orchard_boards::routes::{self, Kind};
use orchard_cache::{Code, Error, Result};
use orchard_device::select::Selector;
use orchard_device::session::Session;
use orchard_runtime::driver::Options;
use orchard_usb::{Host, Mode};
use serde::Serialize;
use std::io::IsTerminal;
use std::time::{Duration, Instant};

/// Short forms of `--access ROUTE`.
#[derive(clap::Args)]
#[group(multiple = false)]
pub struct AccessFlags {
    /// Short for --access palera1n
    #[arg(long, conflicts_with = "access")]
    palera1n: bool,
    /// Short for --access checkra1n
    #[arg(long, conflicts_with = "access")]
    checkra1n: bool,
    /// Short for --access dopamine
    #[arg(long, conflicts_with = "access")]
    dopamine: bool,
}

#[derive(clap::Args)]
pub struct Args {
    /// The access route, the way the app gets into iPadOS (palera1n, ...);
    /// palera1n when none is named
    #[arg(long, value_name = "ROUTE")]
    access: Option<String>,
    #[command(flatten)]
    short: AccessFlags,
    /// dopamine: the Apple ID team to sign with, where Xcode is signed in to
    /// several
    #[arg(long, value_name = "ID")]
    team: Option<String>,
    /// Seconds to wait for the device in DFU mode
    #[arg(long, default_value = "0")]
    wait: u64,
    /// Seconds iPadOS may take to start with the app
    #[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

pub fn run(global: &Global, args: &Args) -> Result<Output> {
    let started = Instant::now();
    let say = |message: &str| output::progress(global, &format!("[{:.3}s] {message}", started.elapsed().as_secs_f64()));
    let short = [(args.short.palera1n, "palera1n"), (args.short.checkra1n, "checkra1n"), (args.short.dopamine, "dopamine")];
    let route = args.access.as_deref().or(short.into_iter().find_map(|(set, name)| set.then_some(name))).unwrap_or("palera1n");

    let data = global.data_dir()?;
    let catalog = Catalog::open(&data)?;
    let routes = routes::read(&catalog)?;
    routes.named(Kind::Access, route)?;
    if !matches!(route, "palera1n" | "checkra1n" | "dopamine") {
        return Err(Error::new(Code::Unsupported, format!("this version does not run the {route} access route")));
    }
    // Signing needs Xcode's account here; elsewhere resize-apfs's own
    // release signs with xtool.
    if route == "dopamine" && !cfg!(target_os = "macos") {
        return Err(Error::new(Code::Unsupported, "this version signs the app on a Mac with Xcode only").hint("resize-apfs's own release does it with xtool: ./resize-apfs boot --dopamine"));
    }
    // checkra1n's session ends when the person says so: that needs a terminal.
    if route == "checkra1n" && !std::io::stdin().is_terminal() {
        return Err(Error::new(Code::Unsupported, "the checkra1n route takes the app off the device when you say you are done, which needs a terminal"));
    }

    let (events, _log) = std::sync::mpsc::channel();
    let mut options = Options::new(orchard_flow::App::Installer);
    options.data = Some(data);
    if let Some(cache) = &global.cache {
        options.cache = cache.clone();
    }
    let executor = options.executor(events)?;

    // Everything is made before the device is touched.
    say("Fetching Resize APFS");
    let app = orchard_runtime::resize_app::latest(&executor)?;
    let overlay = match route {
        "palera1n" => {
            say(&format!("Resize APFS {}: preparing iPadOS", app.version));
            Some(app.overlay(&executor)?)
        }
        _ => None,
    };
    if route == "checkra1n" && app.older.is_none() {
        return Err(Error::new(Code::NotFound, format!("Resize APFS {} has no build for iOS 12 to 14", app.version)));
    }

    if route == "dopamine" {
        return dopamine(&executor, &app, args, &say, started);
    }
    let found = super::run::find(global, Duration::from_secs(args.wait))?;
    let selector = Selector { ecid: found.identity.ecid.clone(), location: Some(found.location.clone()), mode: Some(Mode::Dfu) };
    let mut session = Session::start(Host, &selector, None)?;
    let seen = orchard_runtime::identify(&catalog, session.device())
        .filter(|seen| !seen.device.board.is_empty())
        .ok_or_else(|| Error::new(Code::Unsupported, "the device in DFU mode is no board the data knows"))?;
    let board = catalog.board(&seen.device.board)?;
    let chip = catalog.chip(board.chip)?;
    if !routes.reaches(Kind::Access, route, &chip)? {
        return Err(Error::new(Code::Unsupported, format!("{route} does not work on the {} chip", chip.name)));
    }
    say(&format!("{} at {}", seen.device.name, session.device().location));

    if let Some(overlay) = &overlay {
        say("Starting iPadOS with Resize APFS");
        orchard_runtime::chain::boot_ipados_with(&executor, &mut session, overlay)?;
        session.wait_for(Mode::Normal, Duration::from_secs(args.timeout))?;
        return Ok(Started { board: board.name, route: route.to_owned(), version: app.version, seconds: started.elapsed().as_secs_f64(), removed: false }.into());
    }
    // checkra1n: the session holds the device while checkra1n and iOS run;
    // the app is there until the person says they are done.
    let older = app.older.as_ref().expect("checked above");
    let mut done = || -> Result<()> {
        loop {
            eprint!("When you are done, type done and press Enter to take Resize APFS off the device: ");
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                return Err(Error::new(Code::Refused, "the terminal closed; Resize APFS stays on the device until the next session removes it"));
            }
            if line.trim().eq_ignore_ascii_case("done") {
                return Ok(());
            }
            eprintln!("Resize APFS stays on the device.");
        }
    };
    orchard_runtime::access_checkra1n::run(&executor, older, &say, &mut done)?;
    drop(session);
    Ok(Started { board: board.name, route: route.to_owned(), version: app.version, seconds: started.elapsed().as_secs_f64(), removed: true }.into())
}

#[derive(Serialize)]
struct Started {
    board: String,
    route: String,
    version: String,
    seconds: f64,
    /// The checkra1n session is over and the app is off the device again.
    removed: bool,
}

impl Report for Started {
    fn human(&self) -> Vec<u8> {
        if self.removed {
            return format!("Resize APFS {} is off {} again ({}).\n", self.version, self.board, self.route).into_bytes();
        }
        format!(
            "iPadOS is starting on {} with Resize APFS {} ({}, {:.0} s)\nUnlock the device: Resize APFS appears on the Home Screen within a minute.\n",
            self.board, self.version, self.route, self.seconds
        )
        .into_bytes()
    }
}

/// The route without DFU: the device runs iOS, unlocked, with Developer
/// Mode on. The app is installed for good, and gets access by itself when
/// it is opened.
fn dopamine(executor: &orchard_runtime::exec::Executor, app: &orchard_runtime::resize_app::App, args: &Args, say: &dyn Fn(&str), started: Instant) -> Result<Output> {
    use orchard_runtime::access_dopamine;
    let access = app.access.as_ref().ok_or_else(|| Error::new(Code::NotFound, format!("Resize APFS {} has no build for the route without DFU", app.version)))?;
    let udid = access_dopamine::device()?;
    let work = std::env::temp_dir().join(format!("orchard2-teams-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let teams = access_dopamine::teams(&work);
    let _ = std::fs::remove_dir_all(&work);
    let teams = teams?;
    let team = match (&args.team, teams.as_slice()) {
        (Some(id), _) => teams.iter().find(|(team, _)| team.eq_ignore_ascii_case(id)).ok_or_else(|| Error::new(Code::NotFound, format!("Xcode is not signed in to a team {id}")))?,
        (None, [one]) => one,
        (None, []) => return Err(Error::new(Code::NotFound, "Xcode is not signed in to an Apple ID").hint("add yours in Xcode > Settings > Accounts, then run this again")),
        (None, several) => {
            let names: Vec<String> = several.iter().map(|(id, name)| format!("{id} ({name})")).collect();
            return Err(Error::new(Code::Ambiguous, "Xcode is signed in to several teams").hint(format!("choose one with --team: {}", names.join(", "))));
        }
    };
    say(&format!("Signing with the Apple ID Xcode is signed in to ({})", team.1));
    say("The device needs Developer Mode on: Settings > Privacy & Security > Developer Mode");
    access_dopamine::install(executor, access, &team.0, &udid, say)?;
    Ok(Installed { version: app.version.clone(), seconds: started.elapsed().as_secs_f64() }.into())
}

#[derive(Serialize)]
struct Installed {
    version: String,
    seconds: f64,
}

impl Report for Installed {
    fn human(&self) -> Vec<u8> {
        format!(
            "Resize APFS {} is installed ({:.0} s).\n\
             Before opening it the first time, trust your Apple ID on the device: Settings > General > VPN & Device Management.\n\
             Then open Resize APFS and tap Continue. The screen restarts once; unlock the device when it comes back.\n\
             A free Apple ID signs the app for 7 days: run this again when it stops opening.\n",
            self.version, self.seconds
        )
        .into_bytes()
    }
}

/// stdin and stdout piped to `port` on the device `udid`, until either side
/// closes: what `ssh` runs as its ProxyCommand to reach a device over the cable.
pub fn usbmux_proxy(port: u16, udid: &str) -> Result<Output> {
    use orchard_runtime::usbmux;
    use std::io::{Read, Write};
    let stream = usbmux::connect(std::path::Path::new(usbmux::SOCKET), udid, port).map_err(|e| e.context("connecting to the device"))?;
    let mut to_device = stream.try_clone().map_err(|e| Error::from(e).context("sharing the connection"))?;
    let mut from_device = stream;
    std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        let mut stdin = std::io::stdin().lock();
        while let Ok(n @ 1..) = stdin.read(&mut buffer) {
            if to_device.write_all(&buffer[..n]).is_err() {
                break;
            }
        }
        let _ = to_device.shutdown(std::net::Shutdown::Write);
    });
    let mut buffer = [0u8; 65536];
    let mut stdout = std::io::stdout().lock();
    while let Ok(n @ 1..) = from_device.read(&mut buffer) {
        if stdout.write_all(&buffer[..n]).and_then(|()| stdout.flush()).is_err() {
            break;
        }
    }
    std::process::exit(0)
}
