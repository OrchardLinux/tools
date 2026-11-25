//! Kernel source preparation and the two cached kernel layers.
//!
//! orchard builds one kernel per platform. Its source is the platform's
//! upstream base with its patch series applied, kept as an editable tree in
//! the cache (`kernel-work/PLATFORM-COMMIT`) together with the compiler's
//! output, so a changed series compiles only what changed. A developer's own
//! tree (`--kernel-tree`) is built as it is, inside itself.
//!
//! The base layer is the compiled kernel, its in-tree modules and what
//! external modules build against; the board layer adds the board's
//! external modules and picks its device tree.
use crate::platform::{descriptor, layout, patches, pin};
use crate::recipes::{self, keyed};
use crate::sources::{self, git, git_ok, git_path, working_tree};
use crate::{copy_file, copy_tree, invalid, read_text, write_text, Build};
use orchard_cache::component::{lock, Key, Lock};
use orchard_cache::{Code, Context, Error, Result};
use serde_json::json;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The version of the kernel producers: `kernel/build`,
/// `kernel/build-modules`, `kernel/stage-modules.py`, `kernel/series.py`,
/// the build environment and the layering here. Their text is not part of
/// the key, so a refactor rebuilds nothing; raise this when a change alters
/// what they produce. It has to be orchard's number: CI publishes kernels
/// under keys made with it.
pub const KERNEL_PRODUCER: u32 = 3; // 3: strip module debug sections from staged outputs

/// A developer kernel tree's build output, inside the tree.
pub const OUT: &str = ".orchard-out";

const TOOLCHAIN_IMAGE: &str = "localhost/orchard-hardware-kernel-toolchain:bookworm";

/// The tree a platform's kernel is compiled in: orchard's own in the cache,
/// one per platform and upstream base, or `tree/.orchard-out/PLATFORM` for a
/// developer's tree. Never moved or reset: it holds editable source and
/// compiler output.
pub fn work_directory(build: &Build, device: &str, tree: Option<&Path>) -> Result<PathBuf> {
    let platform = layout(&build.root, device)?.0;
    if let Some(tree) = tree {
        return Ok(tree.join(OUT).join(platform));
    }
    let commit = pin(&build.root, device)?.1;
    Ok(build
        .cache
        .kernel_work()
        .join(format!("{platform}-{}", &commit[..commit.len().min(12)])))
}

/// A developer's kernel tree: a git checkout with the platform's defconfig.
/// Its git is told to ignore the build output inside it, so builds never
/// show in its status or its key.
pub fn check_tree(build: &Build, device: &str, tree: &Path) -> Result<PathBuf> {
    let described = descriptor(&build.root, device)?;
    let tree = tree.canonicalize().map_err(|e| {
        Error::from(e)
            .context(tree.display())
            .hint("the kernel tree is not there; if it is on a disk image, attach it first")
    })?;
    let defconfig = format!("arch/{}/configs/{}", described.arch, described.defconfig);
    if !tree.join("Makefile").is_file() || !tree.join(&defconfig).is_file() {
        return Err(invalid(format!(
            "{} is not a kernel tree with {defconfig} (the {} defconfig)",
            tree.display(),
            described.platform
        )));
    }
    if !git_ok(&tree, &["rev-parse", "--git-dir"]) {
        return Err(invalid(format!(
            "{} is not a git checkout; a kernel tree is identified by its content",
            tree.display()
        )));
    }
    let exclude = git_path(&tree, "info/exclude")?;
    let line = format!("/{OUT}/");
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !text.lines().any(|l| l == line) {
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent).context(parent.display())?;
        }
        let gap = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        write_text(&exclude, &format!("{text}{gap}{line}\n"))?;
    }
    Ok(tree)
}

/// Exclusive use of a kernel tree, shared with `kernel/build` run by
/// anyone: it takes the same lock unless `ORCHARD_KERNEL_WORK_LOCKED` names
/// its tree, which the producers run under this lock are told.
pub struct TreeLock {
    pub work: PathBuf,
    _lock: Lock,
}

