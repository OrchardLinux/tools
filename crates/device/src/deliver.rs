//! The loaders' transfers: what a prepared receiver is sent to start code,
//! as packed by `orchard_boards::pack`.
//!
//! The sequences are remote_boot's `remoteboot.sh` (`e254a58`) for a
//! patched iBoot on the 64-bit chips and Project Cascadia's
//! `devices/a5/tools/flash.sh` (`091d441`) for the autogo iBEC on the
//! 32-bit ones.
use crate::deadline::Deadline;
use crate::host::{Connect, PongoShell, RecoveryMode};
use crate::methods::{self, State};
use crate::prep::{self, starts_in, BootPath, Files, Load, MethodSteps, Request};
use crate::session::Session;
use crate::steps::{Outcome, Run};
use crate::usb;
use orchard_boards::catalog::Chip;
use orchard_boards::pack::{self, Form, Restore};
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use orchard_usb::Mode;
use std::time::Duration;

/// How long the single steps may take; the session's deadline can only
/// shorten them.
const SHELL: Duration = Duration::from_secs(15);
const SEND: Duration = Duration::from_secs(120);
const COMMAND: Duration = Duration::from_secs(10);
const LEAVE: Duration = Duration::from_secs(15);
const ARRIVE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
const M1N1: Duration = Duration::from_secs(30);
/// A kernel unpacks itself and starts its USB gadget before it shows up.
const LINUX: Duration = Duration::from_secs(120);
/// Cascadia's pause between the bundle and the loader. The device shows
/// nothing in between, so there is nothing to wait for instead.
const PAUSE: Duration = Duration::from_secs(1);

/// Packs what the path's loader is sent, from what the request loads. For
/// a chain that waits for keys from the device, a restore kernel is only
/// checked now, and packed once the chain is built.
pub fn pack(
    request: &Request,
    store: &Store,
    path: &BootPath,
    serial: Option<&str>,
    files: &mut Files,
) -> Result<()> {
    let loader = &path.methods.last().expect("a path ends at a loader").method;
    let m1n1 = |asset: &str| match request.load {
        Some(Load::File(file, linux)) => pack::m1n1(file, linux.as_ref()),
        _ => prep::loader_file(store, "m1n1", asset),
    };
    let form = methods::does::<orchard_usb::Host>(loader).and_then(|(_, form)| form);
    match form.expect("a picked loader takes a file") {
        Form::Image => files.image = Some(m1n1("m1n1.bin")?),
        Form::MachO => {
            let macho = m1n1("m1n1-idevice.macho")?;
            let chip = prep::known_chip(path.chip.as_ref())?;
            let stub = match pack::two_stage(chip) {
                true => prep::loader_file(store, "m1n1", "monitor-stub.macho")?,
                false => Vec::new(),
            };
            match &files.pending {
                Some(pending) => {
                    pending.check_restore(&macho, pack::two_stage(chip).then_some(&stub[..]))?
                }
                None => files.restore = Some(pack::restore(chip, &files.images, &macho, &stub)?),
            }
        }
        Form::Cascadia => {
            let Some(Load::File(file, Some(linux))) = request.load else {
                unreachable!("Cascadia's loader is picked only for itself, with Linux")
            };
            let bundle = pack::bundle(prep::board(path)?, file, &linux, serial)?;
            files.bundle = Some(bundle);
        }
    }
    Ok(())
}

fn lacks(what: &str) -> Error {
    Error::new(
        Code::NotFound,
        format!("nothing was packed for the loader that sends {what}"),
    )
}

fn waited<D: Connect>(
    session: &mut Session<D>,
    mode: Mode,
    most: Duration,
) -> Result<(Outcome, ())> {
    session.wait_for(mode, most)?;
    Ok((Outcome::Done, ()))
}

