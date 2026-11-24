//! One test per screen and per edge case of the design (installer-app.pen).
use crate::*;

fn ipad6() -> Device {
    Device {
        id: DeviceId("0x1A2B".into()),
        board: "j71b".into(),
        name: "iPad (6th generation)".into(),
        model: Model::IpadHome,
        sequence: Sequence::HomePower,
        support: Support::Supported,
    }
}

fn ipad7() -> Device {
    Device { id: DeviceId("0x3C4D".into()), name: "iPad (7th generation)".into(), ..ipad6() }
}

fn ipad8() -> Device {
    Device { id: DeviceId("0x5E6F".into()), name: "iPad (8th generation)".into(), support: Support::Unsupported, ..ipad6() }
}

fn fresh() -> Layout {
    Layout { total_gb: 128, ipados_gb: 128, ipados_used_gb: 41, outside: vec![] }
}

fn split_before() -> Layout {
    Layout {
        total_gb: 128,
        ipados_gb: 64,
        ipados_used_gb: 41,
        outside: vec![
            Partition { name: "BOOT".into(), filesystem: "FAT32".into(), gb: 1 },
            Partition { name: "omarchy-a".into(), filesystem: "ext4".into(), gb: 63 },
        ],
    }
}

fn account() -> Account {
    Account {
        username: "tablet".into(),
        password: "correct horse".into(),
        password_again: "correct horse".into(),
        pin: "123456".into(),
        pin_again: "123456".into(),
        ssh: false,
    }
}

/// Runs events through `update` and returns the final state and all effects.
fn run(app: App, state: State, events: Vec<Event>) -> (State, Vec<Effect>) {
    let mut effects = Vec::new();
    let mut state = state;
    for event in events {
        let (next, mut e) = update(app, state, event);
        state = next;
        effects.append(&mut e);
    }
    (state, effects)
}

fn dfu(device: Device) -> Event {
    Event::Attached { device, mode: UsbMode::Dfu }
}

fn kind(app: App, state: &State) -> ScreenKind {
    view(app, state).1
}

// ---- 1 Connect ----

#[test]
fn waiting_highlights_buttons_and_shows_rings() {
    let (d, k) = view(App::Installer, &State::start());
    assert_eq!(k, ScreenKind::WaitingForDfu);
    assert!(d.rings && d.visible);
    assert_eq!(d.buttons, Some(Sequence::HomePower));
    assert_eq!(d.screen, Screen::Off);
}

#[test]
fn tabs_change_the_drawing() {
    let (s, _) = update(App::Installer, State::start(), Event::SelectTab(Sequence::VolumeSequence));
    assert_eq!(view(App::Installer, &s).0.model, Model::IpadNoHome);
}

#[test]
fn dfu_found_lights_the_screen_with_a_check() {
    let (s, e) = update(App::Installer, State::start(), dfu(ipad6()));
    assert!(matches!(s, State::Found { .. }));
    assert!(e.is_empty());
    let (d, k) = view(App::Installer, &s);
    assert_eq!((k, d.screen, d.glyph), (ScreenKind::Found, Screen::Lit, Glyph::Check));
}

#[test]
fn device_in_recovery_selects_its_tab_and_keeps_waiting() {
    let mut iphone7 = ipad6();
    iphone7.sequence = Sequence::VolumeDownSide;
    iphone7.model = Model::IphoneHome;
    let (s, _) = update(App::Installer, State::start(), Event::Attached { device: iphone7, mode: UsbMode::Recovery });
    assert_eq!(kind(App::Installer, &s), ScreenKind::WrongMode);
    assert!(matches!(s, State::WaitingForDfu { tab: Sequence::VolumeDownSide, .. }));
}

#[test]
fn unsupported_device_gets_its_own_screen() {
    let (s, _) = update(App::Installer, State::start(), dfu(ipad8()));
    assert_eq!(kind(App::Installer, &s), ScreenKind::Unsupported);
    assert_eq!(view(App::Installer, &s).0.glyph, Glyph::Sad);
}

#[test]
fn several_in_dfu_asks_to_unplug_until_one_is_left() {
    let (s, _) = run(App::Installer, State::start(), vec![dfu(ipad6()), dfu(ipad7())]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::SeveralInDfu);
    let (s, _) = update(App::Installer, s, Event::Detached { id: ipad7().id });
    assert!(matches!(s, State::Found { ref device, .. } if device.id == ipad6().id));
}

#[test]
fn unplugging_the_found_device_goes_back_to_waiting() {
    let (s, _) = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Detached { id: ipad6().id }]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForDfu);
}

#[test]
fn skip_needs_confirmation_then_installs_from_dfu() {
    let (s, _) = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Skip]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::SkipConfirmation);
    let (cancelled, _) = update(App::Installer, s.clone(), Event::CancelSkip);
    assert_eq!(kind(App::Installer, &cancelled), ScreenKind::Found);
    let (s, e) = update(App::Installer, s, Event::ConfirmSkip);
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount);
    assert_eq!(e, vec![Effect::BootInstallEnv(ipad6().id)]);
}