impl TreeLock {
    pub fn take(build: &Build, device: &str, tree: Option<&Path>) -> Result<TreeLock> {
        let work = work_directory(build, device, tree)?;
        std::fs::create_dir_all(&work).context(work.display())?;
        let held = lock(&work.join(".lock"), true, &|| {
            build.say(format!(
                "Waiting for another kernel build to finish with {} ...",
                work.display()
            ));
        })?;
        Ok(TreeLock { work, _lock: held })
    }

    fn env(&self) -> [(&'static str, &OsStr); 1] {
        [("ORCHARD_KERNEL_WORK_LOCKED", self.work.as_os_str())]
    }
}

/// The series' patches in order, and the marker `kernel/build` writes for
/// them: each patch's blob id and its path under `kernel/`.
fn patch_identity(build: &Build, device: &str) -> Result<(Vec<PathBuf>, Vec<u8>)> {
    let patches = patches(&build.root, device)?;
    let mut marker = Vec::new();
    if !patches.is_empty() {
        // One git process for the series; it prints the ids in argument order.
        let ids = crate::output(
            std::process::Command::new("git")
                .arg("hash-object")
                .args(&patches),
        )?;
        let ids = String::from_utf8_lossy(&ids);
        let kernel = build.root.join("kernel");
        for (id, patch) in ids.split_whitespace().zip(&patches) {
            let relative = patch.strip_prefix(&kernel).unwrap_or(patch);
            marker.extend(format!("{id} {}\n", relative.display()).into_bytes());
        }
    }
    Ok((patches, marker))
}

fn check_origin(source: &Path, url: &str, commit: &str) -> Result<()> {
    if git(source, &["rev-parse", "HEAD"], &[])? != commit
        || git(source, &["remote", "get-url", "origin"], &[])? != url
    {
        return Err(invalid(
            "local kernel revision or remote differs from the upstream lock; sources were not reset",
        ));
    }
    Ok(())
}

/// The tree orchard's kernel for `device` is to be built from, found before
/// the cache is looked at and without changing the source.
///
/// A clean tree still on an older series gives the new series' tree, so it
/// does not poison the key. Local edits take part in the key, or block a
/// series change when they cannot be told apart from it.
pub fn source_tree(build: &Build, device: &str) -> Result<String> {
    let (url, commit) = pin(&build.root, device)?;
    let work = work_directory(build, device, None)?;
    let source = work.join("source");
    let (patches, expected) = patch_identity(build, device)?;
    let declared = sources::declared_tree(
        &build.cache.git(),
        &url,
        &commit,
        &patches,
        Some(&source),
        build.say,
    )?;
    if !source.join(".git").exists() {
        return Ok(declared);
    }
    check_origin(&source, &url, &commit)?;
    let current = working_tree(&source, true)?;
    let marker = std::fs::read(work.join("applied-patches")).ok();
    let baseline = std::fs::read_to_string(work.join("prepared-tree")).ok();
    if marker.as_ref().is_some_and(|m| *m != expected) {
        if current == declared || baseline.is_some_and(|b| b.trim() == current) {
            return Ok(declared);
        }
        return Err(invalid(
            "kernel patches changed with local edits in the kernel tree; refusing to discard edits \
             or select an unrelated cached kernel",
        )
        .hint(format!("commit or remove the edits in {}", source.display())));
    }
    Ok(current)
}

/// Orchard's kernel source for `device`, checked out and on the current
/// series: `(source, compiler output)`. A series change is applied as the
/// difference between the two trees, so unchanged files keep their times
/// and the compiler's objects stay valid.
fn prepare_source(build: &Build, device: &str) -> Result<(PathBuf, PathBuf)> {
    let (url, commit) = pin(&build.root, device)?;
    let work = work_directory(build, device, None)?;
    let source = work.join("source");
    std::fs::create_dir_all(work.join("output")).context(work.display())?;
    if !source.exists() {
        sources::checkout(&build.cache.git(), &url, &commit, &source, build.say)?;
    }
    check_origin(&source, &url, &commit)?;
    let (patches, expected) = patch_identity(build, device)?;
    let marker = work.join("applied-patches");
    let baseline = work.join("prepared-tree");
    let declared = sources::declared_tree(
        &build.cache.git(),
        &url,
        &commit,
        &patches,
        Some(&source),
        build.say,
    )?;
    if marker.exists() {
        if std::fs::read(&marker).context(marker.display())? != expected {
            // source_tree refuses ambiguous transitions before any write.
            let desired = source_tree(build, device)?;
            let current = working_tree(&source, true)?;
            if current != desired {
                let delta = {
                    let (bare, _held) =
                        sources::repository(&build.cache.git(), &url, &commit, None, build.say)?;
                    // The current tree's objects live in the source: a series
                    // built by kernel/build alone was never declared in the
                    // shared store.
                    let objects = git_path(&source, "objects")?;
                    crate::output(
                        std::process::Command::new("git")
                            .arg("-C")
                            .arg(&bare)
                            .args(["diff", "--binary", &current, &desired])
                            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects),
                    )?
                };
                for check in [true, false] {
                    apply(&source, &delta, check)?;
                }
                if working_tree(&source, true)? != desired {
                    return Err(Error::new(
                        Code::Build,
                        "kernel patch transition did not give the declared tree; the compiler was not started",
                    ));
                }
            }
            std::fs::write(&marker, &expected).context(marker.display())?;
            write_text(&baseline, &format!("{declared}\n"))?;
        }
    } else {
        for patch in &patches {
            let patch = patch.as_os_str();
            if !git_ok(
                &source,
                &[
                    OsStr::new("apply"),
                    "--reverse".as_ref(),
                    "--check".as_ref(),
                    patch,
                ],
            ) {
                git(
                    &source,
                    &[OsStr::new("apply"), "--check".as_ref(), patch],
                    &[],
                )?;
                git(&source, &[OsStr::new("apply"), patch], &[])?;
            }
        }
        std::fs::write(&marker, &expected).context(marker.display())?;
        write_text(&baseline, &format!("{declared}\n"))?;
    }
    Ok((source, work.join("output")))
}

