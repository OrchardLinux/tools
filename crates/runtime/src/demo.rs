//! A scripted stand-in for the device and the work, to click through every
//! screen without hardware (`--demo`): no USB, no downloads, nothing on any
//! device. It answers the flow's effects with the events a real run would
//! send, after realistic pauses.
use crate::driver::Observer;
use orchard_flow::{step, App, Device, DeviceId, Effect, Event, Flow, Layout, Model, Partition, Sequence, Support, UsbMode};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

fn ipad() -> Device {
    Device {
        id: DeviceId("de30".into()),
        board: "j71b".into(),
        name: "iPad (6th generation)".into(),
        model: Model::IpadHome,
        sequence: Sequence::HomePower,
        support: Support::Supported,
    }
}

fn attached(mode: UsbMode) -> Event {
    Event::Attached { device: ipad(), mode }
}

/// What iPadOS reports before the split: 128 GB, 41 GB used.
fn ipados_layout() -> Layout {
    Layout { total_gb: 128, ipados_gb: 128, ipados_used_gb: 41, outside: Vec::new() }
}

/// What the install system reports after the split.
fn env_layout(omarchy_gb: u32) -> Layout {
    Layout {
        total_gb: 128,
        ipados_gb: 128 - omarchy_gb,
        ipados_used_gb: 41,
        outside: vec![
            Partition { name: "BOOT".into(), filesystem: "FAT32".into(), gb: 1 },
            Partition { name: "omarchy-a".into(), filesystem: "ext4".into(), gb: omarchy_gb.saturating_sub(1) },
        ],
    }
}

