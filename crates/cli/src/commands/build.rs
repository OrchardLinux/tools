//! `build` and `inputs`: a project's RAM payload or release, and what a
//! distribution's build takes from orchard finished. Both stream what the
//! producers print to stderr; the result is one document at the end.
use crate::args::Global;
use crate::output::{Output, Report};
use orchard_boards::catalog::Catalog;
use orchard_build::components::Given;
use orchard_build::project::{self, Choice};
use orchard_build::{distro, inputs, manage, ram, snapshot, Build};
use orchard_cache::component::Components;
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_device::select;
use orchard_usb::{Host, Mode};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct BuildArgs {
    /// A payload directory (init, optional rootfs/) or a distribution
    /// checkout (orchard.toml [release])
    #[arg(default_value = ".")]
    directory: PathBuf,
    /// Distribution checkouts: the version to build (default: dev-DATE-TIME)
    #[arg(long)]
    version: Option<String>,
    /// A board; its platform is built (default: orchard.local.toml, else
    /// the attached device)
    #[arg(long)]
    device: Option<String>,
    /// The platform to build, such as apple-16k; a distribution checkout
    /// takes it more than once, or all
    #[arg(long)]
    platform: Vec<String>,
    /// Reuse a staged kernel, device tree and module tree
    #[arg(long, conflicts_with = "kernel_tree")]
    kernel_dir: Option<PathBuf>,
    /// Build this kernel tree as it is, with its own defconfig (default:
    /// kernel-tree in orchard.local.toml, else orchard's own kernel)
    #[arg(long)]
    kernel_tree: Option<PathBuf>,
    /// Distribution checkouts: build the image's [orchard] packages from
    /// this checkout of OrchardLinux/packages where it differs from the
    /// published repository
    #[arg(long)]
    packages: Option<PathBuf>,
    /// Reuse this Pongo
    #[arg(long)]
    pongo: Option<PathBuf>,
    /// Reuse this static BusyBox
    #[arg(long)]
    busybox: Option<PathBuf>,
    /// Reuse this loader (m1n1, or a board's Cascadia loader)
    #[arg(long)]
    loader: Option<PathBuf>,
    /// Print what the build ran as plain commands
    #[arg(long)]
    explain: bool,
}

#[derive(clap::Args)]
pub struct InputsArgs {
    /// The JSON document to write
    #[arg(long)]
    output: PathBuf,
    /// A platform to resolve; repeat (default: every arm64 platform)
    #[arg(long)]
    platform: Vec<String>,
    /// A platform's staged kernel bundle instead of orchard's
    #[arg(long, value_name = "PLATFORM=DIRECTORY", value_parser = pair)]
    kernel_dir: Vec<(String, PathBuf)>,
    /// orchard's m1n1 to use as it is
    #[arg(long)]
    m1n1: Option<PathBuf>,
    /// The Pongo to use as it is
    #[arg(long)]
    pongo: Option<PathBuf>,
    /// A directory with the static tools to use as they are
    #[arg(long)]
    tools: Option<PathBuf>,
    /// ARMv7: a board's Cascadia loader to use as it is; repeat
    #[arg(long, value_name = "BOARD=FILE", value_parser = pair)]
    loader: Vec<(String, PathBuf)>,
    /// Build the kernels from this kernel tree
    #[arg(long, env = "ORCHARD_KERNEL_TREE")]
    kernel_tree: Option<PathBuf>,
}

fn pair(text: &str) -> std::result::Result<(String, PathBuf), String> {
    match text.split_once('=') {
        Some((name, path)) if !name.is_empty() && !path.is_empty() => {
            Ok((name.to_owned(), PathBuf::from(path)))
        }
        _ => Err("expected NAME=PATH".into()),
    }
}

/// What a build made.
#[derive(Serialize)]
struct Built {
    /// `payload` or `release`.
    kind: &'static str,
    /// The payload's directory, or the release's index.
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    boards: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// What the build ran, as a script.
    commands: String,
    #[serde(skip)]
    explain: Option<String>,
}

