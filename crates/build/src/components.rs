//! The components a payload is made of besides the kernel: orchard's m1n1,
//! Pongo, the static recovery tools and a 32-bit board's Cascadia loader.
//! Each is taken finished where orchard publishes it for this state, and
//! otherwise built by its recipe and kept in the cache.
use crate::platform::descriptor;
use crate::{invalid, json, kernel, published, recipes, Build, RELEASES};
use orchard_cache::component::Key;
use orchard_cache::{Code, Context, Error, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The static tools a tools directory holds.
pub const TOOLS: [&str; 10] = [
    "busybox",
    "dropbear",
    "dropbearkey",
    "wpa_supplicant",
    "wpa_cli",
    "iw",
    "orchard-write",
    "orchard-install-usb",
    "resize2fs",
    "tune2fs",
];

/// What Pongo is built from.
const PONGO_RECIPE: [&str; 2] = ["boot/quiet/build-pongo.py", "boot/quiet/pongo.patch"];

/// What orchard's m1n1 (the quiet one every device boots) is built from.
pub fn m1n1_inputs(build: &Build) -> Vec<String> {
    vec![
        "boot/m1n1".into(),
        "boot/quiet/m1n1.patch".into(),
        build.producer_path("buildenv.py"),
    ]
}

/// What the static tools are built from; the rest of the recovery
/// directory is recovery runtime and does not change them.
pub fn tool_inputs(build: &Build) -> Vec<String> {
    let mut inputs: Vec<String> = [
        "build-tools",
        "build-tools-in-container",
        "Containerfile",
        "Containerfile.arm",
        "install-toolchain",
        "upstream.lock",
        "check-tools.py",
        "orchard-write.c",
        "orchard-install-usb.c",
    ]
    .iter()
    .map(|name| format!("boot/initramfs/recovery/{name}"))
    .collect();
    inputs.push(build.producer_path("buildenv.py"));
    inputs
}

fn key_of(build: &Build, inputs: &[String]) -> Result<String> {
    let paths: Vec<&str> = inputs.iter().map(String::as_str).collect();
    recipes::key(&build.root, &paths, recipes::IGNORE)
}

/// The static recovery tools for `architecture`, built here.
fn build_tools(build: &Build, architecture: &str) -> Result<PathBuf> {
    let name = match architecture {
        "arm64" => "recovery-tools".to_owned(),
        "arm" => "recovery-tools-arm".to_owned(),
        other => {
            return Err(invalid(format!(
                "unsupported recovery architecture: {other}"
            )))
        }
    };
    let mut key = key_of(build, &tool_inputs(build))?;
    if architecture != "arm64" {
        key = json::sha256(format!("{key}:{architecture}").as_bytes());
    }
    let recipe = build.root.join("boot/initramfs/recovery");
    build.component(&name, &mut || Ok(Key::plain(&key)), &mut |stage| {
        let work = build.cache.recipe_work(&name, &key)?;
        if !work.join("bin/busybox").exists() {
            build.command(
                &[
                    recipe.join("build-tools").as_os_str(),
                    "--arch".as_ref(),
                    architecture.as_ref(),
                ],
                &[("OMA_RECOVERY_TOOLS_WORK", work.as_os_str())],
            )?;
        }
        let tools: Vec<PathBuf> = TOOLS
            .iter()
            .map(|tool| work.join("bin").join(tool))
            .collect();
        let mut args: Vec<&OsStr> = vec!["--arch".as_ref(), architecture.as_ref()];
        args.extend(tools.iter().map(|t| t.as_os_str()));
        build.python(&recipe.join("check-tools.py"), &args)?;
        for item in std::fs::read_dir(work.join("bin")).context(work.display())? {
            let tool = item?.path();
            let meta = std::fs::symlink_metadata(&tool).context(tool.display())?;
            if meta.is_file() {
                use std::os::unix::fs::PermissionsExt;
                let target = stage.join(tool.file_name().expect("a directory entry has a name"));
                std::fs::copy(&tool, &target).context(tool.display())?;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    })
}

/// The directory of static recovery tools for the platform of `device`:
/// the nightly's when it is of this state, else built or reused.
pub fn tools(build: &Build, device: &str) -> Result<PathBuf> {
    let architecture = descriptor(&build.root, device)?.arch;
    if architecture == "arm64" {
        if let Some(tools) = published::tools(build)? {
            return Ok(tools);
        }
    }
    build_tools(build, &architecture)
}

/// A released Pongo: where it is, which release that is, and its SHA-256.
struct Released {
    url: String,
    release: String,
    checksum: String,
}

/// The newest release's Pongo, when this data's recipe is that release's.
///
/// Pongo needs macOS with Xcode and the iPhoneOS SDK to build and is the
/// same for every device, so an unchanged recipe uses the released binary.
/// A checkout says whether its recipe changed since its newest release tag;
/// data without history is a published state, whose Pongo is the newest
/// release's.
fn released_pongo(build: &Build) -> Option<Released> {
    let root = &build.root;
    let (base, release) = if root.join(".git").exists() {
        let tag = crate::sources::git(
            root,
            &["describe", "--tags", "--abbrev=0", "--match", "v*"],
            &[],
        )
        .ok()?;
        let mut unchanged = vec!["diff", "--quiet", &tag, "--"];
        unchanged.extend(PONGO_RECIPE);
        if !crate::sources::git_ok(root, &unchanged) {
            return None;
        }
        (format!("{RELEASES}/{tag}"), tag)
    } else {
        (
            "https://github.com/OrchardLinux/orchard/releases/latest/download".to_owned(),
            "the newest release".to_owned(),
        )
    };
    let text = published::fetch(&format!("{base}/SHA256SUMS"), 1 << 20).ok()?;
    let checksum = published::sums(&String::from_utf8_lossy(&text)).remove("pongo.bin")?;
    Some(Released {
        url: format!("{base}/pongo.bin"),
        release,
        checksum,
    })
}

/// Pongo, released or built here: `pongo.bin`.
pub fn pongo(build: &Build) -> Result<PathBuf> {
    if let Some(released) = released_pongo(build) {
        let Released {
            url,
            release,
            checksum,
        } = &released;
        let entry = build.component(
            "pongo",
            &mut || Ok(Key::plain(format!("release-{checksum}"))),
            &mut |stage| {
                build.say(format!(
                    "Using the released Pongo of {release}; its recipe is unchanged"
                ));
                build
                    .explain
                    .note(format!("Pongo: downloaded from {url} (sha256 {checksum})"));
                let file = build.store.fetch(url, checksum, 64 << 20)?;
                crate::link_or_copy(&file, &stage.join("pongo.bin"))
            },
        )?;
        return Ok(entry.join("pongo.bin"));
    }
    let recipe: Vec<String> = PONGO_RECIPE.iter().map(|p| (*p).to_owned()).collect();
    let key = key_of(build, &recipe)?;
    let entry = build.component(
        "pongo",
        &mut || Ok(Key::plain(&key)),
        &mut |stage| {
            let xcode = std::process::Command::new("xcrun").arg("--version").output().is_ok_and(|o| o.status.success());
            if !cfg!(target_os = "macos") || !xcode {
                return Err(Error::new(
                    Code::Unsupported,
                    "Pongo's recipe (boot/quiet/build-pongo.py, pongo.patch) differs from the latest release, \
                     and building it needs macOS with Xcode and the iPhoneOS SDK",
                )
                .hint("pass --pongo FILE to use another, or build on a Mac"));
            }
            let work = build.cache.recipe_work("pongo", &key)?;
            let output = stage.join("pongo.bin");
            let mut args: Vec<&OsStr> = vec!["--work-dir".as_ref(), work.as_os_str(), "--output".as_ref(), output.as_os_str()];
            if work.join("source").exists() {
                args.push("--resume".as_ref());
            }
            build.python(&build.root.join("boot/quiet/build-pongo.py"), &args)
        },
    )?;
    Ok(entry.join("pongo.bin"))
}

/// orchard's m1n1, built here: `m1n1.bin`. One m1n1 starts every Apple
/// device; the board only picks its device tree.
pub fn build_m1n1(build: &Build) -> Result<PathBuf> {
    let root = &build.root;
    if !has_patches(&root.join("boot/m1n1/patches")) {
        return Err(Error::new(
            Code::NotFound,
            "the data has no m1n1 patches (boot/m1n1/patches)",
        )
        .hint("supply --loader FILE"));
    }
    let inputs = m1n1_inputs(build);
    let entry = build.component(
        "m1n1",
        &mut || Ok(Key::plain(key_of(build, &inputs)?)),
        &mut |stage| {
            // One tree per patch set: a changed series starts a fresh tree
            // instead of resuming one the recipe would refuse. Crates are shared.
            let work = build.cache.recipe_work("m1n1", &key_of(build, &inputs)?)?;
            let (output, cargo) = (
                stage.join("m1n1.bin"),
                build.cache.work().join(".cargo-m1n1"),
            );
            let (builder, patch) = (
                root.join("boot/m1n1/build"),
                root.join("boot/quiet/m1n1.patch"),
            );
            // Quiet, as CI builds it: black, no m1n1 logo, until m1n1 paints the system's splash.
            let mut argv: Vec<&OsStr> = vec![
                builder.as_os_str(),
                "apple".as_ref(),
                "--output".as_ref(),
                output.as_os_str(),
                "--work-dir".as_ref(),
                work.as_os_str(),
                "--cargo-dir".as_ref(),
                cargo.as_os_str(),
                "--extra-patch".as_ref(),
                patch.as_os_str(),
                "--extra-cflags".as_ref(),
                "-DQUIET_BOOT".as_ref(),
            ];
            if work.join("applied-patches").exists() {
                argv.push("--resume".as_ref());
            } else if work.exists() {
                // An earlier attempt stopped before its patches were applied.
                std::fs::remove_dir_all(&work).context(work.display())?;
            }
            build.command(&argv, &[])
        },
    )?;
    Ok(entry.join("m1n1.bin"))
}

fn has_patches(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|items| {
        items.flatten().any(|item| {
            let path = item.path();
            path.extension().is_some_and(|e| e == "patch") || (path.is_dir() && has_patches(&path))
        })
    })
}

/// A board's profile for Cascadia's loader.
pub fn cascadia_profile(root: &Path, board: &str) -> Result<PathBuf> {
    let path = root.join("devices").join(board).join("cascadia.toml");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(path),
        _ => Err(invalid(format!("{board} has no cascadia handoff profile"))),
    }
}

/// A 32-bit board's Cascadia loader, built for its profile: `loader.bin`.
pub fn cascadia_loader(build: &Build, board: &str) -> Result<PathBuf> {
    let root = &build.root;
    let profile = cascadia_profile(root, board)?;
    let inputs = vec![
        "boot/cascadia".to_owned(),
        build.producer_path("cascadia.py"),
        profile
            .strip_prefix(root)
            .unwrap_or(&profile)
            .to_string_lossy()
            .into_owned(),
        build.producer_path("buildenv.py"),
    ];
    let entry = build.component(
        &format!("loader-cascadia-{board}"),
        &mut || Ok(Key::plain(key_of(build, &inputs)?)),
        &mut |stage| {
            let builder = root.join("boot/cascadia/build");
            build.command(
                &[
                    builder.as_os_str(),
                    "--profile".as_ref(),
                    profile.as_os_str(),
                    "--output".as_ref(),
                    stage.as_os_str(),
                ],
                &[],
            )
        },
    )?;
    Ok(entry.join("loader.bin"))
}

/// Components given on the command line; each bypasses its producer.
#[derive(Debug, Clone, Default)]
pub struct Given {
    /// A staged kernel, device tree and module tree.
    pub kernel: Option<PathBuf>,
    pub busybox: Option<PathBuf>,
    pub loader: Option<PathBuf>,
    pub pongo: Option<PathBuf>,
    /// A developer's kernel tree to build the kernel from.
    pub kernel_tree: Option<PathBuf>,
}

/// What a payload is assembled from.
#[derive(Debug, Clone)]
pub struct Components {
    pub kernel: PathBuf,
    pub busybox: PathBuf,
    pub loader: PathBuf,
    /// None for boards Cascadia's loader starts.
    pub pongo: Option<PathBuf>,
}

/// The components of a payload for `device`, built or reused.
pub fn prepare(build: &Build, device: &str, given: &Given) -> Result<Components> {
    let described = descriptor(&build.root, device)?;
    if given.kernel.is_some() && given.kernel_tree.is_some() {
        return Err(invalid(
            "give a staged kernel (--kernel-dir) or a kernel tree (--kernel-tree), not both",
        ));
    }
    let cascadia = described.loader == "cascadia";
    if given.pongo.is_some() && cascadia {
        return Err(invalid(
            "Cascadia payloads use recovery-autogo firmware stages",
        ));
    }
    let tree = match &given.kernel_tree {
        Some(tree) => Some(kernel::check_tree(build, device, tree)?),
        None => None,
    };
    let absolute = |path: &PathBuf| path.canonicalize().context(path.display());
    let pongo = match (&given.pongo, cascadia) {
        (Some(pongo), _) => Some(absolute(pongo)?),
        (None, true) => None,
        (None, false) => Some(pongo(build)?),
    };
    let kernel = match &given.kernel {
        Some(kernel) => absolute(kernel)?,
        None => kernel::prepare(build, device, tree.as_deref())?,
    };
    let busybox = match &given.busybox {
        Some(busybox) => absolute(busybox)?,
        None => build_tools(build, &described.arch)?.join("busybox"),
    };
    let loader = match (&given.loader, cascadia) {
        (Some(loader), _) => absolute(loader)?,
        (None, true) => cascadia_loader(build, device)?,
        (None, false) => build_m1n1(build)?,
    };
    Ok(Components {
        kernel,
        busybox,
        loader,
        pongo,
    })
}

/// Pongo, orchard's m1n1 and the static tools, which every arm64 platform
/// shares: `(pongo, m1n1, tools directory)`.
pub fn shared(build: &Build) -> Result<(PathBuf, PathBuf, PathBuf)> {
    // The nightly's m1n1 and tools when built from the same recipes; built here otherwise.
    let loader = match published::m1n1(build)? {
        Some(loader) => loader,
        None => build_m1n1(build)?,
    };
    let tools = match published::tools(build)? {
        Some(tools) => tools,
        None => build_tools(build, "arm64")?,
    };
    Ok((pongo(build)?, loader, tools))
}
