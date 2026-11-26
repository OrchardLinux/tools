//! Board data from an orchard data directory: `devices/<board>/device.toml`,
//! with the boot recipe in `boot.json` and the peripheral firmware recipe in
//! the file `device.toml` names. The files are read as orchard writes them.
use orchard_cache::{Code, Context, Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Catalog {
    root: PathBuf,
    devices: PathBuf,
}

/// One board, as `device.toml` describes it.
#[derive(Debug, Clone)]
pub struct Board {
    /// The directory name, such as `j71b`.
    pub name: String,
    pub display_name: String,
    /// Apple product types, such as `iPad7,5`.
    pub products: Vec<String>,
    /// Chip and board numbers as the device reports them in DFU mode.
    pub chip: u32,
    pub board: u32,
    /// Support status, such as `supported` or `experimental`.
    pub status: String,
    /// Kernel platform and DTB name.
    pub platform: Option<String>,
    pub dtb: Option<String>,
    /// Feature states (`working`, `untested`, `missing`, `none`), in the
    /// order of the board file.
    pub features: Vec<(String, String)>,
    dir: PathBuf,
    peripheral_recipe: Option<String>,
}

/// A chip, from `chips/*.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Chip {
    pub cpid: u32,
    /// The marketing name, such as `A10`.
    pub name: String,
    pub soc: String,
    /// `arm64` or `arm`; chains and loaders are per architecture.
    #[serde(default)]
    pub architecture: String,
    /// The boot stages the chip loads, such as `["iBSS", "iBEC"]`.
    #[serde(default)]
    pub stages: Vec<String>,
    /// The chip's boot image format, `im4p` (64-bit IMG4) or `img3` (32-bit).
    #[serde(default)]
    pub image_format: String,
    /// The page size its kernel runs with, in bytes.
    #[serde(default)]
    pub page_size: u32,
}

/// A feature from `features.toml`: its key and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
    pub key: String,
    pub label: String,
}

/// Which recipe of a board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Files the boot chain needs, from `boot.json`.
    Boot,
    /// Firmware for the board's peripherals, from the board's firmware recipe.
    Peripheral,
}

/// A firmware archive, the files a recipe takes from it, and for peripheral
/// recipes, the files it makes from them.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub ipsw: Ipsw,
    pub inputs: BTreeMap<String, Input>,
    pub outputs: Vec<Output>,
}

/// A file a peripheral recipe makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Path below the firmware root, such as `lib/firmware/brcm/…`.
    pub destination: String,
    pub check: Check,
    /// Each step works on the previous one's result; the first reads an input.
    pub steps: Vec<Step>,
}

/// How an output is known to be right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Sha256(String),
    /// Not yet checked against real inputs, for the stated reason. Such
    /// outputs are not installed.
    Pending(String),
}

/// One step as the recipe writes it. Its name and parameters are checked by
/// the code that runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    pub input: Option<String>,
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Ipsw {
    pub version: String,
    pub build: String,
    pub url: String,
    #[serde(rename = "bytes")]
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub location: Location,
    /// SHA-256 of the file, where the recipe records one.
    pub sha256: Option<String>,
}

/// Where a recipe input lives in the firmware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A member of the archive.
    Member(String),
    /// A file in the system volume, relative to its root.
    System(String),
    /// The decoded kernel (the Mach-O inside the IM4P), which is what a
    /// recipe's hash for it covers. `board` names another board's kernel by
    /// its device class, such as `n102ap`; `restore` another restore image,
    /// pinned like the recipe's own, to take it from.
    Kernelcache {
        board: Option<String>,
        restore: Option<Ipsw>,
    },
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::Member(m) => write!(f, "member {m}"),
            Location::System(p) => write!(f, "system file {p}"),
            Location::Kernelcache { board, restore } => {
                write!(f, "kernelcache")?;
                if let Some(board) = board {
                    write!(f, " of {board}")?;
                }
                match restore {
                    Some(ipsw) => write!(f, " in {} {}", ipsw.version, ipsw.build),
                    None => Ok(()),
                }
            }
        }
    }
}

/// A board's boot data, from `boot.json` (schema 2): the firmware builds it
/// names, per chain the recipes that build that chain's images, and what a
/// loader needs to know about the board.
#[derive(Debug, Clone)]
pub struct BootData {
    /// Firmware archives by build, such as `12H321`.
    pub firmware: BTreeMap<String, Firmware>,
    /// Chain recipes by chain name, such as `iboot`; each in order of preference.
    pub chains: BTreeMap<String, Vec<ChainRecipe>>,
    /// By loader name, as the file writes it. The code that runs a loader
    /// checks its keys.
    pub loaders: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
}

