//! From recovery mode into DFU mode, guided (R.1–R.5 in installer-app.pen):
//! the user holds the buttons, and the installer restarts the device at the
//! right moment, so the hold takes seconds instead of the full combination.
//!
//! The run belongs to whichever state waits for DFU mode (waiting for DFU,
//! DFU mode again): it says how far the device is on its way there. What
//! follows DFU mode is that state's business, never the run's.
//!
//! Measured once, on an iPad 6 (J71b), by the recovery-dfu session: the
//! restart 2 s into the hold, both held 2 s more, DFU about 8 s after the
//! restart. A failed run comes back as recovery or iPadOS, after up to about
//! 40 s of silence.
use crate::{Device, Effect, Event, Sequence, UsbMode};
use std::time::Duration;

/// How far the run is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The steps, waiting for Start (R.1). `again`: the last run ended in
    /// recovery mode (R.5). `problem`: why the installer couldn't talk to
    /// the device the last time, if that is how it ended.
    Ready { again: bool, problem: Option<String> },
    /// Start was clicked: a countdown before the hold (step 2).
    GetReady,
    /// Both buttons held; the installer restarts the device when it ends (step 3).
    Hold,
    /// The restart went out; both stay held (still step 3).
    Restarted,
    /// Power let go, Home held until DFU mode shows (step 4).
    LetGo,
}

pub const GET_READY: Duration = Duration::from_secs(3);
pub const HOLD: Duration = Duration::from_secs(2);
pub const RESTARTED: Duration = Duration::from_secs(2);
/// A failed run can stay silent for about 40 s before it shows again.
pub const LET_GO: Duration = Duration::from_secs(45);

impl Phase {
    pub fn ready() -> Phase {
        Phase::Ready { again: false, problem: None }
    }

    /// The phase's timer: its name, which changes with every phase, and how
    /// long it runs.
    pub fn timer(&self) -> Option<(&'static str, Duration)> {
        match self {
            Phase::Ready { .. } => None,
            Phase::GetReady => Some(("recovery-get-ready", GET_READY)),
            Phase::Hold => Some(("recovery-hold", HOLD)),
            Phase::Restarted => Some(("recovery-restarted", RESTARTED)),
            Phase::LetGo => Some(("recovery-let-go", LET_GO)),
        }
    }

    /// Whether the device is expected off USB now (the restart went out).
    pub fn restarting(&self) -> bool {
        matches!(self, Phase::Restarted | Phase::LetGo)
    }

    /// Whether a run is under way (Start was clicked).
    pub fn running(&self) -> bool {
        !matches!(self, Phase::Ready { .. })
    }

    /// The step row lit (0-based, as R.1i lists them), if any.
    pub fn row(&self) -> Option<usize> {
        match self {
            Phase::Ready { .. } => None,
            Phase::GetReady => Some(1),
            Phase::Hold | Phase::Restarted => Some(2),
            Phase::LetGo => Some(3),
        }
    }
}

/// A run: the device it is for (kept while the restart takes it off USB)
/// and how far it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub device: Device,
    pub phase: Phase,
}

/// The buttons of the run for a button layout: Home and Power, or Volume
/// Down and Power (the iPhone 7 and later have no Home button to hold).
pub fn buttons(sequence: Sequence) -> (&'static str, &'static str) {
    match sequence {
        Sequence::HomePower => ("Home", "Power"),
        Sequence::VolumeDownSide | Sequence::VolumeSequence => ("Volume Down", "Power"),
    }
}

/// What became of the run after an event. USB events also reach the
/// state, which keeps its record of what is on USB whatever the run does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// The run goes on in this phase.
    Run(Phase, Vec<Effect>),
    /// The run is over without DFU mode: the device is not in recovery any
    /// more (unplugged, or gone too long). The state forgets it.
    Over,
    /// Not the run's business: the state handles the event as without a run.
    Pass,
}

/// The run of `device`, now in `phase`, given `event`. DFU mode and the
/// device showing up in another mode than recovery are the state's to
/// handle (Pass); so is any event about another device.
pub fn next(phase: Phase, device: &Device, event: &Event) -> Next {
    let id = &device.id;
    let none = Vec::new;
    match (phase, event) {
        (Phase::Ready { .. }, Event::Continue) => Next::Run(Phase::GetReady, vec![Effect::PrepareRecovery(id.clone())]),
        // Stop, before the restart went out.
        (Phase::GetReady | Phase::Hold, Event::Cancel) => Next::Run(Phase::ready(), none()),
        (Phase::GetReady, Event::Timeout) => Next::Run(Phase::Hold, none()),
        (Phase::Hold, Event::Timeout) => Next::Run(Phase::Restarted, vec![Effect::RestartFromRecovery(id.clone())]),
        (Phase::Restarted, Event::Timeout) => Next::Run(Phase::LetGo, none()),
        // Nothing showed for long: whatever comes now is not the run's.
        (Phase::LetGo, Event::Timeout) => Next::Over,
        // The installer couldn't talk to it: the steps again, with why.
        (Phase::GetReady | Phase::Hold | Phase::Restarted, Event::RecoveryFailed(why)) => {
            Next::Run(Phase::Ready { again: false, problem: Some(why.clone()) }, none())
        }
        // Unplugged before the restart: the run is over. Off USB after it is
        // expected (the state notes it as for any detach; the run stays).
        (phase, Event::Detached { id: gone }) if gone == id && !phase.restarting() => Next::Over,
        // Back in recovery after the restart: that attempt didn't work.
        (phase, Event::Attached { device: d, mode: UsbMode::Recovery }) if &d.id == id && phase.restarting() => {
            Next::Run(Phase::Ready { again: true, problem: None }, none())
        }
        // Everything else, DFU mode and other modes included, is the state's.
        _ => Next::Pass,
    }
}
