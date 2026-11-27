//! Boot firmware preparation, driven by a board's boot chain.
//!
//! A chain's recipe (`boot.json`) gives, per stage, the ordered patch steps
//! that turn the firmware's boot images into the files the chain loads. The
//! chip's image format decides how each stage is decoded and wrapped and which
//! surrounding files are made: IMG4 for the 64-bit chips (the `iboot` chain),
//! patched ARM32 IMG3 for the 32-bit ones (the `iboot-autogo` chain).
//!
//! The files are built in memory and kept in the cache, found again by
//! everything they are made from. A build that is cached needs neither the
//! firmware nor its keys.
mod patcher;

use crate::catalog::{Board, BootStep, Catalog, Location, Recipe};
use crate::data::{firmware::Inputs, firmware_keys::Keys};
use crate::firmware_build::{self, Boot, Written};
use orchard_cache::store::{self, Key as CacheKey, Store};
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::crypto::{self, Key};
use orchard_formats::{iboot, img3, img4};
use patcher::Patcher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;

const MAX_IMAGE: u64 = 64 << 20;
const MAX_MANIFEST: u64 = 1 << 20;
/// The boot arguments iBoot32Patcher writes, as Project Cascadia's build
/// scripts pass them.
const BOOTARGS: &str = "cs_enforcement_disable=1 debug=0x14";
/// The boot arguments the anchor patcher writes into an A12 iBSS. Project
/// Cascadia's `devices/a12/boot/prepare-device-bootset.py` also passes `-v`,
/// which makes iBoot hand m1n1 a verbose boot (`video.display` 0). No other
/// chip's route asks for that, so this one does not either.
const ANCHOR_BOOTARGS: &str = "wdt=-1";
/// The chip IM4M manifests, keyed by chip id (`0x8010`) with their SHA-256.
/// They are Apple-signed, so not part of this repository or the binary: an
/// IMG4 chain downloads its chip's from remote_boot (BSD-3-Clause) at a pinned
/// commit the first time it is built, into the cache.
const MANIFEST_BASE: &str =
    "https://raw.githubusercontent.com/HoolockLinux/remote_boot/e254a58b7d7c68c9e34a407a0beb0aab4861a6b6/im4m";
const MANIFESTS: &[(&str, &str)] = &[
    ("0x7000", "bc798a6f127d645f7b8d9ac1ddd725e901a9342b1663c11b6ceb60f1ff36f1c5"),
    ("0x7001", "bc798a6f127d645f7b8d9ac1ddd725e901a9342b1663c11b6ceb60f1ff36f1c5"),
    ("0x8000", "0b8dea8a14932afb7097705d9201d769e811ef1b493c086a8e7c5ccb8ebbbc84"),
    ("0x8001", "a28396fde17bab9e18b33f0782879b9a989460c7977e68472c262e36e7d57664"),
    ("0x8003", "ae3d2633d886825927fcb2461f157f21447971d354809c6c5dcedbc2f3c201e5"),
    ("0x8010", "ba24320ab1bd9428a9243a7beac2502e8d8f6753b0afb9e195a0d9d5b1ee596c"),
    ("0x8011", "ba24320ab1bd9428a9243a7beac2502e8d8f6753b0afb9e195a0d9d5b1ee596c"),
    ("0x8012", "451419d4b5401dd0452731e0112fb63776d74fca8b26c1a9e78ead8af0f6fd70"),
    ("0x8015", "f60f1a7e963a6b42ee534fc8ebafd4eb6e16762e647fe38aa3a7b87da0fc3479"),
    ("0x8960", "ccc6974b5e61e5c54159feed8f1fee2119958232bad457c5e74694de46e58e0a"),
];
/// Chips with no manifest of their own in that set: their patched iBSS takes
/// any (`any_manifest`), so they are wrapped with another chip's.
const BORROWED_MANIFESTS: &[(&str, &str)] = &[("0x8020", "0x8015")];
const MANIFEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The number of the build code, part of what a built set is cached under.
/// Raise it with every change to how an image is made here or in `formats`
/// (decoding, patch steps, the trampoline, wrapping, the list of files), so
/// that nothing built by older code is taken from the cache.
const BUILD: u32 = 5;

/// The empty trust cache iBoot is given before the kernelcache: version 1, a
/// UUID, no entries (remote_boot's `empty_trustcache.bin`, byte for byte).
const TRUST_CACHE: &[u8] = &[
    0x01, 0x00, 0x00, 0x00, // version
    0x6e, 0x17, 0x68, 0x17, 0x88, 0x2d, 0x44, 0x40, 0x83, 0xb6, 0xf3, 0x22, 0x06, 0xbc, 0x4f, 0x17, // uuid
    0x00, 0x00, 0x00, 0x00, // entries
];

/// A chain's built files, each with the name the recipe's `outputs` use.
pub type Images = Vec<(String, Vec<u8>)>;

/// What the cache keeps for a built recipe: the files' names, each with the
/// hash its bytes are cached under.
#[derive(Serialize, Deserialize)]
struct Index {
    files: Vec<(String, String)>,
}

/// The files cached for `key`, when all of them are there and intact.
/// The pinned IM4M manifest for a chip: from the cache, else downloaded from
/// remote_boot and checked against its SHA-256.
fn pinned_manifest(chip: u32, store: &Store) -> Result<Vec<u8>> {
    let chip = format!("{chip:#x}");
    let chip = BORROWED_MANIFESTS.iter().find(|(c, _)| *c == chip).map_or(chip.as_str(), |(_, from)| from).to_owned();
    let (_, sha) = MANIFESTS
        .iter()
        .find(|(c, _)| *c == chip)
        .ok_or_else(|| Error::new(Code::NotFound, format!("no IM4M for chip {chip}")).hint("supply --im4m FILE"))?;
    let key = CacheKey::Sha256((*sha).to_owned());
    if let Some(bytes) = store.get(&key)? {
        return Ok(bytes);
    }
    let url = format!("{MANIFEST_BASE}/{chip}.im4m");
    let mut http = orchard_cache::http::Http::probe(&url, MANIFEST_TIMEOUT)?;
    use orchard_cache::source::Source;
    if http.size() > MAX_MANIFEST {
        return Err(Error::new(Code::Format, format!("{url} is larger than a manifest")));
    }
    let size = http.size() as usize;
    let bytes = http.read_at(0, size)?;
    // put checks the bytes against the pinned SHA-256.
    store.put(&key, &bytes).map_err(|e| e.context(url.clone()))?;
    Ok(bytes)
}

fn cached(store: &Store, key: &CacheKey) -> Result<Option<Images>> {
    let Some(index) = store.get(key)? else {
        return Ok(None);
    };
    let Ok(index) = serde_json::from_slice::<Index>(&index) else {
        return Ok(None);
    };
    let mut files = Vec::new();
    for (name, sha256) in index.files {
        match store.get(&CacheKey::Sha256(sha256))? {
            Some(bytes) => files.push((name, bytes)),
            None => return Ok(None),
        }
    }
    Ok(Some(files))
}

