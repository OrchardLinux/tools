//! A device's path: from where the device is to a receiver that waits for a
//! loader (Pongo, a patched iBoot, or an autogo iBEC), and on through the
//! loader where the command loads something.
//!
//! Everything a path sends is made before the first step: the firmware a
//! chain sends is built (unless its keys must come from the entered
//! device), what the loader is sent is packed, and the entry's tool is
//! fetched last, so a missing file of ours stops the work before an
//! executable is downloaded. The steps then run once and in order in one
//! session. A receiver that USB does not tell apart is remembered in the
//! session, with the chain and the files it was prepared with, so a later
//! command of the session knows what waits.
use crate::entry;
use crate::host::{Connect, DfuMode};
use crate::methods::{self, Kind, Method, Place, State};
use crate::session::Session;
use crate::steps::{Outcome, Run, Step};
use crate::{deliver, usb};
use orchard_boards::boot_firmware::{self, Images, Prepared};
use orchard_boards::catalog::{Board, Catalog, Chip};
use orchard_boards::data::loaders;
use orchard_boards::firmware_build::Boot;
use orchard_boards::pack::{self, Linux, Restore};
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use orchard_formats::cascadia::Bundle;
use orchard_usb::dfu::Pwned;
use orchard_usb::{Device, Discovery, Mode};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use std::time::Duration;

/// How long the single steps of a chain may take; the session's deadline
/// can only shorten them.
const SEND: Duration = Duration::from_secs(60);
const IBSS: Duration = Duration::from_secs(30);
const RECOVERY: Duration = Duration::from_secs(30);
/// The pwned boot ROM's return after gaster's reset.
const ROM: Duration = Duration::from_secs(30);

/// What a command works from: orchard's data and the cache, what the user
/// asked for, and what the loader is to start.
#[derive(Clone)]
pub struct Request<'a> {
    pub catalog: &'a Catalog,
    /// Where built firmware, keys, tools and loaders are kept; a plan has
    /// none.
    pub store: Option<&'a Store>,
    /// The key file and manifest a chain's firmware is built with.
    pub firmware: Boot<'a>,
    /// The state the device is in, where USB does not show it.
    pub from: Option<State>,
    pub entry: Option<&'a str>,
    pub chain: Option<&'a str>,
    /// The board, where the device does not report it.
    pub board: Option<&'a str>,
    /// What the loader is to start; with none, the path ends at the
    /// receiver.
    pub load: Option<Load<'a>>,
    /// A mode to wait for after Pongo started the image.
    pub wait: Option<Mode>,
}

impl<'a> Request<'a> {
    /// A request with nothing asked for, and no store.
    pub fn new(catalog: &'a Catalog) -> Request<'a> {
        Request {
            catalog,
            store: None,
            firmware: Boot::default(),
            from: None,
            entry: None,
            chain: None,
            board: None,
            load: None,
            wait: None,
        }
    }
}

/// What a loader is to start.
#[derive(Debug, Clone, Copy)]
pub enum Load<'a> {
    /// A file, with the Linux it starts packed behind it.
    File(&'a [u8], Option<Linux<'a>>),
    /// m1n1 from orchard's builds, which boots an installed system.
    M1n1,
}

/// A device's path: where it is, and the methods that take it on. A plan
/// names each method's steps; a run says how each step ended, and where
/// the device is now.
#[derive(Debug, Clone, Serialize)]
pub struct BootPath {
    /// The board's name in orchard's data, when it is known what the
    /// device is.
    pub board: Option<String>,
    #[serde(skip)]
    pub data: Option<Board>,
    #[serde(skip)]
    pub chip: Option<Chip>,
    pub from: Place,
    pub methods: Vec<MethodSteps>,
    /// After a run: the state the device is in, and the device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
}

/// One method on a path, with the pwned-DFU marker it starts with and its
/// steps.
#[derive(Debug, Clone)]
pub struct MethodSteps {
    pub method: Method,
    pub marker: Option<String>,
    /// The step names, as planned.
    pub steps: Vec<String>,
    /// After a run: each step, and how it ended.
    pub done: Option<Vec<Step>>,
}

/// A plan says what the method needs and leaves and names its steps; a run
/// says where the device went from and to, and how each step ended.
impl Serialize for MethodSteps {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let method = &self.method;
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("kind", &method.kind)?;
        map.serialize_entry("name", &method.name)?;
        let (needs, leaves) = match self.done {
            None => ("needs", "leaves"),
            Some(_) => ("from", "to"),
        };
        map.serialize_entry(needs, &method.needs)?;
        map.serialize_entry(leaves, &method.leaves)?;
        match &self.done {
            None => map.serialize_entry("steps", &self.steps)?,
            Some(done) => map.serialize_entry("steps", done)?,
        }
        map.end()
    }
}

/// What the methods of one command send, made before they run.
#[derive(Default)]
pub struct Files {
    /// Pongo, as the entry's tool uploads it.
    pub pongo: Option<Vec<u8>>,
    /// The Pongo entry's tool, fetched last.
    pub tool: Option<entry::Executable>,
    /// The same worker for entry and for any later key acquisition.
    pub gaster: Option<entry::Executable>,
    /// The built files of the chain on the way, or of the one that left
    /// the receiver.
    pub images: Images,
    /// A chain build that waits for keys from the entered device.
    pub pending: Option<Prepared>,
    /// What the loader is sent: Pongo's image, a patched iBoot's restore
    /// kernel with what goes before it, or an autogo iBEC's bundle.
    pub image: Option<Vec<u8>>,
    pub restore: Option<Restore>,
    pub bundle: Option<Bundle>,
}