/// Applies a binary diff to `source`, or only checks that it applies.
fn apply(source: &Path, delta: &[u8], check: bool) -> Result<()> {
    use std::io::Write;
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(source).arg("apply");
    if check {
        command.arg("--check");
    }
    let mut child = command
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context("git"))?;
    child.stdin.take().expect("piped stdin").write_all(delta)?;
    if !child.wait()?.success() {
        return Err(Error::new(
            Code::Build,
            "the kernel patch transition does not apply to the kernel tree",
        ));
    }
    Ok(())
}

/// `(source, compiler output)`: orchard's patched tree, or the developer's
/// tree as it is.
fn source(build: &Build, device: &str, tree: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    match tree {
        None => prepare_source(build, device),
        Some(tree) => Ok((
            tree.to_owned(),
            work_directory(build, device, Some(tree))?.join("output"),
        )),
    }
}

/// What external modules and device tree overlays build against: a compact
/// copy of the compiler output's headers, scripts and symbol tables. It is
/// copied before use; make never writes into the cached one.
fn export_sdk(compiled: &Path, output: &Path, arch: &str) -> Result<()> {
    const REQUIRED: &[&str] = &[
        ".config",
        "Makefile",
        "Module.symvers",
        "modules.builtin",
        "modules.builtin.modinfo",
        "modules.order",
        "include/config/kernel.release",
        "include/config/auto.conf",
        "include/generated/autoconf.h",
        "include/generated/utsrelease.h",
        "scripts/basic/fixdep",
        "scripts/mod/modpost",
    ];
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|name| !compiled.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(Error::new(
            Code::Build,
            format!("missing module build prerequisites: {}", missing.join(", ")),
        ));
    }
    std::fs::create_dir_all(output).context(output.display())?;
    for name in [
        "include",
        "scripts",
        &format!("arch/{arch}/include"),
        &format!("arch/{arch}/boot/dts"),
    ] {
        copy_tree(&compiled.join(name), &output.join(name), true)?;
    }
    for name in [
        ".config",
        "Makefile",
        "Module.symvers",
        "modules.builtin",
        "modules.builtin.modinfo",
        "modules.order",
        ".version",
    ] {
        if compiled.join(name).is_file() {
            copy_file(&compiled.join(name), &output.join(name))?;
        }
    }
    Ok(())
}