fn cache(store: &Store, key: &CacheKey, files: &[(String, Vec<u8>)]) -> Result<()> {
    let mut index = Index { files: Vec::new() };
    for (name, bytes) in files {
        let sha256 = store::sha256(bytes);
        store.put(&CacheKey::Sha256(sha256.clone()), bytes)?;
        index.files.push((name.clone(), sha256));
    }
    let index = serde_json::to_vec(&index).expect("an index serializes");
    store.put(key, &index)
}

#[derive(Clone)]
pub struct Plan {
    chain: String,
    recipe: Recipe,
    flow: Flow,
    stages: Vec<Stage>,
    /// SHA-256 of a built image by file name, where one has been measured.
    outputs: BTreeMap<String, String>,
    public_keys: BTreeMap<String, crate::catalog::PublicKey>,
}

/// Inputs retained across the device-key step; finishing never fetches again.
pub struct Prepared {
    plan: Plan,
    key: CacheKey,
    manifest: Option<Vec<u8>>,
    inputs: BTreeMap<String, Vec<u8>>,
    keys: Keys,
    missing: Vec<Missing>,
    cached: Option<Images>,
}

struct Missing {
    member: String,
    kind: &'static str,
    bag: [u8; 48],
    pinned: bool,
}

impl Prepared {
    pub fn needs_device(&self) -> bool {
        !self.missing.is_empty()
    }

    pub fn check_restore(&self, kernel: &[u8], stub: Option<&[u8]>) -> Result<()> {
        orchard_formats::kernelcache::restore(
            kernel,
            stub,
            self.manifest
                .as_deref()
                .ok_or_else(|| Error::new(Code::Format, "restore needs an IMG4 manifest"))?,
        )?;
        Ok(())
    }

    /// Stages the missing keys. `decrypt` turns each keybag into its IV and
    /// key on a device; the device side supplies it, so building firmware
    /// never opens USB.
    pub fn acquire(
        &mut self,
        decrypt: impl FnOnce(&[[u8; 48]]) -> Result<Vec<[u8; 48]>>,
    ) -> Result<()> {
        if self.missing.is_empty() {
            return Ok(());
        }
        self.check_acquisition()?;
        let bags: Vec<_> = self.missing.iter().map(|m| m.bag).collect();
        let replies = decrypt(&bags)?;
        for (missing, bytes) in self.missing.iter().zip(replies) {
            let key = Key {
                iv: bytes[..16].try_into().unwrap(),
                key: bytes[16..].to_vec(),
            };
            self.keys.stage(
                &self.plan.recipe.ipsw.sha256,
                &missing.member,
                missing.kind,
                &key,
            )?;
        }
        self.missing.clear();
        Ok(())
    }

    pub fn check_acquisition(&self) -> Result<()> {
        if self.missing.iter().any(|m| !m.pinned) {
            return Err(Error::new(
                Code::Format,
                "device key acquisition requires a SHA-256 for each encrypted input",
            ));
        }
        Ok(())
    }

    pub fn finish(mut self, store: &Store) -> Result<Images> {
        if let Some(files) = self.cached {
            return Ok(files);
        }
        if let Some(missing) = self.missing.first() {
            return Err(Error::new(Code::NotFound, "no firmware key").context(&missing.member)
                .hint("use fw build --device-keys with a compatible DFU device, or supply --keys FILE"));
        }
        let files = match &self.manifest {
            Some(manifest) => self
                .plan
                .build_img4(&self.inputs, &mut self.keys, manifest)?,
            None => self.plan.build_img3(&self.inputs, &mut self.keys)?,
        };
        self.plan.verify(&files)?;
        self.keys.commit()?;
        cache(store, &self.key, &files)?;
        Ok(files)
    }
}

/// How a chip's boot images are decoded, wrapped and laid out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// 64-bit IMG4: each stage wrapped with the manifest, plus DeviceTree,
    /// trust cache and the manifest itself.
    Img4,
    /// 32-bit IMG3: a raw patched iBSS and a plain (and encrypted) iBEC.
    Img3,
}

/// One boot stage and the steps that patch its decoded image.
#[derive(Clone)]
struct Stage {
    name: String,
    steps: Vec<BootStep>,
}

impl Plan {
    pub fn new(catalog: &Catalog, board: &Board, chain: Option<&str>) -> Result<Self> {
        let data = board.boot_data()?;
        let chip = catalog.chip(board.chip)?;
        // The `iboot` chain needs no recipe: the open patcher of the chip's
        // image format on every stage the chip loads, from the board's
        // firmware.
        // The A12's iBoot is beyond the open patcher: there the anchor
        // patcher, and with it any manifest, since no chip manifest of ours
        // is this chip's.
        let patcher = match (chip.patcher.as_str(), chip.image_format.as_str()) {
            ("cascadia-anchor", "im4p") => Some("cascadia-anchor"),
            ("", "im4p") => Some("hbootpatcher"),
            ("", "img3") => Some("iboot32patcher"),
            _ => None,
        };
        let plain = match (chain, data.firmware.keys().next(), patcher) {
            (Some("iboot"), Some(build), Some(patcher)) if !data.chains.contains_key("iboot") => {
                let mut params = serde_json::Map::new();
                if patcher == "cascadia-anchor" {
                    params.insert("boot_args".into(), ANCHOR_BOOTARGS.into());
                    params.insert("any_manifest".into(), true.into());
                }
                let step = crate::catalog::BootStep { name: patcher.into(), params };
                Some(crate::catalog::ChainRecipe {
                    firmware: build.clone(),
                    patch: chip.stages.iter().map(|stage| (stage.clone(), vec![step.clone()])).collect(),
                    outputs: Default::default(),
                    source: "the open patcher on each stage of the chip".into(),
                })
            }
            _ => None,
        };
        let (chain, recipe) = match (&plain, chain) {
            (Some(recipe), _) => ("iboot", recipe),
            (None, Some(name)) => data.chain(name)?,
            (None, None) => data.sole_chain()?,
        };
        let firmware = data.recipe(recipe)?;

        let flow = match chip.image_format.as_str() {
            "im4p" => Flow::Img4,
            "img3" => Flow::Img3,
            other => {
                return Err(Error::new(
                    Code::Unsupported,
                    format!("chip image format {other:?} has no boot builder"),
                )
                .context(&chip.name))
            }
        };
        let chip_stages = chip.stages;
        if chip_stages != ["iBSS"] && chip_stages != ["iBSS", "iBEC"] {
            return Err(
                Error::new(Code::Format, "expected iBSS or iBSS/iBEC boot stages")
                    .context(&chip.name),
            );
        }
        if flow == Flow::Img3 && chip_stages != ["iBSS", "iBEC"] {
            return Err(Error::new(
                Code::Format,
                format!("the {chain} chain needs iBSS and iBEC stages"),
            ));
        }
        // A recipe may only patch stages the chip actually loads.
        for stage in recipe.patch.keys() {
            if !chip_stages.iter().any(|s| s == stage) {
                return Err(Error::new(
                    Code::Format,
                    format!("the {chain} recipe patches {stage}, which this chip does not load"),
                ));
            }
        }
        let stages: Vec<Stage> = chip_stages
            .iter()
            .map(|name| Stage {
                name: name.clone(),
                steps: recipe.patch.get(name).cloned().unwrap_or_default(),
            })
            .collect();
        // Report an unknown or unimplemented step before any firmware is fetched.
        for stage in &stages {
            for step in &stage.steps {
                known_step(&step.name)?;
                if step.name == "cascadia-anchor" {
                    anchor_args(&step.params)?;
                    if flow != Flow::Img4 || stage.name != "iBSS" {
                        return Err(Error::new(
                            Code::Unsupported,
                            "cascadia-anchor patches an ARM64 iBSS only",
                        ));
                    }
                }
            }
        }

        for name in &chip_stages {
            member(&firmware, name)?;
        }
        if flow == Flow::Img4 {
            member(&firmware, "DeviceTree")?;
        }
        Ok(Self {
            chain: chain.to_owned(),
            recipe: firmware,
            flow,
            stages,
            outputs: recipe.outputs.clone(),
            public_keys: data.firmware[&recipe.firmware].keys.clone(),
        })
    }