/// How the boot ROM a tool pwned takes the iBSS, by the tool's marker: the
/// built file it is given, and the payload that starts it where it is not
/// sent as an ordinary upload. Which chain takes which marker is the
/// data's `markers`.
pub fn start(marker: &str) -> Option<(&'static str, Option<Pwned>)> {
    match marker {
        "checkm8" => Some(("ibss.raw", Some(Pwned::Checkm8))),
        "usbliter8" => Some(("ibss.raw", Some(Pwned::Usbliter8))),
        "gaster" => Some(("ibss.img4", None)),
        _ => None,
    }
}

/// Fails unless the device still shows the boot ROM `marker` names.
fn still(device: &Device, marker: &str) -> Result<()> {
    if device.mode == Mode::Dfu && device.identity.pwned.as_deref() == Some(marker) {
        return Ok(());
    }
    Err(Error::new(
        Code::DeviceChanged,
        format!("the device no longer shows the {marker} pwned-DFU marker"),
    ))
}

fn invalid<T>(message: String) -> Result<T> {
    Err(Error::new(Code::InvalidInput, message))
}

/// The board the path is for, where the device has said which it is.
pub(crate) fn board(path: &BootPath) -> Result<&Board> {
    path.data.as_ref().ok_or_else(|| {
        Error::new(Code::NotFound, "the device does not say which board it is")
            .hint("name the board with --board")
    })
}

pub(crate) fn known_chip(chip: Option<&Chip>) -> Result<&Chip> {
    chip.ok_or_else(|| Error::new(Code::NotFound, "the device's chip is not known"))
}

/// Runs an entry's tool; see [`entry::Entry::run`].
pub fn entry<D: Connect>(
    r: &mut Run<D>,
    at: &MethodSteps,
    _: Option<&Chip>,
    _: &Request,
    files: &mut Files,
) -> Result<()> {
    let name = &at.method.name;
    let entry = entry::named(name).ok_or_else(|| {
        Error::new(
            Code::Unsupported,
            format!("{name}: not run by this version"),
        )
    })?;
    let pongo = files.pongo.as_deref();
    if at.method.leaves == State::Pongo && pongo.is_none() && !r.planning() {
        return Err(Error::new(
            Code::NotFound,
            "Pongo was not fetched for the entry",
        ));
    }
    let tool = if name == entry::GASTER.name {
        files.gaster.as_ref()
    } else {
        files.tool.as_ref()
    };
    entry.run(r, tool, pongo.unwrap_or_default())
}

/// The `iboot` chain: starts the patched iBSS on a pwned boot ROM and, on
/// the chips that load one, sends the patched iBEC after it. Leaves a
/// patched iBoot in recovery mode.
pub fn iboot<D: Connect>(
    r: &mut Run<D>,
    at: &MethodSteps,
    chip: Option<&Chip>,
    request: &Request,
    files: &mut Files,
) -> Result<()> {
    let marker = started("iboot", r, at)?;
    let ibec = pack::two_stage(known_chip(chip)?).then_some(("ibec.img4", "dfu send, the iBEC"));
    chain(r, request, files, Some(&marker), ibec)
}

/// The `iboot-autogo` chain. From a pwned boot ROM it starts the raw iBSS
/// and sends the iBEC encrypted as the firmware's own is, which the device
/// can still decrypt there. After kDFU a patched iBSS runs already and the
/// device's own key is off, so it gets the iBEC unencrypted. Leaves an
/// autogo iBEC in recovery mode.
pub fn autogo<D: Connect>(
    r: &mut Run<D>,
    at: &MethodSteps,
    _: Option<&Chip>,
    request: &Request,
    files: &mut Files,
) -> Result<()> {
    if at.method.needs == State::Kdfu {
        starts_in(r, Mode::Dfu, "iboot-autogo", State::Kdfu)?;
        if let Some(marker) = r.session().and_then(|s| s.device().identity.pwned.clone()) {
            return invalid(format!(
                "iboot-autogo starts from kdfu here, and the device shows a pwned boot ROM (marker {marker})"
            ));
        }
        let plain = ("ibec.plain.img3", "dfu send, the plain iBEC");
        return chain(r, request, files, None, Some(plain));
    }
    let marker = started("iboot-autogo", r, at)?;
    let ibec = ("ibec.encrypted.img3", "dfu send, the encrypted iBEC");
    chain(r, request, files, Some(&marker), Some(ibec))
}

/// Fails unless the session's device is in `mode`, where `method` starts
/// from `state`.
pub(crate) fn starts_in<D: Connect>(
    r: &mut Run<D>,
    mode: Mode,
    method: &str,
    state: State,
) -> Result<()> {
    match r.session().map(|s| s.device().mode) {
        Some(found) if found != mode => invalid(format!(
            "{method} starts from {}, and the device is in {} mode",
            state.as_str(),
            found.as_str()
        )),
        _ => Ok(()),
    }
}

/// The marker of the boot ROM a chain from pwned DFU starts on: the
/// device's in a run, the planned one in a plan. A device that does not
/// show a pwned boot ROM is refused before anything is sent.
fn started<D: Connect>(chain: &str, r: &mut Run<D>, at: &MethodSteps) -> Result<String> {
    starts_in(r, Mode::Dfu, chain, State::PwnedDfu)?;
    let marker = match r.session() {
        Some(session) => session.device().identity.pwned.clone().ok_or_else(|| {
            Error::new(
                Code::InvalidInput,
                format!("{chain} starts from pwned-dfu here, and the device shows no pwned marker"),
            )
        })?,
        None => at.marker.clone().ok_or_else(|| {
            Error::new(
                Code::Unsupported,
                format!("{chain}: the pwned-DFU start needs the device's marker"),
            )
            .hint("connect the pwned device and plan without --from")
        })?,
    };
    match start(&marker) {
        Some(_) => Ok(marker),
        None => Err(Error::new(
            Code::Unsupported,
            format!("{chain}: no pwned-DFU start for the {marker} marker"),
        )),
    }
}

