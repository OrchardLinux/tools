//! What the device path tests share: two boards in generated data with a
//! cache of their firmware and m1n1, and one faked device at one port that
//! runs what it was last sent. All bytes are generated.
use crate::boot::{ENTRIES, HEAP_ALLOC, START, STORAGE_INIT};
use crate::entry::Executable;
use crate::host::{Connect, DfuMode, M1n1Proxy, PongoShell, RecoveryMode, Tool, ToolArg};
use crate::methods::{Kind, Method, State};
use crate::prep::{Files, MethodSteps, Request};
use crate::select::Selector;
use crate::session::Session;
use crate::steps::{Run, Step};
use orchard_boards::catalog::{Catalog, Chip};
use orchard_cache::store::{self, Key, Store};
use orchard_cache::{Code, Error, Result};
use orchard_formats::der::{write, IA5_STRING, INTEGER, OCTET_STRING, SEQUENCE};
use orchard_usb::dfu::{Pwned, Status};
use orchard_usb::pongo::Output;
use orchard_usb::{Device, Discovery, Identity, Mode, Sent};
use std::cell::{RefCell, RefMut};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;
use tempfile::TempDir;

/// The two boards' devices: a 64-bit one that reports its serial number,
/// and a 32-bit one.
pub const FIRST: &str = "CPID:8010 BDID:0C ECID:00000000000000A1 SRNM:[TESTSERIAL]";
pub const SECOND: &str = "CPID:8942 BDID:0A ECID:00000000000000B2";

pub fn chip(stages: &[&str]) -> Chip {
    Chip {
        cpid: 0,
        name: "TEST".into(),
        soc: "soc".into(),
        architecture: "arm64".into(),
        stages: stages.iter().map(|s| (*s).into()).collect(),
        image_format: "im4p".into(), page_size: 16384
    }
}

pub fn method(kind: Kind, name: &str, needs: State, leaves: State) -> Method {
    Method {
        name: name.into(),
        kind,
        needs,
        leaves,
        chips: Vec::new(),
        hosts: Vec::new(),
        marker: None,
        arch: None,
        markers: Vec::new(),
    }
}

/// Enough of Cascadia's loader to be told apart: its entry instruction
/// and its configuration record.
pub fn loader() -> Vec<u8> {
    let mut loader = vec![0u8; 0x7f44];
    loader[..4].copy_from_slice(&[0xc0, 0x00, 0x0c, 0xf1]);
    loader[0x7f00..0x7f04].copy_from_slice(b"CB32");
    loader
}

/// A catalog without boards.
pub fn catalog() -> (TempDir, Catalog) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("devices")).unwrap();
    let catalog = Catalog::open(dir.path()).unwrap();
    (dir, catalog)
}