#[test]
fn usb_access_denied_shows_the_access_screen_from_anywhere() {
    let (s, e) = update(App::Installer, State::start(), Event::UsbAccessDenied);
    assert_eq!(kind(App::Installer, &s), ScreenKind::UsbAccess);
    assert!(e.is_empty(), "no password prompt until the user asks for it");
    let (s, e) = update(App::Installer, s, Event::Continue);
    assert_eq!(e, vec![Effect::RequestUsbAccess]);
    let (s, _) = update(App::Installer, s, Event::UsbAccessGranted);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForDfu);
}

// ---- 2 Prepare ----

fn choose_split(layout: Layout) -> State {
    run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Continue, Event::IpadosReady { layout }]).0
}

#[test]
fn continue_boots_ipados_with_the_overlay() {
    let (s, e) = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Continue]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForIpados);
    assert_eq!(e, vec![Effect::BootIpadosWithOverlay(ipad6().id)]);
    assert_eq!(view(App::Installer, &s).0.glyph, Glyph::Spinner);
}

#[test]
fn split_screen_hides_the_device_and_suggests_half() {
    let s = choose_split(fresh());
    let (d, k) = view(App::Installer, &s);
    assert!(!d.visible);
    assert_eq!(k, ScreenKind::ChooseSplit);
    assert!(matches!(s, State::ChooseSplit { omarchy_gb: 64, .. }), "half of 128 GB");
}

#[test]
fn an_existing_split_opens_at_its_current_size() {
    assert!(matches!(choose_split(split_before()), State::ChooseSplit { omarchy_gb: 64, .. }));
}

#[test]
fn little_room_and_no_room_are_their_own_screens() {
    let little = Layout { ipados_used_gb: 110, ..fresh() };
    assert_eq!(kind(App::Installer, &choose_split(little)), ScreenKind::ChooseSplitLittleRoom);
    let none = Layout { ipados_used_gb: 124, ..fresh() };
    let s = choose_split(none);
    assert_eq!(kind(App::Installer, &s), ScreenKind::ChooseSplitNoRoom);
    let (s, e) = run(App::Installer, s, vec![Event::Approve(true), Event::Continue]);
    assert!(e.is_empty(), "no room: Continue does nothing");
    assert_eq!(kind(App::Installer, &s), ScreenKind::ChooseSplitNoRoom);
}

#[test]
fn split_needs_approval_and_clamps_the_size() {
    let s = choose_split(fresh());
    let (s, e) = update(App::Installer, s, Event::Continue);
    assert!(e.is_empty(), "not approved yet");
    let (s, _) = update(App::Installer, s, Event::SetOmarchySize(500));
    assert!(matches!(s, State::ChooseSplit { omarchy_gb: 85, .. }), "clamped to what iPadOS can spare");
    let (s, e) = run(App::Installer, s, vec![Event::Approve(true), Event::Continue]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::Splitting);
    assert_eq!(e, vec![Effect::StartSplit { omarchy_gb: 85 }]);
}

fn splitting() -> State {
    run(App::Installer, choose_split(fresh()), vec![Event::Approve(true), Event::Continue]).0
}

#[test]
fn unplug_during_split_asks_to_reconnect_in_red() {
    let (s, _) = update(App::Installer, splitting(), Event::Detached { id: ipad6().id });
    let (d, k) = view(App::Installer, &s);
    assert_eq!(k, ScreenKind::Reconnect);
    assert_eq!((d.tint, d.glyph, d.rings), (Tint::Error, Glyph::Unplugged, true));
}

#[test]
fn replug_into_the_running_companion_never_restarts_dfu() {
    let (s, e) = run(
        App::Installer,
        splitting(),
        vec![Event::Detached { id: ipad6().id }, Event::Attached { device: ipad6(), mode: UsbMode::Normal }],
    );
    assert_eq!(kind(App::Installer, &s), ScreenKind::Splitting);
    assert!(e.is_empty());
}

#[test]
fn a_device_that_reset_during_the_split_must_be_checked_from_dfu() {
    let (s, _) = run(App::Installer, splitting(), vec![Event::Detached { id: ipad6().id }, dfu(ipad6())]);
    assert!(matches!(s, State::DfuAgain { why: Again::CheckSplit, .. }));
    let (s, e) = update(App::Installer, s, dfu(ipad6()));
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForIpados);
    assert_eq!(e, vec![Effect::BootIpadosWithOverlay(ipad6().id)]);
}

#[test]
fn reopening_the_app_resumes_a_running_split() {
    let (s, _) = update(App::Installer, State::start(), Event::SplitRunning { device: ipad6(), omarchy_gb: 32 });
    assert_eq!(kind(App::Installer, &s), ScreenKind::SplittingReopened);
}

#[test]
fn finished_split_then_dfu_again_for_install() {
    let (s, e) = update(App::Installer, splitting(), Event::SplitFinished { ok: true });
    assert_eq!(kind(App::Installer, &s), ScreenKind::SplitDone);
    assert_eq!(e, vec![Effect::Notify("The storage is split.".into())]);
    let (s, _) = update(App::Installer, s, Event::Continue);
    let (d, k) = view(App::Installer, &s);
    assert_eq!(k, ScreenKind::DfuAgain);
    assert!(d.visible && d.rings && d.buttons.is_some(), "the device slides back in, waiting");
}

// ---- 3 Install ----

fn create_account() -> State {
    let s = run(App::Installer, splitting(), vec![Event::SplitFinished { ok: true }, Event::Continue]).0;
    update(App::Installer, s, dfu(ipad6())).0
}

