//! Everything the window shows, derived from the state and nothing else.
//!
//! The app animates the difference between two views: a new screen slides the
//! text column, a new model slides the device carousel, a hidden device slides
//! out, and screen, glyph and tint changes cross-fade on the persistent device.
use crate::{App, Again, Link, Model, Sequence, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Off,
    Lit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    None,
    Check,
    Spinner,
    /// Only while a system is starting.
    Wordmark,
    Sad,
    Unplugged,
    Alert,
    Usb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    Neutral,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceView {
    /// False when the content needs the full width (the storage slider).
    pub visible: bool,
    pub model: Model,
    pub screen: Screen,
    pub glyph: Glyph,
    pub tint: Tint,
    /// The buttons of this sequence are highlighted: the user has to press them.
    pub buttons: Option<Sequence>,
    /// The app is waiting for the device to show up.
    pub rings: bool,
    /// The no-Home-button tab before a device is known: an iPhone with a
    /// Home button drawn in front of the iPad, both taking these steps.
    pub pair: bool,
}

/// Which screen design the state shows; the text column's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    WaitingForDfu,
    WrongMode,
    SeveralInDfu,
    Unsupported,
    Found,
    SkipConfirmation,
    UsbAccess,
    WaitingForIpados,
    ChooseSplit,
    ChooseSplitLittleRoom,
    ChooseSplitNoRoom,
    Splitting,
    SplittingReopened,
    Reconnect,
    SplitDone,
    DfuAgain,
    CreateAccount,
    ConfirmErase,
    NotPrepared,
    Installing,
    InstallFailed,
    Starting,
    ChooseSystem,
    NothingInstalled,
}

/// What the window shows for `flow`: the state's screen, or the "more than
/// one device" screen while the installer refuses to go on because of
/// other devices on USB (Flow::refusing).
pub fn view_flow(app: App, flow: &crate::Flow) -> (DeviceView, ScreenKind) {
    if flow.refusing(app) {
        return view(app, &State::SeveralInDfu { devices: Vec::new() });
    }
    view(app, &flow.state)
}

pub fn view(app: App, state: &State) -> (DeviceView, ScreenKind) {
    use Glyph as G;
    use State as S;
    let model = |s: &State| s.device().map(|d| d.model);
    let off = |m: Model| DeviceView {
        visible: true,
        model: m,
        screen: Screen::Off,
        glyph: G::None,
        tint: Tint::Neutral,
        buttons: None,
        rings: false,
        pair: false,
    };
    let lit = |m: Model, glyph: Glyph| DeviceView { screen: Screen::Lit, glyph, ..off(m) };
    let error = |m: Model, glyph: Glyph| DeviceView { tint: Tint::Error, glyph, ..off(m) };
    let waiting = |m: Model, seq: Sequence| DeviceView { buttons: Some(seq), rings: true, ..off(m) };
    let m = model(state).unwrap_or(Model::IpadHome);

    match state {
        S::WaitingForDfu { tab, seen, leaving } => {
            // A known device draws as itself, also while it is leaving for
            // DFU; otherwise the tab picks the drawing.
            let (m, kind) = match seen.first().or(leaving.as_ref()) {
                Some(s) => (s.device.model, ScreenKind::WrongMode),
                None => (model_for(*tab), ScreenKind::WaitingForDfu),
            };
            let pair = kind == ScreenKind::WaitingForDfu && *tab == Sequence::VolumeSequence;
            (DeviceView { pair, ..waiting(m, *tab) }, kind)
        }
        S::SeveralInDfu { .. } => (off(Model::IpadHome), ScreenKind::SeveralInDfu),
        S::Unsupported { .. } => (DeviceView { glyph: G::Sad, ..off(m) }, ScreenKind::Unsupported),
        // Experimental support is a badge, not a different drawing.
        // The launcher starts it right away: a spinner, not a check.
        S::Found { .. } if app == App::Launcher => (lit(m, G::Spinner), ScreenKind::Found),
        S::Found { confirm_skip, .. } => {
            let kind = if *confirm_skip { ScreenKind::SkipConfirmation } else { ScreenKind::Found };
            (lit(m, G::Check), kind)
        }
        S::UsbAccessNeeded => (DeviceView { glyph: G::Usb, ..off(Model::IpadHome) }, ScreenKind::UsbAccess),
        S::WaitingForIpados { .. } => (lit(m, G::Spinner), ScreenKind::WaitingForIpados),
        S::ChooseSplit { layout, .. } => {
            let max = layout.max_omarchy_gb();
            let kind = if max < crate::MIN_OMARCHY_GB {
                ScreenKind::ChooseSplitNoRoom
            } else if max < crate::RECOMMENDED_OMARCHY_GB {
                ScreenKind::ChooseSplitLittleRoom
            } else {
                ScreenKind::ChooseSplit
            };
            (DeviceView { visible: false, ..lit(m, G::None) }, kind)
        }
        S::Splitting { link: Link::Lost, reopened: false, .. } => {
            (DeviceView { rings: true, ..error(m, G::Unplugged) }, ScreenKind::Reconnect)
        }
        S::Splitting { reopened: true, .. } => (lit(m, G::Spinner), ScreenKind::SplittingReopened),
        S::Splitting { .. } => (lit(m, G::Spinner), ScreenKind::Splitting),
        S::SplitDone { .. } => (DeviceView { visible: false, ..lit(m, G::None) }, ScreenKind::SplitDone),
        S::DfuAgain { device, why, seen } => {
            let v = waiting(m, device.sequence);
            // The device passing through another mode on its way to DFU
            // (it restarts) keeps 3.1: it asks for the same thing as 1.2, and
            // switching would replay the screen's slide-in.
            match (why, seen) {
                (Again::Split, None) => (error(m, G::Alert), ScreenKind::NotPrepared),
                _ => (v, ScreenKind::DfuAgain),
            }
        }
        // Busy while the install system starts; then just on, waiting for
        // the user (the form says when it's ready).
        S::CreateAccount { env: None, .. } => (lit(m, G::Spinner), ScreenKind::CreateAccount),
        S::CreateAccount { .. } => (lit(m, G::None), ScreenKind::CreateAccount),
        // A dialog over the account form: the device stays as it was (ready).
        S::ConfirmErase { .. } => (lit(m, G::None), ScreenKind::ConfirmErase),
        S::NotPrepared { .. } => (error(m, G::Alert), ScreenKind::NotPrepared),
        // Once the image verified the device needs no cable: no reconnect screen.
        S::Installing { link: Link::Lost, streamed: false, .. } => {
            (DeviceView { rings: true, ..error(m, G::Unplugged) }, ScreenKind::Reconnect)
        }
        S::Installing { .. } => (lit(m, G::Spinner), ScreenKind::Installing),
        S::InstallFailed { .. } => (error(m, G::Alert), ScreenKind::InstallFailed),
        // The launcher is neutral: no distro's wordmark, a check.
        S::Starting { .. } if app == App::Launcher => (lit(m, G::Check), ScreenKind::Starting),
        S::Starting { .. } => (lit(m, G::Wordmark), ScreenKind::Starting),
        S::ChooseSystem { .. } => (lit(m, G::None), ScreenKind::ChooseSystem),
        S::NothingInstalled { .. } => (off(m), ScreenKind::NothingInstalled),
    }
}

/// The drawing for a tab when no device is known yet.
fn model_for(tab: Sequence) -> Model {
    match tab {
        Sequence::HomePower => Model::IpadHome,
        Sequence::VolumeDownSide => Model::IphoneHome,
        Sequence::VolumeSequence => Model::IpadNoHome,
    }
}