fn release(directory: &Path) -> Result<String> {
    let release = read_text(&directory.join("kernel.release"))?
        .trim()
        .to_owned();
    if release.is_empty() || release.contains('/') || release.split_whitespace().count() != 1 {
        return Err(Error::new(Code::Format, "invalid kernel.release").context(directory.display()));
    }
    Ok(release)
}

/// Fills the base layer at `output` with orchard's full kernel producer:
/// the kernel, every board's device tree, the in-tree modules and the SDK.
fn build_base(
    build: &Build,
    device: &str,
    output: &Path,
    tree: Option<&Path>,
    held: &TreeLock,
) -> Result<()> {
    let described = descriptor(&build.root, device)?;
    let (source, compiled) = source(build, device, tree)?;
    let identity = working_tree(&source, tree.is_none())?;
    let temporary = tempfile::Builder::new()
        .prefix("base-")
        .tempdir_in(&held.work)
        .context(held.work.display())?;
    let work = temporary.path();
    let mut argv: Vec<&OsStr> = Vec::new();
    let builder = build.root.join("kernel/build");
    let full = work.join("full");
    argv.extend([
        builder.as_os_str(),
        device.as_ref(),
        "--resume".as_ref(),
        "--work-dir".as_ref(),
        held.work.as_os_str(),
    ]);
    if tree.is_some() {
        argv.extend(["--kernel-tree".as_ref(), source.as_os_str()]);
    }
    argv.extend(["--output".as_ref(), full.as_os_str()]);
    build.command(&argv, &held.env())?;
    stage_modules(build, &compiled, None, &work.join("in-tree"), held)?;
    std::fs::create_dir_all(output).context(output.display())?;
    for name in [described.image.as_str(), "kernel.release"] {
        copy_file(&full.join(name), &output.join(name))?;
    }
    copy_tree(&full.join("dtbs"), &output.join("dtbs"), true)?;
    copy_tree(&work.join("in-tree/lib"), &output.join("lib"), true)?;
    export_sdk(&compiled, &output.join("sdk"), &described.arch)?;
    write_text(
        &output.join("inputs.json"),
        &format!(
            "{}\n",
            crate::json::dumps(&json!({ "source_tree": identity }))
        ),
    )?;
    if working_tree(&source, tree.is_none())? != identity {
        return Err(invalid(
            "kernel source changed during build; refusing result",
        ));
    }
    Ok(())
}

fn stage_modules(
    build: &Build,
    compiled: &Path,
    external: Option<&Path>,
    output: &Path,
    held: &TreeLock,
) -> Result<()> {
    let script = build.root.join("kernel/stage-modules.py");
    let mut argv: Vec<&OsStr> = vec![
        build.python.as_os_str(),
        script.as_os_str(),
        "--build".as_ref(),
        compiled.as_os_str(),
    ];
    if let Some(external) = external {
        argv.extend(["--external".as_ref(), external.as_os_str()]);
    }
    argv.extend(["--output".as_ref(), output.as_os_str()]);
    build.command(&argv, &held.env())
}