fn installed_layout() -> Layout {
    Layout { total_gb: 128, ipados_gb: 96, ipados_used_gb: 41, outside: vec![] }
}

#[test]
fn install_env_boots_while_the_account_is_filled_in() {
    let s = create_account();
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount);
    let (s, e) = update(App::Installer, s, Event::EnvReady { layout: installed_layout() });
    assert_eq!(e, vec![Effect::PrepareInstall], "writing starts before Install is pressed");
    let (s, e) = run(App::Installer, s, vec![Event::EditAccount(account()), Event::Continue]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::Installing);
    assert_eq!(e, vec![Effect::StartInstall(account())]);
}

#[test]
fn account_needs_matching_repeats() {
    let mut a = account();
    a.pin_again = "654321".into();
    let s = run(App::Installer, create_account(), vec![Event::EnvReady { layout: installed_layout() }]).0;
    let (s, e) = run(App::Installer, s, vec![Event::EditAccount(a), Event::Continue]);
    assert!(e.is_empty());
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount);
}

#[test]
fn partitions_to_erase_are_confirmed_on_install_not_while_typing() {
    let s = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Skip, Event::ConfirmSkip]).0;
    let (s, e) = update(App::Installer, s, Event::EnvReady { layout: split_before() });
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount, "the form stays");
    assert!(e.is_empty(), "nothing written yet");
    let (s, e) = run(App::Installer, s, vec![Event::EditAccount(account()), Event::Continue]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::ConfirmErase);
    assert!(e.is_empty());
    let (s, e) = update(App::Installer, s, Event::Continue);
    assert!(e.is_empty(), "not approved");
    // Cancel: back to the form with the account kept.
    let (back, _) = update(App::Installer, s.clone(), Event::Cancel);
    assert!(matches!(&back, State::CreateAccount { account: a, erase_approved: false, .. } if *a == account()));
    let (s, e) = run(App::Installer, s, vec![Event::Approve(true), Event::Continue]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::Installing);
    assert_eq!(e, vec![Effect::PrepareInstall, Effect::StartInstall(account())]);
}

#[test]
fn the_install_never_overtakes_a_preparation_waiting_for_its_files() {
    let skip = vec![Event::Started, dfu(ipad6()), Event::Skip, Event::ConfirmSkip];
    let (flow, _) = steps(App::Installer, skip);
    let (flow, _) = step(App::Installer, flow, Event::EnvReady { layout: installed_layout() });
    assert!(flow.write_waiting);
    let (flow, _) = step(App::Installer, flow, Event::EditAccount(account()));
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(!e.iter().any(|e| matches!(e, Effect::StartInstall(_))), "held behind the preparation");
    let mut flow = flow;
    for asset in [Asset::RootImage, Asset::PlatformLayer, Asset::Firmware] {
        flow = step(App::Installer, flow, Event::AssetReady(asset)).0;
    }
    let (_, e) = step(App::Installer, flow, Event::AssetReady(Asset::KernelPackage));
    assert_eq!(e, vec![Effect::PrepareInstall, Effect::StartInstall(account())]);
}

#[test]
fn skipped_split_without_room_leads_back_to_splitting() {
    let s = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Skip, Event::ConfirmSkip]).0;
    let (s, _) = update(App::Installer, s, Event::EnvReady { layout: fresh() });
    assert_eq!(kind(App::Installer, &s), ScreenKind::NotPrepared);
    assert_eq!(view(App::Installer, &s).0.tint, Tint::Error);
    let (s, _) = run(App::Installer, s, vec![Event::Continue, dfu(ipad6())]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForIpados);
}

fn installing() -> State {
    let s = run(App::Installer, create_account(), vec![Event::EnvReady { layout: installed_layout() }]).0;
    run(App::Installer, s, vec![Event::EditAccount(account()), Event::Continue]).0
}

#[test]
fn install_reports_progress_and_starts_the_system() {
    let (s, _) = update(App::Installer, installing(), Event::InstallProgress(64));
    assert!(matches!(s, State::Installing { percent: 64, .. }));
    let (s, _) = update(App::Installer, s, Event::InstallFinished { ok: true });
    let (d, k) = view(App::Installer, &s);
    assert_eq!((k, d.glyph), (ScreenKind::Starting, Glyph::Wordmark));
}

#[test]
fn unplug_during_install_reconnects_without_restarting() {
    let (s, e) = run(
        App::Installer,
        installing(),
        vec![Event::Detached { id: ipad6().id }, Event::Attached { device: ipad6(), mode: UsbMode::Normal }],
    );
    assert_eq!(kind(App::Installer, &s), ScreenKind::Installing);
    assert!(e.is_empty());
}

#[test]
fn failed_install_retries_from_dfu() {
    let (s, _) = update(App::Installer, installing(), Event::InstallFinished { ok: false });
    assert_eq!(kind(App::Installer, &s), ScreenKind::InstallFailed);
    let (s, e) = run(App::Installer, s, vec![Event::Retry, dfu(ipad6())]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount);
    assert_eq!(e, vec![Effect::BootInstallEnv(ipad6().id)]);
}

// ---- Launcher ----

fn entry(id: &str, title: &str, default: bool) -> Entry {
    Entry { id: id.into(), title: title.into(), kernel: "6.12.4".into(), default }
}

