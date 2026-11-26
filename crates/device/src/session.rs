//! Work on one device: one action, or many across its mode changes.
//!
//! A session holds the device's lock, what is known about the device, and
//! optionally one deadline for all steps. Every protocol handle comes from a
//! session, so a single command and a workflow reach the device the same
//! way. After a step that makes the device start something else,
//! [`Session::wait_for`] finds it again in its new mode and checks that it
//! is still the same device.
//!
//! A session can run a command ([`Session::run`]). The orchard2 commands
//! that command starts continue the session: same lock, same deadline, and
//! what one of them learns about the device the next one knows.
use crate::deadline::Deadline;
use crate::gaster::Executable;
use crate::host::{Connect, DfuMode, M1n1Proxy, PongoShell, RecoveryMode};
use crate::lock::{self, Lock};
use crate::select::{self, normalize_ecid, Selector};
use crate::state::{Shared, State};
use crate::usb;
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_usb::{Device, Discovery, Identity, Mode};
use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Session<D: Discovery> {
    discovery: D,
    /// The device as it was last seen.
    device: Device,
    /// Everything the device has told about itself so far. Only the boot
    /// ROM and iBoot report an identity, so it is kept across other modes.
    known: Identity,
    /// When all steps must be done, if the session has a limit, and the
    /// same as the session's commands share it.
    deadline: Option<Deadline>,
    deadline_unix_ms: Option<u64>,
    limit_ms: Option<u64>,
    /// The state file, when this session was started by another's command.
    shared: Option<Shared>,
    lock: Lock,
}

/// The error for a step that the session's limit leaves no time for.
fn limit_is_up(limit_ms: Option<u64>) -> Error {
    match limit_ms {
        Some(ms) => Error::new(
            Code::Timeout,
            format!("the session's {} s limit is up", ms as f64 / 1000.0),
        ),
        None => Error::new(Code::Timeout, "timed out"),
    }
}

/// The selector to use inside a surrounding session: without a device flag
/// it means the session's device.
pub fn in_session(selector: &Selector) -> Selector {
    let mut selector = selector.clone();
    if selector.location.is_none() && selector.ecid.is_none() {
        selector.location = lock::session_location();
    }
    selector
}

fn unix_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    since.map_or(0, |d| d.as_millis() as u64)
}

impl<D: Discovery> Session<D> {
    /// Chooses the one device the selector matches and locks it. With a
    /// `timeout`, all steps together may take that long. Inside a
    /// surrounding session for the same device, this continues it.
    pub fn start(
        discovery: D,
        selector: &Selector,
        timeout: Option<Duration>,
    ) -> Result<Session<D>> {
        Session::begin(discovery, &in_session(selector), timeout, Lock::acquire)
    }

    /// The same, with the lock files in `dir`.
    pub fn start_in(
        dir: &Path,
        discovery: D,
        selector: &Selector,
        timeout: Option<Duration>,
    ) -> Result<Session<D>> {
        Session::begin(discovery, selector, timeout, |l| Lock::acquire_in(dir, l))
    }

    fn begin(
        mut discovery: D,
        selector: &Selector,
        timeout: Option<Duration>,
        lock: impl FnOnce(&str) -> Result<Lock>,
    ) -> Result<Session<D>> {
        let device = select::one(&mut discovery, selector)?;
        let lock = lock(&device.location)?;
        // Whoever held the lock until now may have moved the device on, so
        // it is chosen again now that nobody else can.
        let here = Selector {
            location: Some(device.location.clone()),
            ..selector.clone()
        };
        let device = select::one(&mut discovery, &here)?;
        Session::open(discovery, device, lock, timeout)
    }

