//! Starting a system that is installed on the device, through m1n1.
//!
//! The device is taken into m1n1 the most convenient way: as it is when it
//! runs m1n1, through the receiver it waits in, or by a prep to one whose
//! loader starts m1n1. Then m1n1 does the work: it reads the device's
//! storage, finds the boot entries, checks them, and loads and starts the
//! one it is told to. The host asks for the list, chooses, and says which.
//! It reads no disk structures and writes nothing. The operations and the
//! list's text are Orchard's own additions to m1n1
//! (`docs/boot-contract.md` in orchard).
use crate::host::{Connect, M1n1Proxy};
use crate::methods::{Place, State};
use crate::prep::{self, BootPath, Load, Request};
use crate::session::Session;
use crate::steps::{Outcome, Run, Step};
use crate::usb;
use orchard_cache::{Code, Error, Result};
use orchard_usb::{Device, Discovery, Mode};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

pub(crate) const HEAP_ALLOC: u64 = 0x600;
const HEAP_SET_LIMIT: u64 = 0x604;
pub(crate) const STORAGE_INIT: u64 = 0xf08;
pub(crate) const ENTRIES: u64 = 0x1500;
const ENTRIES_FREE: u64 = 0x1501;
pub(crate) const START: u64 = 0x1502;
/// `START` flag: do not paint the entry's splash.
const NO_SPLASH: u64 = 1;

/// The memory above its heap's start that m1n1 keeps for itself; the host
/// puts its strings and buffers above that.
const M1N1_HEAP: u64 = 128 << 20;
const HOST_HEAP: u64 = 1 << 30;
/// The list's text is a few lines per entry.
const MAX_TEXT: u64 = 16 << 20;

const QUICK: Duration = Duration::from_secs(10);
/// Bringing storage up, and reading from it, takes the device a while.
const STORAGE: Duration = Duration::from_secs(60);
/// m1n1 answers a start only after it has loaded kernel and initrd.
const LOAD: Duration = Duration::from_secs(120);
const LEAVE: Duration = Duration::from_secs(15);
const ARRIVE: Duration = Duration::from_secs(120);

const NO_BOOT_MANAGER: &str = "this m1n1 may be one without Orchard's boot manager";

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The entry to start; without one, the first that can boot.
    pub entry: Option<String>,
    /// The file name of the board's device tree. m1n1 then only takes
    /// entries that bring this tree.
    pub dtb_name: Option<String>,
    /// Whether m1n1 paints the entry's splash while it loads.
    pub splash: bool,
    /// A mode to wait for after the system has been started.
    pub wait: Option<Mode>,
    /// Only list the boot entries.
    pub list: bool,
}

/// One boot entry, as m1n1 reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: String,
    pub title: Option<String>,
    pub version: Option<String>,
    /// Whether m1n1 found no reason the entry cannot boot.
    pub bootable: bool,
    pub problems: Vec<String>,
    /// The entry's keys with their values, in file order per key.
    pub keys: BTreeMap<String, Vec<String>>,
}

/// What a finished boot or listing did.
#[derive(Debug, Clone, Serialize)]
pub struct Booted {
    /// How the device was taken into m1n1; absent when it ran m1n1.
    pub path: Option<BootPath>,
    /// The entry that was started; absent for a listing.
    pub entry: Option<Entry>,
    /// All entries, in m1n1's boot order.
    pub entries: Vec<Entry>,
    pub steps: Vec<Step>,
    /// The device in the mode that was waited for.
    pub device: Option<Device>,
}

/// What a boot would do: the way into m1n1, then the steps on it.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    #[serde(flatten)]
    pub path: BootPath,
    pub then: Vec<String>,
}

/// The request with a path into m1n1, whose loader starts it for the boot
/// manager.
fn into_m1n1<'a>(request: &Request<'a>) -> Request<'a> {
    Request {
        load: Some(Load::M1n1),
        wait: Some(Mode::M1n1),
        ..request.clone()
    }
}