#[test]
fn launcher_lists_entries_when_the_device_is_found() {
    let (_, e) = update(App::Launcher, State::start(), dfu(ipad6()));
    assert_eq!(e, vec![Effect::ListEntries(ipad6().id)]);
}

#[test]
fn launcher_starts_a_single_entry_immediately() {
    let s = update(App::Launcher, State::start(), dfu(ipad6())).0;
    let (s, e) = update(App::Launcher, s, Event::EntriesListed(vec![entry("omarchy-a-6.12.4", "Omarchy A", true)]));
    assert!(matches!(s, State::Starting { ref title, .. } if title == "Omarchy A"));
    assert_eq!(e, vec![Effect::BootEntry("omarchy-a-6.12.4".into())]);
}

#[test]
fn launcher_lets_the_user_choose_between_several_entries() {
    let s = update(App::Launcher, State::start(), dfu(ipad6())).0;
    let entries = vec![entry("a", "Omarchy A", false), entry("b", "Omarchy A", true), entry("c", "Minimal Linux", false)];
    let (s, _) = update(App::Launcher, s, Event::EntriesListed(entries));
    assert!(matches!(s, State::ChooseSystem { selected: 1, .. }), "the default is preselected");
    let (s, e) = run(App::Launcher, s, vec![Event::SelectEntry(2), Event::Continue]);
    assert!(matches!(s, State::Starting { ref title, .. } if title == "Minimal Linux"));
    assert_eq!(e, vec![Effect::BootEntry("c".into())]);
}

#[test]
fn launcher_keeps_looking_while_the_chain_drops_the_device_off_usb() {
    // As on the iPad 6 (2026-11-23): palera1n's exploit re-enumerates the
    // device a few times while the chain runs. The screen stays, and the
    // chain is not started again.
    let s = update(App::Launcher, State::start(), dfu(ipad6())).0;
    let (s, e) = run(App::Launcher, s, vec![Event::Detached { id: ipad6().id }, dfu(ipad6()), Event::Detached { id: ipad6().id }, dfu(ipad6())]);
    assert!(matches!(s, State::Found { .. }), "still looking, not back to Start your device");
    assert!(!e.iter().any(|e| matches!(e, Effect::ListEntries(_))), "the chain runs once");
    let (s, _) = update(App::Launcher, s, Event::EntriesListed(vec![entry("a", "Omarchy A", true)]));
    assert!(matches!(s, State::Starting { .. }));
}

#[test]
fn launcher_with_nothing_installed_says_so() {
    let s = update(App::Launcher, State::start(), dfu(ipad6())).0;
    let (s, _) = update(App::Launcher, s, Event::EntriesListed(vec![]));
    assert_eq!(kind(App::Launcher, &s), ScreenKind::NothingInstalled);
}

// ---- Assets ----

fn steps(app: App, events: Vec<Event>) -> (Flow, Vec<Effect>) {
    let mut flow = Flow::new();
    let mut effects = Vec::new();
    for event in events {
        let (next, mut e) = step(app, flow, event);
        flow = next;
        effects.append(&mut e);
    }
    (flow, effects)
}

fn fetches(effects: &[Effect]) -> Vec<Asset> {
    effects.iter().filter_map(|e| if let Effect::Fetch(a) = e { Some(*a) } else { None }).collect()
}

#[test]
fn root_image_and_palera1n_start_at_launch() {
    let (_, e) = steps(App::Installer, vec![Event::Started]);
    assert_eq!(fetches(&e), vec![Asset::RootImage, Asset::Palera1n]);
}

#[test]
fn board_assets_start_once_the_board_is_known() {
    let (flow, e) = steps(App::Installer, vec![Event::Started, dfu(ipad6())]);
    assert_eq!(fetches(&e), vec![Asset::RootImage, Asset::Palera1n, Asset::PlatformLayer, Asset::Firmware, Asset::Chain, Asset::KernelPackage]);
    let (_, again) = step(App::Installer, flow, Event::Skip);
    assert!(fetches(&again).is_empty(), "nothing starts twice");
}

#[test]
fn unsupported_devices_fetch_nothing_for_the_board() {
    let (_, e) = steps(App::Installer, vec![dfu(ipad8())]);
    assert!(fetches(&e).is_empty());
}

#[test]
fn continue_also_starts_the_overlay() {
    let (_, e) = steps(App::Installer, vec![dfu(ipad6()), Event::Continue]);
    assert!(fetches(&e).contains(&Asset::Overlay));
    assert!(!e.contains(&Effect::BootIpadosWithOverlay(ipad6().id)), "held until the overlay is built");
}

#[test]
fn writing_waits_for_the_system_and_the_boards_files() {
    let skip = vec![Event::Started, dfu(ipad6()), Event::Skip, Event::ConfirmSkip];
    let (flow, _) = steps(App::Installer, skip);
    let (flow, e) = step(App::Installer, flow, Event::EnvReady { layout: installed_layout() });
    assert!(!e.contains(&Effect::PrepareInstall), "assets are still downloading");
    assert!(flow.write_waiting);
    assert_eq!(flow.waiting_for(), Some(Asset::Palera1n));
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::RootImage));
    assert!(e.is_empty());
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::PlatformLayer));
    assert!(e.is_empty());
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::Firmware));
    assert!(e.is_empty(), "the kernel package is still resolving");
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::KernelPackage));
    assert_eq!(e, vec![Effect::PrepareInstall]);
    assert!(!flow.write_waiting);
}