    /// The chain this plan builds.
    /// The exact Apple firmware selected for this boot plan.
    pub fn firmware(&self) -> &crate::catalog::Ipsw {
        &self.recipe.ipsw
    }

    /// One of the firmware's files as Apple ships it, by its name in the
    /// board's boot data, such as `DeviceTree`.
    pub fn input(&self, board: &Board, store: &Store, name: &str) -> Result<Vec<u8>> {
        Ok(Inputs::new(store, board, &self.recipe).get(name, MAX_IMAGE)?.bytes)
    }

    pub fn chain(&self) -> &str {
        &self.chain
    }

    /// Builds the chain's files and writes them into `out`.
    pub fn build(
        &self,
        board: &Board,
        store: &Store,
        boot: &Boot<'_>,
        out: &std::path::Path,
    ) -> Result<Vec<Written>> {
        self.images(board, store, boot)?
            .iter()
            .map(|(name, bytes)| firmware_build::write(out, name, bytes))
            .collect()
    }

    /// The chain's files, in memory: from the cache when this recipe has
    /// been built before, else built now and cached. Either way they are
    /// checked against the hashes the recipe records.
    pub fn images(&self, board: &Board, store: &Store, boot: &Boot<'_>) -> Result<Images> {
        self.prepare(board, store, boot)?.finish(store)
    }

    pub fn prepare(&self, board: &Board, store: &Store, boot: &Boot<'_>) -> Result<Prepared> {
        let manifest = match self.flow {
            Flow::Img4 => {
                let manifest = self.manifest(board, boot.im4m, store)?;
                img4::manifest(&manifest)?;
                Some(manifest)
            }
            Flow::Img3 if boot.im4m.is_some() => {
                return Err(Error::new(
                    Code::InvalidInput,
                    "this chain uses IMG3, which takes no IM4M manifest",
                ));
            }
            Flow::Img3 => None,
        };
        let key = CacheKey::Origin(self.made_from(manifest.as_deref()));
        let cached = cached(store, &key)?;
        if let Some(files) = &cached {
            self.verify(files)?;
        }
        let mut prepared = Prepared {
            plan: self.clone(),
            key,
            manifest,
            inputs: BTreeMap::new(),
            keys: Keys::new(boot.keys, store.dir()),
            missing: Vec::new(),
            cached,
        };
        if prepared.cached.is_some() {
            return Ok(prepared);
        }
        let mut source = Inputs::new(store, board, &self.recipe);
        let mut names: Vec<_> = self.stages.iter().map(|s| s.name.as_str()).collect();
        if self.flow == Flow::Img4 {
            names.push("DeviceTree");
        }
        for name in names {
            let bytes = source.get(name, MAX_IMAGE)?.bytes;
            let (encrypted, kind) = match self.flow {
                Flow::Img3 => (img3::Image::parse(&bytes)?.encrypted, "img3"),
                Flow::Img4 => (img4::encrypted(&bytes)?, "im4p"),
            };
            if encrypted {
                let member = member(&self.recipe, name)?;
                if prepared
                    .keys
                    .lookup(&self.recipe.ipsw.sha256, member, kind)?
                    .is_none()
                {
                    if let Some(public) = self.public_keys.get(member) {
                        prepared.keys.stage(
                            &self.recipe.ipsw.sha256,
                            member,
                            kind,
                            &public.decode(kind)?,
                        )?;
                    } else {
                        let bag = match (self.flow, name) {
                            (Flow::Img3, "iBSS") => img3::Image::parse(&bytes)?.keybag(b"ssbi")?,
                            (Flow::Img3, "iBEC") => img3::Image::parse(&bytes)?.keybag(b"cebi")?,
                            (Flow::Img4, name) => orchard_formats::im4p::keybag(
                                &bytes,
                                match name {
                                    "iBSS" => b"ibss",
                                    "iBEC" => b"ibec",
                                    _ => b"dtre",
                                },
                            )?,
                            _ => unreachable!(),
                        };
                        prepared.missing.push(Missing {
                            member: member.to_owned(),
                            kind,
                            bag,
                            pinned: self.recipe.inputs[name].sha256.is_some(),
                        });
                    }
                }
            }
            prepared.inputs.insert(name.to_owned(), bytes);
        }
        Ok(prepared)
    }

    /// Everything the built files depend on, as the text they are cached
    /// under: the build code's number, the firmware and the place of each
    /// input in it,
    /// each stage's patch steps with the bundled patcher they run, and what
    /// the image format adds (the manifest and trust cache, or the boot
    /// arguments).
    fn made_from(&self, manifest: Option<&[u8]>) -> String {
        let mut lines = vec![
            format!("boot firmware, build code {BUILD}"),
            format!("chain {}, ipsw {}", self.chain, self.recipe.ipsw.sha256),
        ];
        let input = |name: &str| match self.recipe.inputs.get(name) {
            Some(input) => input.location.to_string(),
            None => String::new(),
        };
        for stage in &self.stages {
            lines.push(format!("stage {}, {}", stage.name, input(&stage.name)));
            for step in &stage.steps {
                let params = serde_json::Value::Object(step.params.clone());
                let tool = match step.name.as_str() {
                    "hbootpatcher" => Patcher::Arm64.sha256(),
                    "iboot32patcher" => Patcher::Arm32.sha256(),
                    _ => String::new(),
                };
                lines.push(format!("step {} {params} {tool}", step.name));
            }
        }
        match manifest {
            Some(manifest) => {
                lines.push(format!("device tree, {}", input("DeviceTree")));
                lines.push(format!("manifest {}", store::sha256(manifest)));
                lines.push(format!("trust cache {}", store::sha256(TRUST_CACHE)));
            }
            None => lines.push(format!("boot arguments {BOOTARGS}")),
        }
        lines.join("\n")
    }