const WAIT_IBSS: &str = "wait until the iBSS is in dfu mode";

/// A chain's steps: the iBSS started on the boot ROM `marker` names where
/// there is one, the next image sent to the running iBSS where there is
/// one, then the wait for recovery mode. Where the iBSS takes an image
/// after the start, it shows up in DFU mode again and takes it as an
/// ordinary upload.
fn chain<D: Connect>(
    r: &mut Run<D>,
    request: &Request,
    files: &mut Files,
    marker: Option<&str>,
    then: Option<(&str, &str)>,
) -> Result<()> {
    let ibss = marker.map(|m| start(m).expect("checked when started"));
    if let Some((_, None)) = ibss {
        keys(r, request, files)?;
    }
    let planning = r.planning();
    let file = |name: &str| match planning {
        true => Ok(&[][..]),
        false => pack::file(&files.images, name),
    };
    let ibss = ibss
        .map(|(name, payload)| Ok::<_, Error>((file(name)?, payload)))
        .transpose()?;
    let image = then.map(|(name, _)| file(name)).transpose()?;
    let mut dfu = r.dfu()?;
    if let (Some(marker), Some((ibss, payload))) = (marker, ibss) {
        if payload.is_none() {
            r.step("dfu clear status before gaster reset", |s| {
                still(s.check()?, marker)?;
                dfu.clear_status(s.timeout(SEND)?).map_err(usb::error)?;
                Ok((Outcome::Done, ()))
            })?;
            r.step("dfu reset the pwned boot ROM", |s| {
                still(s.check()?, marker)?;
                dfu.reset().map_err(usb::error)?;
                Ok((Outcome::Done, ()))
            })?;
            drop(dfu);
            let back = r.step("wait for the pwned boot ROM after reset", |s| {
                // reset has completed; the boot ROM may already be back.
                still(s.wait_for(Mode::Dfu, ROM)?, marker)?;
                Ok((Outcome::Done, Some(s.dfu()?)))
            })?;
            dfu = reopened(r, back)?;
        }
        let name = match payload {
            Some(_) => format!("dfu start with {marker}, the raw iBSS"),
            None => "dfu send, the wrapped iBSS".to_owned(),
        };
        r.step(name, |s| {
            still(s.check()?, marker)?;
            let outcome = match payload {
                Some(payload) => Outcome::of(
                    dfu.start_pwned(payload, ibss, s.timeout(SEND)?)
                        .map_err(usb::error)?,
                ),
                None => {
                    dfu.send(ibss, s.timeout(SEND)?).map_err(usb::error)?;
                    Outcome::Done
                }
            };
            Ok((outcome, ()))
        })?;
    }
    if let (Some((_, label)), Some(image)) = (then, image) {
        if marker.is_some() {
            drop(dfu);
            let again = r.step(WAIT_IBSS, |s| {
                let device = s.wait_again(IBSS)?;
                // A reset alone also makes the boot ROM enumerate again. Its
                // marker tells it from the iBSS, which has none.
                if let Some(marker) = &device.identity.pwned {
                    return Err(Error::new(
                        Code::DeviceChanged,
                        format!("the boot ROM is back (marker {marker}); the iBSS did not start"),
                    ));
                }
                Ok((Outcome::Done, Some(s.dfu()?)))
            })?;
            dfu = reopened(r, again)?;
        }
        r.step(label, |s| {
            if marker.is_none() {
                s.check()?;
            }
            dfu.send(image, s.timeout(SEND)?).map_err(usb::error)?;
            Ok((Outcome::Done, ()))
        })?;
    }
    drop(dfu);
    r.step("wait recovery", |s| {
        s.wait_for(Mode::Recovery, RECOVERY)?;
        Ok((Outcome::Done, ()))
    })
}

/// The DFU handle a step opened on the device that came back; a plan's.
fn reopened<D: Connect>(
    r: &mut Run<D>,
    opened: Option<Box<dyn DfuMode>>,
) -> Result<Box<dyn DfuMode>> {
    match opened {
        Some(dfu) => Ok(dfu),
        None => r.dfu(),
    }
}

/// On a boot ROM gaster left: the firmware keys a cold build lacks,
/// unwrapped on the device, and the build finished with them.
fn keys<D: Connect>(r: &mut Run<D>, request: &Request, files: &mut Files) -> Result<()> {
    r.step(
        "unwrap the firmware keys the build lacks, and build it",
        |s| {
            if let (Some(mut pending), Some(store)) = (files.pending.take(), request.store) {
                let worker = files.gaster.as_ref().ok_or_else(|| {
                    Error::new(Code::NotFound, "gaster: the worker was not prepared")
                })?;
                pending.acquire(|bags| s.gaster(Some(bags), worker))?;
                files.images = pending.finish(store)?;
            }
            Ok((Outcome::Done, ()))
        },
    )
}

/// The methods `board` cannot take, each with the reason: the chains it
/// has no recipe for or whose recipe this version cannot build, and a
/// loader it lacks data for.
fn refused(catalog: &Catalog, board: &Board, methods: &[Method]) -> Result<Vec<(String, String)>> {
    let recipes = board.boot_data()?.chains;
    let mut refused = Vec::new();
    for method in methods.iter().filter(|m| m.kind == Kind::Chain) {
        let why = match recipes.contains_key(&method.name) {
            false => Some("no recipe for this board".to_owned()),
            true => boot_firmware::Plan::new(catalog, board, Some(&method.name))
                .err()
                .map(|e| e.message),
        };
        refused.extend(why.map(|why| (method.name.clone(), why)));
    }
    let why = match board.cascadia() {
        Ok(_) => return Ok(refused),
        Err(e) if e.code == Code::NotFound => "no loader data for this board".to_owned(),
        Err(e) => e.message,
    };
    refused.push(("cascadia".to_owned(), why));
    Ok(refused)
}