#[test]
fn a_failed_asset_is_reported_and_retried() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6())]);
    let (flow, _) = step(App::Installer, flow, Event::AssetFailed(Asset::Firmware, "IPSW range request failed".into()));
    assert_eq!(flow.assets.failed(), Some((Asset::Firmware, "IPSW range request failed")));
    let (flow, e) = step(App::Installer, flow, Event::RetryAssets);
    assert_eq!(e, vec![Effect::Fetch(Asset::Firmware)]);
    assert_eq!(flow.assets.failed(), None);
}

#[test]
fn palera1n_pins_cover_every_supported_host() {
    for (os, arch) in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("linux", "x86_64")] {
        let pin = assets::palera1n_pin(os, arch).expect("pinned");
        assert_eq!(pin.sha256.len(), 64);
        assert!(pin.asset.ends_with(".dmg") || pin.asset.ends_with(".tar.gz"));
        assert!(pin.member.is_some());
    }
    assert!(assets::PALERA1N_RELEASE.ends_with("v3.0.0-beta.4"));
}

// ---- Waits, chain failures, other modes, records ----

#[test]
fn slow_ipados_boot_flags_the_wait_and_detaches_while_booting_are_ignored() {
    let (s, _) = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Continue, Event::Detached { id: ipad6().id }]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForIpados, "re-enumeration while booting is normal");
    let (s, _) = update(App::Installer, s, Event::Timeout);
    assert!(s.slow());
}

#[test]
fn a_failed_ipados_boot_goes_back_to_dfu_and_retries_the_overlay() {
    let (s, _) = run(App::Installer, State::start(), vec![dfu(ipad6()), Event::Continue, Event::ChainFailed("palera1n exited".into())]);
    assert!(matches!(s, State::DfuAgain { why: Again::RetryPrepare, .. }));
    assert_eq!(kind(App::Installer, &s), ScreenKind::DfuAgain);
    let (s, e) = update(App::Installer, s, dfu(ipad6()));
    assert_eq!(kind(App::Installer, &s), ScreenKind::WaitingForIpados);
    assert_eq!(e, vec![Effect::BootIpadosWithOverlay(ipad6().id)]);
}

#[test]
fn a_failed_install_env_boot_goes_back_to_dfu() {
    let (s, _) = update(App::Installer, create_account(), Event::ChainFailed("pongo did not answer".into()));
    assert!(matches!(s, State::DfuAgain { why: Again::Retry, .. }));
}

#[test]
fn dfu_again_stays_while_the_device_passes_through_another_mode() {
    let s = run(App::Installer, splitting(), vec![Event::SplitFinished { ok: true }, Event::Continue]).0;
    let (s, _) = update(App::Installer, s, Event::Attached { device: ipad6(), mode: UsbMode::Normal });
    assert_eq!(kind(App::Installer, &s), ScreenKind::DfuAgain, "no switch, no replayed slide-in");
    let (s, _) = update(App::Installer, s, dfu(ipad6()));
    assert_eq!(kind(App::Installer, &s), ScreenKind::CreateAccount);
}

#[test]
fn a_split_the_engine_stopped_is_chosen_again_but_a_restart_needs_dfu() {
    // iPadOS still runs: back to choosing the split, with the layout as it is now.
    let (s, _) = update(App::Installer, splitting(), Event::SplitFinished { ok: false });
    assert!(matches!(s, State::WaitingForIpados { .. }));
    let (s, _) = update(App::Installer, s, Event::IpadosReady { layout: fresh() });
    assert_eq!(kind(App::Installer, &s), ScreenKind::ChooseSplit);
    // The link was lost (it may have restarted): DFU lets the installer look.
    let lost = update(App::Installer, splitting(), Event::Detached { id: ipad6().id }).0;
    let (s, _) = update(App::Installer, lost, Event::SplitFinished { ok: false });
    assert!(matches!(s, State::DfuAgain { why: Again::CheckSplit, .. }));
}

#[test]
fn records_are_written_for_resume_and_completion() {
    let (_, e) = update(App::Installer, installing(), Event::InstallFinished { ok: true });
    assert!(e.contains(&Effect::Notify("the system is installed.".into())));
    let (_, e) = update(App::Installer, splitting(), Event::SplitFinished { ok: false });
    assert!(e.is_empty());
}

#[test]
fn after_the_image_verified_a_lost_cable_is_fine_and_the_installed_system_ends_it() {
    let (s, _) = run(App::Installer, installing(), vec![Event::InstallProgress(40), Event::Streamed, Event::InstallStep("account".into())]);
    assert!(matches!(s, State::Installing { streamed: true, step: Some(ref st), .. } if st == "account"));
    let (s, _) = update(App::Installer, s, Event::Detached { id: ipad6().id });
    assert_eq!(kind(App::Installer, &s), ScreenKind::Installing, "no reconnect screen once streamed");
    let (s, e) = update(App::Installer, s, Event::Attached { device: ipad6(), mode: UsbMode::Installed });
    assert_eq!(kind(App::Installer, &s), ScreenKind::Starting);
    assert!(e.contains(&Effect::Notify("the system is installed.".into())));
}

