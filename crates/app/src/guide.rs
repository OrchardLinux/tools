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

/// Where the run is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moment {
    pub row: usize,
    pub glow: &'static [Button],
    /// 0…1 through the row's countdown; None for a row without one.
    pub progress: Option<f32>,
}

/// The run of `sequence`, `elapsed` seconds in; None once it is over.
pub fn at(sequence: Sequence, elapsed: f32) -> Option<Moment> {
    let phases = phases(sequence);
    let mut start = 0.;
    for (i, phase) in phases.iter().enumerate() {
        let end = start + phase.seconds;
        if elapsed < end {
            // A countdown spans every phase of its row (one row, one circle).
            let progress = phase.countdown.then(|| {
                let row_start = start - phases[..i].iter().rev().take_while(|p| p.row == phase.row).map(|p| p.seconds).sum::<f32>();
                let row_len: f32 = phases.iter().filter(|p| p.row == phase.row).map(|p| p.seconds).sum();
                ((elapsed - row_start) / row_len).clamp(0., 1.)
            });
            return Some(Moment { row: phase.row, glow: phase.glow, progress });
        }
        start = end;
    }
    None
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
    fn the_home_button_and_iphone_7_runs() {
        assert_eq!(length(Sequence::HomePower), 23.);
        assert_eq!(at(Sequence::HomePower, 5.).unwrap().glow, &[Home, Power]);
        assert_eq!(at(Sequence::HomePower, 14.).unwrap().glow, &[Home]);
        assert_eq!(at(Sequence::VolumeDownSide, 5.).unwrap().glow, &[VolumeDown, Power]);
        assert_eq!(at(Sequence::VolumeDownSide, 23.), None);
    }
}
