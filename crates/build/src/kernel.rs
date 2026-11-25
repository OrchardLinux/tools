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

/// The producers' version in the data's recipes, which are updated apart
/// from this tool: `KERNEL_PRODUCER` in orchard's `src/orchard/build.py`.
/// Recipes newer than the tool are refused, since it would key and layer
/// what they make the old way.
pub fn producer(root: &Path) -> Result<u32> {
    let Ok(text) = std::fs::read_to_string(root.join("src/orchard/build.py")) else {
        return Ok(KERNEL_PRODUCER);
    };
    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("KERNEL_PRODUCER = "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(KERNEL_PRODUCER);
    if version > KERNEL_PRODUCER {
        return Err(Error::new(
            Code::Unsupported,
            format!("these recipes make kernels of version {version}; this orchard2 knows up to {KERNEL_PRODUCER}"),
        )
        .hint("update orchard2"));
    }
    Ok(version)
}

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
    let text = match std::fs::read_to_string(&exclude) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        read => read.context(exclude.display())?,
    };
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
        .hint(format!(
            "save the edits in {} elsewhere (git diff) and undo them, or build a kernel tree of your own with --kernel-tree",
            source.display()
        )));
    }
    if marker.is_some() && current == declared && baseline.is_none_or(|b| b.trim() != declared) {
        // An earlier build stopped between writing the two marks.
        write_text(&work.join("prepared-tree"), &format!("{declared}\n"))?;
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
    if baseline.exists()
        && read_text(&baseline)?.trim() != declared
        && working_tree(&source, true)? == declared
    {
        // An earlier build stopped between writing the two marks.
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
            "producer": producer(root)?,
            "tree": working_tree(tree, false)?,
            "mode": mode(),
        }),
        None => json!({
            "platform": described.platform,
            "build": described.build_fields(false),
            "recipes": recipes::object(root, &[described.config.as_str(), toolchain[0], toolchain[1]])?,
            "producer": producer(root)?,
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
        "producer": producer(&build.root)?,
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::platform::tests::data;
    use crate::sources::tests::{upstream, PATCH};
    use orchard_cache::store::Store;
    use std::os::unix::fs::PermissionsExt;

    const SECOND_PATCH: &str = "--- a/Makefile\n+++ b/Makefile\n@@ -1 +1,2 @@\n all:\n+\t@true\n";

    /// The kernel producers, reduced to what the layering sees: a build
    /// that turns the source into an image, device trees, a module and the
    /// files of an SDK, and a staging step that collects the modules. Each
    /// appends its name to `ran`.
    fn producers(root: &Path) {
        let script = |path: &str, text: &str| {
            std::fs::write(root.join(path), text).unwrap();
            std::fs::set_permissions(root.join(path), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        };
        script(
            "kernel/build",
            r#"#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
shift
tree=
while [ $# -gt 0 ]; do
  case $1 in
    --resume) shift;;
    --work-dir) work=$2; shift 2;;
    --kernel-tree) tree=$2; shift 2;;
    --output) out=$2; shift 2;;
  esac
done
echo build >> "$here/ran"
test "$ORCHARD_KERNEL_WORK_LOCKED" = "$work"
source=${tree:-$work/source}
build=$work/output
mkdir -p "$out/dtbs" "$build/include/config" "$build/include/generated" "$build/scripts/basic" \
  "$build/scripts/mod" "$build/arch/arm64/include" "$build/arch/arm64/boot/dts" "$build/drivers"
cat "$source/file.c" "$source/Makefile" > "$out/Image.gz"
echo 6.0-test > "$out/kernel.release"
echo tree > "$out/dtbs/t32784-aa1.dtb"
echo tree > "$out/dtbs/t32789-bb2.dtb"
echo 6.0-test > "$build/include/config/kernel.release"
cp "$here/configs/wide/config" "$build/.config"
for file in Makefile Module.symvers modules.builtin modules.builtin.modinfo modules.order \
    include/config/auto.conf include/generated/autoconf.h include/generated/utsrelease.h \
    scripts/basic/fixdep scripts/mod/modpost arch/arm64/include/header arch/arm64/boot/dts/x; do
  echo "$file" > "$build/$file"
done
echo module > "$build/drivers/in-tree.ko"
"#,
        );
        script(
            "kernel/stage-modules.py",
            r#"import pathlib, shutil, sys
args = sys.argv[1:]
build = pathlib.Path(args[args.index('--build') + 1])
output = pathlib.Path(args[args.index('--output') + 1])
target = output / 'lib/modules/6.0-test/kernel'
with open(pathlib.Path(__file__).parent / 'ran', 'a') as log:
    log.write('stage\n')
sources = [build] + [pathlib.Path(args[i + 1]) for i, a in enumerate(args) if a == '--external']
for source in sources:
    for module in sorted(source.rglob('*.ko')):
        place = target / module.relative_to(source)
        place.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(module, place)
(target.parent / 'modules.dep').write_text('')
"#,
        );
        script(
            "kernel/build-modules",
            "#!/bin/sh\nset -eu\nhere=$(cd \"$(dirname \"$0\")\" && pwd)\necho modules >> \"$here/ran\"\n\
             while [ $# -gt 0 ]; do case $1 in --output) out=$2; shift 2;; *) shift;; esac; done\n\
             mkdir -p \"$out\"\necho external > \"$out/external.ko\"\n",
        );
        std::fs::create_dir_all(root.join("producers")).unwrap();
        script("producers/buildenv.py", "");
        script("kernel/Containerfile", "FROM scratch\n");
        script("kernel/install-toolchain", "true\n");
    }

    pub struct Fixture {
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        pub root: PathBuf,
        pub store: Store,
    }

    /// Data with the stub producers, an upstream repository and one real
    /// patch in the series; `None` where no Python runs the producers.
    pub fn fixture() -> Option<Fixture> {
        crate::python().ok()?;
        let (data, other) = (data(), tempfile::tempdir().unwrap());
        let root = data.path().canonicalize().unwrap();
        let (url, commit) = upstream(other.path());
        std::fs::write(
            root.join("kernel/upstream.lock"),
            format!("url={url}\ncommit={commit}\n"),
        )
        .unwrap();
        for stale in [
            "common/0002-b.patch",
            "common/sub/0000-z.patch",
            "boards/x/0001-x.patch",
        ] {
            std::fs::remove_file(root.join("kernel").join(stale)).unwrap();
        }
        std::fs::write(root.join("kernel/common/0001-a.patch"), PATCH).unwrap();
        producers(&root);
        let store = Store::open(Some(&other.path().join("cache"))).unwrap();
        Some(Fixture {
            _dirs: (data, other),
            root,
            store,
        })
    }

    fn quiet(_: &str) {}

    pub fn ran(root: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(root.join("kernel/ran")).unwrap_or_default();
        std::fs::write(root.join("kernel/ran"), "").unwrap();
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn recipes_newer_than_the_tool_are_refused() {
        let data = tempfile::tempdir().unwrap();
        assert_eq!(producer(data.path()).unwrap(), KERNEL_PRODUCER);
        let script = data.path().join("src/orchard/build.py");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        let write = |n: u32| {
            std::fs::write(
                &script,
                format!("import os\nKERNEL_PRODUCER = {n}  # why\n"),
            )
            .unwrap()
        };
        write(KERNEL_PRODUCER);
        assert_eq!(producer(data.path()).unwrap(), KERNEL_PRODUCER);
        write(2);
        assert_eq!(producer(data.path()).unwrap(), 2);
        write(KERNEL_PRODUCER + 1);
        assert_eq!(producer(data.path()).unwrap_err().code, Code::Unsupported);
    }

    #[test]
    fn a_kernel_is_built_once_per_platform_and_layered_per_board() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let first = prepare(&build, "aa1", None).unwrap();
        assert_eq!(ran(&fixture.root), ["build", "stage", "stage"]);
        // The source has the series applied; the image is made from it.
        assert_eq!(
            std::fs::read_to_string(first.join("Image.gz")).unwrap(),
            "int a;\nint b;\nall:\n"
        );
        assert!(first.join("aa1.dtb").is_file() && first.join("dtbs/t32789-bb2.dtb").is_file());
        assert!(first
            .join("lib/modules/6.0-test/kernel/drivers/in-tree.ko")
            .is_file());

        // The same again runs nothing; another board of the platform
        // reuses the compiled base and only stages its own layer.
        assert_eq!(prepare(&build, "aa1", None).unwrap(), first);
        assert!(ran(&fixture.root).is_empty());
        let second = prepare(&build, "bb2", None).unwrap();
        assert_eq!(ran(&fixture.root), ["stage"]);
        assert!(second.join("bb2.dtb").is_file());
        let names: Vec<String> = build.cache.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["kernel-aa1", "kernel-base-wide", "kernel-bb2"]);

        // A board's external modules are built against the base and are
        // part of its layer only.
        std::fs::create_dir_all(fixture.root.join("kernel/modules/bb2")).unwrap();
        std::fs::write(fixture.root.join("kernel/modules/bb2/probe.c"), "").unwrap();
        let with_modules = prepare(&build, "bb2", None).unwrap();
        assert_eq!(ran(&fixture.root), ["modules", "stage"]);
        assert!(with_modules
            .join("lib/modules/6.0-test/kernel/external.ko")
            .is_file());
        assert_eq!(prepare(&build, "aa1", None).unwrap(), first);
    }

    #[test]
    fn a_changed_series_moves_the_tree_and_edits_block_it() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let first = prepare(&build, "aa1", None).unwrap();
        let source = work_directory(&build, "aa1", None).unwrap().join("source");
        let untouched = std::fs::metadata(source.join("file.c"))
            .unwrap()
            .modified()
            .unwrap();
        ran(&fixture.root);

        // A patch more: the tree gets the difference, and files the new
        // patch does not touch keep their times for the compiler.
        std::fs::write(
            fixture.root.join("kernel/common/0002-b.patch"),
            SECOND_PATCH,
        )
        .unwrap();
        let second = prepare(&build, "aa1", None).unwrap();
        assert_ne!(first, second);
        assert_eq!(ran(&fixture.root), ["build", "stage", "stage"]);
        assert_eq!(
            std::fs::read_to_string(second.join("Image.gz")).unwrap(),
            "int a;\nint b;\nall:\n\t@true\n"
        );
        assert_eq!(
            std::fs::metadata(source.join("file.c"))
                .unwrap()
                .modified()
                .unwrap(),
            untouched
        );

        // Back to the first series: its kernel is still in the cache.
        std::fs::remove_file(fixture.root.join("kernel/common/0002-b.patch")).unwrap();
        assert_eq!(prepare(&build, "aa1", None).unwrap(), first);
        assert!(ran(&fixture.root).is_empty());

        // A local edit is part of the key, and is never discarded: with it
        // in the tree, a series change is refused.
        prepare(&build, "aa1", None).unwrap();
        std::fs::write(
            fixture.root.join("kernel/common/0002-b.patch"),
            SECOND_PATCH,
        )
        .unwrap();
        prepare(&build, "aa1", None).unwrap();
        ran(&fixture.root);
        std::fs::write(source.join("file.c"), "int a;\nint b;\nint edit;\n").unwrap();
        let edited = prepare(&build, "aa1", None).unwrap();
        assert!(std::fs::read_to_string(edited.join("Image.gz"))
            .unwrap()
            .contains("int edit;"));
        std::fs::remove_file(fixture.root.join("kernel/common/0002-b.patch")).unwrap();
        let error = prepare(&build, "aa1", None).unwrap_err();
        assert!(error.message.contains("local edits"), "{error}");
        assert!(std::fs::read_to_string(source.join("file.c"))
            .unwrap()
            .contains("int edit;"));
    }

    #[test]
    fn a_developers_tree_is_built_as_it_is_inside_itself() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (url, _) = upstream(dir.path());
        let tree = PathBuf::from(url.trim_start_matches("file://"));
        assert!(check_tree(&build, "aa1", &tree)
            .unwrap_err()
            .message
            .contains("is not a kernel tree"));
        std::fs::create_dir_all(tree.join("arch/arm64/configs")).unwrap();
        std::fs::write(
            tree.join("arch/arm64/configs/orchard_wide_defconfig"),
            "CONFIG_X=y\n",
        )
        .unwrap();
        let tree = check_tree(&build, "aa1", &tree).unwrap();

        let first = prepare(&build, "aa1", Some(&tree)).unwrap();
        assert_eq!(
            std::fs::read_to_string(first.join("Image.gz")).unwrap(),
            "int a;\nall:\n"
        );
        assert!(tree.join(OUT).join("wide/output/.config").is_file());
        // Its output is not part of its identity, so nothing is built again.
        assert_eq!(prepare(&build, "aa1", Some(&tree)).unwrap(), first);
        assert_eq!(ran(&fixture.root), ["build", "stage", "stage"]);
        // It is cached apart from orchard's own kernel, and an edit is a new kernel.
        let names: Vec<String> = build.cache.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["kernel-tree-aa1", "kernel-tree-base-wide"]);
        std::fs::write(tree.join("file.c"), "int changed;\n").unwrap();
        let second = prepare(&build, "aa1", Some(&tree)).unwrap();
        assert_eq!(
            std::fs::read_to_string(second.join("Image.gz")).unwrap(),
            "int changed;\nall:\n"
        );
    }
}