/// Runs the flow against the script until the receiver closes.
pub fn run(app: App, mut observer: impl Observer, input: Receiver<Event>, _events: Sender<Event>) {
    // Events due at a time, in order.
    let mut due: Vec<(Instant, Event)> = Vec::new();
    let at = |secs: f32| Instant::now() + Duration::from_secs_f32(secs);
    let mut flow = Flow::new();
    let mut omarchy_gb = 32;
    // The opening: plugged in running iPadOS, then the DFU combination.
    due.push((at(1.5), attached(UsbMode::Normal)));
    due.push((at(4.), Event::Detached { id: ipad().id }));
    due.push((at(6.), attached(UsbMode::Dfu)));
    let mut feed = |flow: Flow, event: Event, due: &mut Vec<(Instant, Event)>, omarchy_gb: &mut u32| -> Flow {
        let (next, effects) = step(app, flow, event);
        for effect in effects {
            observer.effect(&effect);
            match effect {
                Effect::BootIpadosWithOverlay(_) => {
                    // ORCHARD_DEMO_STALL=1: palera1n stalls after checkm8 until "replugged".
                    if std::env::var_os("ORCHARD_DEMO_STALL").is_some() {
                        due.push((at(0.8), Event::BootStalled));
                        due.push((at(3.5), Event::BootResumed));
                    }
                    // ORCHARD_DEMO_RETRY=1: the exploit takes three attempts.
                    if std::env::var_os("ORCHARD_DEMO_RETRY").is_some() {
                        due.push((at(0.6), Event::StartAttempt(2)));
                        due.push((at(1.2), Event::StartAttempt(3)));
                        let log: Vec<String> = ["05:17:10  Setting up the exploit", "05:17:12  UaF race: heuristic strategy failed, setup packet was not accepted (×148)", "05:17:26  Entered initial checkm8 state after 149 steps", "05:17:26  Checkmate!", "05:17:26  Device should now reconnect in download mode", "05:17:33  Entered download mode", "05:17:33  Booting PongoOS..."].iter().map(|l| l.to_string()).collect();
                        for (i, k) in [1usize, 2, 3, 4, 5, 6, 7].into_iter().enumerate() {
                            due.push((at(1.3 + i as f32 * 0.3), Event::StartLog(log[..k].to_vec())));
                        }
                    }
                    due.push((at(3.8), Event::BootBooting));
                    due.push((at(1.), Event::Detached { id: ipad().id }));
                    due.push((at(4.), attached(UsbMode::Normal)));
                    due.push((at(5.), Event::IpadosReady { layout: ipados_layout() }));
                }
                Effect::RefreshLayout => due.push((at(1.), Event::IpadosReady { layout: ipados_layout() })),
                Effect::StartSplit { omarchy_gb: gb } => {
                    *omarchy_gb = gb;
                    // ORCHARD_DEMO_REFUSE=1: the first split is refused, to see that screen.
                    static REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if std::env::var_os("ORCHARD_DEMO_REFUSE").is_some() && !REFUSED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        due.push((at(1.), Event::SplitRefused("The requested size exceeds the contiguous space available after APFS.".into())));
                        due.push((at(2.), Event::IpadosReady { layout: ipados_layout() }));
                    } else {
                        due.push((at(8.), Event::SplitFinished { ok: true }));
                    }
                }
                Effect::BootInstallEnv(_) => {
                    if std::env::var_os("ORCHARD_DEMO_STALL").is_some() {
                        due.push((at(0.8), Event::BootStalled));
                        due.push((at(2.5), Event::BootResumed));
                    }
                    due.push((at(1.), Event::Detached { id: ipad().id }));
                    due.push((at(3.), attached(UsbMode::Install)));
                    due.push((at(5.), Event::EnvReady { layout: env_layout(*omarchy_gb) }));
                }
                Effect::StartInstall(_) => {
                    for i in 1..=50u8 {
                        due.push((at(i as f32 * 0.3), Event::InstallProgress(i * 2)));
                    }
                    due.push((at(15.5), Event::Streamed));
                    for (i, s) in ["filesystem", "firmware", "account", "kernel", "entry", "sync"].iter().enumerate() {
                        due.push((at(16. + i as f32), Event::InstallStep((*s).into())));
                    }
                    due.push((at(22.), Event::InstallFinished { ok: true }));
                    due.push((at(23.), Event::Detached { id: ipad().id }));
                    due.push((at(26.), attached(UsbMode::Installed)));
                }
                // The launcher: m1n1 lists what's installed (ORCHARD_DEMO_ENTRIES:
                // how many, default 3; 0 shows "nothing to start").
                Effect::ListEntries(_) => {
                    let n: usize = std::env::var("ORCHARD_DEMO_ENTRIES").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
                    let names = [("Omarchy A", "6.12.4"), ("Omarchy A", "6.12.3"), ("Minimal Linux", "6.11.9"), ("Alpine Linux", "6.12.1"), ("Arch Linux ARM", "6.12.4"), ("Debian", "6.1.119"), ("Fedora", "6.11.10")];
                    let entries = (0..n)
                        .map(|i| {
                            let (title, kernel) = names[i % names.len()];
                            orchard_flow::Entry { id: format!("entry-{i}"), title: title.into(), kernel: kernel.into(), default: i == 0 }
                        })
                        .collect();
                    due.push((at(4.), Event::EntriesListed(entries)));
                }
                // m1n1 took the start command: it carries on by itself.
                Effect::BootEntry(_) => due.push((at(1.5), Event::EntryStarted)),
                Effect::Quit => observer.quit(),
                Effect::Fetch(asset) => due.push((at(0.5), Event::AssetReady(asset))),
                _ => {}
            }
        }
        // "DFU mode again" (3.1): the iPad comes back in DFU after a moment.
        let again = matches!(next.state, orchard_flow::State::DfuAgain { .. });
        if again && !due.iter().any(|(_, e)| matches!(e, Event::Attached { mode: UsbMode::Dfu, .. })) {
            due.push((at(3.), Event::Detached { id: ipad().id }));
            due.push((at(3.5), attached(UsbMode::Dfu)));
        }
        observer.changed(&next);
        next
    };
    flow = feed(flow, Event::Started, &mut due, &mut omarchy_gb);
    loop {
        due.sort_by_key(|(when, _)| *when);
        while due.first().is_some_and(|(when, _)| *when <= Instant::now()) {
            let (_, event) = due.remove(0);
            flow = feed(flow, event, &mut due, &mut omarchy_gb);
        }
        match input.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => flow = feed(flow, event, &mut due, &mut omarchy_gb),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