    /// The IM4M manifest to sign with: the one given, or the pinned one for
    /// the board's chip (from the cache, else downloaded and checked).
    fn manifest(&self, board: &Board, given: Option<&std::path::Path>, store: &Store) -> Result<Vec<u8>> {
        match given {
            Some(path) => firmware_build::read_capped(
                File::open(path).context(path.display())?,
                MAX_MANIFEST,
                path,
            ),
            None => pinned_manifest(board.chip, store),
        }
    }

    fn build_img4(
        &self,
        inputs: &BTreeMap<String, Vec<u8>>,
        keys: &mut Keys,
        manifest: &[u8],
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut files = Vec::new();
        for stage in &self.stages {
            let bytes = self.decode_im4p(inputs, keys, &stage.name)?;
            let patched = apply_steps(&bytes, &stage.steps, "").context(&stage.name)?;
            let (name, kind) = if stage.name == "iBSS" {
                ("ibss.img4", b"ibss")
            } else {
                ("ibec.img4", b"ibec")
            };
            files.push((name.into(), img4::wrap(kind, &patched, manifest)?));
            if stage.name == "iBSS" {
                files.push(("ibss.raw".into(), patched));
            }
        }
        let tree = self.decode_im4p(inputs, keys, "DeviceTree")?;
        files.push((
            "devicetree.img4".into(),
            img4::wrap(b"rdtr", &tree, manifest)?,
        ));
        files.push((
            "trustcache.img4".into(),
            img4::wrap(b"rtsc", TRUST_CACHE, manifest)?,
        ));
        files.push(("im4m.der".into(), manifest.to_vec()));
        Ok(files)
    }

    fn decode_im4p(
        &self,
        inputs: &BTreeMap<String, Vec<u8>>,
        keys: &mut Keys,
        name: &str,
    ) -> Result<Vec<u8>> {
        let bytes = &inputs[name];
        let key = if img4::encrypted(bytes).context(name)? {
            Some(self.key(keys, name, "im4p")?)
        } else {
            None
        };
        img4::decode(bytes, key.as_ref(), MAX_IMAGE as usize).context(name)
    }

    fn build_img3(
        &self,
        inputs: &BTreeMap<String, Vec<u8>>,
        keys: &mut Keys,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let ibss_steps = self.steps("iBSS");
        let ibss = &inputs["iBSS"];
        let image = img3::Image::parse(ibss).context("iBSS")?;
        let key = self.img3_key(keys, &image, "iBSS")?;
        let ibss =
            apply_steps(&image.decode(key.as_ref())?, ibss_steps, BOOTARGS).context("iBSS")?;

        let ibec_steps = self.steps("iBEC");
        let ibec = &inputs["iBEC"];
        let image = img3::Image::parse(ibec).context("iBEC")?;
        let key = self.img3_key(keys, &image, "iBEC")?;
        let patched =
            apply_steps(&image.decode(key.as_ref())?, ibec_steps, BOOTARGS).context("iBEC")?;

        let mut files = vec![
            ("ibss.raw".into(), ibss),
            ("ibec.plain.img3".into(), image.plain(&patched)?),
        ];
        if let Some(key) = key {
            files.push(("ibec.encrypted.img3".into(), image.encrypt(&patched, &key)?));
        }
        Ok(files)
    }

    /// The patch steps for a stage, or none if the recipe patches it none.
    fn steps(&self, name: &str) -> &[BootStep] {
        self.stages
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.steps.as_slice())
            .unwrap_or_default()
    }

    /// The key for an encrypted IMG3 stage, from the key file.
    fn img3_key(&self, keys: &mut Keys, image: &img3::Image, name: &str) -> Result<Option<Key>> {
        if image.encrypted {
            Ok(Some(self.key(keys, name, "img3")?))
        } else {
            Ok(None)
        }
    }

    fn key(&self, keys: &mut Keys, name: &str, kind: &str) -> Result<Key> {
        keys.get(&self.recipe.ipsw.sha256, member(&self.recipe, name)?, kind)
    }

    /// Fails if a built file does not match the recipe's recorded hash. A file
    /// with no recorded hash is not checked.
    fn verify(&self, files: &[(String, Vec<u8>)]) -> Result<()> {
        for (name, bytes) in files {
            if let Some(want) = self.outputs.get(name) {
                let got = store::sha256(bytes);
                if &got != want {
                    return Err(Error::new(
                        Code::Format,
                        format!("built {name} does not match the recorded hash"),
                    )
                    .hint("the firmware or the patch data may have changed"));
                }
            }
        }
        Ok(())
    }
}

/// Applies a stage's steps in order to its decoded image.
fn apply_steps(bytes: &[u8], steps: &[BootStep], bootargs: &str) -> Result<Vec<u8>> {
    let mut data = bytes.to_vec();
    for step in steps {
        data = apply_step(&step.name, &step.params, &data, bootargs).context(&step.name)?;
    }
    Ok(data)
}

fn apply_step(
    name: &str,
    params: &serde_json::Map<String, serde_json::Value>,
    data: &[u8],
    bootargs: &str,
) -> Result<Vec<u8>> {
    match name {
        "hbootpatcher" => Patcher::Arm64.patch(data, ""),
        "iboot32patcher" => Patcher::Arm32.patch(data, bootargs),
        "autogo" => {
            let mut out = data.to_vec();
            autogo(params)?.apply(&mut out)?;
            Ok(out)
        }
        "bytes" => bytes_patch(params, data),
        "cascadia-anchor" => {
            let patched = iboot::anchor::patch(data, anchor_args(params)?)?;
            Ok(if any_manifest(params) { iboot::anchor::any_manifest(&patched)? } else { patched })
        }
        _ => Err(unsupported_step(name)),
    }
}

/// The steps this version can run; others are reported before any work starts.
fn known_step(name: &str) -> Result<()> {
    match name {
        "hbootpatcher" | "iboot32patcher" | "autogo" | "bytes" | "cascadia-anchor" => Ok(()),
        _ => Err(unsupported_step(name)),
    }
}

fn unsupported_step(name: &str) -> Error {
    Error::new(Code::Unsupported, format!("{name}: unknown patch step"))
}

/// Whether the step also lets the iBSS take any manifest.
fn any_manifest(params: &serde_json::Map<String, serde_json::Value>) -> bool {
    params.get("any_manifest").and_then(|value| value.as_bool()) == Some(true)
}

fn anchor_args(params: &serde_json::Map<String, serde_json::Value>) -> Result<&str> {
    let known = 1 + usize::from(params.get("any_manifest").is_some_and(|value| value.is_boolean()));
    let args = params
        .get("boot_args")
        .and_then(|value| value.as_str())
        .filter(|_| params.len() == known)
        .ok_or_else(|| {
            Error::new(
                Code::Format,
                "cascadia-anchor expects a boot_args string and at most any_manifest",
            )
        })?;
    iboot::anchor::validate_args(args)?;
    Ok(args)
}

