use crate::args::{Fw, Global, ImageArgs, SourceArgs};
use crate::output::{Output, Report};
use orchard_boards::catalog::Board;
use orchard_boards::data::apfs::EntryKind;
use orchard_boards::data::firmware::{Firmware, ImagePath, Inputs, Target};
use orchard_boards::firmware_build::{Boot, Built, Request};
use orchard_cache::store::{self, Store};
use orchard_cache::{Code, Error, Result};
use serde::Serialize;
use std::fmt::Write;

/// Files larger than this are not read into memory.
const MAX_FILE: u64 = 512 << 20;

pub fn run(global: &Global, command: &Fw) -> Result<Output> {
    match command {
        Fw::Info { source } => {
            let target = resolve(global, source)?;
            Ok(info(&target, &target.open()?).into())
        }
        Fw::Ls {
            source,
            path,
            image,
        } => {
            let target = resolve(global, source)?;
            ls(&mut target.open()?, target.board(), path, image)
        }
        Fw::Cat {
            source,
            path,
            image,
        } => Ok(cat(global, &resolve(global, source)?, path, image)?.into()),
        Fw::Build {
            board,
            kind,
            chain,
            keys,
            im4m,
            out,
            skip_unsupported,
            device_keys,
        } => {
            let request = match (*kind).into() {
                orchard_boards::catalog::Kind::Boot => {
                    if *skip_unsupported {
                        return Err(Error::new(
                            Code::InvalidInput,
                            "--skip-unsupported is only for --type peripheral",
                        ));
                    }
                    Request::Boot(Boot {
                        chain: chain.as_deref(),
                        keys: keys.as_deref(),
                        im4m: im4m.as_deref(),
                    })
                }
                orchard_boards::catalog::Kind::Peripheral => {
                    if chain.is_some() || keys.is_some() || im4m.is_some() {
                        return Err(Error::new(
                            Code::InvalidInput,
                            "--chain, --keys and --im4m are only for --type boot",
                        ));
                    }
                    Request::Peripheral {
                        skip_unsupported: *skip_unsupported,
                    }
                }
            };
            let selector = global.device.selector(None);
            Ok(orchard_device::keys::build(
                &global.data_dir()?,
                global.cache.as_deref(),
                board,
                out,
                &request,
                device_keys.then_some(&selector),
            )?
            .into())
        }
    }
}

fn resolve(global: &Global, source: &SourceArgs) -> Result<Target> {
    Target::resolve(&source.firmware, || global.data_dir(), source.kind.into())
}

#[derive(Serialize)]
struct Info {
    version: String,
    build: String,
    boards: Vec<String>,
    /// The recipe, when the firmware was opened by board name.
    recipe: Option<RecipeInfo>,
}

#[derive(Serialize)]
struct RecipeInfo {
    board: String,
    name: String,
    inputs: Vec<String>,
}

impl Report for Info {
    fn human(&self) -> Vec<u8> {
        let mut text = format!(
            "version {} ({})\nboards  {}\n",
            self.version,
            self.build,
            self.boards.join(" ")
        );
        if let Some(r) = &self.recipe {
            let _ = writeln!(text, "\n{} ({}) takes:", r.board, r.name);
            for input in &r.inputs {
                let _ = writeln!(text, "  {input}");
            }
        }
        text.into_bytes()
    }
}

fn info(target: &Target, firmware: &Firmware) -> Info {
    let m = firmware.manifest();
    Info {
        version: m.version.clone(),
        build: m.build.clone(),
        boards: m.device_classes().into_iter().map(str::to_owned).collect(),
        recipe: match target {
            Target::Board { board, recipe } => Some(RecipeInfo {
                board: board.name.clone(),
                name: board.display_name.clone(),
                inputs: recipe.inputs.keys().cloned().collect(),
            }),
            Target::Location(_) => None,
        },
    }
}

#[derive(Serialize)]
struct Members {
    members: Vec<Member>,
}

#[derive(Serialize)]
struct Member {
    name: String,
    size: u64,
}

impl Report for Members {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for m in &self.members {
            let _ = writeln!(text, "{:>12}  {}", m.size, m.name);
        }
        text.into_bytes()
    }
}