#[test]
fn a_cut_off_stream_restarts_from_the_beginning_on_reconnect() {
    let (s, _) = run(App::Installer, installing(), vec![Event::InstallProgress(60), Event::Detached { id: ipad6().id }]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::Reconnect);
    let (s, e) = run(App::Installer, s, vec![Event::Attached { device: ipad6(), mode: UsbMode::Install }, Event::Reconnected { stream_incomplete: true }]);
    assert_eq!(e, vec![Effect::RestartStream]);
    assert!(matches!(s, State::Installing { percent: 0, link: Link::Up, .. }));
}

#[test]
fn done_quits() {
    let s = update(App::Installer, installing(), Event::InstallFinished { ok: true }).0;
    let (_, e) = update(App::Installer, s, Event::Continue);
    assert_eq!(e, vec![Effect::Quit]);
}

#[test]
fn only_waits_that_can_run_long_have_a_timeout() {
    assert!(crate::timeout(&splitting()).is_none());
    let waiting = State::WaitingForIpados { device: ipad6(), slow: false };
    assert!(crate::timeout(&waiting).is_some());
    let (slow, _) = update(App::Installer, waiting, Event::Timeout);
    assert!(crate::timeout(&slow).is_none(), "once slow, no second timeout");
}

#[test]
fn a_device_leaving_for_dfu_keeps_its_screen_for_a_while() {
    let normal = Event::Attached { device: ipad6(), mode: UsbMode::Normal };
    let (s, _) = run(App::Installer, State::start(), vec![normal, Event::Detached { id: ipad6().id }]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::WrongMode, "still 1.2 while the combination runs");
    assert!(crate::timeout(&s).is_some());
    // It comes back in DFU: found.
    let (found, _) = update(App::Installer, s.clone(), dfu(ipad6()));
    assert_eq!(kind(App::Installer, &found), ScreenKind::Found);
    // It comes back in another mode: 1.2 for that mode, the grace period over.
    let (other, _) = update(App::Installer, s.clone(), Event::Attached { device: ipad6(), mode: UsbMode::Recovery });
    assert_eq!(kind(App::Installer, &other), ScreenKind::WrongMode);
    assert!(matches!(&other, State::WaitingForDfu { leaving: None, seen, .. } if seen[0].mode == UsbMode::Recovery));
    assert!(crate::timeout(&other).is_none());
    // It doesn't: back to 1.1.
    let (back, _) = update(App::Installer, s, Event::Timeout);
    assert_eq!(kind(App::Installer, &back), ScreenKind::WaitingForDfu);
}

#[test]
fn check_again_reads_the_storage_anew() {
    let (s, e) = update(App::Installer, choose_split(fresh()), Event::Retry);
    assert_eq!(e, vec![Effect::RefreshLayout]);
    assert_eq!(kind(App::Installer, &s), ScreenKind::ChooseSplit);
}

#[test]
fn the_device_spins_while_the_install_system_starts_and_rests_once_ready() {
    let account = State::CreateAccount { device: ipad6(), account: Account::default(), env: None, erase_approved: false, slow: false };
    assert_eq!(view(App::Installer, &account).0.glyph, Glyph::Spinner);
    let ready = State::CreateAccount { device: ipad6(), account: Account::default(), env: Some(fresh()), erase_approved: false, slow: false };
    assert_eq!(view(App::Installer, &ready).0.glyph, Glyph::None);
}

#[test]
fn palera1n_boots_ipados_only_once_the_overlay_is_built() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6())]);
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(!e.iter().any(|e| matches!(e, Effect::BootIpadosWithOverlay(_))), "the overlay isn't built yet");
    assert!(fetches(&e).contains(&Asset::Overlay));
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::Palera1n));
    assert!(!e.iter().any(|e| matches!(e, Effect::BootIpadosWithOverlay(_))));
    let (flow, e) = step(App::Installer, flow, Event::AssetReady(Asset::Overlay));
    assert_eq!(e, vec![Effect::BootIpadosWithOverlay(ipad6().id)]);
    assert!(flow.boot_waiting.is_none());
}

#[test]
fn no_split_starts_while_the_companion_reconnects() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Continue]);
    let (flow, _) = step(App::Installer, flow, Event::IpadosReady { layout: fresh() });
    assert!(flow.companion);
    let (flow, _) = step(App::Installer, flow, Event::Approve(true));
    let (flow, _) = step(App::Installer, flow, Event::CompanionLost);
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(e.is_empty() && matches!(flow.state, State::ChooseSplit { .. }), "held while disconnected");
    let (flow, _) = step(App::Installer, flow, Event::CompanionBack);
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(e.iter().any(|e| matches!(e, Effect::StartSplit { .. })));
    assert!(matches!(flow.state, State::Splitting { .. }));
}

#[test]
fn a_refused_split_keeps_its_reason_until_the_split_changes() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Continue]);
    let (flow, _) = step(App::Installer, flow, Event::IpadosReady { layout: fresh() });
    let (flow, _) = step(App::Installer, flow, Event::Approve(true));
    let (flow, _) = step(App::Installer, flow, Event::Continue);
    assert!(matches!(flow.state, State::Splitting { .. }));
    let (flow, _) = step(App::Installer, flow, Event::SplitRefused("too big".into()));
    let (flow, _) = step(App::Installer, flow, Event::IpadosReady { layout: fresh() });
    assert!(matches!(flow.state, State::ChooseSplit { .. }));
    assert_eq!(flow.split_problem.as_deref(), Some("too big"));
    let (flow, _) = step(App::Installer, flow, Event::Approve(true));
    assert!(flow.split_problem.is_none());
}

