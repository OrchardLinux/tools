//! "Guide me" on the DFU screen (G.1–G.7 in installer-app.pen): the steps
//! of the selected button layout played in time. Each moment says which step
//! row is active, which buttons glow and how far the step's countdown is.
//! A pure function of the elapsed time, so the screen only reads it.
//!
//! The run: "Get ready" for 2 s in place of the cable step, then the button
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

const GET_READY: Phase = Phase { row: 0, seconds: 2., glow: &[], countdown: true };

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
            // Two taps, each 0.8 s lit and a 0.2 s break, so they read as two.
            Phase { row: 1, seconds: 0.8, glow: &[VolumeUp], countdown: false },
            Phase { row: 1, seconds: 0.2, glow: &[], countdown: false },
            Phase { row: 1, seconds: 0.8, glow: &[VolumeDown], countdown: false },
            Phase { row: 1, seconds: 0.2, glow: &[], countdown: false },
            Phase { row: 2, seconds: 10., glow: &[Power], countdown: true },
            Phase { row: 3, seconds: 5., glow: &[Power, VolumeDown], countdown: true },
            Phase { row: 4, seconds: 10., glow: &[VolumeDown], countdown: true },
        ],
    }
}

/// What the drawing needs of the run at a moment: each button's glow and
/// how far the run's look is faded in.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub levels: [f32; 4],
    pub fade: f32,
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

/// How long a glow takes to come on or go out: slow for a held button,
/// quick for a tap.
const FADE: f32 = 0.7;
const FADE_TAP: f32 = 0.15;

/// Each button's glow (Home, Power, Volume Up, Volume Down), 0…1, at
/// `elapsed`: it fades in where the button's stretch starts and out where it
/// ends; a button lit across adjacent phases stays lit in between.
pub fn levels(sequence: Sequence, elapsed: f32) -> [f32; 4] {
    let mut out = [0.; 4];
    for (slot, button) in [Home, Power, VolumeUp, VolumeDown].into_iter().enumerate() {
        let mut start = 0.;
        // The button's lit stretch: where it began, and whether it is held
        // (any phase of it counts down) or a tap.
        let mut lit: Option<(f32, bool)> = None;
        let mut level: f32 = 0.;
        for phase in phases(sequence) {
            let on = phase.glow.contains(&button);
            match (on, lit) {
                (true, None) => lit = Some((start, phase.countdown)),
                (true, Some((from, held))) => lit = Some((from, held || phase.countdown)),
                (false, Some((from, held))) => {
                    level = level.max(window(from, start, elapsed, if held { FADE } else { FADE_TAP }));
                    lit = None;
                }
                _ => {}
            }
            start += phase.seconds;
        }
        if let Some((from, held)) = lit {
            level = level.max(window(from, start, elapsed, if held { FADE } else { FADE_TAP }));
        }
        out[slot] = level;
    }
    out
}

/// Easing in over `fade` after `from`, full until `to` (the release), then
/// easing out over `fade`: the glow stays whole while the button is held.
fn window(from: f32, to: f32, t: f32, fade: f32) -> f32 {
    let ease = |x: f32| {
        let x = x.clamp(0., 1.);
        x * x * (3. - 2. * x)
    };
    if t < from {
        return 0.;
    }
    ease((t - from) / fade).min(ease((to + fade - t) / fade))
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
        assert_eq!(length(s), 29.);
        let m = at(s, 1.).unwrap();
        assert_eq!((m.row, m.glow, m.progress), (0, &[][..], Some(0.5)));
        assert_eq!(at(s, 2.5).unwrap().glow, &[VolumeUp]);
        let down = at(s, 3.4).unwrap();
        assert_eq!((down.row, down.glow, down.progress), (1, &[VolumeDown][..], None));
        let hold = at(s, 9.).unwrap();
        assert_eq!((hold.row, hold.glow), (2, &[Power][..]));
        assert!((hold.progress.unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(at(s, 16.).unwrap().glow, &[Power, VolumeDown]);
        assert_eq!(at(s, 28.9).unwrap().row, 4);
        assert_eq!(at(s, 29.), None);
    }

    #[test]
    fn glows_stay_whole_while_held_and_fade_after() {
        let s = Sequence::VolumeSequence;
        let at = |t: f32| levels(s, t);
        // Volume Up 2–2.8 s, a tap: in and out within 0.15 s, then a break
        // with nothing lit before Volume Down at 3.
        assert!(at(2.05)[2] > 0. && at(2.05)[2] < 1.);
        assert_eq!(at(2.5)[2], 1.);
        assert_eq!(at(2.97)[2], 0.);
        assert_eq!(at(2.97)[3], 0.);
        assert_eq!(at(3.3)[3], 1.);
        // A held button fades slowly: Power half-way in after 0.35 s.
        assert!((at(4.35)[1] - 0.5).abs() < 1e-3);
        // Power from 4 to 19 s, whole through the step change at 14.
        assert_eq!(at(14.)[1], 1.);
        assert_eq!(at(18.9)[1], 1.);
        assert_eq!(at(1.)[1], 0.);
    }

    #[test]
    fn the_home_button_and_iphone_7_runs() {
        assert_eq!(length(Sequence::HomePower), 22.);
        assert_eq!(at(Sequence::HomePower, 5.).unwrap().glow, &[Home, Power]);
        assert_eq!(at(Sequence::HomePower, 14.).unwrap().glow, &[Home]);
        assert_eq!(at(Sequence::VolumeDownSide, 5.).unwrap().glow, &[VolumeDown, Power]);
        assert_eq!(at(Sequence::VolumeDownSide, 22.), None);
    }
}