/// Board `x`, a 64-bit one whose `iboot` recipe has an encrypted iBSS
/// without a public key, and board `y`, a 32-bit one with addresses for
/// Cascadia's loader; a cache with the firmware's files and m1n1.
pub fn data() -> (TempDir, Catalog, Store) {
    let dir = tempfile::tempdir().unwrap();
    let put = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    put(
        "boot-paths.toml",
        "schema = 1\n\
         [entry.gaster]\nneeds = \"dfu\"\nleaves = \"pwned-dfu\"\nmarker = \"gaster\"\nchips = [\"t8010\"]\nhosts = [\"macos\", \"linux\"]\n\
         [entry.palera1n]\nneeds = \"dfu\"\nleaves = \"pongo\"\nchips = [\"t8010\"]\nhosts = [\"macos\", \"linux\"]\n\
         [chain.iboot]\nneeds = [\"pwned-dfu\"]\nleaves = \"patched-recovery\"\narch = \"arm64\"\n\
         [chain.iboot-autogo]\nneeds = [\"pwned-dfu\", \"kdfu\"]\nleaves = \"autogo\"\narch = \"arm\"\n\
         [loader.m1n1]\nneeds = [\"pongo\", \"patched-recovery\"]\nleaves = \"m1n1\"\narch = \"arm64\"\n\
         [loader.cascadia]\nneeds = [\"autogo\"]\nleaves = \"linux\"\narch = \"arm\"\n",
    );
    put(
        "chips/a.toml",
        "cpid = 0x8010\nname = \"A10\"\nsoc = \"t8010\"\narchitecture = \"arm64\"\nstages = [\"iBSS\"]\nimage_format = \"im4p\"\n",
    );
    put(
        "chips/b.toml",
        "cpid = 0x8942\nname = \"A5\"\nsoc = \"s5l8942x\"\narchitecture = \"arm\"\nstages = [\"iBSS\", \"iBEC\"]\nimage_format = \"img3\"\n",
    );
    put(
        "devices/x/device.toml",
        "[device]\nname = \"X\"\n[match.dfu]\nchip = 0x8010\nboard = 0x0c\n",
    );
    put(
        "devices/y/device.toml",
        "[device]\nname = \"Y\"\n[match.dfu]\nchip = 0x8942\nboard = 0x0a\n",
    );
    put(
        "devices/y/boot.json",
        r#"{"schema":2,"device":"Test2,5","firmware":{"9A1":{"version":"6","url":"https://invalid.invalid/fw",
           "bytes":1,"sha256":"00","inputs":{"iBSS":{"ipsw":{"zip":"ibss"}},"iBEC":{"ipsw":{"zip":"ibec"}}}}},
           "chains":{"iboot-autogo":[{"firmware":"9A1","patch":{"iBSS":[{"step":"iboot32patcher"}],
           "iBEC":[{"step":"iboot32patcher"}]},"source":"generated"}]},"loaders":{"cascadia":{
           "load_address":"0x80000000","kernel_offset":"0x8000","watchdog_address":"0x0f103020",
           "l2_address":"0x3e000000","ram_start":"0x80000000","ram_end":"0xa0000000",
           "initrd_address":"0x88000000","initrd_limit":"0x90000000","source":"test"}}}"#,
    );
    let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
    let key = orchard_formats::crypto::Key {
        iv: [9; 16],
        key: vec![7; 32],
    };
    let mut encrypted = vec![b'a'; 16];
    key.encrypt(&mut encrypted).unwrap();
    let bag = write(
        SEQUENCE,
        &[
            write(INTEGER, &[1]),
            write(OCTET_STRING, &[1; 16]),
            write(OCTET_STRING, &[2; 32]),
        ]
        .concat(),
    );
    let image = |kind: &[u8], data: &[u8], encrypted| {
        let mut body = [
            write(IA5_STRING, b"IM4P"),
            write(IA5_STRING, kind),
            write(IA5_STRING, b""),
            write(OCTET_STRING, data),
        ]
        .concat();
        if encrypted {
            body.extend(write(OCTET_STRING, &write(SEQUENCE, &bag)));
        }
        write(SEQUENCE, &body)
    };
    let ibss = image(b"ibss", &encrypted, true);
    let tree = image(b"dtre", b"tree", false);
    let (hash, tree_hash) = (store::sha256(&ibss), store::sha256(&tree));
    store.put(&Key::Sha256(hash.clone()), &ibss).unwrap();
    store.put(&Key::Sha256(tree_hash.clone()), &tree).unwrap();
    let output = store::sha256(&[b'b'; 16]);
    put(
        "devices/x/boot.json",
        &format!(
            r#"{{"schema":2,"device":"Test1,1","firmware":{{"1A1":{{"version":"1","url":"https://invalid.invalid/fw","bytes":1,"sha256":"abc","inputs":{{"iBSS":{{"ipsw":{{"zip":"ibss"}},"sha256":"{hash}"}},"DeviceTree":{{"ipsw":{{"zip":"tree"}},"sha256":"{tree_hash}"}}}}}}}},"chains":{{"iboot":[{{"firmware":"1A1","patch":{{"iBSS":[{{"step":"bytes","offset":"0x0","original":"61616161616161616161616161616161","replacement":"62626262626262626262626262626262"}}]}},"outputs":{{"ibss.raw":"{output}"}},"source":"generated"}}]}}}}"#
        ),
    );
    let macho = b"macho";
    store
        .put(
            &Key::Origin("loader m1n1 test m1n1-idevice.macho".into()),
            macho,
        )
        .unwrap();
    std::fs::create_dir_all(store.dir().join("data")).unwrap();
    std::fs::write(
        store.dir().join("data/loaders.json"),
        format!(
            r#"{{"release":"test","checked":0,"loaders":{{"m1n1":{{"key":"test","files":[{{"asset":"m1n1-idevice.macho","sha256":"{}"}}]}}}}}}"#,
            store::sha256(macho)
        ),
    )
    .unwrap();
    let catalog = Catalog::open(dir.path()).unwrap();
    (dir, catalog, store)
}