/// Builds the `autogo` trampoline patch from a step's parameters.
fn autogo(params: &serde_json::Map<String, serde_json::Value>) -> Result<iboot::Autogo> {
    let str = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::new(Code::Format, format!("autogo step needs a {key}")))
    };
    let address = |key: &str| -> Result<u32> {
        let s = str(key)?;
        u32::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
            .map_err(|_| Error::new(Code::Format, format!("autogo {key} is not a hex address")))
    };
    Ok(iboot::Autogo {
        hook: address("hook")?,
        original: crypto::hex(str("original")?)?,
        trampoline: address("trampoline")?,
        cache_flush: address("cache_flush")?,
        done_flag: address("done_flag")?,
        usb_continue: address("usb_continue")?,
    })
}

/// Replaces `original` with `replacement` at `offset` in the decoded image,
/// after checking the original bytes are there.
fn bytes_patch(
    params: &serde_json::Map<String, serde_json::Value>,
    data: &[u8],
) -> Result<Vec<u8>> {
    let str = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::new(Code::Format, format!("bytes step needs a {key}")))
    };
    let s = str("offset")?;
    let offset = usize::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
        .map_err(|_| Error::new(Code::Format, "bytes offset is not a hex number"))?;
    let original = crypto::hex(str("original")?)?;
    let replacement = crypto::hex(str("replacement")?)?;
    if original.len() != replacement.len() {
        return Err(Error::new(
            Code::Format,
            "bytes original and replacement are different lengths",
        ));
    }
    let end = offset
        .checked_add(original.len())
        .filter(|&end| end <= data.len())
        .ok_or_else(|| Error::new(Code::Format, "bytes offset is past the end of the image"))?;
    let mut out = data.to_vec();
    if out[offset..end] != original[..] {
        return Err(Error::new(
            Code::Format,
            "bytes original does not match the image at that offset",
        )
        .hint("the image may be from a different build than the data expects"));
    }
    out[offset..end].copy_from_slice(&replacement);
    Ok(out)
}

fn member<'a>(recipe: &'a Recipe, name: &str) -> Result<&'a str> {
    match recipe.inputs.get(name).map(|i| &i.location) {
        Some(Location::Member(path)) => Ok(path),
        _ => Err(Error::new(Code::Format, "boot input needs an archive member").context(name)),
    }
}

