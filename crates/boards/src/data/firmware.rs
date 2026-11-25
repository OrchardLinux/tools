//! Files from a firmware archive (IPSW): its own members, or files inside
//! the filesystem of one of its disk images.
use super::apfs::Container;
use super::archive::Archive;
use super::disk_image::DiskImage;
use crate::catalog::{Board, Catalog, Input, Ipsw, Kind, Location, Recipe};
use orchard_cache::http::Http;
use orchard_cache::source::{FileSource, Slice, Source};
use orchard_cache::store::{Key, Store};
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::apfs::DirEntry;
use orchard_formats::build_manifest::{Identity, Manifest};
use orchard_formats::kernelcache;
use orchard_formats::udif;
use sha2::{Digest, Sha256};
use std::collections::hash_map::{Entry, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to wait for the server before giving up on a request.
const TIMEOUT: Duration = Duration::from_secs(30);
const MANIFEST: &str = "BuildManifest.plist";
const MAX_MANIFEST: u64 = 16 << 20;

/// Where a file lives inside a firmware's disk images.
#[derive(Debug, Clone, Copy)]
pub struct ImagePath<'a> {
    /// The board, such as `n71ap`. May be left out when the firmware covers
    /// only one board.
    pub board: Option<&'a str>,
    /// The manifest component holding the disk image, such as `OS`.
    pub component: &'a str,
    /// The volume name. May be left out when the image has one volume.
    pub volume: Option<&'a str>,
    pub path: &'a str,
}

/// What a user named as a firmware: a URL or file, or a board and its recipe.
pub enum Target {
    Location(String),
    Board { board: Box<Board>, recipe: Recipe },
}

impl Target {
    /// Reads an `https://` URL, a local file, or a board name from the orchard
    /// data (with that board's `kind` recipe). `data` gives the data directory
    /// and is only called for a board name. Opens nothing.
    pub fn resolve(
        spec: &str,
        data: impl FnOnce() -> Result<PathBuf>,
        kind: Kind,
    ) -> Result<Target> {
        if spec.contains(['/', ':', '.']) || Path::new(spec).exists() {
            return Ok(Target::Location(spec.to_owned()));
        }
        let board = Catalog::open(&data()?)?.board(spec)?;
        let recipe = board.recipe(kind)?;
        Ok(Target::Board {
            board: Box::new(board),
            recipe,
        })
    }

    pub fn open(&self) -> Result<Firmware> {
        match self {
            Target::Location(location) => Firmware::open(location),
            Target::Board { recipe, .. } => Firmware::open_recipe(recipe),
        }
    }

    pub fn board(&self) -> Option<&Board> {
        match self {
            Target::Board { board, .. } => Some(board),
            Target::Location(_) => None,
        }
    }
}

/// A board's recipe inputs, taken from the cache where possible. A firmware
/// (the recipe's, or another restore image an input names) is opened only
/// when an input from it is missing, and then only once.
pub struct Inputs<'a> {
    store: &'a Store,
    board: &'a Board,
    recipe: &'a Recipe,
    /// Opened firmwares by URL.
    firmware: HashMap<String, Firmware>,
}

/// An input's bytes, and whether they came from the cache.
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub cached: bool,
}

impl<'a> Inputs<'a> {
    pub fn new(store: &'a Store, board: &'a Board, recipe: &'a Recipe) -> Inputs<'a> {
        Inputs {
            store,
            board,
            recipe,
            firmware: HashMap::new(),
        }
    }

    pub fn get(&mut self, name: &str, limit: u64) -> Result<Fetched> {
        let input = self.recipe.inputs.get(name).ok_or_else(|| {
            Error::new(Code::NotFound, "not in the recipe").context(format!("input {name}"))
        })?;
        let ipsw = match &input.location {
            Location::Kernelcache {
                restore: Some(restore),
                ..
            } => restore,
            _ => &self.recipe.ipsw,
        };
        let key = match &input.sha256 {
            Some(hex) => Key::Sha256(hex.clone()),
            // Without a recorded hash, the file is known by the IPSW it comes
            // from and its place in it.
            None => Key::Origin(format!("ipsw {}, {}", ipsw.sha256, input.location)),
        };
        if let Some(bytes) = self.store.get(&key)? {
            if bytes.len() as u64 > limit {
                return Err(
                    Error::new(Code::TooLarge, "cached input exceeds size limit").context(name),
                );
            }
            return Ok(Fetched {
                bytes,
                cached: true,
            });
        }
        let firmware = match self.firmware.entry(ipsw.url.clone()) {
            Entry::Occupied(f) => f.into_mut(),
            Entry::Vacant(slot) => slot.insert(Firmware::open_ipsw(ipsw)?),
        };
        let bytes = firmware.input(name, input, self.board, limit)?;
        self.store.put(&key, &bytes)?;
        Ok(Fetched {
            bytes,
            cached: false,
        })
    }
}

type Filesystem<'a> = Container<Slice<DiskImage<Slice<&'a mut Box<dyn Source>>>>>;

pub struct Firmware {
    archive: Archive<Box<dyn Source>>,
    manifest: Manifest,
}

impl Firmware {
    /// Opens a firmware from an `https://` URL or a local file.
    pub fn open(location: &str) -> Result<Firmware> {
        let source: Box<dyn Source> = if location.starts_with("https://") {
            Box::new(Http::probe(location, TIMEOUT)?)
        } else if location.contains("://") {
            return Err(Error::new(
                Code::InvalidInput,
                "firmware must be an https:// URL or a local file",
            ));
        } else {
            Box::new(FileSource::open(Path::new(location))?)
        };
        Firmware::from_source(source)
    }