/// What runs on the one device at the one port.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Runs {
    /// The boot ROM in DFU mode; pwned when the world has a marker.
    Rom,
    /// An iBSS in DFU mode.
    Ibss,
    /// An iBoot in recovery mode.
    Iboot,
    Pongo,
    M1n1,
    Linux,
}

pub struct World {
    /// What runs; nothing while the device is off the bus.
    pub runs: Option<Runs>,
    pub serial: &'static str,
    pub marker: Option<&'static str>,
    /// What starting an image on the boot ROM, and an upload to the iBSS,
    /// lead to, and the marker the boot ROM shows after a reset.
    pub after_start: Runs,
    pub after_ibss: Runs,
    pub reset_marker: Option<&'static str>,
    /// What `bootx`, `bootm`, or the upload numbered `starts_after_upload`
    /// start, and whether such a command goes unanswered.
    pub boots_into: Option<Runs>,
    pub starts_after_upload: Option<usize>,
    pub silent: bool,
    /// How many more times the device is listed as off the bus, and the
    /// Pongo shell as busy.
    pub away: u32,
    pub busy: u32,
    pub failing_upload: Option<usize>,
    pub extra_dfu: bool,
    pub wrong_port: bool,
    pub tool_error: Option<Code>,
    pub worker_error: Option<Code>,
    pub worker_resolutions: usize,
    pub key_error: bool,
    /// m1n1's boot manager: the list it answers, whether storage is up,
    /// whether a start is refused, its memory and console, and what it was
    /// called with.
    pub text: String,
    pub storage: bool,
    pub refuses: bool,
    pub memory: BTreeMap<u64, u8>,
    pub console: Vec<u8>,
    pub calls: Vec<(u64, Vec<u64>)>,
    /// The strings a start was given, and its flags.
    pub started: Option<(String, Option<String>, u64)>,
    pub opened: usize,
    pub payloads: Vec<Pwned>,
    pub uploads: Vec<Vec<u8>>,
    pub sent: Vec<String>,
}

#[derive(Clone)]
pub struct Fake(Rc<RefCell<World>>);

pub fn fake(serial: &'static str, runs: Runs, marker: Option<&'static str>) -> Fake {
    Fake(Rc::new(RefCell::new(World {
        runs: Some(runs),
        serial,
        marker,
        after_start: Runs::Iboot,
        after_ibss: Runs::Iboot,
        reset_marker: None,
        boots_into: Some(Runs::M1n1),
        starts_after_upload: None,
        silent: false,
        away: 0,
        busy: 0,
        failing_upload: None,
        extra_dfu: false,
        wrong_port: false,
        tool_error: None,
        worker_error: None,
        worker_resolutions: 0,
        key_error: false,
        text: String::new(),
        storage: false,
        refuses: false,
        memory: BTreeMap::new(),
        console: Vec::new(),
        calls: Vec::new(),
        started: None,
        opened: 0,
        payloads: Vec::new(),
        uploads: Vec::new(),
        sent: Vec::new(),
    })))
}