/// Starts an installed system, or lists the boot entries, from wherever
/// the device is. A device that runs m1n1 needs no board data, so
/// `request` is only used for one that does not.
pub fn run<D: Connect>(
    session: &mut Session<D>,
    request: Option<&Request>,
    options: &Options,
    progress: &mut dyn FnMut(&str),
) -> Result<Booted> {
    let mut options = options.clone();
    let path = match (session.device().mode, request) {
        (Mode::M1n1, _) => None,
        (_, Some(request)) => {
            let went = prep::run(session, &into_m1n1(request), progress)?;
            // Without a name from the caller, the board's device tree.
            if options.dtb_name.is_none() {
                let dtb = went.data.as_ref().and_then(|b| b.dtb.as_ref());
                options.dtb_name = dtb.map(|dtb| format!("{dtb}.dtb"));
            }
            Some(went)
        }
        (_, None) => {
            return Err(Error::new(
                Code::InvalidInput,
                "the device does not run m1n1, and there is no board data to take it there",
            ))
        }
    };
    let mut quiet = |_: &str| {};
    let mut planned = Run::<D>::plan(&mut quiet);
    manage(&mut planned, &options)?;
    let mut r = Run::new(session, planned.names(), progress);
    let (entries, entry) = manage(&mut r, &options)?;
    let steps = r.finish();
    Ok(Booted {
        path,
        entry,
        entries,
        steps,
        device: options.wait.map(|_| session.device().clone()),
    })
}

/// What a boot would do, from where the device is or, without one, from
/// the stated state of the named board.
pub fn plan<D: Discovery>(
    request: &Request,
    session: Option<&Session<D>>,
    options: &Options,
) -> Result<Plan> {
    let at_m1n1 = match session {
        Some(session) => session.device().mode == Mode::M1n1,
        None => request.from == Some(State::M1n1),
    };
    let path = match at_m1n1 {
        true => BootPath {
            board: request.board.map(str::to_owned),
            data: None,
            chip: None,
            from: Place {
                state: State::M1n1,
                marker: None,
            },
            methods: Vec::new(),
            state: None,
            device: None,
        },
        false => prep::plan(&into_m1n1(request), session)?,
    };
    let mut quiet = |_: &str| {};
    let mut r = Run::<orchard_usb::Host>::plan(&mut quiet);
    manage(&mut r, options)?;
    Ok(Plan {
        path,
        then: r.names(),
    })
}

/// The work on m1n1: the boot entries, and unless only listing, the start
/// of one and the waits after it.
fn manage<D: Connect>(r: &mut Run<D>, options: &Options) -> Result<(Vec<Entry>, Option<Entry>)> {
    let mut proxy = r.m1n1()?;
    let mut manager = connect(r, proxy.as_mut())?;
    let dtb_name = options.dtb_name.as_deref();
    let entries = read_entries(r, &mut manager, dtb_name)?;
    if options.list {
        return Ok((entries, None));
    }
    let (name, entry) = match r.planning() {
        true => {
            let name = match &options.entry {
                Some(id) => format!("start {id}"),
                None => "start the first entry that can boot".to_owned(),
            };
            (name, None)
        }
        false => {
            let entry = choose(&entries, options.entry.as_deref())?.clone();
            (format!("start {}", entry.id), Some(entry))
        }
    };
    let id = entry.as_ref().map_or("", |e| e.id.as_str());
    r.step(name, |s| {
        // The last moment to notice that the port holds another device.
        s.check()?;
        let id_at = manager.string(s, id)?;
        let tree = match dtb_name {
            Some(name) => manager.string(s, name)?,
            None => 0,
        };
        let flags = if options.splash { 0 } else { NO_SPLASH };
        let timeout = s.timeout(LOAD)?;
        let answer = manager.proxy.call(START, &[id_at, tree, flags], timeout);
        let failed = answer.map_err(|e| usb::error(e).hint(NO_BOOT_MANAGER))?;
        if failed != 0 {
            // m1n1 stays in the proxy and says why on its console.
            let why = reason(&manager.proxy.take_console());
            return Err(Error::new(
                Code::Refused,
                format!("m1n1 could not start {id}{why}"),
            ));
        }
        Ok((Outcome::Acknowledged, ()))
    })?;
    drop(proxy);
    r.step("wait until the device has left m1n1 mode", |s| {
        s.wait_left(LEAVE)?;
        Ok((Outcome::Done, ()))
    })?;
    if let Some(mode) = options.wait {
        r.step(format!("wait {}", mode.as_str()), |s| {
            s.wait_for(mode, ARRIVE)?;
            Ok((Outcome::Done, ()))
        })?;
    }
    Ok((entries, entry))
}