#[cfg(test)]
mod tests {
    /// The pinned manifest table: one entry per chip, real SHA-256 hashes.
    #[test]
    fn the_manifest_pins_are_well_formed() {
        let mut chips: Vec<_> = MANIFESTS.iter().map(|(c, _)| *c).collect();
        chips.dedup();
        assert_eq!(chips.len(), MANIFESTS.len());
        assert!(MANIFESTS.iter().all(|(c, s)| c.starts_with("0x") && s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())));
    }

    /// Every pinned manifest downloads and matches its hash (network; run with
    /// `cargo test -p orchard-boards -- --ignored manifests_download`).
    #[test]
    #[ignore]
    fn manifests_download() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        for (chip, _) in MANIFESTS {
            let chip = u32::from_str_radix(&chip[2..], 16).unwrap();
            let bytes = pinned_manifest(chip, &store).unwrap();
            img4::manifest(&bytes).unwrap();
            // The second time it comes from the cache.
            assert_eq!(pinned_manifest(chip, &store).unwrap(), bytes);
        }
    }

    use super::*;
    use std::fs;

    const FW: &str = r#""12H321":{"version":"8.4.1","url":"https://e/x.ipsw","bytes":1,"sha256":"00","inputs":{
        "iBSS":{"ipsw":{"zip":"Firmware/dfu/iBSS"}},
        "iBEC":{"ipsw":{"zip":"Firmware/dfu/iBEC"}},
        "DeviceTree":{"ipsw":{"zip":"Firmware/DeviceTree"}}}}"#;

    const AUTOGO: &str = r#"{"step":"autogo","hook":"0x1000","original":"01020304","trampoline":"0x2000",
        "cache_flush":"0x9ff20000","done_flag":"0x9ff40000","usb_continue":"0x9ff50000"}"#;

    /// A data directory with one chip and a board whose `boot.json` offers an
    /// `iboot` chain (hBootPatcher) and an `iboot-autogo` chain (iBoot32Patcher
    /// then autogo).
    fn catalog(stages: &str, image_format: &str) -> (tempfile::TempDir, Catalog, Board) {
        let dir = tempfile::tempdir().unwrap();
        let chips = dir.path().join("chips");
        fs::create_dir_all(&chips).unwrap();
        fs::write(
            chips.join("c.toml"),
            format!(
                "cpid = 0x8010\nname = \"A10\"\nsoc = \"t8010\"\nstages = {stages}\nimage_format = \"{image_format}\"\n"
            ),
        )
        .unwrap();
        let board = dir.path().join("devices/x");
        fs::create_dir_all(&board).unwrap();
        fs::write(
            board.join("device.toml"),
            "[device]\nname = \"X\"\n[match.dfu]\nchip = 0x8010\nboard = 0x18\n",
        )
        .unwrap();
        fs::write(
            board.join("boot.json"),
            format!(
                r#"{{"schema":2,"device":"Test1,1","firmware":{{{FW}}},"chains":{{
                    "iboot":[{{"firmware":"12H321","patch":{{"iBSS":[{{"step":"hbootpatcher"}}]}},"source":"s"}}],
                    "iboot-autogo":[{{"firmware":"12H321","patch":{{
                        "iBSS":[{{"step":"iboot32patcher"}}],
                        "iBEC":[{{"step":"iboot32patcher"}},{AUTOGO}]}},"source":"s"}}]}}}}"#
            ),
        )
        .unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let board = catalog.board("x").unwrap();
        (dir, catalog, board)
    }

    fn plan(stages: &str, image_format: &str, chain: &str) -> Result<()> {
        let (_dir, catalog, board) = catalog(stages, image_format);
        Plan::new(&catalog, &board, Some(chain)).map(|_| ())
    }

    /// Generated encrypted containers, cached by input hash; no network or real firmware.
    fn encrypted_inputs(plan: &mut Plan, store: &Store) -> Key {
        use orchard_formats::der::{write, IA5_STRING, INTEGER, OCTET_STRING, SEQUENCE};
        let key = Key {
            iv: [9; 16],
            key: vec![7; 32],
        };
        for stage in &mut plan.stages {
            stage.steps.clear();
        }
        let mut names: Vec<_> = plan.stages.iter().map(|s| s.name.clone()).collect();
        if plan.flow == Flow::Img4 {
            names.push("DeviceTree".into());
        }
        for name in names {
            let mut payload = vec![b'a'; 16];
            key.encrypt(&mut payload).unwrap();
            let bytes = if plan.flow == Flow::Img4 {
                let kind: &[u8] = match name.as_str() {
                    "iBSS" => b"ibss",
                    "iBEC" => b"ibec",
                    _ => b"dtre",
                };
                let bag = write(
                    SEQUENCE,
                    &[
                        write(INTEGER, &[1]),
                        write(OCTET_STRING, &[1; 16]),
                        write(OCTET_STRING, &[2; 32]),
                    ]
                    .concat(),
                );
                write(
                    SEQUENCE,
                    &[
                        write(IA5_STRING, b"IM4P"),
                        write(IA5_STRING, kind),
                        write(IA5_STRING, b""),
                        write(OCTET_STRING, &payload),
                        write(OCTET_STRING, &write(SEQUENCE, &bag)),
                    ]
                    .concat(),
                )
            } else {
                let mut body = b"GABK".to_vec();
                body.extend(68u32.to_le_bytes());
                body.extend(56u32.to_le_bytes());
                body.extend(1u32.to_le_bytes());
                body.extend(256u32.to_le_bytes());
                body.extend([2; 48]);
                body.extend(b"ATAD");
                body.extend(28u32.to_le_bytes());
                body.extend(16u32.to_le_bytes());
                body.extend(payload);
                let mut image = b"3gmI".to_vec();
                image.extend((20 + body.len() as u32).to_le_bytes());
                image.extend((body.len() as u32).to_le_bytes());
                image.extend([0; 4]);
                image.extend(if name == "iBSS" { b"ssbi" } else { b"cebi" });
                image.extend(body);
                image
            };
            let hash = store::sha256(&bytes);
            plan.recipe.inputs.get_mut(&name).unwrap().sha256 = Some(hash.clone());
            store.put(&CacheKey::Sha256(hash), &bytes).unwrap();
        }
        plan.outputs
            .insert("ibss.raw".into(), store::sha256(&[b'a'; 16]));
        key
    }

    #[test]
    fn encrypted_builds_use_public_defaults_then_automatic_cache() {
        for (format, stages, chain) in [
            ("img3", r#"["iBSS","iBEC"]"#, "iboot-autogo"),
            ("im4p", r#"["iBSS","iBEC"]"#, "iboot"),
        ] {
            let (dir, catalog, board) = catalog(stages, format);
            let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
            let mut plan = Plan::new(&catalog, &board, Some(chain)).unwrap();
            let key = encrypted_inputs(&mut plan, &store);
            let encode =
                |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            for input in plan.recipe.inputs.values() {
                if let Location::Member(member) = &input.location {
                    plan.public_keys.insert(
                        member.clone(),
                        crate::catalog::PublicKey {
                            kind: format.into(),
                            key: encode(&key.key),
                            iv: encode(&key.iv),
                            source: "generated test".into(),
                        },
                    );
                }
            }
            let files = plan.images(&board, &store, &Boot::default()).unwrap();
            assert!(store.dir().join("firmware-keys").is_dir());
            plan.public_keys.clear();
            // Changing only cache identity forces a real rebuild with cached keys.
            plan.chain.push_str("-again");
            assert_eq!(
                plan.images(&board, &store, &Boot::default()).unwrap(),
                files
            );
        }
    }

    #[test]
    fn acquired_keys_are_not_persisted_after_failed_verification() {
        let (dir, catalog, board) = catalog(r#"["iBSS"]"#, "im4p");
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let mut plan = Plan::new(&catalog, &board, Some("iboot")).unwrap();
        let key = encrypted_inputs(&mut plan, &store);
        let mut pending = plan.prepare(&board, &store, &Boot::default()).unwrap();
        assert!(pending.needs_device());
        for missing in &pending.missing {
            pending
                .keys
                .stage(
                    &plan.recipe.ipsw.sha256,
                    &missing.member,
                    missing.kind,
                    &key,
                )
                .unwrap();
        }
        pending.missing.clear();
        pending
            .plan
            .outputs
            .insert("ibss.raw".into(), store::sha256(b"wrong"));
        assert!(pending.finish(&store).is_err());
        assert!(!store.dir().join("firmware-keys").exists());
    }

    #[test]
    fn missing_keys_are_prepared_before_any_device_operation_and_require_pins() {
        let (dir, catalog, board) = catalog(r#"["iBSS"]"#, "im4p");
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let mut plan = Plan::new(&catalog, &board, Some("iboot")).unwrap();
        encrypted_inputs(&mut plan, &store);
        let mut prepared = plan.prepare(&board, &store, &Boot::default()).unwrap();
        assert_eq!(prepared.missing.len(), 2);
        prepared.check_acquisition().unwrap();
        prepared.missing[0].pinned = false;
        assert!(prepared.check_acquisition().is_err());
        assert!(prepared
            .finish(&store)
            .unwrap_err()
            .hint
            .unwrap()
            .contains("--device-keys"));
    }

    #[test]
    fn plans_both_chains() {
        assert!(plan(r#"["iBSS", "iBEC"]"#, "img3", "iboot-autogo").is_ok());
        assert!(plan(r#"["iBSS"]"#, "im4p", "iboot").is_ok());
    }

    /// A stand-in IM4M: a sequence beginning with the string `IM4M`.
    fn im4m(dir: &std::path::Path, extra: &[u8]) -> std::path::PathBuf {
        use orchard_formats::der::{write, IA5_STRING, OCTET_STRING, SEQUENCE};
        let mut body = write(IA5_STRING, b"IM4M");
        body.extend(write(OCTET_STRING, extra));
        let path = dir.join(format!("{}.im4m", extra.len()));
        fs::write(&path, write(SEQUENCE, &body)).unwrap();
        path
    }

    #[test]
    fn a_built_recipe_comes_from_the_cache_without_firmware_or_keys() {
        let (dir, catalog, board) = catalog(r#"["iBSS"]"#, "im4p");
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let manifest = im4m(dir.path(), b"");
        let boot = Boot {
            im4m: Some(&manifest),
            ..Boot::default()
        };
        let plan = Plan::new(&catalog, &board, Some("iboot")).unwrap();
        let key = CacheKey::Origin(plan.made_from(Some(&fs::read(&manifest).unwrap())));
        assert!(cached(&store, &key).unwrap().is_none());

        // As a first build leaves them. The firmware's address resolves
        // nowhere and there is no key file, so only the cache can answer.
        let built = vec![
            ("ibss.img4".to_owned(), b"stage".to_vec()),
            ("devicetree.img4".to_owned(), b"tree".to_vec()),
        ];
        cache(&store, &key, &built).unwrap();
        assert_eq!(plan.images(&board, &store, &boot).unwrap(), built);

        let out = dir.path().join("out");
        let written = plan.build(&board, &store, &boot, &out).unwrap();
        assert_eq!(written[0].destination, "ibss.img4");
        assert_eq!(fs::read(out.join("devicetree.img4")).unwrap(), b"tree");

        // A cached file that is gone or damaged means building again.
        let tree = store::sha256(b"tree");
        fs::write(store.dir().join(format!("files/sha256-{tree}")), b"bad").unwrap();
        assert!(cached(&store, &key).unwrap().is_none());
    }

    #[test]
    fn img4_build_keeps_the_patched_ibss_raw_and_caches_both_forms() {
        use orchard_formats::der;
        for stages in [r#"["iBSS"]"#, r#"["iBSS", "iBEC"]"#] {
            let (dir, catalog, board) = catalog(stages, "im4p");
            let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
            let manifest = im4m(dir.path(), b"test");
            let manifest_bytes = fs::read(&manifest).unwrap();
            let mut plan = Plan::new(&catalog, &board, Some("iboot")).unwrap();
            plan.stages[0].steps = vec![BootStep {
                name: "bytes".into(),
                params: serde_json::from_str(
                    r#"{"offset":"0x0","original":"6161","replacement":"6262"}"#,
                )
                .unwrap(),
            }];
            // Real decoding, patching and wrapping, with cached IPSW inputs.
            for input in plan.recipe.inputs.values() {
                let wrapped = img4::wrap(b"ibss", b"aa payload", &manifest_bytes).unwrap();
                let outer = der::read(&wrapped).unwrap();
                let fields = der::items(outer.body).unwrap();
                let key = CacheKey::Origin(format!(
                    "ipsw {}, {}",
                    plan.recipe.ipsw.sha256, input.location
                ));
                store.put(&key, fields[1].encoded).unwrap();
            }
            let boot = Boot {
                im4m: Some(&manifest),
                ..Boot::default()
            };
            let files = plan.images(&board, &store, &boot).unwrap();
            let raw = &files.iter().find(|(n, _)| n == "ibss.raw").unwrap().1;
            assert_eq!(raw, b"bb payload");
            let wrapped = &files.iter().find(|(n, _)| n == "ibss.img4").unwrap().1;
            let outer = der::read(wrapped).unwrap();
            let fields = der::items(outer.body).unwrap();
            assert_eq!(img4::decode(fields[1].encoded, None, 1024).unwrap(), *raw);
            assert!(!files.iter().any(|(n, _)| n == "ibec.raw"));
            assert_eq!(plan.images(&board, &store, &boot).unwrap(), files);
            let out = dir.path().join("out");
            plan.build(&board, &store, &boot, &out).unwrap();
            assert_eq!(fs::read(out.join("ibss.raw")).unwrap(), *raw);
        }
    }

    #[test]
    fn the_cache_tells_recipes_manifests_and_chains_apart() {
        let (dir64, catalog64, board64) = catalog(r#"["iBSS"]"#, "im4p");
        let iboot = Plan::new(&catalog64, &board64, Some("iboot")).unwrap();
        let (dir, catalog, board) = catalog(r#"["iBSS", "iBEC"]"#, "img3");
        let autogo = Plan::new(&catalog, &board, Some("iboot-autogo")).unwrap();
        let (one, other) = (b"one".as_slice(), b"other".as_slice());
        let keys = [
            autogo.made_from(None),
            iboot.made_from(Some(one)),
            iboot.made_from(Some(other)),
        ];
        assert!(keys[0] != keys[1] && keys[1] != keys[2]);
        assert_eq!(keys[1], iboot.made_from(Some(one)));
        assert!(keys[1].starts_with(&format!("boot firmware, build code {BUILD}\n")));

        // Another patch step for the same stage is another build.
        let json = dir64.path().join("devices/x/boot.json");
        let text = fs::read_to_string(&json).unwrap().replace(
            r#"[{"step":"hbootpatcher"}]"#,
            r#"[{"step":"hbootpatcher"},{"step":"bytes","offset":"0x4","original":"aa","replacement":"bb"}]"#,
        );
        fs::write(&json, text).unwrap();
        let patched = Plan::new(&catalog64, &catalog64.board("x").unwrap(), Some("iboot")).unwrap();
        assert_ne!(patched.made_from(Some(one)), keys[1]);

        // A recorded output hash is checked on what the cache returns too.
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let mut checked = autogo;
        checked
            .outputs
            .insert("ibss.raw".into(), store::sha256(b"expected"));
        let key = CacheKey::Origin(checked.made_from(None));
        cache(&store, &key, &[("ibss.raw".into(), b"other".to_vec())]).unwrap();
        let e = checked
            .images(&board, &store, &Boot::default())
            .err()
            .unwrap();
        assert_eq!(e.message, "built ibss.raw does not match the recorded hash");
    }

    #[test]
    fn autogo_chain_needs_two_stages() {
        let e = plan(r#"["iBSS"]"#, "img3", "iboot-autogo").unwrap_err();
        assert_eq!(e.code, Code::Format);
    }

    #[test]
    fn unknown_chain_lists_the_offered_ones() {
        let e = plan(r#"["iBSS"]"#, "im4p", "sideload").unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert!(e.hint.unwrap().contains("iboot"));
    }

    #[test]
    fn autogo_parameters_are_parsed() {
        let p = autogo(&serde_json::from_str::<serde_json::Map<_, _>>(AUTOGO).unwrap()).unwrap();
        assert_eq!((p.hook, p.trampoline), (0x1000, 0x2000));
        assert_eq!(p.original, [1, 2, 3, 4]);
    }

    #[test]
    fn bytes_step_checks_and_replaces() {
        let params =
            serde_json::from_str(r#"{"offset":"0x4","original":"aabb","replacement":"ccdd"}"#)
                .unwrap();
        let out = bytes_patch(&params, &[0, 1, 2, 3, 0xaa, 0xbb, 6, 7]).unwrap();
        assert_eq!(out, [0, 1, 2, 3, 0xcc, 0xdd, 6, 7]);
        // A mismatch at the offset is refused, not silently applied.
        let e = bytes_patch(&params, &[0; 8]).unwrap_err();
        assert_eq!(e.code, Code::Format);
    }

    #[test]
    fn cascadia_anchor_requires_explicit_args_but_no_output_hash() {
        let dir = tempfile::tempdir().unwrap();
        let chips = dir.path().join("chips");
        fs::create_dir_all(&chips).unwrap();
        fs::write(
            chips.join("c.toml"),
            "cpid = 0x8020\nname = \"A12\"\nsoc = \"t8020\"\nstages = [\"iBSS\"]\nimage_format = \"im4p\"\n",
        )
        .unwrap();
        let board = dir.path().join("devices/x");
        fs::create_dir_all(&board).unwrap();
        fs::write(
            board.join("device.toml"),
            "[device]\nname = \"X\"\n[match.dfu]\nchip = 0x8020\nboard = 0x02\n",
        )
        .unwrap();
        fs::write(
            board.join("boot.json"),
            r#"{"schema":2,"device":"iPhone11,8","firmware":{"22H374":{"version":"18.7.10","url":"https://e/x.ipsw","bytes":1,"sha256":"00","inputs":{"iBSS":{"ipsw":{"zip":"a"}},"DeviceTree":{"ipsw":{"zip":"b"}}}}},
               "chains":{"iboot":[{"firmware":"22H374","patch":{"iBSS":[{"step":"cascadia-anchor"}]},"source":"s"}]}}"#,
        )
        .unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let e = Plan::new(&catalog, &catalog.board("x").unwrap(), None)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(e.code, Code::Format);
        assert!(e.message.contains("boot_args"));
        let path = board.join("boot.json");
        let text = fs::read_to_string(&path).unwrap().replace(
            r#"{"step":"cascadia-anchor"}"#,
            r#"{"step":"cascadia-anchor","boot_args":"-v wdt=-1 serial=3"}"#,
        );
        fs::write(path, text).unwrap();
        let plan = Plan::new(&catalog, &catalog.board("x").unwrap(), None).unwrap();
        assert!(plan.outputs.is_empty());
        assert_eq!(plan.stages[0].steps[0].name, "cascadia-anchor");
    }

    /// Synthetic ARM64 instructions matching the three independent anchors.
    fn anchor_image() -> Vec<u8> {
        let mut data = vec![0xcc; 0x1100];
        let words = [
            (0x100, 0x540007c1u32),
            (0x104, 0xaa1403e0),
            (0x120, 0xa94d7bfd),
            (0x200, 0x90000002),
            (0x204, 0x91240042),
            (0x300, 0xaa1403e0),
            (0x304, 0xaa1503e3),
            (0x308, 0x94000001),
            (0x30c, 0x340003a0),
            (0x310, 0x51007b75),
            (0x314, 0x14000001),
            (0x380, 0x910083e8),
            (0x384, 0x9100c119),
            (0x500, 0x90000002),
            (0x504, 0x91220042),
            (0x508, 0x910003e0),
            (0x50c, 0x52808001),
            (0x510, 0x94000001),
        ];
        for (offset, word) in words {
            data[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        let anchor = b"Unknown ASN1 type %llu\n\0";
        data[0x900..0x900 + anchor.len()].copy_from_slice(anchor);
        data[0x87f..0x883].copy_from_slice(b"\0%s\0");
        data[0xf00..0x1000].fill(0);
        data
    }

    #[test]
    fn anchor_builds_wrap_and_cache_without_output_hashes_and_args_change_the_cache_key() {
        use orchard_formats::der;
        let (dir, catalog, board) = catalog(r#"["iBSS"]"#, "im4p");
        let store = Store::open(Some(&dir.path().join("cache"))).unwrap();
        let manifest = im4m(dir.path(), b"synthetic");
        let manifest_bytes = fs::read(&manifest).unwrap();
        let boot = Boot {
            im4m: Some(&manifest),
            ..Boot::default()
        };
        let mut plan = Plan::new(&catalog, &board, Some("iboot")).unwrap();
        let original = anchor_image();
        for (name, input) in &plan.recipe.inputs {
            let bytes = if name == "iBSS" {
                original.as_slice()
            } else {
                b"tree"
            };
            let wrapped = img4::wrap(b"ibss", bytes, &manifest_bytes).unwrap();
            let fields = der::items(der::read(&wrapped).unwrap().body).unwrap();
            let key = CacheKey::Origin(format!(
                "ipsw {}, {}",
                plan.recipe.ipsw.sha256, input.location
            ));
            store.put(&key, fields[1].encoded).unwrap();
        }
        let mut prior_key = None;
        for args in ["-v wdt=-1 serial=3", "-v wdt=-1"] {
            plan.stages[0].steps = vec![BootStep {
                name: "cascadia-anchor".into(),
                params: [("boot_args".into(), serde_json::Value::String(args.into()))]
                    .into_iter()
                    .collect(),
            }];
            assert!(plan.outputs.is_empty());
            let cache_key = plan.made_from(Some(&manifest_bytes));
            assert_ne!(prior_key.as_ref(), Some(&cache_key));
            prior_key = Some(cache_key);
            let files = plan.images(&board, &store, &boot).unwrap();
            let raw = &files.iter().find(|(name, _)| name == "ibss.raw").unwrap().1;
            assert_eq!(raw, &iboot::anchor::patch(&original, args).unwrap());
            assert_eq!(plan.images(&board, &store, &boot).unwrap(), files);
            // The next named step sees the patched bytes, in recipe order.
            let mut steps = plan.stages[0].steps.clone();
            steps.push(BootStep {
                name: "bytes".into(),
                params: serde_json::from_str(
                    r#"{"offset":"0x104","original":"000080d2","replacement":"1f2003d5"}"#,
                )
                .unwrap(),
            });
            let chained = apply_steps(&original, &steps, "").unwrap();
            assert_eq!(&chained[0x104..0x108], &[0x1f, 0x20, 0x03, 0xd5]);
        }
    }

    #[test]
    fn anchor_parameters_are_checked_before_patching() {
        for params in [
            r#"{}"#,
            r#"{"boot_args":3}"#,
            r#"{"boot_args":""}"#,
            r#"{"boot_args":"-v","serial":true}"#,
            r#"{"boot_args":"%s"}"#,
        ] {
            let params = serde_json::from_str(params).unwrap();
            assert_eq!(anchor_args(&params).unwrap_err().code, Code::Format);
        }
        let params = serde_json::from_str(r#"{"boot_args":"-v"}"#).unwrap();
        let error = apply_step("cascadia-anchor", &params, &[], "").unwrap_err();
        assert_eq!(error.code, Code::Format);
        assert!(error.message.contains("ASN1"));
    }

    /// Two boards on different chips each get the flow their chip's format
    /// allows; the 32-bit chain is refused on the 64-bit chip.
    #[test]
    fn each_board_is_planned_by_its_own_chip() {
        let (dir, _, _) = catalog(r#"["iBSS", "iBEC"]"#, "img3");
        fs::write(
            dir.path().join("chips/d.toml"),
            "cpid = 0x8030\nname = \"A13\"\nsoc = \"t8030\"\nstages = [\"iBSS\"]\nimage_format = \"im4p\"\n",
        )
        .unwrap();
        let y = dir.path().join("devices/y");
        fs::create_dir_all(&y).unwrap();
        fs::write(
            y.join("device.toml"),
            "[device]\nname = \"Y\"\n[match.dfu]\nchip = 0x8030\nboard = 0x02\n",
        )
        .unwrap();
        fs::write(
            y.join("boot.json"),
            format!(
                r#"{{"schema":2,"device":"Test1,2","firmware":{{{FW}}},
                   "chains":{{"iboot":[{{"firmware":"12H321","patch":{{"iBSS":[{{"step":"hbootpatcher"}}]}},"source":"s"}}]}}}}"#
            ),
        )
        .unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let (x, y) = (catalog.board("x").unwrap(), catalog.board("y").unwrap());
        assert!(Plan::new(&catalog, &x, Some("iboot-autogo")).is_ok());
        assert!(Plan::new(&catalog, &y, Some("iboot")).is_ok());
    }
}