/// A session on the device at the fake's port.
pub fn session(fake: &Fake, limit: Option<Duration>) -> Session<Fake> {
    let dir = tempfile::tempdir().unwrap();
    let port = Selector {
        location: Some("0x100000".into()),
        ..Selector::default()
    };
    Session::start_in(dir.path(), fake.clone(), &port, limit).unwrap()
}

/// What one method did: its plan, what was heard, and its steps or error.
pub struct Did {
    pub plan: Vec<String>,
    pub progress: Vec<String>,
    pub result: Result<Vec<Step>>,
}

/// Plans, then runs one method on the session's device, as a command
/// does. A plan the device contradicts names no steps; the run says how.
pub fn one(
    session: &mut Session<Fake>,
    method: Method,
    chip: &Chip,
    files: &mut Files,
    wait: Option<Mode>,
) -> Did {
    let (_dir, catalog) = catalog();
    let request = Request {
        wait,
        ..Request::new(&catalog)
    };
    let (does, _) = crate::methods::does::<Fake>(&method).unwrap();
    let at = MethodSteps {
        method,
        marker: session.device().identity.pwned.clone(),
        steps: Vec::new(),
        done: None,
    };
    let mut quiet = |_: &str| {};
    let mut planned = Run::<Fake>::plan(&mut quiet);
    let plan = match does(
        &mut planned,
        &at,
        Some(chip),
        &request,
        &mut Files::default(),
    ) {
        Ok(()) => planned.names(),
        Err(_) => Vec::new(),
    };
    let mut progress = Vec::new();
    let mut hear = |step: &str| progress.push(step.to_owned());
    let mut r = Run::new(session, plan.clone(), &mut hear);
    let result = does(&mut r, &at, Some(chip), &request, files).map(|()| r.finish());
    Did {
        plan,
        progress,
        result,
    }
}

type UsbResult<T> = std::result::Result<T, orchard_usb::Error>;

impl Fake {
    pub fn world(&self) -> RefMut<'_, World> {
        self.0.borrow_mut()
    }

    /// The device takes `image`, leaves the bus and comes back running
    /// `next`.
    fn take(&self, what: &str, image: &[u8], next: Runs) {
        let mut world = self.world();
        let line = format!("{what} {}", String::from_utf8_lossy(image));
        world.sent.push(line);
        world.runs = Some(next);
        if next != Runs::Rom {
            world.marker = None;
        }
        world.away = 2;
    }

    fn upload(&self, action: &'static str, image: &[u8]) -> UsbResult<()> {
        let mut world = self.world();
        world.sent.push(format!("{action} {}", image.len()));
        let number = world.uploads.len();
        if world.failing_upload == Some(number) {
            return Err(orchard_usb::Error {
                action,
                kind: orchard_usb::Kind::Stall,
                detail: "the device rejected the request".into(),
            });
        }
        world.uploads.push(image.to_vec());
        if world.starts_after_upload == Some(number) {
            world.runs = world.boots_into;
        }
        Ok(())
    }

    fn answer(&self, line: String, boots: bool) -> Sent {
        let mut world = self.world();
        world.sent.push(line);
        if !boots {
            return Sent::Acknowledged;
        }
        world.runs = world.boots_into;
        match world.silent {
            true => Sent::Unconfirmed,
            false => Sent::Acknowledged,
        }
    }
}

impl World {
    pub fn put(&mut self, address: u64, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.memory.insert(address + i as u64, *byte);
        }
    }

    pub fn string_at(&self, address: u64) -> String {
        let bytes: Vec<u8> = (address..)
            .map(|a| self.memory[&a])
            .take_while(|b| *b != 0)
            .collect();
        String::from_utf8(bytes).unwrap()
    }
}