/// Reads the text m1n1 answers a list request with.
pub fn parse_entries(text: &str) -> Result<Vec<Entry>> {
    let mut lines = text.lines();
    match lines.next() {
        Some("contract 1") => {}
        other => {
            let said = other.unwrap_or("nothing");
            return Err(Error::new(
                Code::Unsupported,
                format!("m1n1 lists boot entries in a form this version does not know ({said})"),
            ));
        }
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match (key, entries.last_mut()) {
            // The disk as a whole could not be read.
            ("error", _) => return Err(Error::new(Code::Refused, format!("m1n1: {value}"))),
            ("entry", _) => entries.push(Entry {
                id: value.to_owned(),
                title: None,
                version: None,
                bootable: true,
                problems: Vec::new(),
                keys: BTreeMap::new(),
            }),
            (_, None) => {
                return Err(Error::new(
                    Code::Format,
                    "boot entries: a line before the first entry",
                ))
            }
            ("problem", Some(entry)) => {
                entry.bootable = false;
                entry.problems.push(value.to_owned());
            }
            (_, Some(entry)) => {
                match key {
                    "title" => entry.title = Some(value.to_owned()),
                    "version" => entry.version = Some(value.to_owned()),
                    _ => {}
                }
                entry
                    .keys
                    .entry(key.to_owned())
                    .or_default()
                    .push(value.to_owned());
            }
        }
    }
    Ok(entries)
}

/// The entry to start: the one asked for, or the first that can boot.
pub fn choose<'a>(entries: &'a [Entry], wanted: Option<&str>) -> Result<&'a Entry> {
    let ids = || {
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        format!("the entries are: {}", ids.join(", "))
    };
    if entries.is_empty() {
        return Err(Error::new(
            Code::NotFound,
            "there are no boot entries on the device",
        ));
    }
    let entry = match wanted {
        Some(id) => entries.iter().find(|e| e.id == id).ok_or_else(|| {
            Error::new(Code::NotFound, format!("there is no boot entry {id}")).hint(ids())
        })?,
        None => match entries.iter().find(|e| e.bootable) {
            Some(entry) => entry,
            // Say why the first one cannot, and name the rest.
            None => &entries[0],
        },
    };
    if !entry.bootable {
        return Err(Error::new(
            Code::Refused,
            format!(
                "boot entry {} cannot boot: {}",
                entry.id,
                entry.problems.join("; ")
            ),
        )
        .hint(ids()));
    }
    Ok(entry)
}

/// The proxy with the host's share of the device's memory.
struct Manager<'a> {
    proxy: &'a mut dyn M1n1Proxy,
    /// The next free address, and the end of the host's share.
    next: u64,
    end: u64,
}

/// The steps every use of the boot manager begins with.
fn connect<'a, D: Connect>(r: &mut Run<D>, proxy: &'a mut dyn M1n1Proxy) -> Result<Manager<'a>> {
    r.step("m1n1 ping", |s| {
        proxy.ping(s.timeout(QUICK)?).map_err(usb::error)?;
        Ok((Outcome::Done, ()))
    })?;
    let start = r.step("set memory aside for the host", |s| {
        let base = proxy
            .call(HEAP_ALLOC, &[0], s.timeout(QUICK)?)
            .map_err(usb::error)?;
        let start = base
            .checked_add(M1N1_HEAP)
            .filter(|_| base != 0)
            .ok_or_else(|| Error::new(Code::Refused, "m1n1 gave no memory"))?;
        // m1n1 must not grow its own heap into the host's share.
        proxy
            .call(HEAP_SET_LIMIT, &[start], s.timeout(QUICK)?)
            .map_err(usb::error)?;
        Ok((Outcome::Done, start))
    })?;
    r.step("bring the device's storage up", |s| {
        // Storage that is already up stays as it is, so this is safe to
        // repeat. It is never shut down from here: m1n1 does that itself
        // when it starts a system, and a storage that was shut down does
        // not come up again in the same run of m1n1.
        if proxy
            .call(STORAGE_INIT, &[], s.timeout(STORAGE)?)
            .map_err(usb::error)?
            == 0
        {
            return Err(Error::new(
                Code::Refused,
                "m1n1 could not bring the device's storage up",
            )
            .hint("this m1n1 may have no storage support for the device"));
        }
        Ok((Outcome::Done, ()))
    })?;
    Ok(Manager {
        proxy,
        next: start,
        end: start.saturating_add(HOST_HEAP),
    })
}

