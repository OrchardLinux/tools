//! The launcher's device side: starts m1n1 on the device in DFU, lists the
//! boot entries it reads from the device, and starts the chosen one. Holds
//! m1n1's proxy between the two. Everything it learns goes to the flow.
use crate::exec::Executor;
use orchard_flow::{Device, Entry, Event};
use orchard_usb::boot_manager::BootManager;
use std::sync::mpsc::{self, Sender};

pub enum Job {
    /// Start m1n1 and list the entries.
    List(Device),
    /// Start this entry.
    Boot(String),
}

pub fn spawn(executor: Executor, events: Sender<Event>) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut manager: Option<BootManager> = None;
        for job in rx {
            let result = match job {
                Job::List(device) => list(&executor, &device).map(|(m, entries)| {
                    manager = Some(m);
                    let _ = events.send(Event::EntriesListed(entries));
                }),
                Job::Boot(id) => match manager.as_mut() {
                    Some(m) => {
                        // When m1n1 answers relative to the command (it answers after
                        // loading the entry, just before jumping into it).
                        let sent = std::time::Instant::now();
                        eprintln!("{} launcher: start command sent for {id}", crate::install::stamp());
                        m.start(&id).map_err(|e| e.to_string()).map(|()| {
                            eprintln!("{} launcher: m1n1 answered after {:.1}s", crate::install::stamp(), sent.elapsed().as_secs_f64());
                            let _ = events.send(Event::EntryStarted);
                        })
                    }
                    None => Err("m1n1 is not running".into()),
                },
            };
            if let Err(e) = result {
                eprintln!("launcher: {e}");
                let _ = events.send(Event::ChainFailed(e));
            }
        }
    });
    tx
}

/// m1n1 up, storage up, the entries that can boot (in BLS order).
fn list(executor: &Executor, device: &Device) -> Result<(BootManager, Vec<Entry>), String> {
    let usb = crate::chain::start_m1n1(executor, device).map_err(|e| e.to_string())?;
    let proxy = orchard_usb::m1n1::Proxy::open(&usb).map_err(|e| e.to_string())?;
    let mut manager = BootManager::new(proxy).map_err(|e| e.to_string())?;
    manager.storage_init().map_err(|e| e.to_string())?;
    let entries = manager
        .entries()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|e| e.problems.is_empty())
        .map(|e| Entry {
            title: e.title().to_owned(),
            kernel: e.get("version").unwrap_or_default().to_owned(),
            default: e.default(),
            id: e.id,
        })
        .collect();
    Ok((manager, entries))
}