    /// Opens the firmware a recipe names, at the size the recipe records.
    pub fn open_recipe(recipe: &Recipe) -> Result<Firmware> {
        Firmware::open_ipsw(&recipe.ipsw)
    }

    /// Opens a pinned firmware at the size it records. Only the parts read
    /// are fetched.
    pub fn open_ipsw(ipsw: &Ipsw) -> Result<Firmware> {
        Firmware::from_source(Box::new(Http::new(&ipsw.url, ipsw.size, TIMEOUT)?))
            .context(&ipsw.url)
    }

    fn from_source(source: Box<dyn Source>) -> Result<Firmware> {
        let mut archive = Archive::open(source)?;
        let manifest = Manifest::parse(&archive.read(MANIFEST, MAX_MANIFEST)?).context(MANIFEST)?;
        Ok(Firmware { archive, manifest })
    }

    /// Reads one recipe input for a board and checks it against the
    /// recipe's SHA-256, where there is one.
    pub fn input(
        &mut self,
        name: &str,
        input: &Input,
        board: &Board,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let bytes = match &input.location {
            Location::Member(member) => self.member(member, limit)?,
            Location::System(path) => {
                let class = self
                    .manifest
                    .identity_for(board.chip, board.board)?
                    .device_class
                    .clone();
                let at = ImagePath {
                    board: Some(&class),
                    component: "OS",
                    volume: None,
                    path: &format!("/{path}"),
                };
                self.read(&at, limit)?
            }
            Location::Kernelcache { board: named, .. } => {
                // Another board's kernel by its device class; it need not be
                // the installing board, nor in the recipe's own image.
                let identity = match named {
                    Some(class) => self.manifest.identity(class)?,
                    None => self.manifest.identity_for(board.chip, board.board)?,
                };
                let member = identity
                    .components
                    .get("KernelCache")
                    .cloned()
                    .ok_or_else(|| {
                        Error::new(Code::NotFound, "no component KernelCache")
                            .context(&identity.device_class)
                    })?;
                let im4p = self.member(&member, limit)?;
                kernelcache::decode(&im4p).map_err(|e| Error::from(e).context(&member))?
            }
        };
        if let Some(expected) = &input.sha256 {
            let actual: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(Error::new(
                    Code::Format,
                    format!("SHA-256 is {actual}, the recipe expects {expected}"),
                )
                .context(format!("input {name}")));
            }
        }
        Ok(bytes)
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Names and sizes of the archive's members.
    pub fn members(&self) -> impl Iterator<Item = (&str, u64)> {
        self.archive.entries().map(|e| (e.name.as_str(), e.size))
    }

    /// A member of the archive, refusing members larger than `limit`.
    pub fn member(&mut self, name: &str, limit: u64) -> Result<Vec<u8>> {
        self.archive.read(name, limit)
    }

    /// A file inside a disk image, refusing files larger than `limit`.
    pub fn read(&mut self, at: &ImagePath, limit: u64) -> Result<Vec<u8>> {
        let mut fs = self.filesystem(at)?;
        let mut volume = match at.volume {
            Some(name) => fs.volume_named(name)?,
            None => only_volume(&mut fs)?,
        };
        volume.read(at.path, limit)
    }

    /// A directory inside a disk image.
    pub fn list(&mut self, at: &ImagePath) -> Result<Vec<DirEntry>> {
        let mut fs = self.filesystem(at)?;
        let mut volume = match at.volume {
            Some(name) => fs.volume_named(name)?,
            None => only_volume(&mut fs)?,
        };
        volume.list(at.path)
    }

    /// The build identity for a board, or the only one the firmware has.
    pub fn identity(&self, board: Option<&str>) -> Result<&Identity> {
        if let Some(board) = board {
            return Ok(self.manifest.identity(board)?);
        }
        match self.manifest.device_classes().as_slice() {
            [only] => Ok(self.manifest.identity(only)?),
            boards => Err(
                Error::new(Code::InvalidInput, "firmware covers several boards")
                    .hint(format!("choose one with --board: {}", boards.join(", "))),
            ),
        }
    }

    fn filesystem(&mut self, at: &ImagePath) -> Result<Filesystem<'_>> {
        let identity = self.identity(at.board)?;
        let name = identity
            .components
            .get(at.component)
            .cloned()
            .ok_or_else(|| {
                Error::new(
                    Code::NotFound,
                    format!(
                        "board {} has no component {}",
                        identity.device_class, at.component
                    ),
                )
            })?;
        let image = DiskImage::open(self.archive.stored(&name)?).context(&name)?;
        let apfs: Vec<_> = image
            .partitions()
            .iter()
            .filter(|p| p.name.contains("Apple_APFS"))
            .cloned()
            .collect();
        let [partition] = apfs.as_slice() else {
            return Err(Error::new(
                Code::Format,
                format!("{name} does not have exactly one APFS partition"),
            ));
        };
        let start = partition.first_sector * udif::SECTOR;
        Container::open(Slice::new(image, start, partition.sectors * udif::SECTOR)?).context(&name)
    }
}

fn only_volume<'a, S: Source>(fs: &'a mut Container<S>) -> Result<super::apfs::Volume<'a, S>> {
    match fs.volume_count() {
        1 => fs.volume(0),
        n => Err(
            Error::new(Code::InvalidInput, format!("disk image has {n} volumes"))
                .hint("choose one with --volume"),
        ),
    }
}