fn read_entries<D: Connect>(
    r: &mut Run<D>,
    manager: &mut Manager,
    dtb_name: Option<&str>,
) -> Result<Vec<Entry>> {
    r.step("read the boot entries", |s| {
        let out = manager.alloc(16)?;
        let tree = match dtb_name {
            Some(name) => manager.string(s, name)?,
            None => 0,
        };
        let failed = manager
            .proxy
            .call(ENTRIES, &[out, tree], s.timeout(STORAGE)?)
            .map_err(|e| usb::error(e).hint(NO_BOOT_MANAGER))?;
        // m1n1 wrote where the text is into `out`: address, then size.
        let place = manager
            .proxy
            .read(out, 16, s.timeout(QUICK)?)
            .map_err(usb::error)?;
        let address = u64::from_le_bytes(place[..8].try_into().expect("8 bytes"));
        let size = u64::from_le_bytes(place[8..].try_into().expect("8 bytes"));
        if failed != 0 || address == 0 || size > MAX_TEXT {
            return Err(Error::new(
                Code::Refused,
                "m1n1 could not list the boot entries",
            ));
        }
        let text = manager
            .proxy
            .read(address, size as usize, s.timeout(STORAGE)?);
        // The text is m1n1's to free, whether or not it arrived.
        let freed = manager.proxy.call(ENTRIES_FREE, &[out], s.timeout(QUICK)?);
        let text = text.map_err(usb::error)?;
        freed.map_err(usb::error)?;
        let entries = parse_entries(&String::from_utf8_lossy(&text))?;
        Ok((Outcome::Done, entries))
    })
}

impl Manager<'_> {
    /// Room for `size` bytes in the host's share, at an 8-byte boundary.
    fn alloc(&mut self, size: u64) -> Result<u64> {
        let address = self.next.checked_add(7).map(|a| a & !7);
        let end = address.and_then(|a| a.checked_add(size));
        match (address, end) {
            (Some(address), Some(end)) if end <= self.end => {
                self.next = end;
                Ok(address)
            }
            _ => Err(Error::new(
                Code::TooLarge,
                "the host's memory on the device is used up",
            )),
        }
    }

    /// Puts a NUL-terminated string into device memory.
    fn string<D: Connect>(&mut self, session: &Session<D>, text: &str) -> Result<u64> {
        if text.is_empty() || text.len() > 4096 || text.chars().any(char::is_control) {
            return Err(Error::new(
                Code::InvalidInput,
                "an entry or device tree name is one line of up to 4096 bytes",
            ));
        }
        let bytes = [text.as_bytes(), &[0]].concat();
        let address = self.alloc(bytes.len() as u64)?;
        self.proxy
            .write(address, &bytes, session.timeout(QUICK)?)
            .map_err(usb::error)?;
        Ok(address)
    }
}

