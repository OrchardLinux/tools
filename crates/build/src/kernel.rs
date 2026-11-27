//! Kernel source preparation and the two cached kernel layers.
//!
//! orchard builds one kernel per platform. Its source is the platform's
//! lock's commit of OrchardLinux/linux, kept as an editable tree in
//! the cache (`kernel-work/PLATFORM-COMMIT`) together with the compiler's
//! output, so another commit compiles only what changed. A developer's own
//! tree (`--kernel-tree`) is built as it is, inside itself.
//!
//! The base layer is the compiled kernel, its in-tree modules and what
//! external modules build against; the board layer adds the board's
//! external modules and picks its device tree.
use crate::platform::{descriptor, layout, pin};
use crate::recipes::{self, keyed};
use crate::sources::{self, git, git_ok, git_path, working_tree};
use crate::{copy_file, copy_tree, invalid, read_text, write_text, Build};
use orchard_cache::component::{lock, Key, Lock};
use orchard_cache::{Code, Context, Error, Result};
use serde_json::json;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A developer kernel tree's build output, inside the tree.
pub const OUT: &str = ".orchard-out";

const TOOLCHAIN_IMAGE: &str = "localhost/orchard-hardware-kernel-toolchain:bookworm";

/// The tree a platform's kernel is compiled in: orchard's own in the cache,
/// one per platform, or `tree/.orchard-out/PLATFORM` for a developer's
/// tree. It holds editable source and compiler output: a new kernel commit
/// moves a clean tree in place, and never one with edits.
pub fn work_directory(build: &Build, device: &str, tree: Option<&Path>) -> Result<PathBuf> {
    let platform = layout(&build.root, device)?.0;
    if let Some(tree) = tree {
        return Ok(tree.join(OUT).join(platform));
    }
    Ok(build.cache.kernel_work().join(platform))
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

/// The origin the tree was fetched from has to be the lock's.
fn check_url(source: &Path, url: &str) -> Result<()> {
    if git(source, &["remote", "get-url", "origin"], &[])? != url {
        return Err(invalid(
            "the kernel tree's remote differs from the upstream lock; sources were not reset",
        ));
    }
    Ok(())
}

fn check_origin(source: &Path, url: &str, commit: &str) -> Result<()> {
    check_url(source, url)?;
    if git(source, &["rev-parse", "HEAD"], &[])? != commit {
        return Err(invalid(
            "local kernel revision differs from the upstream lock; sources were not reset",
        ));
    }
    Ok(())
}

/// Whether the tree in `work` holds nothing of anyone's: no edit, and no
/// commit, branch, stash or staged change made in it.
fn clean(work: &Path) -> Result<bool> {
    // On the commit orchard put it on: a commit made in it is someone's.
    let was = std::fs::read_to_string(work.join("applied-patches")).unwrap_or_default();
    let head = git(&work.join("source"), &["rev-parse", "HEAD"], &[])?;
    Ok((was.trim().is_empty() || was.trim() == head) && crate::manage::is_clean(work))
}

fn edited(source: &Path) -> Error {
    invalid(
        "the kernel commit changed and the kernel tree has local edits or commits; refusing to discard them",
    )
    .hint(format!(
        "save the edits in {} elsewhere (git diff) and undo them, or build a kernel tree of your own with --kernel-tree",
        source.display()
    ))
}

/// The tree orchard's kernel for `device` is to be built from, found before
/// the cache is looked at and without changing the source.
///
/// A clean tree still on another commit gives the lock's commit's tree, so
/// it does not poison the key. On the lock's commit, local edits take part
/// in the key; on another one they block the move.
pub fn source_tree(build: &Build, device: &str) -> Result<String> {
    let (url, commit) = pin(&build.root, device)?;
    let work = work_directory(build, device, None)?;
    let source = work.join("source");
    let declared =
        sources::declared_tree(&build.cache.git(), &url, &commit, Some(&source), build.say)?;
    if !source.join(".git").exists() {
        return Ok(declared);
    }
    check_url(&source, &url)?;
    if git(&source, &["rev-parse", "HEAD"], &[])? != commit {
        return match clean(&work)? {
            true => Ok(declared),
            false => Err(edited(&source)),
        };
    }
    working_tree(&source, true)
}

/// Orchard's kernel source for `device`, on the lock's commit:
/// `(source, compiler output)`. A clean tree on another commit is moved in
/// place, so unchanged files keep their times and the compiler's objects
/// stay valid.
fn prepare_source(build: &Build, device: &str) -> Result<(PathBuf, PathBuf)> {
    let (url, commit) = pin(&build.root, device)?;
    let work = work_directory(build, device, None)?;
    let source = work.join("source");
    std::fs::create_dir_all(work.join("output")).context(work.display())?;
    let marker = work.join("applied-patches");
    let baseline = work.join("prepared-tree");
    // What the checkout gave, taken while nobody can have edited it: the
    // mark edits are told apart by.
    let mut fresh = false;
    if !source.exists() {
        sources::checkout(&build.cache.git(), &url, &commit, &source, build.say)?;
        fresh = true;
    }
    check_url(&source, &url)?;
    if git(&source, &["rev-parse", "HEAD"], &[])? != commit {
        if !clean(&work)? {
            return Err(edited(&source));
        }
        (build.say)(&format!("Moving the kernel tree to {}", &commit[..commit.len().min(12)]));
        sources::move_to(&build.cache.git(), &url, &commit, &source, build.say)?;
        fresh = true;
    }
    check_origin(&source, &url, &commit)?;
    if fresh {
        write_text(&baseline, &format!("{}\n", working_tree(&source, true)?))?;
    } else if !baseline.exists() {
        // Unknown what the checkout gave: the commit's tree, so that an
        // edit made meanwhile still counts as one.
        write_text(&baseline, &format!("{}\n", git(&source, &["rev-parse", "HEAD^{tree}"], &[])?))?;
    }
    // The other mark a tree is recognised by: the commit it is on.
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(&format!("{commit}\n")) {
        write_text(&marker, &format!("{commit}\n"))?;
    }
    Ok((source, work.join("output")))
}

/// `(source, compiler output)`: orchard's own tree, or the developer's
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
            "producer": build.contract.kernel_producer,
            "tree": working_tree(tree, false)?,
            "mode": mode(),
        }),
        None => json!({
            "platform": described.platform,
            "build": described.build_fields(false),
            "recipes": recipes::object(root, &[described.config.as_str(), toolchain[0], toolchain[1]])?,
            "producer": build.contract.kernel_producer,
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
        "producer": build.contract.kernel_producer,
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
            "kernel source: orchard's {platform} tree, OrchardLinux/linux (branch {}) at the commit of \
             its upstream lock, kept in the cache (git rev-parse HEAD^{{tree}}: see kernel-base \
             key-inputs.json \"tree\").",
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
    use crate::sources::tests::{advance, upstream};
    use orchard_cache::store::Store;
    use std::os::unix::fs::PermissionsExt;


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

    /// Data with the stub producers and an upstream repository the lock
    /// names; `None` where no Python runs the producers.
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
    fn a_kernel_is_built_once_per_platform_and_layered_per_board() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let first = prepare(&build, "aa1", None).unwrap();
        assert_eq!(ran(&fixture.root), ["build", "stage", "stage"]);
        // The source is the lock's commit; the image is made from it.
        assert_eq!(
            std::fs::read_to_string(first.join("Image.gz")).unwrap(),
            "int a;\nall:\n"
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
    fn a_new_kernel_commit_moves_the_tree_and_edits_block_it() {
        let Some(fixture) = fixture() else { return };
        let build = Build::new(&fixture.root, &fixture.store, &quiet).unwrap();
        let lock = fixture.root.join("kernel/upstream.lock");
        let (url, commit) = pin(&fixture.root, "aa1").unwrap();
        let name = |commit: &str| std::fs::write(&lock, format!("url={url}\ncommit={commit}\n")).unwrap();
        let first = prepare(&build, "aa1", None).unwrap();
        let work = work_directory(&build, "aa1", None).unwrap();
        let source = work.join("source");
        assert_eq!(work.file_name().unwrap(), "wide");
        let untouched = std::fs::metadata(source.join("file.c")).unwrap().modified().unwrap();
        ran(&fixture.root);

        // Another commit: the one tree moves to it, and files the commit
        // does not touch keep their times for the compiler.
        let next = advance(&url);
        name(&next);
        let second = prepare(&build, "aa1", None).unwrap();
        assert_ne!(first, second);
        assert_eq!(ran(&fixture.root), ["build", "stage", "stage"]);
        assert_eq!(std::fs::read_to_string(second.join("Image.gz")).unwrap(), "int a;\nall:\n\t@true\n");
        assert_eq!(git(&source, &["rev-parse", "HEAD"], &[]).unwrap(), next);
        assert_eq!(std::fs::metadata(source.join("file.c")).unwrap().modified().unwrap(), untouched);
        assert_eq!(std::fs::read_to_string(work.join("applied-patches")).unwrap(), format!("{next}\n"));
        // A moved tree is again one that can be made anew.
        assert!(crate::manage::is_clean(&work));

        // Back to the first commit: its kernel is still in the cache.
        name(&commit);
        assert_eq!(prepare(&build, "aa1", None).unwrap(), first);
        assert!(ran(&fixture.root).is_empty());

        // A local edit is part of the key, and is never discarded: with it
        // in the tree, another commit is refused.
        name(&next);
        prepare(&build, "aa1", None).unwrap();
        std::fs::write(source.join("file.c"), "int a;\nint edit;\n").unwrap();
        let edited = prepare(&build, "aa1", None).unwrap();
        assert!(std::fs::read_to_string(edited.join("Image.gz")).unwrap().contains("int edit;"));
        name(&commit);
        let error = prepare(&build, "aa1", None).unwrap_err();
        assert!(error.message.contains("local edits"), "{error}");
        assert!(std::fs::read_to_string(source.join("file.c")).unwrap().contains("int edit;"));

        // So is a commit made in the tree, which only its own log names.
        git(&source, &["-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-a", "-m", "mine"], &[]).unwrap();
        let mine = git(&source, &["rev-parse", "HEAD"], &[]).unwrap();
        let error = prepare(&build, "aa1", None).unwrap_err();
        assert!(error.message.contains("commits"), "{error}");
        assert_eq!(git(&source, &["rev-parse", "HEAD"], &[]).unwrap(), mine);
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