/// Finds where a connected device is, or, without one, takes the stated
/// state of the named board, and picks the methods for it.
fn choose<D: Discovery>(
    request: &Request,
    methods: &[Method],
    session: Option<&Session<D>>,
) -> Result<BootPath> {
    let catalog = request.catalog;
    let from = match (session, request.from) {
        (Some(session), stated) => methods::place(session.device(), session.left_in(), stated)?,
        (None, Some(state)) => Place {
            state,
            marker: None,
        },
        (None, None) => return Err(Error::new(Code::NoDevice, "no matching device connected")),
    };
    let hex = |s: &Option<String>| u32::from_str_radix(s.as_deref()?, 16).ok();
    let reported = session.and_then(|s| {
        let identity = s.identity();
        Some((hex(&identity.chip)?, hex(&identity.board_id)?))
    });
    let board = match (request.board, reported) {
        (Some(name), reported) => {
            let board = catalog.board(name)?;
            if reported.is_some_and(|ids| ids != (board.chip, board.board)) {
                return Err(Error::new(
                    Code::InvalidInput,
                    format!("the device is not a {name}: it reports another chip or board number"),
                )
                .hint("`orchard2 devices` shows the board of each device"));
            }
            Some(board)
        }
        (None, Some((chip, board))) => Some(catalog.board_for(chip, board)?),
        (None, None) => None,
    };
    let chip = board.as_ref().map(|b| catalog.chip(b.chip)).transpose()?;
    let refused = match &board {
        Some(board) => refused(catalog, board, methods)?,
        None => Vec::new(),
    };
    let picked = methods::pick(methods, &from, chip.as_ref(), &refused, request);
    Ok(BootPath {
        methods: picked.map_err(|e| match &board {
            Some(board) => e.context(&board.name),
            None => e,
        })?,
        board: board.as_ref().map(|b| b.name.clone()),
        data: board,
        chip,
        from,
        state: None,
        device: None,
    })
}

/// The names of the steps `at` runs, from a plan of it.
fn plan_steps<D: Connect>(
    at: &MethodSteps,
    chip: Option<&Chip>,
    request: &Request,
) -> Result<Vec<String>> {
    let (does, _) = methods::does::<D>(&at.method).ok_or_else(|| {
        Error::new(
            Code::Unsupported,
            format!("{}: not run by this version", at.method.name),
        )
    })?;
    let mut quiet = |_: &str| {};
    let mut r = Run::<D>::plan(&mut quiet);
    does(&mut r, at, chip, request, &mut Files::default())?;
    Ok(r.names())
}

/// What a command would do, from where the session's device is or, without
/// one, from the stated state of the named board: the methods with their
/// steps, up to the receiver, and the loader where the request loads
/// something.
pub fn plan<D: Discovery>(request: &Request, session: Option<&Session<D>>) -> Result<BootPath> {
    let methods = methods::read(request.catalog)?;
    let mut path = choose(request, &methods, session)?;
    if request.load.is_none() {
        path.methods.pop();
    }
    for i in 0..path.methods.len() {
        path.methods[i].steps =
            plan_steps::<orchard_usb::Host>(&path.methods[i], path.chip.as_ref(), request)?;
    }
    Ok(path)
}

/// A loader's file from the record of orchard's builds.
pub(crate) fn loader_file(store: &Store, loader: &str, asset: &str) -> Result<Vec<u8>> {
    loaders::file(store, &loaders::resolve(store)?, loader, asset)
}

/// Builds the chain's firmware now, or keeps the build for keys from the
/// device where it lacks them.
fn build(
    request: &Request,
    store: &Store,
    board: &Board,
    chain: &str,
    files: &mut Files,
) -> Result<()> {
    let plan = boot_firmware::Plan::new(request.catalog, board, Some(chain))?;
    let boot = Boot {
        chain: Some(chain),
        ..request.firmware
    };
    let prepared = plan.prepare(board, store, &boot)?;
    if prepared.needs_device() {
        prepared.check_acquisition()?;
        files.pending = Some(prepared);
    } else {
        files.images = prepared.finish(store)?;
    }
    Ok(())
}

/// What the session remembers a receiver was prepared with.
fn note(chain: &str, images: &Images) -> String {
    format!("{chain} {}", pack::digest(images))
}

/// What the path's methods send, made now. A receiver prepared earlier
/// gets the files of the chain that left it, and must have been prepared
/// with the files these inputs build.
fn prepare(
    request: &Request,
    store: &Store,
    methods: &[Method],
    path: &BootPath,
    note_left: Option<&str>,
) -> Result<Files> {
    let mut files = Files::default();
    for at in &path.methods {
        match at.method.kind {
            Kind::Entry if at.method.leaves == State::Pongo => {
                files.pongo = Some(loader_file(store, "pongo", "pongo.bin")?)
            }
            Kind::Chain => build(request, store, board(path)?, &at.method.name, &mut files)?,
            _ => {}
        }
    }
    let receiver = path
        .methods
        .last()
        .expect("a path ends at a loader")
        .method
        .needs;
    let chained = path.methods.iter().any(|m| m.method.kind == Kind::Chain);
    if receiver == State::PatchedRecovery && !chained {
        let board = board(path)?;
        let refused = refused(request.catalog, board, methods)?;
        let remembered = note_left.and_then(|n| n.split(' ').next());
        let leaving = methods.iter().find(|m| {
            m.kind == Kind::Chain
                && m.leaves == receiver
                && !refused.iter().any(|(n, _)| *n == m.name)
        });
        let name = remembered
            .or(request.chain)
            .or(leaving.map(|m| m.name.as_str()))
            .ok_or_else(|| {
                Error::new(
                    Code::NotFound,
                    format!(
                        "{}: no chain recipe gives the device tree m1n1 is started with",
                        board.name
                    ),
                )
            })?;
        let plan = boot_firmware::Plan::new(request.catalog, board, Some(name))?;
        let boot = Boot {
            chain: Some(name),
            ..request.firmware
        };
        files.images = plan.images(board, store, &boot)?;
        if note_left.is_some_and(|n| n != note(name, &files.images)) {
            return Err(Error::new(
                Code::InvalidInput,
                "the device was prepared with other boot firmware than these inputs build",
            )
            .hint("put the device into DFU mode and run again"));
        }
    }
    Ok(files)
}