/// One firmware archive and the files a chain takes from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub ipsw: Ipsw,
    pub inputs: BTreeMap<String, Input>,
    /// Sourced public defaults, keyed by exact archive member.
    pub keys: BTreeMap<String, PublicKey>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct PublicKey {
    pub kind: String,
    pub key: String,
    pub iv: String,
    pub source: String,
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicKey")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl PublicKey {
    pub fn decode(&self, kind: &str) -> Result<orchard_formats::crypto::Key> {
        use orchard_formats::crypto::{hex, Key};
        let key = hex(&self.key)?;
        let iv = hex(&self.iv)?;
        if self.kind != kind
            || self.source.trim().is_empty()
            || !matches!(key.len(), 16 | 24 | 32)
            || iv.len() != 16
        {
            return Err(Error::new(
                Code::Format,
                "invalid public firmware key (values suppressed)",
            ));
        }
        Ok(Key {
            key,
            iv: iv.try_into().unwrap(),
        })
    }
}

/// One way to build a chain's images for a board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRecipe {
    /// The firmware build this recipe uses, a key of [`BootData::firmware`].
    pub firmware: String,
    /// The patch steps for each stage (`iBSS`, `iBEC`), in order.
    pub patch: BTreeMap<String, Vec<BootStep>>,
    /// The SHA-256 of a built image by file name, where one has been measured.
    pub outputs: BTreeMap<String, String>,
    pub source: String,
}

/// One patch step of a boot image. Its name and parameters are checked by the
/// code that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootStep {
    pub name: String,
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl BootData {
    /// The board's only chain when it has one, else an error naming the choice.
    pub fn sole_chain(&self) -> Result<(&str, &ChainRecipe)> {
        let mut chains = self.chains.iter();
        let (name, recipes) = chains
            .next()
            .ok_or_else(|| Error::new(Code::NotFound, "the board offers no boot chain"))?;
        if chains.next().is_some() {
            let mut names: Vec<_> = self.chains.keys().map(String::as_str).collect();
            names.sort_unstable();
            return Err(
                Error::new(Code::Ambiguous, "the board offers several boot chains")
                    .hint(format!("choose one with --chain: {}", names.join(", "))),
            );
        }
        self.first(name, recipes)
    }

    /// The chain's preferred recipe, by name.
    pub fn chain(&self, name: &str) -> Result<(&str, &ChainRecipe)> {
        let (key, recipes) = self.chains.get_key_value(name).ok_or_else(|| {
            let mut names: Vec<_> = self.chains.keys().map(String::as_str).collect();
            names.sort_unstable();
            Error::new(Code::NotFound, format!("the board has no {name} chain"))
                .hint(format!("it offers: {}", names.join(", ")))
        })?;
        self.first(key, recipes)
    }

    fn first<'a>(
        &self,
        name: &'a str,
        recipes: &'a [ChainRecipe],
    ) -> Result<(&'a str, &'a ChainRecipe)> {
        recipes
            .first()
            .map(|r| (name, r))
            .ok_or_else(|| Error::new(Code::Format, format!("chain {name} lists no recipe")))
    }

    /// The firmware a chain recipe names, as a [`Recipe`] of archive and inputs.
    pub fn recipe(&self, chain: &ChainRecipe) -> Result<Recipe> {
        let firmware = self.firmware.get(&chain.firmware).ok_or_else(|| {
            Error::new(
                Code::Format,
                format!(
                    "a chain names firmware build {}, which is not listed",
                    chain.firmware
                ),
            )
        })?;
        Ok(Recipe {
            ipsw: firmware.ipsw.clone(),
            inputs: firmware.inputs.clone(),
            outputs: Vec::new(),
        })
    }
}

impl Catalog {
    /// Opens an orchard data directory, the one that contains `devices/`.
    pub fn open(dir: &Path) -> Result<Catalog> {
        let devices = dir.join("devices");
        if !devices.is_dir() {
            return Err(Error::new(Code::NotFound, "no devices/ directory")
                .context(dir.display())
                .hint("point --data at an orchard checkout, or leave it out to use orchard's main branch"));
        }
        Ok(Catalog {
            root: dir.to_owned(),
            devices,
        })
    }