/// The boot manager's own words from m1n1's console, for an error message.
fn reason(console: &[u8]) -> String {
    let text = String::from_utf8_lossy(console);
    let said: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("bls:") || line.starts_with("bootmgr:"))
        .map(|line| line.chars().filter(|c| !c.is_control()).collect())
        .collect();
    match said.last() {
        Some(line) => format!(": {line}"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, fake, session, Fake, Runs, HEAP};

    const TEXT: &str = "contract 1\n\
        entry old-6.1\n\
        title Old\n\
        version 6.1\n\
        devicetree /old/other.dtb\n\
        problem devicetree is not board.dtb\n\
        \n\
        entry new-6.18\n\
        title New\n\
        version 6.18\n\
        linux /new/linux\n\
        initrd /new/initrd-a\n\
        initrd /new/initrd-b\n\
        options root=PARTUUID=1234 rw quiet\n\
        \n";

    #[test]
    fn entries_text() {
        let entries = parse_entries(TEXT).unwrap();
        assert_eq!(entries.len(), 2);
        let old = &entries[0];
        assert_eq!((old.id.as_str(), old.bootable), ("old-6.1", false));
        assert_eq!(old.problems, ["devicetree is not board.dtb"]);
        let new = &entries[1];
        assert!(new.bootable);
        assert_eq!(new.title.as_deref(), Some("New"));
        assert_eq!(new.version.as_deref(), Some("6.18"));
        assert_eq!(new.keys["initrd"], ["/new/initrd-a", "/new/initrd-b"]);
        assert_eq!(new.keys["options"], ["root=PARTUUID=1234 rw quiet"]);
    }

    #[test]
    fn texts_that_are_not_a_list() {
        assert_eq!(parse_entries("contract 1\n").unwrap(), []);
        let e = parse_entries("contract 2\nentry a\n").unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
        assert_eq!(parse_entries("").unwrap_err().code, Code::Unsupported);
        let e = parse_entries("contract 1\nerror no XBOOTLDR partition\n").unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert_eq!(e.message, "m1n1: no XBOOTLDR partition");
        let e = parse_entries("contract 1\ntitle Stray\n").unwrap_err();
        assert_eq!(e.code, Code::Format);
    }

    #[test]
    fn choosing() {
        let entries = parse_entries(TEXT).unwrap();
        // The first that can boot, not the first.
        assert_eq!(choose(&entries, None).unwrap().id, "new-6.18");
        assert_eq!(choose(&entries, Some("new-6.18")).unwrap().id, "new-6.18");
        let e = choose(&entries, Some("old-6.1")).unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(e.message.ends_with("devicetree is not board.dtb"));
        let e = choose(&entries, Some("gone")).unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(
            e.hint.as_deref(),
            Some("the entries are: old-6.1, new-6.18")
        );
        assert_eq!(choose(&entries[..1], None).unwrap_err().code, Code::Refused);
        assert_eq!(choose(&[], None).unwrap_err().code, Code::NotFound);
    }

    #[test]
    fn console_reason() {
        let console = b"usb: ok\nbls: no linux in entry\nbootmgr: cannot load new-6.18\n";
        assert_eq!(reason(console), ": bootmgr: cannot load new-6.18");
        assert_eq!(reason(b"usb: ok\n"), "");
    }

    /// A device running m1n1 whose boot manager lists `text`.
    fn m1n1(text: &str) -> Fake {
        let fake = fake("", Runs::M1n1, None);
        fake.world().text = text.to_owned();
        fake
    }

    fn boot(fake: &Fake, options: &Options) -> (Result<Booted>, Vec<String>) {
        let mut session = session(fake, None);
        let mut progress = Vec::new();
        let booted = run(&mut session, None, options, &mut |s| {
            progress.push(s.to_owned())
        });
        (booted, progress)
    }

    fn operations(fake: &Fake) -> Vec<u64> {
        fake.world().calls.iter().map(|c| c.0).collect()
    }

    #[test]
    fn boots_the_first_entry_that_can() {
        let fake = m1n1(TEXT);
        let options = Options {
            dtb_name: Some("board.dtb".into()),
            splash: true,
            wait: Some(Mode::Linux),
            ..Options::default()
        };
        let (booted, progress) = boot(&fake, &options);
        let booted = booted.unwrap();
        assert_eq!(booted.entry.unwrap().id, "new-6.18");
        assert_eq!(booted.entries.len(), 2);
        assert!(booted.path.is_none());
        assert_eq!(booted.device.unwrap().mode, Mode::Linux);
        assert_eq!(
            operations(&fake),
            [
                HEAP_ALLOC,
                HEAP_SET_LIMIT,
                STORAGE_INIT,
                ENTRIES,
                ENTRIES_FREE,
                START
            ]
        );
        let world = fake.world();
        // m1n1's heap ends where the host's share begins.
        assert_eq!(world.calls[1].1, [HEAP + M1N1_HEAP]);
        assert_eq!(
            world.started,
            Some(("new-6.18".into(), Some("board.dtb".into()), 0))
        );
        // The step that starts carries the name of what it started.
        assert_eq!(progress[4], "start new-6.18");
        assert_eq!(booted.steps[4].outcome, Outcome::Acknowledged);
        assert_eq!(booted.steps.len(), 7);
    }

    #[test]
    fn boots_the_entry_asked_for_without_splash_or_tree() {
        let fake = m1n1("contract 1\nentry a\n\nentry b\n\n");
        let options = Options {
            entry: Some("b".into()),
            ..Options::default()
        };
        let booted = boot(&fake, &options).0.unwrap();
        assert_eq!(booted.entry.unwrap().id, "b");
        assert_eq!(fake.world().started, Some(("b".into(), None, NO_SPLASH)));
        assert!(booted.device.is_none());
    }

    #[test]
    fn an_entry_that_cannot_boot_is_not_started() {
        let fake = m1n1(TEXT);
        let options = Options {
            entry: Some("old-6.1".into()),
            ..Options::default()
        };
        let e = boot(&fake, &options).0.unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(!operations(&fake).contains(&START));
    }

    #[test]
    fn a_refused_start_says_why_in_m1n1s_words() {
        let fake = m1n1(TEXT);
        fake.world().refuses = true;
        let e = boot(&fake, &Options::default()).0.unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert_eq!(
            e.message,
            "step 5 of 6 (start new-6.18): m1n1 could not start new-6.18: \
             bootmgr: cannot load the kernel"
        );
    }

    #[test]
    fn listing_leaves_storage_up_for_a_boot_on_the_same_m1n1() {
        let fake = m1n1(TEXT);
        let list = Options {
            dtb_name: Some("board.dtb".into()),
            list: true,
            ..Options::default()
        };
        let listed = boot(&fake, &list).0.unwrap();
        assert_eq!(listed.entries.len(), 2);
        assert!(listed.entry.is_none());
        assert_eq!(
            operations(&fake),
            [
                HEAP_ALLOC,
                HEAP_SET_LIMIT,
                STORAGE_INIT,
                ENTRIES,
                ENTRIES_FREE
            ]
        );
        let world = fake.world();
        // The tree name went to the list request as a string in memory.
        assert_eq!(world.string_at(world.calls[3].1[1]), "board.dtb");
        assert!(world.storage);
        drop(world);
        let booted = boot(&fake, &Options::default()).0.unwrap();
        assert_eq!(booted.entry.unwrap().id, "new-6.18");
    }

    #[test]
    fn a_disk_m1n1_cannot_read() {
        let fake = m1n1("contract 1\nerror no XBOOTLDR partition\n");
        let list = Options {
            list: true,
            ..Options::default()
        };
        let e = boot(&fake, &list).0.unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(e.message.ends_with("m1n1: no XBOOTLDR partition"));
    }

    #[test]
    fn a_plan_names_the_steps_on_m1n1_without_a_device() {
        let (_dir, catalog) = fixture::catalog();
        let request = Request {
            from: Some(State::M1n1),
            ..Request::new(&catalog)
        };
        let options = Options {
            wait: Some(Mode::Linux),
            ..Options::default()
        };
        let plan = plan(&request, None::<&Session<Fake>>, &options).unwrap();
        assert!(plan.path.methods.is_empty());
        assert_eq!(plan.then.len(), 7);
        assert_eq!(plan.then[4], "start the first entry that can boot");
        let list = Options {
            list: true,
            ..Options::default()
        };
        assert_eq!(
            super::plan(&request, None::<&Session<Fake>>, &list)
                .unwrap()
                .then
                .len(),
            4
        );
    }
}
