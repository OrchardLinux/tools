use super::{plan_session, planned_text, session_to_start, went_text, Way};
use crate::args::Global;
use crate::output::{self, Output, Report};
use crate::table::{self, Cell, Style};
use orchard_boards::catalog::Catalog;
use orchard_boards::data::board_data;
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_device::boot::{self, Booted, Entry, Options, Plan};
use orchard_usb::{Identity, Mode};
use std::fmt::Write;
use std::time::Duration;

/// The entries as a table, in m1n1's boot order.
fn entries_table(entries: &[Entry]) -> String {
    let problems: Vec<String> = entries.iter().map(|e| e.problems.join("; ")).collect();
    let rows: Vec<Vec<Cell>> = entries
        .iter()
        .zip(&problems)
        .map(|(entry, problems)| {
            vec![
                Cell::plain(&entry.id),
                Cell::optional(entry.title.as_deref(), Style::Plain),
                Cell::optional(entry.version.as_deref(), Style::Number),
                match entry.bootable {
                    true => Cell::optional(Some("yes"), Style::Board),
                    false => Cell::plain(problems),
                },
            ]
        })
        .collect();
    table::render(&["Entry", "Title", "Version", "Can boot"], &rows)
}

impl Report for Booted {
    fn human(&self) -> Vec<u8> {
        let mut text = self.path.as_ref().map(went_text).unwrap_or_default();
        let Some(entry) = &self.entry else {
            if self.entries.is_empty() {
                text.push_str("No boot entries on the device.\n");
            } else {
                let _ = writeln!(text, "{}", entries_table(&self.entries));
            }
            return text.into_bytes();
        };
        let name = entry.title.as_deref().unwrap_or(&entry.id);
        let _ = writeln!(text, "started {name} ({})", entry.id);
        for step in &self.steps {
            let _ = writeln!(text, "  {}: {}", step.name, step.outcome.as_str());
        }
        if let Some(device) = &self.device {
            let _ = writeln!(
                text,
                "the device at {} is in {} mode",
                device.location,
                device.mode.as_str()
            );
        }
        text.into_bytes()
    }
}

impl Report for Plan {
    fn human(&self) -> Vec<u8> {
        let mut text = planned_text(&self.path);
        text.push_str("then, on m1n1:\n");
        for (number, step) in self.then.iter().enumerate() {
            let _ = writeln!(text, "  {}. {step}", number + 1);
        }
        text.into_bytes()
    }
}

/// The file name of the device tree of the board with this identity, from
/// board data that is already here. m1n1 itself reports no identity, so
/// this is known when the session followed the device from an earlier mode.
fn board_dtb(global: &Global, identity: &Identity) -> Option<String> {
    let hex = |s: &Option<String>| u32::from_str_radix(s.as_deref()?, 16).ok();
    let store = Store::open(global.cache.as_deref()).ok()?;
    let data = board_data::existing(global.data.as_deref(), &store)?;
    let board = Catalog::open(&data)
        .ok()?
        .board_for(hex(&identity.chip)?, hex(&identity.board_id)?)
        .ok()?;
    Some(format!("{}.dtb", board.dtb?))
}

pub fn run(
    global: &Global,
    mut options: Options,
    way: &Way,
    plan: bool,
    timeout: Duration,
) -> Result<Output> {
    if plan {
        let catalog = Catalog::open(&global.data_dir()?)?;
        let session = plan_session(global, way.path)?;
        let request = way.request(&catalog, None);
        return Ok(boot::plan(&request, session.as_ref(), &options)?.into());
    }
    let mut session = session_to_start(global, timeout)?;
    let progress = &mut |step: &str| output::progress(global, step);
    // A device that runs m1n1 already needs no board data.
    if session.device().mode == Mode::M1n1 {
        if options.dtb_name.is_none() {
            options.dtb_name = board_dtb(global, session.identity());
        }
        return Ok(boot::run(&mut session, None, &options, progress)?.into());
    }
    let store = Store::open(global.cache.as_deref())?;
    let catalog = Catalog::open(&global.data_dir()?)?;
    let request = way.request(&catalog, Some(&store));
    Ok(boot::run(&mut session, Some(&request), &options, progress)?.into())
}