/// Starts an image through the Pongo shell: waits until the shell is idle,
/// sends, waits again, boots it, and sees the device leave Pongo.
pub fn pongo<D: Connect>(
    r: &mut Run<D>,
    _: &MethodSteps,
    _: Option<&Chip>,
    request: &Request,
    files: &mut Files,
) -> Result<()> {
    let image: &[u8] = match (&files.image, r.planning()) {
        (Some(image), _) => image,
        (_, true) => &[],
        _ => return Err(lacks("an image")),
    };
    starts_in(r, Mode::Pongo, "the Pongo shell", State::Pongo)?;
    let mut shell = r.pongo()?;
    let idle = |session: &mut Session<D>, shell: &mut dyn PongoShell| {
        let deadline = Deadline::after(session.timeout(SHELL)?);
        while shell
            .output(deadline.step(COMMAND)?)
            .map_err(usb::error)?
            .busy
        {
            deadline
                .sleep(POLL)
                .map_err(|_| Error::new(Code::Timeout, "the Pongo shell stayed busy"))?;
        }
        Ok((Outcome::Done, ()))
    };
    r.step("pongo output, until the shell is idle", |s| {
        idle(s, shell.as_mut())
    })?;
    r.step("pongo send", |s| {
        shell.send(image, s.timeout(SEND)?).map_err(usb::error)?;
        Ok((Outcome::Done, ()))
    })?;
    r.step("pongo output, until the shell is idle", |s| {
        idle(s, shell.as_mut())
    })?;
    r.step("pongo command bootm", |s| {
        // The last moment to notice that the port holds another device.
        s.check()?;
        let sent = shell
            .command("bootm", s.timeout(COMMAND)?)
            .map_err(usb::error)?;
        Ok((Outcome::of(sent), ()))
    })?;
    drop(shell);
    r.step("wait until the device has left pongo mode", |s| {
        s.wait_left(LEAVE)?;
        Ok((Outcome::Done, ()))
    })?;
    if let Some(mode) = request.wait {
        r.step(format!("wait {}", mode.as_str()), |s| {
            waited(s, mode, ARRIVE)
        })?;
    }
    Ok(())
}

/// Starts m1n1 on a patched iBoot as a restore kernel: the device tree, the
/// trust cache as firmware, then m1n1 as the kernelcache and `bootx`.
pub fn restore<D: Connect>(
    r: &mut Run<D>,
    _: &MethodSteps,
    chip: Option<&Chip>,
    _: &Request,
    files: &mut Files,
) -> Result<()> {
    let empty = Restore {
        devicetree: Vec::new(),
        trustcache: Vec::new(),
        kernelcache: Vec::new(),
    };
    let files = match (&files.restore, r.planning()) {
        (Some(files), _) => files,
        (_, true) => &empty,
        _ => return Err(lacks("m1n1")),
    };
    let chip = prep::known_chip(chip)?;
    starts_in(r, Mode::Recovery, "m1n1", State::PatchedRecovery)?;
    let kernelcache = match pack::two_stage(chip) {
        true => "recovery send, m1n1 and the monitor stub as the restore kernelcache",
        false => "recovery send, m1n1 as the restore kernelcache",
    };
    let mut handle = r.recovery()?;
    let iboot = handle.as_mut();
    send(
        r,
        iboot,
        "recovery send, the device tree",
        &files.devicetree,
    )?;
    command(r, iboot, "devicetree")?;
    send(
        r,
        iboot,
        "recovery send, the trust cache",
        &files.trustcache,
    )?;
    command(r, iboot, "firmware")?;
    send(r, iboot, kernelcache, &files.kernelcache)?;
    r.step("recovery command bootx", |s| {
        // The last moment to notice that the port holds another device.
        s.check()?;
        let sent = iboot
            .command("bootx", s.timeout(COMMAND)?)
            .map_err(usb::error)?;
        Ok((Outcome::of(sent), ()))
    })?;
    drop(handle);
    r.step("wait m1n1", |s| waited(s, Mode::M1n1, M1N1))
}