impl Discovery for Fake {
    fn list(&mut self) -> UsbResult<Vec<Device>> {
        let mut world = self.world();
        if world.away > 0 {
            world.away -= 1;
            return Ok(Vec::new());
        }
        let Some(runs) = world.runs else {
            return Ok(Vec::new());
        };
        let serial = world.serial.to_owned();
        let (mode, serial) = match runs {
            Runs::Rom => match world.marker {
                Some(marker) => (Mode::Dfu, format!("{serial} PWND:[{marker}]")),
                None => (Mode::Dfu, serial),
            },
            Runs::Ibss => (Mode::Dfu, serial),
            Runs::Iboot => (Mode::Recovery, serial),
            Runs::Pongo => (Mode::Pongo, String::new()),
            Runs::M1n1 => (Mode::M1n1, String::new()),
            Runs::Linux => (Mode::Linux, String::new()),
        };
        let location = match world.wrong_port && mode == Mode::Pongo {
            true => "0x200000",
            false => "0x100000",
        };
        let mut devices = vec![Device {
            mode,
            system: None,
            location: location.into(),
            product: None,
            identity: Identity::parse(&serial),
        }];
        if world.extra_dfu {
            devices.push(Device {
                location: "0x300000".into(),
                mode: Mode::Dfu,
                identity: Identity::parse(SECOND),
                ..devices[0].clone()
            });
        }
        Ok(devices)
    }
}

impl Connect for Fake {
    fn gaster_worker(&self, store: &Store) -> Result<Executable> {
        let mut world = self.world();
        world.worker_resolutions += 1;
        if let Some(code) = world.worker_error {
            return Err(Error::new(code, "gaster: worker unavailable"));
        }
        Ok(Executable {
            bytes: b"worker fixture".to_vec(),
            dir: store.dir().join("run"),
        })
    }

    fn gaster(
        &mut self,
        device: &Device,
        bags: Option<&[[u8; 48]]>,
        worker: &Executable,
        _: Duration,
        _: &dyn Fn(&mut std::process::Command) -> Result<()>,
    ) -> Result<Vec<[u8; 48]>> {
        assert_eq!(worker.bytes, b"worker fixture");
        assert_eq!(device.location, "0x100000");
        let mut world = self.world();
        let Some(bags) = bags else {
            world.sent.push("gaster entry".into());
            world.marker = Some("gaster");
            return Ok(Vec::new());
        };
        world.sent.push(format!("unwrap {} keybags", bags.len()));
        if world.key_error {
            return Err(Error::new(Code::Timeout, "key worker timed out"));
        }
        let mut reply = [7; 48];
        reply[..16].fill(9);
        Ok(vec![reply; bags.len()])
    }

    fn entry_tool(&mut self, tool: &Tool<'_>, _: Duration) -> Result<()> {
        assert_eq!(tool.executable, b"tool");
        assert_eq!(tool.files, [b"PONGO".as_slice()]);
        let mut line = tool.name.to_owned();
        for arg in tool.args {
            line.push(' ');
            line.push_str(match arg {
                ToolArg::Text(text) => text,
                ToolArg::File(_) => "pongo.bin",
            });
        }
        let mut world = self.world();
        world.sent.push(line);
        if let Some(code) = world.tool_error {
            return Err(Error::new(code, "fake tool stopped"));
        }
        world.runs = Some(Runs::Pongo);
        Ok(())
    }

    fn dfu(&mut self, _: &Device) -> UsbResult<Box<dyn DfuMode>> {
        self.world().opened += 1;
        Ok(Box::new(self.clone()))
    }
    fn recovery(&mut self, _: &Device) -> UsbResult<Box<dyn RecoveryMode>> {
        Ok(Box::new(self.clone()))
    }
    fn pongo(&mut self, _: &Device) -> UsbResult<Box<dyn PongoShell>> {
        Ok(Box::new(self.clone()))
    }
    fn m1n1(&mut self, _: &Device) -> UsbResult<Box<dyn M1n1Proxy>> {
        Ok(Box::new(self.clone()))
    }
}

