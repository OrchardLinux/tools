//! The two things the installer asks of a device in recovery mode during the
//! guided run into DFU mode (orchard_flow::recovery): to start normally if
//! the run is given up, and to restart while the user holds the buttons.
//! The same commands the recovery-dfu session used with irecovery.
use orchard_flow::{DeviceId, Event};
use orchard_usb::recovery::Recovery;
use std::sync::mpsc::Sender;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);

/// The device `id` in recovery mode on USB.
fn open(id: &DeviceId) -> Result<Recovery, String> {
    let listed = orchard_usb::list().map_err(|e| e.to_string())?;
    let device = listed
        .iter()
        .find(|d| d.mode == orchard_usb::Mode::Recovery && d.identity.ecid.as_deref().is_some_and(|e| crate::ecid_key(e) == id.0))
        .ok_or_else(|| "the device is no longer in recovery mode".to_owned())?;
    Recovery::open(device).map_err(|e| e.to_string())
}

/// Entering recovery by software leaves auto-boot off: set it back on and
/// save it, so a run given up starts the system instead of recovery again.
fn prepare(id: &DeviceId) -> Result<(), String> {
    let mut recovery = open(id)?;
    for command in ["setenv auto-boot true", "saveenv"] {
        recovery.command(command, TIMEOUT).map_err(|e| format!("{command}: {e}"))?;
    }
    Ok(())
}

/// The restart. iBoot may be gone before it answers: that is the restart
/// going through, not a failure (Sent::Unconfirmed).
fn restart(id: &DeviceId) -> Result<(), String> {
    open(id)?.command("reboot", TIMEOUT).map(|_| ()).map_err(|e| format!("reboot: {e}"))
}

/// Carries out `prepare` or `restart` on its own thread; a failure comes back
/// as Event::RecoveryFailed.
pub fn spawn(id: DeviceId, restart_it: bool, events: Sender<Event>) {
    std::thread::spawn(move || {
        let done = if restart_it { restart(&id) } else { prepare(&id) };
        let what = if restart_it { "restart" } else { "prepare" };
        match done {
            Ok(()) => eprintln!("{} recovery: {what} sent", crate::install::stamp()),
            Err(why) => {
                eprintln!("{} recovery: {what} failed: {why}", crate::install::stamp());
                let _ = events.send(Event::RecoveryFailed(why));
            }
        }
    });
}