fn with_install_system() -> Flow {
    steps(App::Installer, vec![Event::Started, Event::Attached { device: ipad6(), mode: UsbMode::Install }]).0
}

#[test]
fn an_install_system_found_at_start_continues_by_its_phase() {
    // Plan written, account staged: stream again from the start.
    let (flow, e) = step(App::Installer, with_install_system(), Event::Reconnected { stream_incomplete: true });
    assert!(matches!(flow.state, State::Installing { percent: 0, .. }));
    assert!(e.contains(&Effect::RestartStream));
    // Image in: it finishes by itself.
    let (flow, _) = step(App::Installer, with_install_system(), Event::Reconnected { stream_incomplete: false });
    let (flow, _) = step(App::Installer, flow, Event::Streamed);
    assert!(matches!(flow.state, State::Installing { streamed: true, .. }));
    // Done and failed.
    let (flow, e) = step(App::Installer, with_install_system(), Event::InstallFinished { ok: true });
    assert!(matches!(flow.state, State::Starting { .. }) && e.contains(&Effect::Notify("the system is installed.".into())));
    let (flow, _) = step(App::Installer, with_install_system(), Event::InstallRefused("disk".into()));
    assert!(matches!(flow.state, State::InstallFailed { .. }));
    assert_eq!(flow.install_problem.as_deref(), Some("disk"));
    // Nothing written yet: the account form, as after a boot.
    let (flow, _) = step(App::Installer, with_install_system(), Event::EnvReady { layout: installed_layout() });
    assert!(matches!(flow.state, State::CreateAccount { env: Some(_), erase_approved: false, .. }));
}

#[test]
fn reopening_the_app_on_a_ready_install_system_prepares_into_free_space() {
    let free = Layout { total_gb: 128, ipados_gb: 64, ipados_used_gb: 20, outside: Vec::new() };
    let (flow, e) = step(App::Installer, with_install_system(), Event::EnvReady { layout: free });
    assert!(matches!(flow.state, State::CreateAccount { env: Some(_), .. }));
    // Preparing starts (after the assets it needs).
    assert!(!e.is_empty());
    let (flow, _) = step(App::Installer, flow, Event::EditAccount(account()));
    let (flow, _) = step(App::Installer, flow, Event::Continue);
    assert!(matches!(flow.state, State::Installing { .. }));
    // Too little room: not prepared.
    let small = Layout { total_gb: 64, ipados_gb: 60, ipados_used_gb: 20, outside: Vec::new() };
    let (flow, _) = step(App::Installer, with_install_system(), Event::EnvReady { layout: small });
    assert!(matches!(flow.state, State::NotPrepared { .. }));
}

#[test]
fn a_plan_without_the_account_asks_for_it_and_continues() {
    let (flow, _) = step(App::Installer, with_install_system(), Event::AccountNeeded);
    assert!(matches!(flow.state, State::CreateAccount { .. }) && flow.resuming);
    let (flow, _) = step(App::Installer, flow, Event::EditAccount(account()));
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(matches!(flow.state, State::Installing { .. }) && !flow.resuming);
    assert!(e.contains(&Effect::StartInstall(account())));
}

#[test]
fn an_install_system_that_fails_to_open_ends_the_wait_on_the_account_screen() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Skip, Event::ConfirmSkip]);
    let (flow, _) = step(App::Installer, flow, Event::InstallRefused("the install system has no receiver interface".into()));
    assert!(matches!(flow.state, State::InstallFailed { .. }));
    assert!(flow.install_problem.is_some());
}

#[test]
fn a_stalled_boot_shows_until_the_device_is_back_and_ends_with_the_screen() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Continue]);
    assert!(!flow.booting);
    let (flow, _) = step(App::Installer, flow, Event::BootStalled);
    assert!(flow.boot_stalled);
    let (flow, _) = step(App::Installer, flow, Event::BootResumed);
    assert!(!flow.boot_stalled);
    let (flow, _) = step(App::Installer, flow, Event::BootStalled);
    let (flow, _) = step(App::Installer, flow, Event::BootBooting);
    assert!(flow.booting && flow.boot_stalled);
    // Leaving the boot's screen ends both.
    let (flow, _) = step(App::Installer, flow, Event::IpadosReady { layout: fresh() });
    assert!(!flow.booting && !flow.boot_stalled);
}

#[test]
fn ipados_that_never_answers_offers_to_start_over() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Continue]);
    assert_eq!(timeout(&flow.state), Some(std::time::Duration::from_secs(90)));
    let (flow, _) = step(App::Installer, flow, Event::Timeout);
    let (flow, _) = step(App::Installer, flow, Event::Retry);
    assert!(matches!(flow.state, State::DfuAgain { why: Again::RetryPrepare, .. }));
    let (flow, e) = step(App::Installer, flow, dfu(ipad6()));
    // The boot again (held until the overlay is built, as always).
    let booting = e.iter().any(|e| matches!(e, Effect::BootIpadosWithOverlay(_))) || flow.boot_waiting.is_some();
    assert!(matches!(flow.state, State::WaitingForIpados { slow: false, .. }) && booting);
}