    /// Board names, sorted.
    pub fn boards(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.devices).context(self.devices.display())? {
            let entry = entry?;
            if entry.path().join("device.toml").is_file() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn board(&self, name: &str) -> Result<Board> {
        let dir = self.devices.join(name);
        if name.contains(['/', '\\']) || !dir.join("device.toml").is_file() {
            return Err(Error::new(Code::NotFound, "no such board in the data").context(name));
        }
        load(name, &dir).context(dir.join("device.toml").display())
    }

    /// The chip with this CPID.
    pub fn chip(&self, cpid: u32) -> Result<Chip> {
        let dir = self.root.join("chips");
        for entry in std::fs::read_dir(&dir).context(dir.display())? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "toml") {
                let text = std::fs::read_to_string(&path).context(path.display())?;
                let chip: Chip = toml::from_str(&text)
                    .map_err(|e| Error::new(Code::Format, e.message().to_owned()))
                    .context(path.display())?;
                if chip.cpid == cpid {
                    return Ok(chip);
                }
            }
        }
        Err(Error::new(
            Code::NotFound,
            format!("no chip {cpid:#x} in the data"),
        ))
    }

    /// The directory the catalog was opened from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The feature list, in display order.
    pub fn features(&self) -> Result<Vec<Feature>> {
        let path = self.root.join("features.toml");
        let text = std::fs::read_to_string(&path).context(path.display())?;
        let table: toml::Table = toml::from_str(&text)
            .map_err(|e| Error::new(Code::Format, e.message().to_owned()))
            .context(path.display())?;
        Ok(table
            .into_iter()
            .filter_map(|(key, label)| {
                Some(Feature {
                    key,
                    label: label.as_str()?.to_owned(),
                })
            })
            .collect())
    }

    /// The board of a device running iOS or iPadOS, by the model it reports
    /// (`ProductType`, e.g. iPad7,5).
    pub fn board_for_product(&self, product: &str) -> Result<Board> {
        for name in self.boards()? {
            let b = self.board(&name)?;
            if b.products.iter().any(|p| p == product) {
                return Ok(b);
            }
        }
        Err(Error::new(Code::NotFound, format!("no board for {product} in the data")))
    }

    /// The board a device in DFU mode belongs to, by its chip and board numbers.
    pub fn board_for(&self, chip: u32, board: u32) -> Result<Board> {
        for name in self.boards()? {
            let b = self.board(&name)?;
            if (b.chip, b.board) == (chip, board) {
                return Ok(b);
            }
        }
        Err(Error::new(
            Code::NotFound,
            format!("no board with chip {chip:#x} and board {board:#x} in the data"),
        ))
    }
}

impl Board {
    /// The board's boot data, read from `boot.json`.
    pub fn boot_data(&self) -> Result<BootData> {
        let path = self.dir.join("boot.json");
        let text = std::fs::read_to_string(&path).context(path.display())?;
        boot_data(&text).context(path.display())
    }

    /// The board's addresses for Cascadia's loader, from `loaders.cascadia`
    /// in its `boot.json`: eight hexadecimal addresses and where they come
    /// from.
    pub fn cascadia(&self) -> Result<orchard_formats::cascadia::Profile> {
        let mut data = self
            .boot_data()?
            .loaders
            .remove("cascadia")
            .ok_or_else(|| {
                Error::new(
                    Code::NotFound,
                    "the board's boot data has nothing for Cascadia's loader",
                )
                .context(&self.name)
            })?;
        let what = format!("{}: loaders.cascadia in boot.json", self.name);
        data.remove("source");
        let mut address = |key: &str| -> Result<u32> {
            let value = data.remove(key);
            let text = value.as_ref().and_then(|v| v.as_str());
            text.and_then(|s| u32::from_str_radix(s.strip_prefix("0x")?, 16).ok())
                .ok_or_else(|| {
                    Error::new(Code::Format, format!("{key} is not a hexadecimal address"))
                })
        };
        let profile = orchard_formats::cascadia::Profile {
            load_address: address("load_address").context(&what)?,
            kernel_offset: address("kernel_offset").context(&what)?,
            watchdog_address: address("watchdog_address").context(&what)?,
            l2_address: address("l2_address").context(&what)?,
            ram_start: address("ram_start").context(&what)?,
            ram_end: address("ram_end").context(&what)?,
            initrd_address: address("initrd_address").context(&what)?,
            initrd_limit: address("initrd_limit").context(&what)?,
        };
        if let Some(unknown) = data.keys().next() {
            return Err(Error::new(Code::Format, format!("unknown key {unknown}")).context(&what));
        }
        profile.check().context(&what)?;
        Ok(profile)
    }

    pub fn recipe(&self, kind: Kind) -> Result<Recipe> {
        match kind {
            Kind::Boot => {
                let data = self.boot_data()?;
                // Data that names the firmware and no chains: that firmware.
                if let (true, Some(firmware)) = (data.chains.is_empty(), data.firmware.values().next()) {
                    return Ok(Recipe { ipsw: firmware.ipsw.clone(), inputs: firmware.inputs.clone(), outputs: Vec::new() });
                }
                let (_, chain) = data.sole_chain().or_else(|_| {
                    // For a plain look at the firmware, any chain's recipe does.
                    let (name, recipes) = data.chains.iter().next().ok_or_else(|| {
                        Error::new(Code::NotFound, "the board offers no boot chain")
                    })?;
                    data.first(name, recipes)
                })?;
                data.recipe(chain)
            }
            Kind::Peripheral => {
                let file = self.peripheral_recipe.clone().ok_or_else(|| {
                    Error::new(Code::NotFound, "no peripheral firmware recipe").context(&self.name)
                })?;
                let path = self.dir.join(&file);
                let text = std::fs::read_to_string(&path).context(path.display())?;
                peripheral_recipe(&text).context(path.display())
            }
        }
    }
}