impl DfuMode for Fake {
    fn status(&mut self, _: Duration) -> UsbResult<Status> {
        unreachable!("a chain sends and waits")
    }
    fn clear_status(&mut self, _: Duration) -> UsbResult<()> {
        self.world().sent.push("clear status".into());
        Ok(())
    }
    fn abort(&mut self, _: Duration) -> UsbResult<()> {
        unreachable!("a chain sends and waits")
    }
    fn reset(&mut self) -> UsbResult<()> {
        let mut world = self.world();
        world.sent.push("reset".into());
        if let Some(marker) = world.reset_marker {
            world.marker = Some(marker);
        }
        Ok(())
    }
    fn send(&mut self, image: &[u8], _: Duration) -> UsbResult<()> {
        let next = {
            let world = self.world();
            match world.runs {
                Some(Runs::Rom) => {
                    assert_eq!(world.marker, Some("gaster"));
                    assert!(world.sent.iter().any(|s| s == "reset"));
                    world.after_start
                }
                _ => world.after_ibss,
            }
        };
        self.take("send", image, next);
        Ok(())
    }
    fn start_pwned(&mut self, payload: Pwned, image: &[u8], _: Duration) -> UsbResult<Sent> {
        let next = self.world().after_start;
        assert_eq!(self.world().runs, Some(Runs::Rom));
        self.take("start", image, next);
        self.world().payloads.push(payload);
        Ok(Sent::Acknowledged)
    }
}

impl RecoveryMode for Fake {
    fn send(&mut self, image: &[u8], _: Duration) -> UsbResult<()> {
        self.upload("recovery send", image)
    }
    fn command(&mut self, command: &str, _: Duration) -> UsbResult<Sent> {
        let line = format!("recovery command {command}");
        Ok(self.answer(line, command == "bootx"))
    }
    fn getenv(&mut self, _: &str, _: Duration) -> UsbResult<String> {
        unreachable!("no path reads iBoot's environment")
    }
}

impl PongoShell for Fake {
    fn output(&mut self, _: Duration) -> UsbResult<Output> {
        let mut world = self.world();
        world.sent.push("pongo output".into());
        let busy = world.busy > 0;
        world.busy = world.busy.saturating_sub(1);
        Ok(Output {
            busy,
            text: String::new(),
        })
    }
    fn send(&mut self, image: &[u8], _: Duration) -> UsbResult<()> {
        self.upload("pongo send", image)
    }
    fn command(&mut self, command: &str, _: Duration) -> UsbResult<Sent> {
        Ok(self.answer(format!("pongo command {command}"), true))
    }
}

/// Where m1n1's heap starts, and where it puts the list's text.
pub const HEAP: u64 = 0x8_0800_0000;
pub const TEXT_AT: u64 = 0x8_0400_0000;

impl M1n1Proxy for Fake {
    fn ping(&mut self, _: Duration) -> UsbResult<()> {
        Ok(())
    }
    fn call(&mut self, operation: u64, args: &[u64], _: Duration) -> UsbResult<u64> {
        let mut world = self.world();
        world.calls.push((operation, args.to_vec()));
        Ok(match operation {
            HEAP_ALLOC => HEAP,
            STORAGE_INIT => {
                world.storage = true;
                1
            }
            ENTRIES => {
                assert!(world.storage, "the list needs storage");
                let text = world.text.clone().into_bytes();
                world.put(TEXT_AT, &text);
                let place = [TEXT_AT.to_le_bytes(), (text.len() as u64).to_le_bytes()];
                world.put(args[0], &place.concat());
                0
            }
            START if world.refuses => {
                world.console = b"bootmgr: cannot load the kernel\n".to_vec();
                u64::MAX
            }
            START => {
                let tree = (args[1] != 0).then(|| world.string_at(args[1]));
                world.started = Some((world.string_at(args[0]), tree, args[2]));
                world.runs = Some(Runs::Linux);
                world.storage = false;
                0
            }
            _ => 0,
        })
    }
    fn read(&mut self, address: u64, size: usize, _: Duration) -> UsbResult<Vec<u8>> {
        let world = self.world();
        Ok((address..address + size as u64)
            .map(|a| world.memory[&a])
            .collect())
    }
    fn write(&mut self, address: u64, data: &[u8], _: Duration) -> UsbResult<()> {
        self.world().put(address, data);
        Ok(())
    }
    fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.world().console)
    }
}