#[derive(Serialize)]
struct Entries {
    entries: Vec<Entry>,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    kind: Kind,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Directory,
    File,
    Symlink,
    Other,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Directory => "directory",
            Kind::File => "file",
            Kind::Symlink => "symlink",
            Kind::Other => "other",
        }
    }
}

impl From<EntryKind> for Kind {
    fn from(k: EntryKind) -> Kind {
        match k {
            EntryKind::Directory => Kind::Directory,
            EntryKind::File => Kind::File,
            EntryKind::Symlink => Kind::Symlink,
            EntryKind::Other => Kind::Other,
        }
    }
}

impl Report for Entries {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for e in &self.entries {
            let _ = writeln!(text, "{:<9}  {}", e.kind.as_str(), e.name);
        }
        text.into_bytes()
    }
}

fn ls(fw: &mut Firmware, board: Option<&Board>, path: &str, image: &ImageArgs) -> Result<Output> {
    let Some((class, at)) = at(fw, board, image, path) else {
        let members = fw
            .members()
            .map(|(name, size)| Member {
                name: name.to_owned(),
                size,
            })
            .collect();
        return Ok(Members { members }.into());
    };
    let entries = fw.list(&ImagePath {
        board: class.as_deref(),
        ..at
    })?;
    let entries = entries
        .into_iter()
        .map(|e| Entry {
            name: e.name,
            kind: e.kind.into(),
        })
        .collect();
    Ok(Entries { entries }.into())
}

/// A file's bytes. People get the bytes; `--json` gets their description.
#[derive(Serialize)]
struct File {
    path: String,
    size: u64,
    sha256: String,
    /// Whether a recipe input came from the cache.
    cached: bool,
    #[serde(skip)]
    bytes: Vec<u8>,
}

impl Report for File {
    fn human(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

fn cat(global: &Global, target: &Target, path: &str, image: &ImageArgs) -> Result<File> {
    let (bytes, cached) = match target {
        // A recipe input: cached, and checked against the recipe's hash.
        Target::Board { board, recipe }
            if image.image.is_none() && recipe.inputs.contains_key(path) =>
        {
            let store = Store::open(global.cache.as_deref())?;
            let fetched = Inputs::new(&store, board, recipe).get(path, MAX_FILE)?;
            (fetched.bytes, fetched.cached)
        }
        _ => {
            let mut fw = target.open()?;
            let bytes = match at(&fw, target.board(), image, path) {
                None => fw.member(path, MAX_FILE)?,
                Some((class, at)) => fw.read(
                    &ImagePath {
                        board: class.as_deref(),
                        ..at
                    },
                    MAX_FILE,
                )?,
            };
            (bytes, false)
        }
    };
    Ok(File {
        path: path.to_owned(),
        size: bytes.len() as u64,
        sha256: store::sha256(&bytes),
        cached,
        bytes,
    })
}

/// Where in a disk image `path` is, when `--image` is given. Without
/// `--board`, a firmware opened by board name uses that board.
fn at<'a>(
    fw: &Firmware,
    board: Option<&Board>,
    image: &'a ImageArgs,
    path: &'a str,
) -> Option<(Option<String>, ImagePath<'a>)> {
    let component = image.image.as_deref()?;
    let class = image.board.clone().or_else(|| {
        let b = board?;
        Some(
            fw.manifest()
                .identity_for(b.chip, b.board)
                .ok()?
                .device_class
                .clone(),
        )
    });
    Some((
        class,
        ImagePath {
            board: None,
            component,
            volume: image.volume.as_deref(),
            path,
        },
    ))
}

impl Report for Built {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for w in &self.written {
            let _ = writeln!(text, "{:>10}  {}", w.size, w.destination);
        }
        let kind = match self.kind {
            orchard_boards::catalog::Kind::Boot => "boot",
            orchard_boards::catalog::Kind::Peripheral => "peripheral",
        };
        let _ = writeln!(
            text,
            "{} {kind} firmware files for {} in {}",
            self.written.len(),
            self.board,
            self.out
        );
        if let Some(chain) = &self.chain {
            let _ = writeln!(text, "chain: {chain}");
        }
        for u in &self.unsupported {
            let _ = writeln!(text, "not made: {} (needs {})", u.destination, u.step);
        }
        for p in &self.pending {
            let _ = writeln!(text, "pending:  {p}");
        }
        text.into_bytes()
    }
}