    /// Inside a surrounding session: waits up to `most` for the session's
    /// device to show up in `mode`, and continues the session on it. This
    /// is how a script follows its device through a mode change, where for
    /// a moment nothing is connected at all. Outside a session it returns
    /// `None`.
    pub fn resume(mut discovery: D, mode: Mode, most: Duration) -> Result<Option<Session<D>>> {
        let Some(location) = lock::session_location() else {
            return Ok(None);
        };
        let lock = Lock::acquire(&location)?;
        let state = match lock.inherited() {
            true => Shared::inherited().map(|s| s.read()).unwrap_or_default(),
            false => State::default(),
        };
        let left = match state.deadline_unix_ms {
            Some(end) => Deadline::after(Duration::from_millis(end.saturating_sub(unix_ms())))
                .step(most)
                .map_err(|_| limit_is_up(state.limit_ms))?,
            None => most,
        };
        let selector = Selector {
            location: Some(location),
            mode: Some(mode),
            ecid: None,
        };
        let wait = Deadline::after(left);
        let device = select::wait_one(&mut discovery, &selector, &wait)?;
        Session::open(discovery, device, lock, None).map(Some)
    }

    /// Makes the session for `device`, whose lock is held.
    fn open(
        discovery: D,
        device: Device,
        lock: Lock,
        timeout: Option<Duration>,
    ) -> Result<Session<D>> {
        // A continued session brings its deadline and what it knows.
        let shared = match lock.inherited() {
            true => Shared::inherited(),
            false => None,
        };
        let state = shared.as_ref().map(Shared::read).unwrap_or_default();
        let now = unix_ms();
        let ours = timeout.map(|t| t.as_millis() as u64);
        let ours_ends = ours.map(|ms| now.saturating_add(ms));
        // Of two limits, the one that ends first counts.
        let (deadline_unix_ms, limit_ms) = match (state.deadline_unix_ms, ours_ends) {
            (Some(theirs), Some(ours_ends)) if ours_ends < theirs => (Some(ours_ends), ours),
            (Some(theirs), _) => (Some(theirs), state.limit_ms),
            (None, ends) => (ends, ours),
        };
        let mut session = Session {
            discovery,
            known: Identity {
                ecid: state.ecid,
                chip: state.chip,
                board_id: state.board_id,
                serial_number: state.serial_number,
                ..Identity::default()
            },
            device: device.clone(),
            deadline: deadline_unix_ms
                .map(|end| Deadline::after(Duration::from_millis(end.saturating_sub(now)))),
            deadline_unix_ms,
            limit_ms,
            shared,
            lock,
        };
        session.adopt(device)?;
        session.remember();
        Ok(session)
    }

    /// The device as it was last seen.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// What the device has told about itself in any mode so far.
    pub fn identity(&self) -> &Identity {
        &self.known
    }

    /// The timeout for one protocol action: `most`, or the session's time
    /// left when that is less.
    pub fn timeout(&self, most: Duration) -> Result<Duration> {
        match &self.deadline {
            Some(deadline) => deadline.step(most).map_err(|_| limit_is_up(self.limit_ms)),
            None => Ok(most),
        }
    }