/// Fills a board's layer at `output` from the base: its device tree, and
/// its external modules built against a private copy of the base's SDK.
fn build_device(
    build: &Build,
    device: &str,
    base: &Path,
    output: &Path,
    tree: Option<&Path>,
    held: &TreeLock,
) -> Result<()> {
    let root = &build.root;
    let (platform, dtb) = layout(root, device)?;
    let described = descriptor(root, device)?;
    let (source, _) = source(build, device, tree)?;
    let identity = working_tree(&source, tree.is_none())?;
    let recorded: serde_json::Value = serde_json::from_str(&read_text(&base.join("inputs.json"))?)
        .map_err(|e| {
            Error::new(Code::Format, e.to_string()).context(base.join("inputs.json").display())
        })?;
    if recorded["source_tree"] != identity.as_str() {
        return Err(invalid(
            "cached kernel source identity differs; rebuild the base",
        ));
    }
    // A developer's tree brings its own defconfig; orchard's config is for its own tree.
    let config = |path: PathBuf| std::fs::read(&path).context(path.display());
    if tree.is_none() && config(base.join("sdk/.config"))? != config(root.join(&described.config))?
    {
        return Err(invalid(
            "cached kernel configuration differs; rebuild the base",
        ));
    }
    let release = release(base)?;
    if read_text(&base.join("sdk/include/config/kernel.release"))?.trim() != release {
        return Err(invalid("cached module SDK release differs"));
    }
    let temporary = tempfile::Builder::new()
        .prefix("modules-")
        .tempdir_in(&held.work)
        .context(held.work.display())?;
    let work = temporary.path();
    let sdk = work.join("output");
    copy_tree(&base.join("sdk"), &sdk, true)?;
    // The staging recipe scans the compiler-output tree, so the in-tree
    // modules go back to their places there and external ones can replace them.
    restore_modules(
        &base.join("lib/modules").join(&release).join("kernel"),
        &sdk,
    )?;
    build.python(
        &build.producer("buildenv.py"),
        &[
            OsStr::new("build"),
            "--file".as_ref(),
            root.join("kernel/Containerfile").as_os_str(),
            "--context".as_ref(),
            root.join("kernel").as_os_str(),
            "--image".as_ref(),
            TOOLCHAIN_IMAGE.as_ref(),
        ],
    )?;
    let modules = root.join("kernel/modules").join(device);
    let external = work.join("external");
    if modules.is_dir() {
        let localversion = format!("-orchard-{platform}");
        let builder = root.join("kernel/build-modules");
        build.command(
            &[
                builder.as_os_str(),
                "--source".as_ref(),
                source.as_os_str(),
                "--build".as_ref(),
                sdk.as_os_str(),
                "--modules".as_ref(),
                modules.as_os_str(),
                "--localversion".as_ref(),
                localversion.as_ref(),
                "--arch".as_ref(),
                described.arch.as_ref(),
                "--output".as_ref(),
                external.as_os_str(),
            ],
            &held.env(),
        )?;
    }
    stage_modules(
        build,
        &sdk,
        modules.is_dir().then_some(external.as_path()),
        &work.join("staged"),
        held,
    )?;
    std::fs::create_dir_all(output).context(output.display())?;
    for name in [described.image.as_str(), "kernel.release"] {
        copy_file(&base.join(name), &output.join(name))?;
    }
    copy_file(
        &base.join("dtbs").join(format!("{dtb}.dtb")),
        &output.join(format!("{device}.dtb")),
    )?;
    // Every board of the platform: an installed root serves all of them.
    copy_tree(&base.join("dtbs"), &output.join("dtbs"), true)?;
    copy_tree(&work.join("staged/lib"), &output.join("lib"), true)?;
    if working_tree(&source, tree.is_none())? != identity {
        return Err(invalid(
            "kernel source changed during module build; refusing result",
        ));
    }
    Ok(())
}

fn restore_modules(from: &Path, sdk: &Path) -> Result<()> {
    fn walk(directory: &Path, from: &Path, sdk: &Path) -> Result<()> {
        for item in std::fs::read_dir(directory).context(directory.display())? {
            let path = item?.path();
            let meta = std::fs::symlink_metadata(&path).context(path.display())?;
            if meta.is_dir() {
                walk(&path, from, sdk)?;
            } else if path.extension().is_some_and(|e| e == "ko") {
                if meta.is_symlink() {
                    return Err(Error::new(Code::Format, "link in cached in-tree modules")
                        .context(path.display()));
                }
                let target = sdk.join(path.strip_prefix(from).expect("under from"));
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).context(parent.display())?;
                }
                copy_file(&path, &target)?;
            }
        }
        Ok(())
    }
    if from.is_dir() {
        walk(from, from, sdk)?;
    }
    Ok(())
}