#[test]
fn a_split_that_erases_partitions_is_confirmed_in_a_dialog() {
    let (flow, _) = steps(App::Installer, vec![Event::Started, dfu(ipad6()), Event::Continue]);
    let (flow, _) = step(App::Installer, flow, Event::IpadosReady { layout: split_before() });
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(flow.confirming_split && e.is_empty(), "the dialog opens");
    let (flow, _) = step(App::Installer, flow, Event::Cancel);
    assert!(!flow.confirming_split && matches!(flow.state, State::ChooseSplit { approved: false, .. }));
    let (flow, _) = step(App::Installer, flow, Event::Continue);
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(e.is_empty(), "not approved yet");
    let (flow, _) = step(App::Installer, flow, Event::Approve(true));
    let (flow, e) = step(App::Installer, flow, Event::Continue);
    assert!(e.iter().any(|e| matches!(e, Effect::StartSplit { .. })) && !flow.confirming_split);
}

// ---- Suggested split and resume ----

fn layout_of(total: u32, used: u32) -> Layout {
    Layout { total_gb: total, ipados_gb: total, ipados_used_gb: used, outside: Vec::new() }
}

#[test]
fn the_suggested_split_is_half_with_room_for_ipados_and_the_minimum_for_omarchy() {
    assert_eq!(crate::suggested_size(&layout_of(128, 41)), 64, "50:50");
    assert_eq!(crate::suggested_size(&layout_of(128, 100)), 18, "iPadOS keeps 10 GB beyond what it uses");
    assert_eq!(crate::suggested_size(&layout_of(32, 10)), 12, "never below the minimum");
    assert_eq!(crate::suggested_size(&layout_of(128, 110)), 12, "the minimum, inside the hard limit");
}

#[test]
fn a_relaunched_installer_picks_up_at_the_split() {
    // The iPad still runs this install's iPadOS: the companion answers.
    let s = update(App::Installer, State::start(), Event::Attached { device: ipad6(), mode: UsbMode::Normal }).0;
    let (s, e) = update(App::Installer, s, Event::IpadosReady { layout: layout_of(128, 41) });
    assert!(matches!(s, State::ChooseSplit { omarchy_gb: 64, approved: false, .. }));
    assert!(e.is_empty(), "no second boot");
}

#[test]
fn quitting_from_the_temporary_ipados_restarts_the_ipad_first() {
    let s = State::ChooseSplit { device: ipad6(), layout: layout_of(128, 41), omarchy_gb: 64, approved: false };
    let (_, e) = update(App::Installer, s, Event::Quit);
    assert_eq!(e, vec![Effect::RestartIpad, Effect::Quit]);
    let (_, e) = update(App::Installer, State::start(), Event::Quit);
    assert_eq!(e, vec![Effect::Quit], "nothing to restart before palera1n");
}

#[test]
fn dfu_on_the_split_screen_starts_from_found_again() {
    let s = State::ChooseSplit { device: ipad6(), layout: layout_of(128, 41), omarchy_gb: 64, approved: false };
    let (s, _) = update(App::Installer, s, dfu(ipad6()));
    assert!(matches!(s, State::Found { .. }));
}

#[test]
fn dfu_where_the_next_step_asks_for_it_takes_that_step() {
    let d = ipad6();
    let l = layout_of(128, 41);
    let go = |s: State| update(App::Installer, s, dfu(ipad6()));
    let (s, e) = go(State::SplitDone { device: d.clone(), omarchy_gb: 64 });
    assert!(matches!(s, State::CreateAccount { env: None, .. }) && e == vec![Effect::BootInstallEnv(d.id.clone())]);
    let (s, _) = go(State::InstallFailed { device: d.clone() });
    assert!(matches!(s, State::CreateAccount { env: None, .. }));
    let (s, e) = go(State::NotPrepared { device: d.clone() });
    assert!(matches!(s, State::WaitingForIpados { .. }) && e == vec![Effect::BootIpadosWithOverlay(d.id.clone())]);
    // The install system restarted into DFU: boot it again, keeping the account.
    let (s, e) = go(State::ConfirmErase { device: d.clone(), account: account(), layout: l.clone(), approved: true });
    assert!(matches!(&s, State::CreateAccount { env: None, account: a, .. } if *a == account()));
    assert_eq!(e, vec![Effect::BootInstallEnv(d.id.clone())]);
}

#[test]
fn the_installed_system_coming_up_finishes_an_install_whose_done_got_lost() {
    let d = ipad6();
    let s = State::Installing { device: d.clone(), percent: 100, link: Link::Up, streamed: true, step: None };
    let (s, _) = update(App::Installer, s, Event::Attached { device: d, mode: UsbMode::Installed });
    assert!(matches!(s, State::Starting { .. }));
}


#[test]
fn palera1n_2_pins_cover_every_supported_host() {
    for (os, arch) in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("linux", "x86_64")] {
        let pin = assets::palera1n_2_pin(os, arch).expect("pinned");
        assert_eq!(pin.package, assets::Package::Bare);
        assert_eq!(pin.sha256.len(), 64);
    }
    assert!(assets::PALERA1N_2_RELEASE.ends_with("v2.4"));
}
