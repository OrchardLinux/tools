use super::{device_text, plan_session, read_file, session_to_start, went_text, LinuxFiles, Way};
use crate::args::{Global, PayloadArgs};
use crate::output::{self, Output, Report};
use orchard_boards::catalog::Catalog;
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_device::load::{self, Loaded};
use orchard_device::prep::{Load, Request};
use orchard_usb::Mode;
use std::path::Path;
use std::time::Duration;

impl Report for Loaded {
    fn human(&self) -> Vec<u8> {
        let (went, device) = (went_text(&self.went), device_text(&self.went));
        format!("{went}loaded {} bytes; {device}\n", self.size).into_bytes()
    }
}

pub fn run(
    global: &Global,
    file: &Path,
    payload: &PayloadArgs,
    way: &Way,
    wait: Option<Mode>,
    plan: bool,
    timeout: Duration,
) -> Result<Output> {
    // Everything that can fail without the device comes first.
    let first = read_file(file)?;
    let files = LinuxFiles::read(payload)?;
    let dtbs = files.as_ref().map(LinuxFiles::dtbs).unwrap_or_default();
    let load = Load::File(&first, files.as_ref().map(|f| f.linux(payload, &dtbs)));
    let catalog = Catalog::open(&global.data_dir()?)?;
    if plan {
        let session = plan_session(global, way.path)?;
        let request = Request {
            load: Some(load),
            wait,
            ..way.request(&catalog, None)
        };
        return Ok(load::plan(&request, session.as_ref())?.into());
    }
    let store = Store::open(global.cache.as_deref())?;
    let request = Request {
        load: Some(load),
        wait,
        ..way.request(&catalog, Some(&store))
    };
    let mut session = session_to_start(global, timeout)?;
    let loaded = load::run(&mut session, &request, &mut |step| {
        output::progress(global, step)
    })?;
    Ok(loaded.into())
}