fn send<D: Connect>(
    r: &mut Run<D>,
    iboot: &mut dyn RecoveryMode,
    name: &str,
    image: &[u8],
) -> Result<()> {
    r.step(name, |s| {
        iboot.send(image, s.timeout(SEND)?).map_err(usb::error)?;
        Ok((Outcome::Done, ()))
    })
}

fn command<D: Connect>(r: &mut Run<D>, iboot: &mut dyn RecoveryMode, command: &str) -> Result<()> {
    r.step(format!("recovery command {command}"), |s| {
        iboot
            .command(command, s.timeout(COMMAND)?)
            .map_err(usb::error)?;
        Ok((Outcome::Done, ()))
    })
}

/// Starts Linux through Cascadia's loader on an autogo iBEC: the bundle, a
/// pause, then the bundle's own loader again. The iBEC enters what it was
/// sent when the upload is over, so no command follows.
pub fn autogo<D: Connect>(
    r: &mut Run<D>,
    _: &MethodSteps,
    _: Option<&Chip>,
    _: &Request,
    files: &mut Files,
) -> Result<()> {
    let bundle = match (&files.bundle, r.planning()) {
        (Some(bundle), _) => Some(bundle),
        (_, true) => None,
        _ => return Err(lacks("the bundle")),
    };
    let image = bundle.map_or(&[][..], |b| &b.image);
    // The loader as the bundle holds it: one without the filled-in
    // configuration would replace the one the bundle brought.
    let loader = bundle.map_or(&[][..], |b| b.loader());
    starts_in(r, Mode::Recovery, "Cascadia's loader", State::Autogo)?;
    let mut handle = r.recovery()?;
    let iboot = handle.as_mut();
    send(r, iboot, "recovery send, the bundle", image)?;
    r.step("pause 1 s between the bundle and the loader", |s| {
        std::thread::sleep(s.timeout(PAUSE)?);
        Ok((Outcome::Done, ()))
    })?;
    r.step("recovery send, the loader", |s| {
        // The last moment to notice that the port holds another device.
        s.check()?;
        iboot.send(loader, s.timeout(SEND)?).map_err(usb::error)?;
        Ok((Outcome::Done, ()))
    })?;
    drop(handle);
    r.step("wait linux", |s| waited(s, Mode::Linux, LINUX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, chip, fake, method, one, session, Did, Fake, Runs, FIRST, SECOND};
    use crate::methods::Kind;
    use orchard_formats::cascadia::{self, Bundle};
    use orchard_formats::der::{self, IA5_STRING, SEQUENCE};

    /// Plans, then runs the loader that starts on `from` with `files`.
    fn deliver(fake: &Fake, name: &str, from: State, stages: &[&str], files: Files) -> Did {
        deliver_waiting(fake, name, from, stages, files, None, None)
    }

    fn deliver_waiting(
        fake: &Fake,
        name: &str,
        from: State,
        stages: &[&str],
        mut files: Files,
        wait: Option<Mode>,
        limit: Option<Duration>,
    ) -> Did {
        let leaves = match name {
            "cascadia" => State::Linux,
            _ => State::M1n1,
        };
        let loader = method(Kind::Loader, name, from, leaves);
        one(
            &mut session(fake, limit),
            loader,
            &chip(stages),
            &mut files,
            wait,
        )
    }

    fn image() -> Files {
        Files {
            image: Some(b"IMAGE".to_vec()),
            ..Files::default()
        }
    }

    fn restore(stages: &[&str]) -> Files {
        let im4m = der::write(SEQUENCE, &der::write(IA5_STRING, b"IM4M"));
        let images = vec![
            ("devicetree.img4".to_owned(), b"device tree".to_vec()),
            ("trustcache.img4".to_owned(), b"trust".to_vec()),
            ("im4m.der".to_owned(), im4m),
        ];
        let restore = pack::restore(&chip(stages), &images, &[7; 3000], b"monitor stub").unwrap();
        Files {
            restore: Some(restore),
            ..Files::default()
        }
    }

    #[test]
    fn pongo_starts_the_image_once_the_shell_is_idle() {
        let fake = fake("", Runs::Pongo, None);
        fake.world().busy = 2;
        let wait = Some(Mode::M1n1);
        let did = deliver_waiting(&fake, "m1n1", State::Pongo, &[], image(), wait, None);
        let steps = did.result.unwrap();
        assert_eq!(
            fake.world().sent,
            [
                "pongo output",
                "pongo output",
                "pongo output",
                "pongo send 5",
                "pongo output",
                "pongo command bootm"
            ]
        );
        // The plan named every step that ran, in order.
        assert_eq!(did.progress, did.plan);
        assert_eq!(did.plan.len(), 6);
        assert_eq!(did.plan[5], "wait m1n1");
        assert_eq!(steps[3].outcome, Outcome::Acknowledged);
    }

    #[test]
    fn a_failure_says_where_it_stopped_and_what_was_done() {
        let fake = fake("", Runs::Pongo, None);
        fake.world().failing_upload = Some(0);
        fake.world().boots_into = None;
        let e = deliver(&fake, "m1n1", State::Pongo, &[], image())
            .result
            .unwrap_err();
        assert_eq!(e.code, Code::Usb);
        assert_eq!(
            e.message,
            "step 2 of 5 (pongo send): pongo send: the device rejected the request"
        );
        assert_eq!(
            e.hint.as_deref(),
            Some("completed before it: pongo output, until the shell is idle")
        );
        assert_eq!(fake.world().sent, ["pongo output", "pongo send 5"]);
    }

    #[test]
    fn m1n1_goes_to_a_patched_iboot_as_a_restore_kernel_in_order() {
        // Two boards: one whose chip loads the iBSS alone, one with an iBEC.
        for (serial, stages) in [(FIRST, &["iBSS"][..]), (SECOND, &["iBSS", "iBEC"][..])] {
            let fake = fake(serial, Runs::Iboot, None);
            let files = restore(stages);
            let size = files.restore.as_ref().unwrap().kernelcache.len();
            let did = deliver(&fake, "m1n1", State::PatchedRecovery, stages, files);
            let steps = did.result.unwrap();
            assert_eq!(
                fake.world().sent,
                [
                    "recovery send 11".to_owned(),
                    "recovery command devicetree".into(),
                    "recovery send 5".into(),
                    "recovery command firmware".into(),
                    format!("recovery send {size}"),
                    "recovery command bootx".into(),
                ]
            );
            assert_eq!(did.progress, did.plan);
            assert_eq!(steps.len(), 7);
            assert_eq!(did.plan[4].contains("monitor stub"), stages.len() == 2);
        }
    }

    #[test]
    fn an_unanswered_bootx_is_settled_by_the_wait() {
        let fake = fake(FIRST, Runs::Iboot, None);
        fake.world().silent = true;
        let files = restore(&["iBSS"]);
        let did = deliver(&fake, "m1n1", State::PatchedRecovery, &["iBSS"], files);
        let steps = did.result.unwrap();
        assert_eq!(steps[5].outcome, Outcome::Unconfirmed);
        assert_eq!(steps[6].outcome, Outcome::Done);
    }

    #[test]
    fn a_loader_refuses_a_device_in_another_mode() {
        let fake = fake(FIRST, Runs::Pongo, None);
        let files = restore(&["iBSS"]);
        let did = deliver(&fake, "m1n1", State::PatchedRecovery, &["iBSS"], files);
        let e = did.result.unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert_eq!(
            e.message,
            "m1n1 starts from patched-recovery, and the device is in pongo mode"
        );
        assert!(fake.world().sent.is_empty());
    }

    /// A bundle as the packer leaves it, from stand-in parts.
    fn bundle() -> Bundle {
        let profile = cascadia::Profile {
            load_address: 0x8000_0000,
            kernel_offset: 0x8000,
            watchdog_address: 0x0f10_3020,
            l2_address: 0x3e00_0000,
            ram_start: 0x8000_0000,
            ram_end: 0xa000_0000,
            initrd_address: 0x8800_0000,
            initrd_limit: 0x9000_0000,
        };
        let mut loader = fixture::loader();
        let words = [1, 68, profile.load_address, profile.kernel_offset];
        for (index, word) in words.iter().enumerate() {
            loader[0x7f04 + index * 4..0x7f08 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        let mut kernel = vec![0u8; 0x80];
        for (at, value) in [
            (0x24, 0x016f_2818u32),
            (0x2c, 0x80),
            (0x30, 0x0403_0201),
            (0x34, 0x4545_4545),
            (0x38, 0x40),
            (0x40, 6),
            (0x44, 0x5a53_4c4b),
            (0x48, 0x60),
            (0x50, 0x8000),
            (0x60, 0x1000),
        ] {
            kernel[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        // A tree whose /chosen holds the two initrd cells and nothing else.
        let names = b"linux,initrd-start\0linux,initrd-end\0";
        let mut tree = Vec::new();
        for word in [1u32, 0, 1] {
            tree.extend(word.to_be_bytes());
        }
        tree.extend(b"chosen\0\0");
        for name_at in [0u32, 19] {
            for word in [3, 4, name_at, 0] {
                tree.extend(word.to_be_bytes());
            }
        }
        for word in [2u32, 2, 9] {
            tree.extend(word.to_be_bytes());
        }
        let total = 56 + tree.len() + names.len();
        let mut dtb = Vec::new();
        for word in [
            0xd00d_feed,
            total,
            56,
            56 + tree.len(),
            40,
            17,
            16,
            0,
            names.len(),
            tree.len(),
        ] {
            dtb.extend((word as u32).to_be_bytes());
        }
        dtb.extend([0; 16]);
        dtb.extend(tree);
        dtb.extend(names);
        cascadia::bundle(&loader, &kernel, &dtb, b"initramfs", &profile, None).unwrap()
    }

    fn bundled(bundle: &Bundle) -> Files {
        Files {
            bundle: Some(bundle.clone()),
            ..Files::default()
        }
    }

    #[test]
    fn cascadias_loader_gets_the_bundle_then_its_own_loader_and_no_command() {
        let bundle = bundle();
        for serial in [FIRST, SECOND] {
            let fake = fake(serial, Runs::Iboot, None);
            fake.world().boots_into = Some(Runs::Linux);
            fake.world().starts_after_upload = Some(1);
            let stages = &["iBSS", "iBEC"];
            let did = deliver(&fake, "cascadia", State::Autogo, stages, bundled(&bundle));
            assert_eq!(did.result.unwrap().len(), 4);
            let world = fake.world();
            assert_eq!(
                world.sent,
                [
                    format!("recovery send {}", bundle.image.len()),
                    "recovery send 32580".into()
                ]
            );
            // The loader that follows is the bundle's own, configuration and all.
            assert_eq!(world.uploads[1], bundle.image[..0x7f44]);
            assert_eq!(did.progress, did.plan);
        }
    }

    #[test]
    fn a_device_that_never_shows_linux_is_a_timeout_that_says_what_is_there() {
        let fake = fake(SECOND, Runs::Iboot, None);
        fake.world().boots_into = Some(Runs::Iboot);
        let limit = Some(Duration::from_millis(1300));
        let stages = &["iBSS", "iBEC"];
        let files = bundled(&bundle());
        let did = deliver_waiting(&fake, "cascadia", State::Autogo, stages, files, None, limit);
        let e = did.result.unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(
            e.message,
            "step 4 of 4 (wait linux): the device did not show up in linux mode in time"
        );
        assert!(e
            .hint
            .unwrap()
            .starts_with("the device at 0x100000 is in recovery mode"));
    }
}