#[derive(Deserialize)]
struct DeviceFile {
    device: DeviceSection,
    #[serde(rename = "match")]
    matching: MatchSection,
    #[serde(default)]
    firmware: FirmwareSection,
    #[serde(default)]
    kernel: KernelSection,
    #[serde(default)]
    support: SupportSection,
    #[serde(default)]
    features: toml::Table,
}

#[derive(Deserialize, Default)]
struct KernelSection {
    platform: Option<String>,
    dtb: Option<String>,
}

#[derive(Deserialize, Default)]
struct SupportSection {
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct DeviceSection {
    name: String,
    #[serde(default)]
    apple_products: Vec<String>,
}

#[derive(Deserialize)]
struct MatchSection {
    dfu: DfuMatch,
}

#[derive(Deserialize)]
struct DfuMatch {
    chip: u32,
    board: u32,
}

#[derive(Deserialize, Default)]
struct FirmwareSection {
    recipe: Option<String>,
}

fn load(name: &str, dir: &Path) -> Result<Board> {
    let text = std::fs::read_to_string(dir.join("device.toml"))?;
    let file: DeviceFile =
        toml::from_str(&text).map_err(|e| Error::new(Code::Format, e.message().to_owned()))?;
    Ok(Board {
        name: name.to_owned(),
        display_name: file.device.name,
        products: file.device.apple_products,
        chip: file.matching.dfu.chip,
        board: file.matching.dfu.board,
        status: file.support.status,
        platform: file.kernel.platform,
        dtb: file.kernel.dtb,
        features: file
            .features
            .into_iter()
            .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
            .collect(),
        dir: dir.to_owned(),
        peripheral_recipe: file.firmware.recipe,
    })
}

/// Fails unless a file's `schema` is the one this version reads, saying which
/// way to resolve the mismatch.
pub fn check_schema(found: u32, want: u32) -> Result<()> {
    match found.cmp(&want) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(Error::new(
            Code::Unsupported,
            format!("board data is schema {found}, older than this version reads (schema {want})"),
        )
        .hint("run `orchard2 data update` for newer board data")),
        std::cmp::Ordering::Greater => Err(Error::new(
            Code::Unsupported,
            format!("board data is schema {found}, newer than this version reads (schema {want})"),
        )
        .hint("update orchard2 to read this board data")),
    }
}