    /// Runs a command that inherits the session, and waits for it. The
    /// device stays locked until the command and everything it started have
    /// ended.
    pub fn run(&self, command: &[String]) -> Result<ExitStatus> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| Error::new(Code::InvalidInput, "no command to run"))?;
        let mut child = std::process::Command::new(program);
        child.args(args);
        self.lock.hand_to(&mut child)?;
        // The command's orchard2 commands share the state through a file
        // made for them, or the one this session was given itself.
        let made;
        let shared = match &self.shared {
            Some(shared) => shared,
            None => {
                made = Shared::create()?;
                made.write(&self.state())?;
                &made
            }
        };
        shared.hand_to(&mut child)?;
        child.status().context(program)
    }

    /// Confirms the device is still connected, in the mode it was last seen
    /// in.
    pub fn check(&mut self) -> Result<&Device> {
        let location = self.device.location.clone();
        let now = self.at_location()?.ok_or_else(|| {
            Error::new(Code::NoDevice, format!("the device at {location} is gone"))
        })?;
        if now.mode != self.device.mode {
            return Err(Error::new(
                Code::DeviceChanged,
                format!(
                    "the device at {location} went from {} to {} mode",
                    self.device.mode.as_str(),
                    now.mode.as_str()
                ),
            ));
        }
        self.adopt(now)
    }

    /// Waits up to `most` for the device to show up in `mode`, at the port
    /// it has been on all along, and checks that it is the same device.
    pub fn wait_for(&mut self, mode: Mode, most: Duration) -> Result<&Device> {
        let deadline = Deadline::after(self.timeout(most)?);
        self.arrive(mode, &deadline)
    }

    /// Waits up to `most` until the device is no longer in the mode it was
    /// last seen in: gone from the port, or there in another mode. This is
    /// what shows that a command which starts something else took effect,
    /// whatever it started.
    pub fn wait_left(&mut self, most: Duration) -> Result<()> {
        let deadline = Deadline::after(self.timeout(most)?);
        self.leave(&deadline)
    }

    /// Waits up to `most` for the device to arrive again in the mode it was
    /// last seen in: first until it is gone from that mode, then until it is
    /// back in it. This follows a device whose next program shows up in the
    /// same mode, as an iBSS does after the boot ROM, both in DFU mode. It
    /// tells that the device enumerated anew; what it now runs is for the
    /// caller to read from what the device reports.
    pub fn wait_again(&mut self, most: Duration) -> Result<&Device> {
        let deadline = Deadline::after(self.timeout(most)?);
        let mode = self.device.mode;
        self.leave(&deadline)?;
        self.arrive(mode, &deadline)
    }

    fn arrive(&mut self, mode: Mode, deadline: &Deadline) -> Result<&Device> {
        let selector = Selector {
            location: Some(self.device.location.clone()),
            mode: Some(mode),
            ecid: None,
        };
        match select::wait_one(&mut self.discovery, &selector, deadline) {
            Ok(found) => self.adopt(found),
            Err(e) if e.code == Code::Timeout => {
                let location = self.device.location.clone();
                let hint = match self.at_location()? {
                    Some(other) => format!(
                        "the device at {location} is in {} mode",
                        other.mode.as_str()
                    ),
                    None => format!("nothing is connected at {location}"),
                };
                Err(Error::new(
                    Code::Timeout,
                    format!(
                        "the device did not show up in {} mode in time",
                        mode.as_str()
                    ),
                )
                .hint(hint))
            }
            Err(e) => Err(e),
        }
    }

    fn leave(&mut self, deadline: &Deadline) -> Result<()> {
        loop {
            match self.at_location()? {
                Some(there) if there.mode == self.device.mode => {}
                _ => return Ok(()),
            }
            deadline.sleep(select::POLL).map_err(|_| {
                Error::new(
                    Code::Timeout,
                    format!(
                        "the device at {} is still in {} mode",
                        self.device.location,
                        self.device.mode.as_str()
                    ),
                )
            })?;
        }
    }

    /// Whatever is at the session's port right now.
    fn at_location(&mut self) -> Result<Option<Device>> {
        let selector = Selector {
            location: Some(self.device.location.clone()),
            ..Selector::default()
        };
        Ok(select::matching(&mut self.discovery, &selector)?.pop())
    }

    /// Takes `seen` as the session's device if nothing it reports
    /// contradicts what is known, and remembers what it adds.
    fn adopt(&mut self, seen: Device) -> Result<&Device> {
        let known = &mut self.known;
        let mut learned = false;
        let ecids = (&mut known.ecid, &seen.identity.ecid, true);
        let chips = (&mut known.chip, &seen.identity.chip, false);
        let boards = (&mut known.board_id, &seen.identity.board_id, false);
        for (known, seen, is_ecid) in [ecids, chips, boards] {
            let spelled = |s: &str| match is_ecid {
                true => normalize_ecid(s),
                false => s.to_owned(),
            };
            match (known.as_deref(), seen.as_deref()) {
                (Some(k), Some(s)) if spelled(k) != spelled(s) => {
                    return Err(Error::new(
                        Code::DeviceChanged,
                        format!(
                            "another device is now connected at {}",
                            self.device.location
                        ),
                    )
                    .hint("leave the device plugged into the same port until the work is done"));
                }
                (None, Some(s)) => {
                    *known = Some(s.to_owned());
                    learned = true;
                }
                _ => {}
            }
        }
        // The serial number is kept for what is packed for the device; it
        // does not decide whether this is the same device, the ECID does.
        if known.serial_number.is_none() && seen.identity.serial_number.is_some() {
            known.serial_number = seen.identity.serial_number.clone();
            learned = true;
        }
        self.device = seen;
        if learned {
            self.remember();
        }
        Ok(&self.device)
    }

    fn state(&self) -> State {
        State {
            deadline_unix_ms: self.deadline_unix_ms,
            limit_ms: self.limit_ms,
            ecid: self.known.ecid.clone(),
            chip: self.known.chip.clone(),
            board_id: self.known.board_id.clone(),
            serial_number: self.known.serial_number.clone(),
        }
    }

    /// Shares what is known with the other commands of a surrounding
    /// session. One that cannot be told learns it again from the device.
    fn remember(&self) {
        if let Some(shared) = &self.shared {
            let _ = shared.write(&self.state());
        }
    }
}