fn mode() -> String {
    let var =
        |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_owned());
    format!(
        "{}:{}",
        var("ORCHARD_BUILD_NATIVE", "0"),
        var("ORCHARD_BUILD_ENV_ID", "")
    )
}

/// What decides a platform's compiled kernel: the source tree, the config,
/// the descriptor's build fields, the toolchain recipes and the producer
/// version. Boards of one platform share it.
pub fn base_inputs(build: &Build, device: &str, tree: Option<&Path>) -> Result<serde_json::Value> {
    let root = &build.root;
    let described = descriptor(root, device)?;
    let toolchain = ["kernel/Containerfile", "kernel/install-toolchain"];
    Ok(match tree {
        // A developer's tree: its content, its own defconfig included.
        Some(tree) => json!({
            "platform": described.platform,
            "source": "kernel-tree",
            "build": described.build_fields(true),
            "recipes": recipes::object(root, &toolchain)?,
            "producer": KERNEL_PRODUCER,
            "tree": working_tree(tree, false)?,
            "mode": mode(),
        }),
        None => json!({
            "platform": described.platform,
            "build": described.build_fields(false),
            "recipes": recipes::object(root, &[described.config.as_str(), toolchain[0], toolchain[1]])?,
            "producer": KERNEL_PRODUCER,
            "tree": source_tree(build, device)?,
            "mode": mode(),
        }),
    })
}

/// What decides a board's layer on a base: its device tree and its
/// external modules.
pub fn device_inputs(build: &Build, device: &str, base_key: &str) -> Result<serde_json::Value> {
    let (platform, dtb) = layout(&build.root, device)?;
    Ok(json!({
        "base": base_key,
        "platform": platform,
        "dtb": dtb,
        "producer": KERNEL_PRODUCER,
        "recipes": recipes::object(&build.root, &[&format!("kernel/modules/{device}")])?,
    }))
}

/// A coherent kernel, device trees and modules for `device`, built or
/// reused: the `kernel` directory of its board layer. A developer's tree is
/// cached apart from orchard's own kernels, so it never displaces them.
/// The caller holds the cache's lock.
pub fn prepare(build: &Build, device: &str, tree: Option<&Path>) -> Result<PathBuf> {
    let root = &build.root;
    let described = descriptor(root, device)?;
    let platform = &described.platform;
    let prefix = if tree.is_some() {
        "kernel-tree-"
    } else {
        "kernel-"
    };
    match tree {
        None => build.explain.note(format!(
            "kernel source: orchard's {platform} tree, its upstream lock plus kernel/series/{platform}, \
             prepared in its cache.\nStandalone: OrchardLinux/linux branch {} regenerated from this orchard \
             commit has the same content (git rev-parse HEAD^{{tree}}: see kernel-base key-inputs.json \"tree\").",
            described.branch
        )),
        Some(tree) => build.explain.note(format!(
            "kernel source: the tree {} as it is, with arch/{}/configs/{}",
            tree.display(),
            described.arch,
            described.defconfig
        )),
    }
    let base = build
        .component(
            &format!("{prefix}base-{platform}"),
            &mut || -> Result<Key> { Ok(keyed(base_inputs(build, device, tree)?)) },
            &mut |stage| {
                let held = TreeLock::take(build, device, tree)?;
                build_base(build, device, &stage.join("kernel"), tree, &held)
            },
        )?
        .join("kernel");
    let base_key = base
        .parent()
        .and_then(Path::file_name)
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let layer = build.component(
        &format!("{prefix}{device}"),
        &mut || -> Result<Key> { Ok(keyed(device_inputs(build, device, &base_key)?)) },
        &mut |stage| {
            let held = TreeLock::take(build, device, tree)?;
            build_device(build, device, &base, &stage.join("kernel"), tree, &held)
        },
    )?;
    Ok(layer.join("kernel"))
}

/// Fails unless the data has what a kernel build of `device` needs.
pub fn check_recipe(build: &Build, device: &str) -> Result<()> {
    descriptor(&build.root, device).map(|_| ())
}