impl Report for Built {
    fn human(&self) -> Vec<u8> {
        match &self.explain {
            Some(script) => script.clone().into_bytes(),
            None => format!(
                "{}\nWhat it ran, as plain commands: {} (or orchard2 build --explain)\n",
                self.output, self.commands
            )
            .into_bytes(),
        }
    }
}

fn say(text: &str) {
    eprintln!("{text}");
}

/// The board of the one device attached in DFU or Pongo mode, if the data
/// describes it. Only lists USB devices: nothing is opened.
fn attached_board(global: &Global, root: &Path) -> Option<String> {
    let devices = select::matching(&mut Host, &global.device.selector(None)).ok()?;
    let mut waiting = devices
        .iter()
        .filter(|d| matches!(d.mode, Mode::Dfu | Mode::Pongo));
    let (device, None) = (waiting.next()?, waiting.next()) else {
        return None;
    };
    let hex = |text: &Option<String>| {
        text.as_deref()
            .and_then(|t| u32::from_str_radix(t, 16).ok())
    };
    let catalog = Catalog::open(root).ok()?;
    Some(
        catalog
            .board_for(hex(&device.identity.chip)?, hex(&device.identity.board_id)?)
            .ok()?
            .name,
    )
}

impl BuildArgs {
    /// The loader given to use as it is.
    pub fn loader(&self) -> Option<&Path> {
        self.loader.as_deref()
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

pub fn build(global: &Global, args: &BuildArgs) -> Result<Output> {
    built(global, args).map(Into::into)
}

/// Builds the RAM payload of the project in `args`' directory and returns
/// where it is.
pub fn payload(global: &Global, args: &BuildArgs) -> Result<PathBuf> {
    built(global, args).map(|built| PathBuf::from(built.output))
}

/// Where `args`' directory is a distribution checkout: the index of its
/// release, built first when `build`, else the last build's.
pub fn release(global: &Global, args: &BuildArgs, build: bool) -> Result<Option<PathBuf>> {
    let directory = args.directory.canonicalize().context(args.directory.display())?;
    let Some((checkout, _)) = project::find_release(&directory)? else {
        return Ok(None);
    };
    match build {
        true => built(global, args).map(|built| Some(PathBuf::from(built.output))),
        false => distro::latest(&checkout).map(Some),
    }
}

fn built(global: &Global, args: &BuildArgs) -> Result<Built> {
    orchard_build::catch_interrupts();
    let store = Store::open(global.cache.as_deref())?;
    let directory = args
        .directory
        .canonicalize()
        .context(args.directory.display())?;
    let cache = Components::open(store.dir())?;
    manage::fit(&cache, manage::budget(), &say);
    manage::check_free(&[cache.root(), &directory])?;
    // Held from before the recipes are chosen: what deletes recipes and
    // entries waits for it or leaves them alone.
    let _held = cache.lock(false, &say)?;
    let root = snapshot::resolve(global.data.as_deref(), &store, &say)?;
    let build = Build::new(&root, &store, &say)?;
    let attached = attached_board(global, &build.root);
    let choice = Choice {
        platforms: &args.platform,
        device: args.device.as_deref(),
        attached: attached.as_deref(),
    };
    let read = |path: &Path| std::fs::read_to_string(path).context(path.display());
    if let Some((checkout, release)) = project::find_release(&directory)? {
        let payload_only = [
            ("--kernel-dir", args.kernel_dir.is_some()),
            ("--pongo", args.pongo.is_some()),
            ("--busybox", args.busybox.is_some()),
            ("--loader", args.loader.is_some()),
        ];
        if let Some((flag, _)) = payload_only.iter().find(|(_, given)| *given) {
            return Err(Error::new(
                Code::InvalidInput,
                format!("{flag} is for RAM payloads; {} is a distribution checkout (orchard.toml [release])", checkout.display()),
            ));
        }
        let platforms = match release.selects_platform {
            true => Some(project::platforms(&checkout, &build.root, &choice)?),
            false if args.platform.is_empty() && args.device.is_none() => None,
            false => {
                return Err(Error::new(
                    Code::InvalidInput,
                    format!(
                        "{}'s build entry builds every platform and takes no --platform or --device; update the \
                         distribution (orchard.toml [release] selects-platform = true)",
                        release.name
                    ),
                ))
            }
        };
        let request = distro::Request {
            version: args.version.clone(),
            platforms,
            kernel_tree: project::kernel_tree(&checkout, args.kernel_tree.as_deref())?,
            packages: project::packages(&checkout, args.packages.as_deref())?,
        };
        let built = distro::build(&build, &checkout, &release, &request)?;
        return Ok(Built {
            kind: "release",
            output: built.index.display().to_string(),
            platform: None,
            boards: Vec::new(),
            version: Some(built.version),
            explain: if args.explain {
                Some(read(&built.commands)?)
            } else {
                None
            },
            commands: built.commands.display().to_string(),
        });
    }
    for (flag, given) in [
        ("--packages", args.packages.is_some()),
        ("--version", args.version.is_some()),
    ] {
        if given {
            return Err(Error::new(
                Code::InvalidInput,
                format!("{flag} is for distribution checkouts (orchard.toml [release])"),
            ));
        }
    }
    let given = Given {
        kernel: args.kernel_dir.clone(),
        busybox: args.busybox.clone(),
        loader: args.loader.clone(),
        pongo: args.pongo.clone(),
        kernel_tree: args.kernel_tree.clone(),
    };
    let built = ram::build_directory(&build, &directory, &choice, &given)?;
    Ok(Built {
        kind: "payload",
        output: built.output.display().to_string(),
        platform: Some(built.platform),
        boards: built.boards,
        version: None,
        explain: if args.explain {
            Some(read(&built.commands)?)
        } else {
            None
        },
        commands: built.commands.display().to_string(),
    })
}

/// Where an inputs document was written.
#[derive(Serialize)]
struct Resolved {
    output: String,
    platforms: Vec<String>,
}

impl Report for Resolved {
    fn human(&self) -> Vec<u8> {
        format!(
            "orchard inputs for {}: {}\n",
            self.platforms.join(", "),
            self.output
        )
        .into_bytes()
    }
}

pub fn inputs(global: &Global, args: &InputsArgs) -> Result<Output> {
    orchard_build::catch_interrupts();
    let store = Store::open(global.cache.as_deref())?;
    let _held = Components::open(store.dir())?.lock(false, &say)?;
    // Under `build`, the entry's orchard is the data the build was started with.
    let checkout = std::env::var_os("ORCHARD_CHECKOUT").map(PathBuf::from);
    let root = snapshot::resolve(global.data.as_deref().or(checkout.as_deref()), &store, &say)?;
    let build = Build::new(&root, &store, &say)?;
    let request = inputs::Request {
        platforms: args.platform.clone(),
        kernels: args.kernel_dir.iter().cloned().collect(),
        m1n1: args.m1n1.clone(),
        pongo: args.pongo.clone(),
        tools: args.tools.clone(),
        loaders: args.loader.iter().cloned().collect(),
        kernel_tree: args.kernel_tree.clone(),
    };
    let document = inputs::resolve(&build, &request)?;
    if let Some(parent) = args.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context(parent.display())?;
    }
    std::fs::write(&args.output, orchard_build::json::pretty(&document))
        .context(args.output.display())?;
    Ok(Resolved {
        output: args.output.display().to_string(),
        platforms: document["platforms"]
            .as_object()
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default(),
    }
    .into())
}

/// Where the published components' keys were written.
#[derive(Serialize)]
struct Keys {
    output: String,
    keys: serde_json::Value,
}

impl Report for Keys {
    fn human(&self) -> Vec<u8> {
        orchard_build::json::pretty(&self.keys).into_bytes()
    }
}

pub fn component_keys(global: &Global, output: &Path, platforms: &[String]) -> Result<Output> {
    let store = Store::open(global.cache.as_deref())?;
    let root = snapshot::resolve(global.data.as_deref(), &store, &say)?;
    let build = Build::new(&root, &store, &say)?;
    let platforms = match platforms.is_empty() {
        true => orchard_build::platform::platforms(&build.root, "arm64")?,
        false => platforms.to_vec(),
    };
    let keys = orchard_build::published::keys_document(&build, &platforms)?;
    std::fs::write(output, orchard_build::json::pretty(&keys)).context(output.display())?;
    Ok(Keys {
        output: output.display().to_string(),
        keys,
    }
    .into())
}
