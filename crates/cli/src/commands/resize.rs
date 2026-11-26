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
    if route != "palera1n" {
        return Err(Error::new(Code::Unsupported, format!("this version gets the app onto a device by palera1n only, not by {route} yet"))
            .hint("resize-apfs's own release does it: ./resize-apfs boot"));
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
    say(&format!("Resize APFS {}: preparing iPadOS's overlay", app.version));
    let overlay = app.overlay(&executor)?;

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

    say("Starting iPadOS with Resize APFS");
    orchard_runtime::chain::boot_ipados_with(&executor, &mut session, &overlay)?;
    session.wait_for(Mode::Normal, Duration::from_secs(args.timeout))?;
    Ok(Started { board: board.name, route: route.to_owned(), version: app.version, seconds: started.elapsed().as_secs_f64() }.into())
}

#[derive(Serialize)]
struct Started {
    board: String,
    route: String,
    version: String,
    seconds: f64,
}

impl Report for Started {
    fn human(&self) -> Vec<u8> {
        format!(
            "iPadOS is starting on {} with Resize APFS {} ({}, {:.0} s)\nUnlock the device: Resize APFS appears on the Home Screen within a minute.\n",
            self.board, self.version, self.route, self.seconds
        )
        .into_bytes()
    }
}