/// Runs the path's method `i` on the session's device, after a plan of it
/// names its steps. A state USB does not show is remembered in the session.
fn run_one<D: Connect>(
    session: &mut Session<D>,
    path: &BootPath,
    i: usize,
    request: &Request,
    files: &mut Files,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Step>> {
    let at = &path.methods[i];
    let chip = path.chip.as_ref();
    let names = plan_steps::<D>(at, chip, request)?;
    let (does, _) = methods::does::<D>(&at.method).expect("planned");
    let mut r = Run::new(session, names, progress);
    does(&mut r, at, chip, request, files)?;
    let done = r.finish();
    let method = &at.method;
    if !method.leaves.shown() {
        // Recovery mode looks the same whatever iBoot runs in it.
        session.arrived_in(method.leaves);
        if method.kind == Kind::Chain {
            session.prepared_with(note(&method.name, &files.images));
        }
    }
    Ok(done)
}

/// Takes the session's device along its path: to a receiver, using the one
/// it waits in where that fits, and where the request loads something,
/// packs it and starts it there.
pub fn run<D: Connect>(
    session: &mut Session<D>,
    request: &Request,
    progress: &mut dyn FnMut(&str),
) -> Result<BootPath> {
    let store = request.store.expect("a run has a store");
    let methods = methods::read(request.catalog)?;
    let mut path = choose(request, &methods, Some(&*session))?;
    let mut files = prepare(request, store, &methods, &path, session.prep())?;
    let starts_with_gaster = path
        .methods
        .first()
        .is_some_and(|m| m.method.kind == Kind::Entry && m.method.name == "gaster");
    if files.pending.is_some()
        && !starts_with_gaster
        && path.from.marker.as_deref() != Some("gaster")
    {
        return Err(Error::new(
            Code::InvalidInput,
            "missing firmware keys cannot be acquired from this starting state",
        )
        .hint(
            "prepare keys with fw build --device-keys from compatible DFU, or supply --keys FILE",
        ));
    }
    let serial = session.identity().serial_number.clone();
    match request.load {
        Some(_) => deliver::pack(request, store, &path, serial.as_deref(), &mut files)?,
        None => {
            path.methods.pop();
        }
    }
    // The tool comes last: a path that lacks a file of ours stops before an
    // executable is downloaded.
    let entry = path
        .methods
        .iter()
        .find(|m| m.method.kind == Kind::Entry)
        .and_then(|at| entry::named(&at.method.name));
    if entry.is_some_and(|e| e.name == entry::GASTER.name) || files.pending.is_some() {
        files.gaster = Some(session.gaster_worker(store)?);
    }
    if let Some(entry) = entry.filter(|e| e.name != entry::GASTER.name) {
        files.tool = Some(entry.tool(store)?);
    }
    for i in 0..path.methods.len() {
        // A restore kernel is packed once the chain has its keys.
        let packed = files.image.is_some() || files.restore.is_some() || files.bundle.is_some();
        if path.methods[i].method.kind == Kind::Loader && !packed {
            deliver::pack(request, store, &path, serial.as_deref(), &mut files)?;
        }
        path.methods[i].done = Some(run_one(session, &path, i, request, &mut files, progress)?);
    }
    path.state = Some(
        path.methods
            .last()
            .map_or(path.from.state, |m| m.method.leaves),
    );
    path.device = Some(session.device().clone());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{
        self, chip, data, fake, method, one, session, Did, Fake, Runs, FIRST, SECOND,
    };

    fn images(names: &[&str]) -> Images {
        names
            .iter()
            .map(|n| ((*n).to_owned(), n.as_bytes().to_vec()))
            .collect()
    }

    const IMG4: &[&str] = &["ibss.img4", "ibss.raw", "ibec.img4", "devicetree.img4"];
    const IMG3: &[&str] = &["ibss.raw", "ibec.plain.img3", "ibec.encrypted.img3"];

    fn chain(fake: &Fake, name: &str, from: State, stages: &[&str], built: &[&str]) -> Did {
        let leaves = match name {
            "iboot" => State::PatchedRecovery,
            _ => State::Autogo,
        };
        let mut files = Files {
            images: images(built),
            ..Files::default()
        };
        let method = method(Kind::Chain, name, from, leaves);
        let mut session = session(fake, Some(Duration::from_secs(5)));
        one(&mut session, method, &chip(stages), &mut files, None)
    }

    #[test]
    fn iboot_starts_the_ibss_and_sends_the_ibec_where_the_chip_loads_one() {
        // A chip that loads the iBSS alone comes up in recovery mode.
        let fake = fake(FIRST, Runs::Rom, Some("usbliter8"));
        let did = chain(&fake, "iboot", State::PwnedDfu, &["iBSS"], IMG4);
        assert_eq!(did.result.unwrap().len(), 2);
        assert_eq!(fake.world().sent, ["start ibss.raw"]);
        assert_eq!(fake.world().payloads, [Pwned::Usbliter8]);
        assert_eq!(did.progress, did.plan);
        assert_eq!(
            did.plan,
            ["dfu start with usbliter8, the raw iBSS", "wait recovery"]
        );

        // With an iBEC: the iBSS shows up in DFU mode again and takes it.
        let fake = self::fake(SECOND, Runs::Rom, Some("gaster"));
        fake.world().after_start = Runs::Ibss;
        let did = chain(&fake, "iboot", State::PwnedDfu, &["iBSS", "iBEC"], IMG4);
        assert_eq!(did.result.unwrap().len(), 8);
        assert_eq!(
            fake.world().sent,
            ["clear status", "reset", "send ibss.img4", "send ibec.img4"]
        );
        assert_eq!(did.progress, did.plan);
    }

    #[test]
    fn autogo_sends_the_encrypted_ibec_after_the_exploit_and_the_plain_one_after_kdfu() {
        let fake = fake(SECOND, Runs::Rom, Some("checkm8"));
        fake.world().after_start = Runs::Ibss;
        let stages = &["iBSS", "iBEC"];
        let did = chain(&fake, "iboot-autogo", State::PwnedDfu, stages, IMG3);
        assert_eq!(did.result.unwrap().len(), 4);
        assert_eq!(
            fake.world().sent,
            ["start ibss.raw", "send ibec.encrypted.img3"]
        );
        assert_eq!(fake.world().payloads, [Pwned::Checkm8]);

        let fake = self::fake(SECOND, Runs::Ibss, None);
        let did = chain(&fake, "iboot-autogo", State::Kdfu, stages, IMG3);
        assert_eq!(did.plan, ["dfu send, the plain iBEC", "wait recovery"]);
        did.result.unwrap();
        assert_eq!(fake.world().sent, ["send ibec.plain.img3"]);
    }

    #[test]
    fn a_boot_rom_that_comes_back_is_not_taken_for_the_ibss() {
        let fake = fake(SECOND, Runs::Rom, Some("checkm8"));
        fake.world().after_start = Runs::Rom;
        let stages = &["iBSS", "iBEC"];
        let did = chain(&fake, "iboot-autogo", State::PwnedDfu, stages, IMG3);
        let e = did.result.unwrap_err();
        assert_eq!(e.code, Code::DeviceChanged);
        assert_eq!(
            e.message,
            "step 2 of 4 (wait until the iBSS is in dfu mode): \
             the boot ROM is back (marker checkm8); the iBSS did not start"
        );
        assert_eq!(fake.world().sent, ["start ibss.raw"]);
    }

    #[test]
    fn a_chain_refuses_a_device_that_does_not_show_its_state() {
        let cases = [
            (
                fake(SECOND, Runs::Ibss, None),
                "iboot-autogo",
                State::PwnedDfu,
                "iboot-autogo starts from pwned-dfu here, and the device shows no pwned marker",
            ),
            (
                fake(SECOND, Runs::Rom, Some("checkm8")),
                "iboot-autogo",
                State::Kdfu,
                "iboot-autogo starts from kdfu here, and the device shows a pwned boot ROM (marker checkm8)",
            ),
            (
                fake(FIRST, Runs::Iboot, None),
                "iboot",
                State::PwnedDfu,
                "iboot starts from pwned-dfu, and the device is in recovery mode",
            ),
        ];
        for (fake, name, from, message) in cases {
            let e = chain(&fake, name, from, &["iBSS"], IMG3)
                .result
                .unwrap_err();
            assert_eq!(e.code, Code::InvalidInput);
            assert_eq!(e.message, message);
            assert!(fake.world().sent.is_empty());
        }
    }

    #[test]
    fn a_missing_file_or_an_unknown_marker_is_found_before_usb_is_opened() {
        let fake = fake(SECOND, Runs::Rom, Some("checkm8"));
        let did = chain(
            &fake,
            "iboot-autogo",
            State::PwnedDfu,
            &["iBSS"],
            &["ibss.raw"],
        );
        let e = did.result.unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(
            e.message,
            "the chain's boot firmware has no ibec.encrypted.img3"
        );
        assert_eq!(fake.world().opened, 0);
        for (marker, missing) in [("gaster", "ibss.img4"), ("usbliter8", "ibss.raw")] {
            let fake = self::fake(FIRST, Runs::Rom, Some(marker));
            let built: Vec<&str> = IMG4.iter().copied().filter(|n| *n != missing).collect();
            let did = chain(&fake, "iboot", State::PwnedDfu, &["iBSS"], &built);
            assert!(did.result.unwrap_err().message.contains(missing));
            assert_eq!(fake.world().opened, 0);
        }
        let fake = self::fake(FIRST, Runs::Rom, Some("unknown"));
        let e = chain(&fake, "iboot", State::PwnedDfu, &["iBSS"], IMG4)
            .result
            .unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
        assert_eq!(
            e.message,
            "iboot: no pwned-DFU start for the unknown marker"
        );
        assert_eq!(fake.world().opened, 0);
    }

    #[test]
    fn gaster_checks_the_marker_after_reset() {
        let fake = fake(FIRST, Runs::Rom, Some("gaster"));
        fake.world().reset_marker = Some("usbliter8");
        let e = chain(&fake, "iboot", State::PwnedDfu, &["iBSS"], IMG4)
            .result
            .unwrap_err();
        assert_eq!(e.code, Code::DeviceChanged);
        assert_eq!(fake.world().sent, ["clear status", "reset"]);
    }

    fn palera1n(fake: &Fake, limit: Duration) -> Did {
        let mut files = Files {
            pongo: Some(b"PONGO".to_vec()),
            tool: Some(entry::Executable {
                bytes: b"tool".to_vec(),
                dir: std::env::temp_dir(),
            }),
            ..Files::default()
        };
        let method = method(Kind::Entry, "palera1n", State::Dfu, State::Pongo);
        let mut session = session(fake, Some(limit));
        one(&mut session, method, &chip(&[]), &mut files, None)
    }

    #[test]
    fn an_entry_tool_takes_the_one_device_in_dfu_to_pongo() {
        let fake = fake(FIRST, Runs::Rom, None);
        let did = palera1n(&fake, Duration::from_secs(5));
        did.result.unwrap();
        assert_eq!(fake.world().sent, ["palera1n -p -k pongo.bin"]);
        // The cable hint follows the first step.
        assert_eq!(
            did.progress,
            [
                "run palera1n -p -k pongo.bin",
                "If the Apple logo stays, reconnect the cable at the device.",
                "wait pongo"
            ]
        );
        assert_eq!(did.plan, ["run palera1n -p -k pongo.bin", "wait pongo"]);

        for code in [Code::Io, Code::Timeout] {
            let fake = self::fake(FIRST, Runs::Rom, None);
            fake.world().tool_error = Some(code);
            let did = palera1n(&fake, Duration::from_secs(5));
            assert_eq!(did.result.unwrap_err().code, code);
        }
        let fake = self::fake(FIRST, Runs::Rom, None);
        fake.world().extra_dfu = true;
        let e = palera1n(&fake, Duration::from_secs(5)).result.unwrap_err();
        assert_eq!(e.code, Code::Ambiguous);
        assert!(fake.world().sent.is_empty());
        // Pongo at another port is not the session's device.
        let fake = self::fake(FIRST, Runs::Rom, None);
        fake.world().wrong_port = true;
        let did = palera1n(&fake, Duration::from_millis(50));
        assert_eq!(did.result.unwrap_err().code, Code::Timeout);
    }

    /// Takes the session's device into m1n1, as `boot` does.
    fn to_m1n1(session: &mut Session<Fake>, catalog: &Catalog, store: &Store) -> Result<BootPath> {
        let request = Request {
            store: Some(store),
            load: Some(Load::M1n1),
            ..Request::new(catalog)
        };
        run(session, &request, &mut |_| {})
    }

    fn prep(session: &mut Session<Fake>, catalog: &Catalog, store: &Store) -> Result<BootPath> {
        let request = Request {
            store: Some(store),
            ..Request::new(catalog)
        };
        run(session, &request, &mut |_| {})
    }

    fn names(path: &BootPath) -> Vec<&str> {
        path.methods
            .iter()
            .map(|m| m.method.name.as_str())
            .collect()
    }

    fn cold(serial: &'static str, marker: Option<&'static str>) -> (Fake, Session<Fake>) {
        let fake = fake(serial, Runs::Rom, marker);
        let session = session(&fake, Some(Duration::from_secs(10)));
        (fake, session)
    }

    #[test]
    fn a_cold_build_enters_once_unwraps_builds_and_goes_on_in_the_same_session() {
        let (_dir, catalog, store) = data();
        let (fake, mut session) = cold(FIRST, None);
        let went = to_m1n1(&mut session, &catalog, &store).unwrap();
        assert_eq!(went.state, Some(State::M1n1));
        assert_eq!(went.device.as_ref().unwrap().mode, Mode::M1n1);
        assert_eq!(names(&went), ["gaster", "iboot", "m1n1"]);
        assert_eq!(fake.world().worker_resolutions, 1);
        let sent = fake.world().sent.clone();
        assert_eq!(sent[..2], ["gaster entry", "unwrap 1 keybags"]);
        assert_eq!(sent[2..4], ["clear status", "reset"]);
        assert!(sent[4].starts_with("send "));
        assert_eq!(sent.last().unwrap(), "recovery command bootx");
        assert!(store.dir().join("firmware-keys").is_dir());
        // A run's JSON says where each method went from and to, and how
        // each step ended.
        let json = serde_json::to_value(&went).unwrap();
        assert_eq!(json["methods"][0]["to"], "pwned-dfu");
        assert_eq!(json["methods"][0]["steps"][0]["outcome"], "done");
        assert_eq!(json["state"], "m1n1");

        // The plan names the steps that ran.
        let (looking, looked) = cold(FIRST, None);
        let request = Request {
            load: Some(Load::M1n1),
            ..Request::new(&catalog)
        };
        let planned = plan(&request, Some(&looked)).unwrap();
        assert_eq!(looking.world().worker_resolutions, 0);
        for (plan, ran) in planned.methods.iter().zip(&went.methods) {
            let steps: Vec<&str> = ran.done.iter().flatten().map(|s| s.name.as_str()).collect();
            assert_eq!(plan.steps, steps);
        }

        // Warm: the keys are there, and nothing is unwrapped.
        let (warm, mut session) = cold(FIRST, None);
        to_m1n1(&mut session, &catalog, &store).unwrap();
        assert!(!warm.world().sent.iter().any(|s| s.starts_with("unwrap")));
    }

    #[test]
    fn an_entered_device_only_needs_the_worker_for_missing_keys() {
        let (_dir, catalog, store) = data();
        let (fake, mut session) = cold(FIRST, Some("gaster"));
        let went = to_m1n1(&mut session, &catalog, &store).unwrap();
        assert_eq!(names(&went), ["iboot", "m1n1"]);
        assert_eq!(fake.world().sent[0], "unwrap 1 keybags");
        assert_eq!(fake.world().worker_resolutions, 1);

        let (warm, mut session) = cold(FIRST, Some("gaster"));
        warm.world().worker_error = Some(Code::Network);
        to_m1n1(&mut session, &catalog, &store).unwrap();
        assert_eq!(warm.world().worker_resolutions, 0);
    }

    #[test]
    fn worker_resolution_failure_stops_before_device_changes() {
        let (_dir, catalog, store) = data();
        for marker in [None, Some("gaster")] {
            let (fake, mut session) = cold(FIRST, marker);
            fake.world().worker_error = Some(Code::Network);
            let error = to_m1n1(&mut session, &catalog, &store).unwrap_err();
            assert_eq!(error.code, Code::Network);
            assert_eq!(fake.world().worker_resolutions, 1);
            assert!(fake.world().sent.is_empty());
        }
    }

    #[test]
    fn what_is_missing_stops_the_path_before_anything_is_sent() {
        // Failed key acquisition sends nothing further.
        let (_dir, catalog, store) = data();
        let (fake, mut session) = cold(FIRST, None);
        fake.world().key_error = true;
        let e = to_m1n1(&mut session, &catalog, &store).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert_eq!(fake.world().sent, ["gaster entry", "unwrap 1 keybags"]);
        assert!(!store.dir().join("firmware-keys").exists());

        // Keys are not taken from a boot ROM another tool pwned.
        let (fake, mut session) = cold(FIRST, Some("usbliter8"));
        let e = to_m1n1(&mut session, &catalog, &store).unwrap_err();
        assert!(e.message.contains("starting state"), "{}", e.message);
        assert!(fake.world().sent.is_empty());

        // A missing loader stops before the entry runs.
        let loaders = r#"{"release":"test","checked":0,"loaders":{}}"#;
        std::fs::write(store.dir().join("data/loaders.json"), loaders).unwrap();
        let (fake, mut session) = cold(FIRST, None);
        assert!(to_m1n1(&mut session, &catalog, &store).is_err());
        assert!(fake.world().sent.is_empty());
    }

    #[test]
    fn a_prepared_receiver_is_remembered_with_what_prepared_it() {
        let (_dir, catalog, store) = data();
        let (fake, mut session) = cold(FIRST, None);
        let went = prep(&mut session, &catalog, &store).unwrap();
        assert_eq!(went.state, Some(State::PatchedRecovery));
        assert_eq!(session.left_in(), Some(State::PatchedRecovery));
        assert!(session.prep().unwrap().starts_with("iboot "));

        // A prep of a device that waits in its receiver does nothing.
        let again = prep(&mut session, &catalog, &store).unwrap();
        assert!(again.methods.is_empty());
        // A load goes on from it with the files it was prepared with.
        let before = fake.world().sent.len();
        let went = to_m1n1(&mut session, &catalog, &store).unwrap();
        assert_eq!(names(&went), ["m1n1"]);
        let board = catalog.board("x").unwrap();
        let boot = Boot {
            chain: Some("iboot"),
            ..Boot::default()
        };
        let built = boot_firmware::Plan::new(&catalog, &board, Some("iboot"))
            .unwrap()
            .images(&board, &store, &boot)
            .unwrap();
        let tree = pack::file(&built, "devicetree.img4").unwrap().len();
        assert_eq!(fake.world().sent[before], format!("recovery send {tree}"));

        // Prepared with other files, the receiver is not used.
        let (_, mut other) = cold(FIRST, None);
        prep(&mut other, &catalog, &store).unwrap();
        other.prepared_with("iboot another-digest".into());
        let e = to_m1n1(&mut other, &catalog, &store).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert_eq!(
            e.hint.as_deref(),
            Some("put the device into DFU mode and run again")
        );
    }

    #[test]
    fn an_offline_plan_needs_only_the_board_and_the_state() {
        let (_dir, catalog, _store) = data();
        let offline = |board, from, load| {
            let request = Request {
                from: Some(from),
                board: Some(board),
                load,
                ..Request::new(&catalog)
            };
            plan(&request, None::<&Session<Fake>>).unwrap()
        };
        let loader = fixture::loader();
        let planned = offline("y", State::Kdfu, Some(Load::File(&loader, None)));
        assert_eq!(names(&planned), ["iboot-autogo", "cascadia"]);
        assert_eq!(
            planned.methods[0].steps,
            ["dfu send, the plain iBEC", "wait recovery"]
        );
        assert_eq!(planned.methods[1].steps.last().unwrap(), "wait linux");
        // Prep alone stops at the receiver.
        assert_eq!(offline("y", State::Kdfu, None).methods.len(), 1);
        // The 64-bit board from DFU: the worker, its chain, then m1n1.
        let planned = offline("x", State::Dfu, Some(Load::M1n1));
        assert_eq!(planned.board.as_deref(), Some("x"));
        assert_eq!(
            planned.methods[1].steps[0],
            "unwrap the firmware keys the build lacks, and build it"
        );
        assert_eq!(planned.methods[2].steps.len(), 7);
        // A plan's JSON names what each method needs and leaves; a run's
        // where it went from and to.
        let json = serde_json::to_value(&planned).unwrap();
        assert_eq!(json["methods"][0]["leaves"], "pwned-dfu");
        assert!(json.get("state").is_none());
    }

    #[test]
    fn a_named_board_must_be_the_device() {
        let (_dir, catalog, _store) = data();
        let (_, session) = cold(SECOND, None);
        let request = Request {
            board: Some("x"),
            ..Request::new(&catalog)
        };
        let e = plan(&request, Some(&session)).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert_eq!(
            e.message,
            "the device is not a x: it reports another chip or board number"
        );
    }
}