impl<D: Connect> Session<D> {
    /// Makes sure the session's device is the one device in DFU, with its
    /// boot ROM untouched: what a tool needs that takes the first DFU device
    /// it finds. A device flag picks the session's device, not the tool's,
    /// so every device in DFU counts.
    pub fn require_single_dfu(&mut self, tool: &str) -> Result<()> {
        self.check()?;
        if self.device.mode != Mode::Dfu || self.device.identity.pwned.is_some() {
            return Err(Error::new(
                Code::InvalidInput,
                format!("{tool} needs an untouched device in DFU"),
            ));
        }
        let devices = self.discovery.list().map_err(usb::error)?;
        if devices.iter().filter(|d| d.mode == Mode::Dfu).count() != 1 {
            let message = format!("{tool} needs exactly one device in DFU");
            let hint = format!(
                "disconnect the other DFU devices; \
                 --ecid and --location cannot select one for {tool}"
            );
            return Err(Error::new(Code::Ambiguous, message).hint(hint));
        }
        let current = devices
            .into_iter()
            .find(|d| d.location == self.device.location && d.mode == Mode::Dfu)
            .ok_or_else(|| Error::new(Code::DeviceChanged, "the selected DFU device is gone"))?;
        self.adopt(current)?;
        if self.device.identity.pwned.is_some() {
            return Err(Error::new(
                Code::DeviceChanged,
                "the selected device is now pwned",
            ));
        }
        Ok(())
    }

    pub(crate) fn gaster_worker(&self, store: &Store) -> Result<Executable> {
        self.discovery.gaster_worker(store)
    }

    /// Runs the pinned entry or unwrap-only worker under this session's lock.
    pub fn gaster(
        &mut self,
        bags: Option<&[[u8; 48]]>,
        worker: &Executable,
    ) -> Result<Vec<[u8; 48]>> {
        self.check()?;
        let marker = self.device.identity.pwned.as_deref();
        if self.device.mode != Mode::Dfu || (bags.is_some() && marker != Some("gaster")) || (bags.is_none() && marker.is_some()) {
            return Err(Error::new(
                Code::InvalidInput,
                "key acquisition needs compatible gaster DFU",
            )
            .hint("use cached/public keys or --keys FILE from this starting state"));
        }
        let timeout = self.timeout(Duration::from_secs(120))?;
        let lock = &self.lock;
        let reply = self
            .discovery
            .gaster(&self.device, bags, worker, timeout, &|c| lock.hand_to(c))?;
        self.wait_for(Mode::Dfu, Duration::from_secs(10))?;
        if self.device.identity.pwned.as_deref() != Some("gaster") {
            return Err(Error::new(
                Code::DeviceChanged,
                "gaster did not leave the selected device in compatible DFU",
            ));
        }
        if reply.len() != bags.map_or(0, <[_]>::len) {
            return Err(Error::new(
                Code::Format,
                "device key reply count mismatch (values suppressed)",
            ));
        }
        Ok(reply)
    }