#[derive(Deserialize)]
struct BootFile {
    schema: u32,
    firmware: BTreeMap<String, FirmwareFile>,
    #[serde(default)]
    chains: BTreeMap<String, Vec<ChainRecipeFile>>,
    #[serde(default)]
    loaders: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct FirmwareFile {
    version: String,
    url: String,
    #[serde(rename = "bytes")]
    size: u64,
    sha256: String,
    inputs: BTreeMap<String, InputFile>,
    #[serde(default)]
    keys: BTreeMap<String, PublicKey>,
}

#[derive(Deserialize)]
struct ChainRecipeFile {
    firmware: String,
    #[serde(default)]
    patch: BTreeMap<String, Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
    source: String,
}

/// `boot.json` as orchard's main branch writes it (schema 1): the firmware
/// builds the board's boot stages come from, and nothing about chains.
#[derive(Deserialize)]
struct PlainBootFile {
    firmware: Vec<PlainFirmware>,
}

#[derive(Deserialize)]
struct PlainFirmware {
    ipsw: Ipsw,
    inputs: BTreeMap<String, InputFile>,
    /// Public keys by archive member, where the firmware's are published.
    #[serde(default)]
    keys: BTreeMap<String, PublicKey>,
}

fn plain_boot_data(text: &str) -> Result<BootData> {
    let file: PlainBootFile =
        serde_json::from_str(text).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    let mut firmware = BTreeMap::new();
    for f in file.firmware {
        let inputs = f.inputs.into_iter().map(convert_input).collect::<Result<_>>()?;
        firmware.insert(f.ipsw.build.clone(), Firmware { ipsw: f.ipsw, inputs, keys: f.keys });
    }
    Ok(BootData { firmware, chains: BTreeMap::new(), loaders: BTreeMap::new() })
}

/// Parses `boot.json` into a board's boot data: schema 1 names the firmware
/// only, schema 2 also chains and loaders.
fn boot_data(text: &str) -> Result<BootData> {
    #[derive(Deserialize)]
    struct Schema {
        #[serde(default)]
        schema: u32,
    }
    let seen: Schema =
        serde_json::from_str(text).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    if seen.schema == 1 {
        return plain_boot_data(text);
    }
    let file: BootFile =
        serde_json::from_str(text).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    check_schema(file.schema, 2)?;
    let firmware = file
        .firmware
        .into_iter()
        .map(|(build, f)| {
            let inputs = f
                .inputs
                .into_iter()
                .map(convert_input)
                .collect::<Result<_>>()?;
            Ok((
                build.clone(),
                Firmware {
                    ipsw: Ipsw {
                        version: f.version,
                        build,
                        url: f.url,
                        size: f.size,
                        sha256: f.sha256,
                    },
                    inputs,
                    keys: f.keys,
                },
            ))
        })
        .collect::<Result<_>>()?;
    let chains = file
        .chains
        .into_iter()
        .map(|(name, recipes)| {
            let recipes = recipes
                .into_iter()
                .map(ChainRecipeFile::into_recipe)
                .collect::<Result<_>>()?;
            Ok((name, recipes))
        })
        .collect::<Result<_>>()?;
    Ok(BootData {
        firmware,
        chains,
        loaders: file.loaders,
    })
}

impl ChainRecipeFile {
    fn into_recipe(self) -> Result<ChainRecipe> {
        let patch = self
            .patch
            .into_iter()
            .map(|(stage, steps)| {
                Ok((
                    stage,
                    steps.into_iter().map(boot_step).collect::<Result<_>>()?,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(ChainRecipe {
            firmware: self.firmware,
            patch,
            outputs: self.outputs,
            source: self.source,
        })
    }
}

/// Turns one `{ "step": name, ... }` object into a [`BootStep`].
fn boot_step(mut params: serde_json::Map<String, serde_json::Value>) -> Result<BootStep> {
    let name = match params.remove("step") {
        Some(serde_json::Value::String(name)) => name,
        _ => return Err(Error::new(Code::Format, "a patch step has no name")),
    };
    Ok(BootStep { name, params })
}

#[derive(Deserialize)]
struct RecipeFile {
    /// Absent in a file means 0, which no version reads.
    #[serde(default)]
    schema: u32,
    ipsw: Ipsw,
    inputs: BTreeMap<String, InputFile>,
    #[serde(default)]
    outputs: Vec<OutputFile>,
}

#[derive(Deserialize)]
struct OutputFile {
    destination: String,
    sha256: Option<String>,
    pending: Option<String>,
    steps: Vec<serde_json::Map<String, serde_json::Value>>,
}

impl OutputFile {
    fn into_output(self) -> Result<Output> {
        let check = match (self.sha256, self.pending) {
            (Some(hex), None) => Check::Sha256(hex),
            (None, Some(reason)) => Check::Pending(reason),
            _ => {
                return Err(Error::new(Code::Format, "needs either sha256 or pending")
                    .context(&self.destination))
            }
        };
        let steps = self
            .steps
            .into_iter()
            .map(|mut params| {
                let name = match params.remove("step") {
                    Some(serde_json::Value::String(name)) => name,
                    _ => {
                        return Err(Error::new(Code::Format, "a step has no name")
                            .context(&self.destination))
                    }
                };
                let input = match params.remove("input") {
                    None => None,
                    Some(serde_json::Value::String(input)) => Some(input),
                    Some(_) => {
                        return Err(Error::new(Code::Format, "a step input is not a name")
                            .context(&self.destination))
                    }
                };
                Ok(Step {
                    name,
                    input,
                    params,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Output {
            destination: self.destination,
            check,
            steps,
        })
    }
}

#[derive(Deserialize)]
struct InputFile {
    ipsw: InputLocation,
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct InputLocation {
    zip: Option<String>,
    rootfs: Option<String>,
    kernelcache: Option<KernelcacheFile>,
    restore: Option<Ipsw>,
}

/// `"kernelcache": true` (the recipe's board) or `"kernelcache": "n102ap"`.
#[derive(Deserialize)]
#[serde(untagged)]
enum KernelcacheFile {
    Own(bool),
    Board(String),
}

fn peripheral_recipe(text: &str) -> Result<Recipe> {
    let file: RecipeFile =
        serde_json::from_str(text).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    // Steps mean what they mean in this schema only.
    check_schema(file.schema, 2)?;
    file.into_recipe()
}

/// Turns one firmware input into its `(name, Input)`, used by both recipe kinds.
fn convert_input((name, input): (String, InputFile)) -> Result<(String, Input)> {
    let InputLocation {
        zip,
        rootfs,
        kernelcache,
        restore,
    } = input.ipsw;
    let kernelcache = match kernelcache {
        None | Some(KernelcacheFile::Own(false)) => None,
        Some(KernelcacheFile::Own(true)) => Some(None),
        Some(KernelcacheFile::Board(board)) => Some(Some(board)),
    };
    // Another restore image than the recipe's: always for a named board's
    // kernel, and the input pinned (orchard's recipe.supported).
    if restore.is_some()
        && (input.sha256.is_none() || !matches!(kernelcache, Some(Some(_))) || zip.is_some())
    {
        return Err(Error::new(
            Code::Format,
            "an input from another restore image needs its sha256 and board",
        )
        .context(format!("input {name}")));
    }
    let location = match (zip, rootfs, kernelcache) {
        (Some(member), None, None) => Location::Member(member),
        (None, Some(path), None) => Location::System(path),
        (None, None, Some(board)) => Location::Kernelcache { board, restore },
        _ => {
            return Err(Error::new(Code::Format, "needs exactly one location")
                .context(format!("input {name}")))
        }
    };
    Ok((
        name,
        Input {
            location,
            sha256: input.sha256,
        },
    ))
}

impl RecipeFile {
    fn into_recipe(self) -> Result<Recipe> {
        let inputs = self
            .inputs
            .into_iter()
            .map(convert_input)
            .collect::<Result<_>>()?;
        let outputs = self
            .outputs
            .into_iter()
            .map(OutputFile::into_output)
            .collect::<Result<_>>()?;
        Ok(Recipe {
            ipsw: self.ipsw,
            inputs,
            outputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const CASCADIA: &str = r#""loaders":{"cascadia":{"load_address":"0x80000000","kernel_offset":"0x8000",
        "watchdog_address":"0x0f103020","l2_address":"0x3e000000","ram_start":"0x80000000",
        "ram_end":"0xa0000000","initrd_address":"0x88000000","initrd_limit":"0x90000000","source":"test"}}"#;

    #[test]
    fn reads_a_boards_addresses_for_cascadias_loader() {
        let dir = tempfile::tempdir().unwrap();
        board(dir.path(), "x", 0x8942, 0x0a, false);
        board(dir.path(), "y", 0x8940, 0x04, false);
        let file = dir.path().join("devices/x/boot.json");
        let with = |loaders: &str| {
            let text = boot_json("Test1,10");
            let text = format!("{},{loaders}}}", text.trim_end().strip_suffix('}').unwrap());
            fs::write(&file, text).unwrap();
        };
        with(CASCADIA);
        let catalog = Catalog::open(dir.path()).unwrap();

        let board = catalog.board("x").unwrap();
        let profile = board.cascadia().unwrap();
        assert_eq!(profile.load_address, 0x8000_0000);
        assert_eq!(profile.initrd_address, 0x8800_0000);
        assert_eq!(profile.watchdog_address, 0x0f10_3020);
        // The chains are read as before.
        assert!(board.boot_data().unwrap().chains.contains_key("iboot"));
        // A board whose boot data names no loader has no such loader.
        let other = catalog.board("y").unwrap();
        assert!(other.boot_data().unwrap().loaders.is_empty());
        assert_eq!(other.cascadia().unwrap_err().code, Code::NotFound);

        // An address that is a number or lacks its prefix, a key this version
        // does not know, a missing address and addresses that overlap are
        // refused.
        let refused = |loaders: String| {
            with(&loaders);
            catalog.board("x").unwrap().cascadia().unwrap_err()
        };
        let number = refused(CASCADIA.replace(r#""0x8000""#, "32768"));
        assert_eq!(
            number.message,
            "x: loaders.cascadia in boot.json: kernel_offset is not a hexadecimal address"
        );
        assert_eq!(
            refused(CASCADIA.replace("0x8000\"", "8000\"")).code,
            Code::Format
        );
        let unknown = refused(CASCADIA.replace(r#""source""#, r#""stack":"0x1","source""#));
        assert!(unknown.message.ends_with("unknown key stack"));
        let missing = refused(CASCADIA.replace(r#""l2_address":"0x3e000000","#, ""));
        assert!(missing
            .message
            .ends_with("l2_address is not a hexadecimal address"));
        let overlap = refused(CASCADIA.replace("0x88000000", "0x80000000"));
        assert!(overlap
            .message
            .ends_with("handoff memory ranges overlap or have invalid alignment"));
    }

    /// A peripheral firmware entry (`firmware.json`), which keeps `build` inline.
    const IPSW: &str = r#""ipsw": {"version": "1.0", "build": "1A1", "url": "https://example.com/x.ipsw", "bytes": 100, "sha256": "00"}"#;

    /// A schema-2 `boot.json` with one firmware build and one `iboot` chain.
    fn boot_json(device: &str) -> String {
        format!(
            r#"{{"schema":2,"device":"{device}",
               "firmware":{{"1A1":{{"version":"1.0","url":"https://example.com/x.ipsw","bytes":100,"sha256":"00",
                 "inputs":{{"iBSS":{{"ipsw":{{"zip":"Firmware/dfu/iBSS.im4p"}},"sha256":"aa"}}}}}}}},
               "chains":{{"iboot":[{{"firmware":"1A1","patch":{{"iBSS":[{{"step":"hbootpatcher"}}]}},"source":"test"}}]}}}}"#
        )
    }

    fn board(root: &Path, name: &str, chip: u32, board: u32, recipe: bool) {
        let dir = root.join("devices").join(name);
        fs::create_dir_all(&dir).unwrap();
        let firmware = if recipe {
            "[firmware]\nrecipe = \"firmware.json\"\n"
        } else {
            ""
        };
        fs::write(
            dir.join("device.toml"),
            format!(
                "schema = 1\n[device]\nprofile = \"{name}\"\nname = \"Test {name}\"\napple_products = [\"Test1,{board}\"]\n\
                 [match.dfu]\nchip = {chip:#x}\nboard = {board:#x}\n\
                 [boot]\nentries = [\"checkra1n\"]\n[support]\nstatus = \"supported\"\n[features]\nwifi = \"working\"\ntouch = \"missing\"\n{firmware}"
            ),
        )
        .unwrap();
        fs::write(dir.join("boot.json"), boot_json(&format!("Test1,{board}"))).unwrap();
        if recipe {
            fs::write(
                dir.join("firmware.json"),
                format!(
                    r#"{{"schema": 2, {IPSW}, "inputs": {{
                        "wifi.bin": {{"ipsw": {{"rootfs": "System/wifi.bin"}}, "sha256": "bb"}},
                        "kernelcache": {{"ipsw": {{"kernelcache": true}}}}}}, "outputs": []}}"#
                ),
            )
            .unwrap();
        }
    }

    fn sample() -> (tempfile::TempDir, Catalog) {
        let root = tempfile::tempdir().unwrap();
        board(root.path(), "x1", 0x8010, 0x18, true);
        board(root.path(), "x2", 0x8011, 0x0a, false);
        fs::create_dir_all(root.path().join("chips")).unwrap();
        fs::write(
            root.path().join("chips/t1.toml"),
            "cpid = 0x8010\nname = \"A10\"\nsoc = \"t8010\"\nstages = [\"iBSS\"]\nimage_format = \"im4p\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("features.toml"),
            "# comment\ntouch = \"Touchscreen\"\nwifi = \"Wi-Fi\"\n",
        )
        .unwrap();
        let catalog = Catalog::open(root.path()).unwrap();
        (root, catalog)
    }

    #[test]
    fn reads_chips_and_features() {
        let (_root, catalog) = sample();
        let chip = catalog.chip(0x8010).unwrap();
        assert_eq!((chip.name.as_str(), chip.stages.len()), ("A10", 1));
        assert_eq!(catalog.chip(0x8011).unwrap_err().code, Code::NotFound);
        let features: Vec<_> = catalog
            .features()
            .unwrap()
            .into_iter()
            .map(|f| f.key)
            .collect();
        assert_eq!(features, ["touch", "wifi"]);
        let b = catalog.board("x1").unwrap();
        assert_eq!(b.status, "supported");
        assert_eq!(
            b.features,
            [
                ("wifi".to_owned(), "working".to_owned()),
                ("touch".to_owned(), "missing".to_owned())
            ]
        );
    }

    #[test]
    fn reads_boards() {
        let (_root, catalog) = sample();
        assert_eq!(catalog.boards().unwrap(), ["x1", "x2"]);
        let b = catalog.board("x1").unwrap();
        assert_eq!((b.chip, b.board), (0x8010, 0x18));
        assert_eq!(b.products, ["Test1,24"]);
        assert_eq!(catalog.board_for(0x8011, 0x0a).unwrap().name, "x2");
        assert_eq!(catalog.board("x9").unwrap_err().code, Code::NotFound);
        assert_eq!(catalog.board("../x1").unwrap_err().code, Code::NotFound);
    }

    #[test]
    fn reads_both_recipes() {
        let (_root, catalog) = sample();
        let b = catalog.board("x1").unwrap();
        let boot = b.recipe(Kind::Boot).unwrap();
        assert_eq!((boot.ipsw.size, boot.ipsw.build.as_str()), (100, "1A1"));
        assert_eq!(
            boot.inputs["iBSS"].location,
            Location::Member("Firmware/dfu/iBSS.im4p".into())
        );
        let peripheral = b.recipe(Kind::Peripheral).unwrap();
        assert_eq!(
            peripheral.inputs["wifi.bin"].location,
            Location::System("System/wifi.bin".into())
        );
        assert_eq!(
            peripheral.inputs["kernelcache"].location,
            Location::Kernelcache {
                board: None,
                restore: None
            }
        );
        assert_eq!(peripheral.inputs["kernelcache"].sha256, None);
        let e = catalog
            .board("x2")
            .unwrap()
            .recipe(Kind::Peripheral)
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (Code::NotFound, "x2: no peripheral firmware recipe")
        );
    }

    #[test]
    fn reads_a_kernelcache_from_another_restore_image() {
        let restore = r#""restore": {"version": "12.4", "build": "16G77", "url": "https://example.com/y.ipsw", "bytes": 200, "sha256": "11"}"#;
        let recipe = |input: &str| {
            peripheral_recipe(&format!(
                r#"{{"schema": 2, {IPSW}, "inputs": {{"kc": {input}}}, "outputs": []}}"#
            ))
        };
        let read = recipe(&format!(
            r#"{{"ipsw": {{"kernelcache": "n102ap", {restore}}}, "sha256": "cc"}}"#
        ))
        .unwrap();
        let Location::Kernelcache {
            board,
            restore: image,
        } = &read.inputs["kc"].location
        else {
            panic!("not a kernelcache");
        };
        assert_eq!(board.as_deref(), Some("n102ap"));
        let image = image.as_ref().unwrap();
        assert_eq!(
            (image.build.as_str(), image.size, image.url.as_str()),
            ("16G77", 200, "https://example.com/y.ipsw")
        );
        assert_eq!(
            read.inputs["kc"].location.to_string(),
            "kernelcache of n102ap in 12.4 16G77"
        );
        // Another image's input is pinned and names its board.
        for refused in [
            format!(r#"{{"ipsw": {{"kernelcache": "n102ap", {restore}}}}}"#),
            format!(r#"{{"ipsw": {{"kernelcache": true, {restore}}}, "sha256": "cc"}}"#),
        ] {
            let e = recipe(&refused).unwrap_err();
            assert_eq!(e.code, Code::Format);
            assert!(e.message.contains("needs its sha256 and board"), "{}", e.message);
        }
        // A board's kernel in the recipe's own image.
        let own = recipe(r#"{"ipsw": {"kernelcache": "j71tap"}}"#).unwrap();
        assert_eq!(
            own.inputs["kc"].location,
            Location::Kernelcache {
                board: Some("j71tap".into()),
                restore: None
            }
        );
    }

    #[test]
    fn missing_data_directory() {
        let e = Catalog::open(Path::new("/nonexistent")).err().unwrap();
        assert_eq!(e.code, Code::NotFound);
        assert!(e.hint.is_some());
    }

    #[test]
    fn reads_boot_chains_and_steps() {
        let (_root, catalog) = sample();
        let data = catalog.board("x1").unwrap().boot_data().unwrap();
        let (name, recipe) = data.sole_chain().unwrap();
        assert_eq!(name, "iboot");
        assert_eq!(recipe.firmware, "1A1");
        assert_eq!(recipe.patch["iBSS"][0].name, "hbootpatcher");
        assert_eq!(data.recipe(recipe).unwrap().ipsw.build, "1A1");
    }

    #[test]
    fn names_the_chain_choice_when_several() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("devices/x");
        fs::create_dir_all(&board).unwrap();
        fs::write(
            board.join("device.toml"),
            "[device]\nname = \"X\"\n[match.dfu]\nchip = 0x8010\nboard = 0x18\n",
        )
        .unwrap();
        fs::write(
            board.join("boot.json"),
            r#"{"schema":2,"device":"Test1,1","firmware":{"1A1":{"version":"1.0","url":"https://e/x.ipsw","bytes":1,"sha256":"00","inputs":{"iBSS":{"ipsw":{"zip":"a"}}}}},
               "chains":{"iboot":[{"firmware":"1A1","patch":{"iBSS":[{"step":"hbootpatcher"}]},"source":"s"}],
                         "iboot-autogo":[{"firmware":"1A1","patch":{"iBSS":[{"step":"iboot32patcher"}]},"source":"s"}]}}"#,
        )
        .unwrap();
        let data = Catalog::open(dir.path())
            .unwrap()
            .board("x")
            .unwrap()
            .boot_data()
            .unwrap();
        let e = data.sole_chain().unwrap_err();
        assert_eq!(e.code, Code::Ambiguous);
        assert!(e.hint.unwrap().contains("iboot, iboot-autogo"));
        assert_eq!(data.chain("iboot-autogo").unwrap().0, "iboot-autogo");
        assert_eq!(data.chain("nope").unwrap_err().code, Code::NotFound);
    }

    #[test]
    fn refuses_a_firmware_recipe_of_another_schema() {
        let recipe = |schema: &str| format!(r#"{{{schema} {IPSW}, "inputs": {{}}, "outputs": []}}"#);
        peripheral_recipe(&recipe(r#""schema": 2,"#)).unwrap();
        for other in [r#""schema": 3,"#, r#""schema": 1,"#, ""] {
            let e = peripheral_recipe(&recipe(other)).unwrap_err();
            assert_eq!(e.code, Code::Unsupported, "{other}");
        }
    }

    #[test]
    fn refuses_other_boot_schema() {
        let (root, catalog) = sample();
        fs::write(
            root.path().join("devices/x1/boot.json"),
            r#"{"schema":3,"firmware":{},"chains":{}}"#,
        )
        .unwrap();
        let e = catalog.board("x1").unwrap().boot_data().unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
    }

    /// Schema 1 is what orchard's main branch writes: the firmware, no chains.
    #[test]
    fn reads_the_firmware_of_a_plain_boot_file() {
        let (root, catalog) = sample();
        fs::write(
            root.path().join("devices/x1/boot.json"),
            r#"{"schema":1,"device":"Test1,1","firmware":[{"ipsw":{"version":"1.0","build":"1A1","url":"https://e/x.ipsw","bytes":1,"sha256":"00"},
               "selected_by":"x","inputs":{"iBSS":{"ipsw":{"zip":"a"},"sha256":"11"}}}],"methods":{}}"#,
        )
        .unwrap();
        let data = catalog.board("x1").unwrap().boot_data().unwrap();
        assert!(data.chains.is_empty());
        assert_eq!(data.firmware["1A1"].ipsw.version, "1.0");
        assert!(data.firmware["1A1"].inputs.contains_key("iBSS"));
    }
}
