//! "Guide me" on the DFU screen (G.1–G.7 in installer-app.pen): the steps
//! of the selected button layout played in time. Each moment says which step
//! row is active, which buttons glow and how far the step's countdown is.
//! A pure function of the elapsed time, so the screen only reads it.
//!
//! The run: "Get ready" for 3 s in place of the cable step, then the button
//! steps; a step with a duration counts it down, a step of two presses in a
//! row ("Volume Up, then Volume Down") lights one button a second each. It
//! ends when the last step's countdown is full; "Let go when this window
//! changes" is never a step of its own.
use orchard_flow::Sequence;

/// A physical button, as the drawing shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Home,
    /// Top on iPads, Side on iPhones.
    Power,
    VolumeUp,
    VolumeDown,
}
use Button::*;

/// One stretch of the run.
struct Phase {
    /// The step row (0: the cable step, shown as "Get ready").
    row: usize,
    seconds: f32,
    glow: &'static [Button],
    /// Whether the row counts its time down (steps with a duration).
    countdown: bool,
}

const GET_READY: Phase = Phase { row: 0, seconds: 3., glow: &[], countdown: true };

/// The run of each button layout; rows as in screens.rs's step lists.
fn phases(sequence: Sequence) -> &'static [Phase] {
    match sequence {
        Sequence::HomePower => &[
            GET_READY,
            Phase { row: 1, seconds: 10., glow: &[Home, Power], countdown: true },
            Phase { row: 2, seconds: 10., glow: &[Home], countdown: true },
        ],
        Sequence::VolumeDownSide => &[
            GET_READY,
            Phase { row: 1, seconds: 10., glow: &[VolumeDown, Power], countdown: true },
            Phase { row: 2, seconds: 10., glow: &[VolumeDown], countdown: true },
        ],
        Sequence::VolumeSequence => &[
            GET_READY,
            Phase { row: 1, seconds: 1., glow: &[VolumeUp], countdown: false },
            Phase { row: 1, seconds: 1., glow: &[VolumeDown], countdown: false },
            Phase { row: 2, seconds: 10., glow: &[Power], countdown: true },
            Phase { row: 3, seconds: 5., glow: &[Power, VolumeDown], countdown: true },
            Phase { row: 4, seconds: 10., glow: &[VolumeDown], countdown: true },
        ],
    }
}

/// What the drawing needs of the run at a moment: each button's glow and
/// urgency, how far the run's look is faded in, and the time (for pulsing).
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub levels: [f32; 4],
    pub urgency: [f32; 4],
    pub fade: f32,
    pub seconds: f32,
}

/// Where the run is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moment {
    pub row: usize,
    pub glow: &'static [Button],
    /// 0…1 through the row's countdown; None for a row without one.
    pub progress: Option<f32>,
    /// Seconds left in the row's countdown.
    pub remaining: Option<f32>,
}

/// The run of `sequence`, `elapsed` seconds in; None once it is over.
pub fn at(sequence: Sequence, elapsed: f32) -> Option<Moment> {
    let phases = phases(sequence);
    let mut start = 0.;
    for (i, phase) in phases.iter().enumerate() {
        let end = start + phase.seconds;
        if elapsed < end {
            // A countdown spans every phase of its row (one row, one circle).
            let row_start = start - phases[..i].iter().rev().take_while(|p| p.row == phase.row).map(|p| p.seconds).sum::<f32>();
            let row_len: f32 = phases.iter().filter(|p| p.row == phase.row).map(|p| p.seconds).sum();
            let progress = phase.countdown.then(|| ((elapsed - row_start) / row_len).clamp(0., 1.));
            let remaining = phase.countdown.then(|| (row_start + row_len - elapsed).max(0.));
            return Some(Moment { row: phase.row, glow: phase.glow, progress, remaining });
        }
        start = end;
    }
    None
}

/// How long a glow takes to come on or go out.
const FADE: f32 = 0.7;

/// Each button's glow (Home, Power, Volume Up, Volume Down), 0…1, at
/// `elapsed`: it fades in where the button's stretch starts and out where it
/// ends; a button lit across adjacent phases stays lit in between.
pub fn levels(sequence: Sequence, elapsed: f32) -> [f32; 4] {
    let mut out = [0.; 4];
    for (slot, button) in [Home, Power, VolumeUp, VolumeDown].into_iter().enumerate() {
        let mut start = 0.;
        let mut lit: Option<f32> = None;
        let mut level: f32 = 0.;
        for phase in phases(sequence) {
            let on = phase.glow.contains(&button);
            match (on, lit) {
                (true, None) => lit = Some(start),
                (false, Some(from)) => {
                    level = level.max(window(from, start, elapsed));
                    lit = None;
                }
                _ => {}
            }
            start += phase.seconds;
        }
        if let Some(from) = lit {
            level = level.max(window(from, start, elapsed));
        }
        out[slot] = level;
    }
    out
}