    /// Opens the device for DFU.
    pub fn dfu(&mut self) -> Result<Box<dyn DfuMode>> {
        self.discovery.dfu(&self.device).map_err(usb::error)
    }

    /// Opens the device for recovery mode.
    pub fn recovery(&mut self) -> Result<Box<dyn RecoveryMode>> {
        self.discovery.recovery(&self.device).map_err(usb::error)
    }

    /// Opens the device for the Pongo shell.
    pub fn pongo(&mut self) -> Result<Box<dyn PongoShell>> {
        self.discovery.pongo(&self.device).map_err(usb::error)
    }

    /// Opens the device's m1n1 proxy.
    pub fn m1n1(&mut self) -> Result<Box<dyn M1n1Proxy>> {
        self.discovery.m1n1(&self.device).map_err(usb::error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Replays one list of devices per call; the last one repeats.
    struct Fake(VecDeque<Vec<Device>>);

    impl Discovery for Fake {
        fn list(&mut self) -> std::result::Result<Vec<Device>, orchard_usb::Error> {
            Ok(match self.0.len() {
                1 => self.0[0].clone(),
                _ => self.0.pop_front().expect("at least one list"),
            })
        }
    }

    fn device(location: &str, mode: Mode, serial: &str) -> Device {
        Device {
            mode,
            system: None,
            location: location.into(),
            product: None,
            identity: Identity::parse(serial),
        }
    }

    const FIRST: &str = "CPID:8010 BDID:0C ECID:00000000000000A1";
    const SECOND: &str = "CPID:8000 BDID:04 ECID:00000000000000B2";
    const LONG: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(120);

    fn session(lists: Vec<Vec<Device>>, timeout: Duration) -> Result<Session<Fake>> {
        // The directory may go: a held lock lives in the open file.
        let dir = tempfile::tempdir().unwrap();
        let selector = Selector {
            location: Some("0x100000".into()),
            ..Selector::default()
        };
        Session::start_in(dir.path(), Fake(lists.into()), &selector, Some(timeout))
    }

    #[test]
    fn follows_the_device_through_modes_and_keeps_its_identity() {
        let dfu = device("0x100000", Mode::Dfu, FIRST);
        let other = device("0x200000", Mode::Dfu, SECOND);
        let gone = vec![other.clone()];
        let m1n1 = vec![other.clone(), device("0x100000", Mode::M1n1, "")];
        let start = vec![dfu.clone(), other.clone()];
        let mut s = session(vec![start.clone(), start, gone, m1n1], LONG).unwrap();
        assert_eq!(s.device().mode, Mode::Dfu);

        assert_eq!(s.wait_for(Mode::M1n1, LONG).unwrap().mode, Mode::M1n1);
        // m1n1 reports no identity; the session still knows the device.
        assert_eq!(s.device().identity.ecid, None);
        assert_eq!(s.identity().ecid.as_deref(), Some("00000000000000a1"));
        assert_eq!(s.identity().chip.as_deref(), Some("8010"));
    }

    #[test]
    fn learns_the_identity_when_the_device_first_reports_it() {
        let m1n1 = vec![device("0x100000", Mode::M1n1, "")];
        let recovery = vec![device("0x100000", Mode::Recovery, FIRST)];
        let mut s = session(vec![m1n1.clone(), m1n1, recovery], LONG).unwrap();
        assert_eq!(s.identity().ecid, None);
        s.wait_for(Mode::Recovery, LONG).unwrap();
        assert_eq!(s.identity().ecid.as_deref(), Some("00000000000000a1"));
    }

    #[test]
    fn another_device_at_the_port_is_refused() {
        let first = vec![device("0x100000", Mode::Dfu, FIRST)];
        let swapped = vec![device("0x100000", Mode::Recovery, SECOND)];
        let mut s = session(vec![first.clone(), first, swapped], LONG).unwrap();
        let e = s.wait_for(Mode::Recovery, LONG).unwrap_err();
        assert_eq!(e.code, Code::DeviceChanged);
        // The session keeps what it knew.
        assert_eq!(s.device().mode, Mode::Dfu);
    }

    #[test]
    fn same_ecid_in_another_spelling_is_the_same_device() {
        let first = vec![device("0x100000", Mode::Dfu, FIRST)];
        let again = vec![device("0x100000", Mode::Recovery, "ECID:A1")];
        let mut s = session(vec![first.clone(), first, again], LONG).unwrap();
        s.wait_for(Mode::Recovery, LONG).unwrap();
    }

    #[test]
    fn waiting_ends_with_the_session_and_says_what_is_there() {
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let mut s = session(vec![dfu], SHORT).unwrap();
        let e = s.wait_for(Mode::M1n1, LONG).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(
            e.hint.as_deref(),
            Some("the device at 0x100000 is in dfu mode")
        );

        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let mut s = session(vec![dfu.clone(), dfu, vec![]], SHORT).unwrap();
        let e = s.wait_for(Mode::M1n1, LONG).unwrap_err();
        assert_eq!(e.hint.as_deref(), Some("nothing is connected at 0x100000"));
    }

    #[test]
    fn check_notices_a_device_that_left_or_moved_on() {
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let mut s = session(vec![dfu.clone(), dfu.clone(), dfu.clone(), vec![]], LONG).unwrap();
        s.check().unwrap();
        assert_eq!(s.check().unwrap_err().code, Code::NoDevice);

        let recovery = vec![device("0x100000", Mode::Recovery, FIRST)];
        let mut s = session(vec![dfu.clone(), dfu, recovery], LONG).unwrap();
        assert_eq!(s.check().unwrap_err().code, Code::DeviceChanged);
    }

    #[test]
    fn leaving_a_mode_is_seen_whether_the_device_goes_or_changes() {
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let recovery = vec![device("0x100000", Mode::Recovery, FIRST)];
        for after in [vec![], recovery] {
            let lists = vec![dfu.clone(), dfu.clone(), dfu.clone(), after];
            let mut s = session(lists, LONG).unwrap();
            s.wait_left(LONG).unwrap();
            // The session still describes the device as it last saw it.
            assert_eq!(s.device().mode, Mode::Dfu);
        }
        let mut s = session(vec![dfu], LONG).unwrap();
        let e = s.wait_left(SHORT).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(e.message, "the device at 0x100000 is still in dfu mode");
    }

    #[test]
    fn a_device_in_use_cannot_start_a_second_session() {
        let dir = tempfile::tempdir().unwrap();
        let selector = Selector::default();
        let start = || {
            let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
            Session::start_in(dir.path(), Fake(vec![dfu].into()), &selector, None)
        };
        let held = start().unwrap();
        assert_eq!(start().err().unwrap().code, Code::Locked);
        drop(held);
        start().unwrap();
    }

    #[test]
    fn the_device_is_chosen_again_once_the_lock_is_held() {
        // In DFU when first seen, moved on by the time the lock is free.
        let before = vec![device("0x100000", Mode::Dfu, FIRST)];
        let after = vec![device("0x100000", Mode::Recovery, FIRST)];
        let dir = tempfile::tempdir().unwrap();
        let dfu = Selector {
            mode: Some(Mode::Dfu),
            ..Selector::default()
        };
        let moved = Fake(vec![before, after].into());
        let e = Session::start_in(dir.path(), moved, &dfu, None)
            .err()
            .unwrap();
        assert_eq!(e.code, Code::NoDevice);
    }

    #[test]
    fn without_a_deadline_a_step_gets_its_own_time() {
        let dir = tempfile::tempdir().unwrap();
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let s = Session::start_in(
            dir.path(),
            Fake(vec![dfu].into()),
            &Selector::default(),
            None,
        )
        .unwrap();
        assert_eq!(s.timeout(LONG).unwrap(), LONG);
    }

    #[test]
    fn what_the_session_learns_reaches_its_shared_state() {
        let dir = tempfile::tempdir().unwrap();
        let m1n1 = vec![device("0x100000", Mode::M1n1, "")];
        let recovery = vec![device("0x100000", Mode::Recovery, FIRST)];
        let lists = vec![m1n1.clone(), m1n1, recovery];
        let mut s = Session::start_in(
            dir.path(),
            Fake(lists.into()),
            &Selector::default(),
            Some(LONG),
        )
        .unwrap();
        // As in a command that a session started.
        s.shared = Some(Shared::create().unwrap());

        s.wait_for(Mode::Recovery, LONG).unwrap();
        let state = s.shared.as_ref().unwrap().read();
        assert_eq!(state.ecid.as_deref(), Some("00000000000000a1"));
        assert_eq!(state.chip.as_deref(), Some("8010"));
        assert_eq!(state.limit_ms, Some(5000));
        assert!(state.deadline_unix_ms.is_some());
    }

    #[test]
    fn a_device_is_followed_into_the_same_mode_again() {
        // The boot ROM, pwned, then off the bus, then the iBSS, in DFU too.
        let rom = vec![device(
            "0x100000",
            Mode::Dfu,
            &format!("{FIRST} PWND:[tool]"),
        )];
        let ibss = vec![device("0x100000", Mode::Dfu, FIRST)];
        let lists = vec![rom.clone(), rom.clone(), rom, vec![], ibss];
        let mut s = session(lists, LONG).unwrap();
        assert!(s.device().identity.pwned.is_some());
        let again = s.wait_again(LONG).unwrap();
        assert_eq!(again.mode, Mode::Dfu);
        assert_eq!(again.identity.pwned, None);

        // A device that never leaves has not arrived again.
        let rom = vec![device("0x100000", Mode::Dfu, FIRST)];
        let mut s = session(vec![rom], LONG).unwrap();
        let e = s.wait_again(SHORT).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(e.message, "the device at 0x100000 is still in dfu mode");

        // One that left and came back as something else is named.
        let rom = vec![device("0x100000", Mode::Dfu, FIRST)];
        let recovery = vec![device("0x100000", Mode::Recovery, FIRST)];
        let mut s = session(vec![rom.clone(), rom, recovery], SHORT).unwrap();
        let e = s.wait_again(LONG).unwrap_err();
        assert_eq!(e.message, "the device did not show up in dfu mode in time");
        assert_eq!(
            e.hint.as_deref(),
            Some("the device at 0x100000 is in recovery mode")
        );

        // Another device arriving at the port is refused.
        let rom = vec![device("0x100000", Mode::Dfu, FIRST)];
        let other = vec![device("0x100000", Mode::Dfu, SECOND)];
        let mut s = session(vec![rom.clone(), rom, vec![], other], LONG).unwrap();
        assert_eq!(s.wait_again(LONG).unwrap_err().code, Code::DeviceChanged);
    }

    #[test]
    fn a_session_out_of_time_says_whose_limit_it_was() {
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let s = session(vec![dfu], Duration::from_millis(1500)).unwrap();
        std::thread::sleep(Duration::from_millis(1510));
        let e = s.timeout(LONG).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(e.message, "the session's 1.5 s limit is up");
    }

    #[test]
    fn a_step_never_gets_more_time_than_the_session_has() {
        let dfu = vec![device("0x100000", Mode::Dfu, FIRST)];
        let s = session(vec![dfu], Duration::from_secs(2)).unwrap();
        assert!(s.timeout(Duration::from_secs(30)).unwrap() <= Duration::from_secs(2));
    }
}