/// 1 inside [from, to], easing in over FADE after `from` and out over FADE
/// before `to`.
fn window(from: f32, to: f32, t: f32) -> f32 {
    let ease = |x: f32| {
        let x = x.clamp(0., 1.);
        x * x * (3. - 2. * x)
    };
    ease((t - from) / FADE).min(ease((to - t) / FADE))
}

impl Button {
    /// Its slot in [`levels`].
    pub fn slot(self) -> usize {
        match self {
            Home => 0,
            Power => 1,
            VolumeUp => 2,
            VolumeDown => 3,
        }
    }
}

/// How long before a held button's release it starts pulsing.
const URGENT: f32 = 1.5;

/// Each button's urgency (Home, Power, Volume Up, Volume Down), 0…1: a
/// button held through a countdown pulses in the last URGENT seconds before
/// it is to be let go (it isn't held in the next phase, or the run ends).
/// Taps (phases without a countdown) don't.
pub fn urgency(sequence: Sequence, elapsed: f32) -> [f32; 4] {
    let phases = phases(sequence);
    let mut out = [0.; 4];
    let mut start = 0.;
    for (i, phase) in phases.iter().enumerate() {
        let end = start + phase.seconds;
        if elapsed >= start && elapsed < end && phase.countdown {
            let left = end - elapsed;
            if left < URGENT {
                for b in phase.glow {
                    let held_on = phases.get(i + 1).is_some_and(|next| next.glow.contains(b));
                    if !held_on {
                        out[b.slot()] = 1. - left / URGENT;
                    }
                }
            }
        }
        start = end;
    }
    out
}

/// How long the run of `sequence` takes.
pub fn length(sequence: Sequence) -> f32 {
    phases(sequence).iter().map(|p| p.seconds).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_no_home_button_run() {
        let s = Sequence::VolumeSequence;
        assert_eq!(length(s), 30.);
        let m = at(s, 1.5).unwrap();
        assert_eq!((m.row, m.glow, m.progress), (0, &[][..], Some(0.5)));
        assert_eq!(at(s, 3.5).unwrap().glow, &[VolumeUp]);
        let down = at(s, 4.5).unwrap();
        assert_eq!((down.row, down.glow, down.progress), (1, &[VolumeDown][..], None));
        let hold = at(s, 10.).unwrap();
        assert_eq!((hold.row, hold.glow), (2, &[Power][..]));
        assert!((hold.progress.unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(at(s, 17.).unwrap().glow, &[Power, VolumeDown]);
        assert_eq!(at(s, 29.9).unwrap().row, 4);
        assert_eq!(at(s, 30.), None);
    }

    #[test]
    fn glows_fade_and_stay_lit_across_steps() {
        let s = Sequence::VolumeSequence;
        let at = |t: f32| levels(s, t);
        // Volume Up 3–4 s: rising, then falling; at the hand-over to Volume
        // Down both are out, and Volume Down rises.
        assert!(at(3.2)[2] > 0. && at(3.2)[2] < at(3.5)[2]);
        assert!(at(4.)[2] < 0.01 && at(4.)[3] < 0.01);
        assert!(at(4.5)[3] > 0.5);
        // Power from 5 to 20 s, through the step change at 15 s without a dip.
        assert_eq!(at(15.)[1], 1.);
        assert_eq!(at(1.)[1], 0.);
    }

    #[test]
    fn held_buttons_pulse_before_they_are_let_go() {
        let s = Sequence::VolumeSequence;
        // Power: held 5–20 s, let go at 20: urgent from 18, not at 14 (held on at 15).
        assert_eq!(urgency(s, 14.)[1], 0.);
        assert_eq!(urgency(s, 18.5)[1], 0.);
        assert!((urgency(s, 19.25)[1] - 0.5).abs() < 1e-5);
        // Volume Down held on from 15 to the end: not urgent at 19, urgent at 29.
        assert_eq!(urgency(s, 19.)[3], 0.);
        assert!(urgency(s, 29.5)[3] > 0.6);
        // Taps never.
        assert_eq!(urgency(s, 3.9)[2], 0.);
    }

    #[test]
    fn the_home_button_and_iphone_7_runs() {
        assert_eq!(length(Sequence::HomePower), 23.);
        assert_eq!(at(Sequence::HomePower, 5.).unwrap().glow, &[Home, Power]);
        assert_eq!(at(Sequence::HomePower, 14.).unwrap().glow, &[Home]);
        assert_eq!(at(Sequence::VolumeDownSide, 5.).unwrap().glow, &[VolumeDown, Power]);
        assert_eq!(at(Sequence::VolumeDownSide, 23.), None);
    }
}
