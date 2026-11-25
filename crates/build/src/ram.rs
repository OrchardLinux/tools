//! A RAM payload: a project's `init` and `rootfs/` packed with the kernel,
//! every board's device tree and the loader into `out/PLATFORM/linux.bin`.
//!
//! Device startup belongs to the project's `/init`. The components come
//! from the cache; the image itself is made by orchard's assembly producers
//! (`initramfs.py`, `assemble.py`, `cascadia.py`), so its format is theirs.
use crate::components::{self, cascadia_profile, Given};
use crate::platform::{boards, descriptor, Descriptor};
use crate::project::{self, Choice, Project};
use crate::{copy_tree, explain, invalid, read_text, recipes, write_text, Build};
use orchard_cache::component::digest;
use orchard_cache::{Code, Context, Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The largest image Pongo's uploader takes.
const LIMIT: u64 = 128 << 20;
/// The static tools every payload gets besides BusyBox, and where they go.
const TOOL_FILES: [(&str, &str); 5] = [
    ("dropbear", "usr/sbin"),
    ("dropbearkey", "usr/bin"),
    ("wpa_supplicant", "usr/sbin"),
    ("wpa_cli", "usr/bin"),
    ("iw", "usr/sbin"),
];
const SESSION_FILES: [&str; 2] = ["usb-session", "usb-identity"];
const AUTHORIZED: &str = "etc/orchard/authorized_keys";

/// A built payload.
#[derive(Debug, Clone)]
pub struct Built {
    /// `DIRECTORY/out/PLATFORM`.
    pub output: PathBuf,
    pub platform: String,
    pub boards: Vec<String>,
    pub kernel_release: String,
    /// What the build ran, as a script.
    pub commands: PathBuf,
}

/// Builds `directory/init` plus the optional `directory/rootfs/` for every
/// board of one platform, into `directory/out/PLATFORM/`.
pub fn build_directory(
    build: &Build,
    directory: &Path,
    choice: &Choice,
    given: &Given,
) -> Result<Built> {
    let directory = directory.canonicalize().context(directory.display())?;
    let mut built = assemble_directory(build, &directory, choice, given)?;
    built.commands = built.output.join("commands.sh");
    let name = built
        .output
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let title = format!("orchard build {} ({name})", directory.display());
    write_text(
        &built.commands,
        &explain::script(&build.explain.steps(), &title, &build.python),
    )?;
    Ok(built)
}

fn assemble_directory(
    build: &Build,
    directory: &Path,
    choice: &Choice,
    given: &Given,
) -> Result<Built> {
    let root = &build.root;
    let settings = match project::read(directory)? {
        Project::Payload(settings) => settings,
        Project::Release(_) => {
            return Err(invalid(format!(
                "{} describes a distribution, not a RAM payload",
                directory.join(project::NAME).display()
            )))
        }
    };
    let platform = project::platform(directory, root, choice)?;
    let described = descriptor(root, &platform)?;
    let mut platform_boards = boards(root, &platform)?;
    let chosen = project::board(directory, root, &platform, choice)?;
    if described.loader == "cascadia" {
        let board = chosen
            .clone()
            .expect("a Cascadia platform needs a chosen board");
        platform_boards.retain(|name, _| *name == board);
    }
    // The kernel's board layer carries one board's external modules: the
    // chosen board's, else the first's.
    let board = chosen.unwrap_or_else(|| {
        platform_boards
            .keys()
            .next()
            .expect("a platform has a board")
            .clone()
    });
    let output = directory.join("out").join(&platform);
    std::fs::create_dir_all(&output).context(output.display())?;
    let _ = std::fs::remove_file(output.join("build.json"));
    let init = directory.join("init");
    if !init.is_file() {
        return Err(invalid(format!(
            "missing device startup script: {}",
            init.display()
        )));
    }
    let overlay = directory.join("rootfs");
    let mut files = overlay_files(&overlay)?;
    let tools = components::tools(build, &board)?;
    for (tool, place) in TOOL_FILES {
        if tools.join(tool).is_file() {
            files
                .entry(format!("{place}/{tool}"))
                .or_insert(tools.join(tool));
        }
    }
    if settings.transport {
        for (destination, source) in session_files(build)? {
            if files.contains_key(&destination) {
                return Err(invalid(format!(
                    "rootfs/ provides {destination}; set transport = false under [payload] in orchard.toml to supply your own"
                )));
            }
            files.insert(destination, source);
        }
    }
    let mut given = given.clone();
    if given.kernel.is_none() {
        given.kernel_tree = project::kernel_tree(directory, given.kernel_tree.as_deref())?;
        if let Some(tree) = &given.kernel_tree {
            build.say(format!("Kernel from the tree {}", tree.display()));
        } else if platform_boards.len() > 1 {
            build.say(format!(
                "Kernel layer of {board}: its external modules are the payload's"
            ));
        }
    }
    if given.busybox.is_none() {
        given.busybox = Some(tools.join("busybox"));
    }
    let parts = components::prepare(build, &board, &given)?;
    let name = directory.file_name().unwrap_or_default().to_string_lossy();
    let hostname: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let hostname: String = hostname.trim_matches(['-', '_']).chars().take(63).collect();
    let payload = Payload {
        described: &described,
        boards: &platform_boards,
        parts: &parts,
        init: &init,
        files: &files,
        hostname: if hostname.is_empty() {
            "orchard-ram"
        } else {
            &hostname
        },
        modules: settings.modules,
        bootargs: "console=tty0 loglevel=7 rdinit=/init",
    };
    let mut receipt = assemble(build, &output, &payload)?;
    // Added and removed overlay files count too, not only changes to old ones.
    receipt["overlay"] =
        json!({"path": overlay, "sha256": recipes::key(directory, &["rootfs"], &[])?});
    receipt["firmware"] = json!(settings.firmware);
    receipt["kernel_tree"] = json!(given.kernel_tree);
    write_text(&output.join("build.json"), &pretty(&receipt))?;
    build.say(format!(
        "Built {}. No device was accessed.",
        output.join("linux.bin").display()
    ));
    Ok(Built {
        output,
        platform,
        boards: platform_boards.into_keys().collect(),
        kernel_release: receipt["kernel_release"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        commands: PathBuf::new(),
    })
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a receipt serializes");
    text.push('\n');
    text
}

/// `destination → file` for the regular files under a project's `rootfs/`.
fn overlay_files(overlay: &Path) -> Result<BTreeMap<String, PathBuf>> {
    fn walk(directory: &Path, overlay: &Path, files: &mut BTreeMap<String, PathBuf>) -> Result<()> {
        for item in std::fs::read_dir(directory).context(directory.display())? {
            let path = item?.path();
            let meta = std::fs::symlink_metadata(&path).context(path.display())?;
            if meta.is_symlink() {
                return Err(invalid(format!(
                    "links in rootfs are not supported yet: {}",
                    path.display()
                )));
            }
            if meta.is_dir() {
                walk(&path, overlay, files)?;
            } else if meta.is_file() {
                let name = path
                    .strip_prefix(overlay)
                    .expect("under the overlay")
                    .to_string_lossy()
                    .into_owned();
                files.insert(name, path);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    match std::fs::symlink_metadata(overlay) {
        Err(_) => {}
        Ok(meta) if meta.is_dir() => walk(overlay, overlay, &mut files)?,
        Ok(_) => {
            return Err(invalid(format!(
                "expected a regular rootfs directory: {}",
                overlay.display()
            )))
        }
    }
    Ok(files)
}

/// The files that give a payload its USB session (network, SSH): orchard's
/// helper scripts and this computer's key for RAM payloads.
fn session_files(build: &Build) -> Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for name in SESSION_FILES {
        let found = ["boot/session", "src/orchard"]
            .iter()
            .map(|d| build.root.join(d).join(name))
            .find(|p| p.is_file());
        let Some(path) = found else {
            return Err(Error::new(
                Code::NotFound,
                format!("the data has no {name} for the USB session"),
            )
            .hint("set transport = false under [payload] in orchard.toml to build without it"));
        };
        files.push((format!("usr/libexec/orchard/{name}"), path));
    }
    files.push((AUTHORIZED.to_owned(), authorized_keys(build.cache.root())?));
    Ok(files)
}

/// The keys a payload lets in: this computer's key for RAM payloads, made
/// once in the cache and never leaving the host, and the one orchard's
/// Python tool made, while its `shell` and `exec` are in use.
fn authorized_keys(cache: &Path) -> Result<PathBuf> {
    let directory = cache.join("private/ram-ssh");
    std::fs::create_dir_all(&directory).context(directory.display())?;
    for private in [cache.join("private"), directory.clone()] {
        if std::fs::symlink_metadata(&private)
            .context(private.display())?
            .is_symlink()
        {
            return Err(invalid(format!(
                "linked private state refused: {}",
                private.display()
            )));
        }
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
            .context(private.display())?;
    }
    let key = directory.join("id_ed25519");
    if std::fs::symlink_metadata(&key).is_ok_and(|m| m.is_symlink()) {
        return Err(invalid("linked RAM SSH key refused"));
    }
    if !key.exists() {
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "orchard-ram", "-f"])
            .arg(&key)
            .output()
            .map_err(|e| {
                Error::new(
                    Code::NotFound,
                    format!("ssh-keygen is needed for the USB session: {e}"),
                )
            })?;
        if !made.status.success() {
            return Err(Error::new(
                Code::Io,
                "ssh-keygen could not make this computer's RAM key",
            ));
        }
    }
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))
        .context(key.display())?;
    let mut lines = vec![read_text(&key.with_extension("pub"))?.trim().to_owned()];
    if let Some(other) = python_key().and_then(|path| std::fs::read_to_string(path).ok()) {
        if !lines.contains(&other.trim().to_owned()) {
            lines.push(other.trim().to_owned());
        }
    }
    let text = lines.join("\n") + "\n";
    let path = directory.join("authorized_keys");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(&text) {
        write_text(&path, &text)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .context(path.display())?;
    }
    Ok(path)
}

fn python_key() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let store = match std::env::var_os("ORCHARD_HOME") {
        Some(path) => PathBuf::from(path),
        None if cfg!(target_os = "macos") => home?.join("Library/Application Support/Orchard"),
        None => match std::env::var_os("XDG_DATA_HOME") {
            Some(data) => PathBuf::from(data).join("orchard"),
            None => home?.join(".local/share/orchard"),
        },
    };
    Some(store.join("private/ram-ssh/id_ed25519.pub"))
}

struct Payload<'a> {
    described: &'a Descriptor,
    /// Each board and its device tree; the loader picks the booted board's.
    boards: &'a BTreeMap<String, String>,
    parts: &'a components::Components,
    init: &'a Path,
    /// RAM-root destinations and the files that go there.
    files: &'a BTreeMap<String, PathBuf>,
    hostname: &'a str,
    modules: bool,
    bootargs: &'a str,
}

/// Lays out the RAM root, has the producers pack it and assemble the image,
/// and returns the build's receipt.
fn assemble(build: &Build, output: &Path, payload: &Payload) -> Result<Value> {
    let Payload {
        described,
        boards,
        parts,
        ..
    } = payload;
    let cascadia = described.loader == "cascadia";
    let first = boards.keys().next().expect("a payload has a board");
    let profile = match cascadia {
        true if boards.len() != 1 => {
            return Err(invalid(
                "a Cascadia RAM payload requires one selected board",
            ))
        }
        true => Some(cascadia_profile(&build.root, first)?),
        false => None,
    };
    if parts.pongo.is_none() && !cascadia {
        return Err(invalid("an m1n1 RAM payload requires Pongo"));
    }
    check_busybox(&parts.busybox, &described.arch)?;
    let release = read_text(&parts.kernel.join("kernel.release"))?
        .trim()
        .to_owned();
    if release.is_empty() || release.contains('/') || release.split_whitespace().count() != 1 {
        return Err(
            Error::new(Code::Format, "invalid kernel.release").context(parts.kernel.display())
        );
    }
    let kernel = parts.kernel.join(&described.image);
    let dtbs: Vec<PathBuf> = boards
        .values()
        .map(|dtb| parts.kernel.join("dtbs").join(format!("{dtb}.dtb")))
        .collect();
    if let Some(missing) = dtbs.iter().find(|path| !path.is_file()) {
        let name = missing.file_name().unwrap_or_default().to_string_lossy();
        return Err(Error::new(
            Code::NotFound,
            format!("the kernel bundle lacks {name}"),
        ));
    }
    if cascadia {
        check_compatible(&dtbs[0], &build.root, first)?;
    }
    let temporary = tempfile::Builder::new()
        .prefix("build-")
        .tempdir_in(output)
        .context(output.display())?;
    let work = temporary.path();
    let root = work.join("root");
    for name in [
        "bin", "sbin", "usr/bin", "usr/sbin", "dev", "proc", "sys", "run", "tmp", "root", "etc",
    ] {
        std::fs::create_dir_all(root.join(name)).context(root.display())?;
    }
    let place = |from: &Path, to: &Path, mode: u32| -> Result<()> {
        std::fs::copy(from, to).context(from.display())?;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode)).context(to.display())
    };
    place(&parts.busybox, &root.join("bin/busybox"), 0o755)?;
    place(payload.init, &root.join("init"), 0o755)?;
    write_text(
        &root.join("etc/hostname"),
        &format!("{}\n", payload.hostname),
    )?;
    if payload.modules {
        let tree = parts.kernel.join("lib/modules").join(&release);
        if !tree.join("modules.dep").is_file() {
            return Err(Error::new(
                Code::NotFound,
                "the kernel bundle has no staged module tree for its release",
            ));
        }
        copy_tree(&tree, &root.join("lib/modules").join(&release), false)?;
    }
    for (destination, source) in payload.files {
        let plain = !destination.is_empty()
            && destination
                .split('/')
                .all(|part| !["", ".", ".."].contains(&part));
        if !plain {
            return Err(invalid(format!("invalid RAM-root path: {destination}")));
        }
        let target = root.join(destination);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(invalid(format!("conflicting RAM-root path: {destination}")));
        }
        let parent = target.parent().expect("inside the root");
        let mut linked = parent.ancestors().take_while(|p| p.starts_with(&root));
        if linked.any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink())) {
            return Err(invalid(format!("linked RAM-root parent: {destination}")));
        }
        std::fs::create_dir_all(parent).context(parent.display())?;
        let mode = std::fs::metadata(source)
            .context(source.display())?
            .permissions()
            .mode()
            & 0o777;
        place(source, &target, mode)?;
    }
    build.explain.note(format!(
        "root ({}): orchard laid out BusyBox at bin/busybox, init, etc/hostname{} and {} files (rootfs/, the static \
         tools and the session helper); any directory of that shape works",
        root.display(),
        if payload.modules { ", the kernel modules" } else { "" },
        payload.files.len()
    ));
    let initramfs = work.join("initramfs.cpio.gz");
    build.python(
        &build.producer("initramfs.py"),
        &[
            OsStr::new("--root"),
            root.as_os_str(),
            "--output".as_ref(),
            initramfs.as_os_str(),
        ],
    )?;
    let image = work.join("linux.bin");
    run_assembly(
        build,
        &Assembly {
            bootargs: payload.bootargs,
            loader: &parts.loader,
            dtbs: &dtbs,
            kernel: &kernel,
            initramfs: &initramfs,
            profile: profile.as_deref(),
            output: &image,
        },
    )?;
    if std::fs::metadata(&image).context(image.display())?.len() > LIMIT {
        return Err(Error::new(
            Code::TooLarge,
            "the payload exceeds the Pongo uploader limit (128 MiB)",
        ));
    }
    let mut names = vec!["linux.bin"];
    if let Some(pongo) = &parts.pongo {
        std::fs::copy(pongo, work.join("pongo.bin")).context(pongo.display())?;
        names.push("pongo.bin");
    }
    let mut inputs: Vec<&Path> = vec![payload.init, &parts.busybox, &parts.loader, &kernel];
    inputs.extend(dtbs.iter().map(PathBuf::as_path));
    inputs.extend(profile.as_deref());
    inputs.extend(payload.files.values().map(PathBuf::as_path));
    let mut sources = serde_json::Map::new();
    for input in inputs {
        let path = input.canonicalize().context(input.display())?;
        sources.insert(path.to_string_lossy().into_owned(), json!(digest(&path)?));
    }
    let mut hashes = serde_json::Map::new();
    for name in &names {
        hashes.insert((*name).to_owned(), json!(digest(&work.join(name))?));
    }
    let mut receipt = json!({
        "kind": "orchard.ram-build",
        "platform": described.platform,
        "boards": boards.keys().collect::<Vec<_>>(),
        "kernel_release": release,
        "hardware_tested": false,
        // What `run` needs to add a board's firmware layer.
        "assembly": {"loader": parts.loader, "kernel": kernel, "dtbs": dtbs, "bootargs": payload.bootargs},
        "files": hashes,
        "sources": sources,
    });
    if let Some(profile) = &profile {
        receipt["loader_format"] = json!("cascadia");
        receipt["boot_method"] = json!("recovery-autogo");
        receipt["assembly"]["profile"] = json!(profile);
    }
    names.extend(["linux.bin.json", "initramfs.cpio.gz"]);
    for name in names {
        std::fs::rename(work.join(name), output.join(name)).context(output.join(name).display())?;
    }
    Ok(receipt)
}

/// What the assembly producers are given.
pub struct Assembly<'a> {
    pub bootargs: &'a str,
    pub loader: &'a Path,
    pub dtbs: &'a [PathBuf],
    pub kernel: &'a Path,
    pub initramfs: &'a Path,
    /// A 32-bit board's Cascadia profile; its payload keeps the board
    /// tree's own boot arguments.
    pub profile: Option<&'a Path>,
    pub output: &'a Path,
}

/// Has orchard's producer assemble the image: m1n1's payload format, or
/// Cascadia's for a board with a profile.
pub fn run_assembly(build: &Build, assembly: &Assembly) -> Result<()> {
    if assembly.bootargs.contains(['\n', '\r']) {
        return Err(invalid("boot arguments are one line"));
    }
    let mut args: Vec<&OsStr> = Vec::new();
    let elf = assembly.loader.with_extension("elf");
    let script = match assembly.profile {
        Some(profile) => {
            args.extend([
                "--profile".as_ref(),
                profile.as_os_str(),
                "--loader".as_ref(),
                assembly.loader.as_os_str(),
            ]);
            if elf.is_file() {
                args.extend(["--loader-elf".as_ref(), elf.as_os_str()]);
            }
            args.extend(["--dtb".as_ref(), assembly.dtbs[0].as_os_str()]);
            "cascadia.py"
        }
        None => {
            args.extend([
                "--bootargs".as_ref(),
                assembly.bootargs.as_ref(),
                "--loader".as_ref(),
                assembly.loader.as_os_str(),
            ]);
            for dtb in assembly.dtbs {
                args.extend(["--dtb".as_ref(), dtb.as_os_str()]);
            }
            "assemble.py"
        }
    };
    args.extend([
        "--kernel".as_ref(),
        assembly.kernel.as_os_str(),
        "--initramfs".as_ref(),
        assembly.initramfs.as_os_str(),
        "--output".as_ref(),
        assembly.output.as_os_str(),
    ]);
    build.python(&build.producer(script), &args)
}

/// BusyBox has to be a static executable of the payload's architecture: a
/// RAM root has no loader or libraries for it.
pub fn check_busybox(path: &Path, architecture: &str) -> Result<()> {
    let data = std::fs::read(path).context(path.display())?;
    let wrong = |what: &str| Error::new(Code::Format, what.to_owned()).context(path.display());
    let u16_at = |at: usize| {
        data.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("four bytes")))
    };
    let u64_at = |at: usize| {
        data.get(at..at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("eight bytes")))
    };
    let (wide, machine, what) = match architecture {
        "arm" => (
            false,
            40,
            "BusyBox must be static ARM EABI5 hard-float ELF32",
        ),
        _ => (true, 183, "BusyBox must be little-endian AArch64 ELF64"),
    };
    let class = if wide { 2 } else { 1 };
    if data.len() < 64
        || data[..4] != *b"\x7fELF"
        || data[4] != class
        || data[5] != 1
        || u16_at(18) != Some(machine)
    {
        return Err(wrong(what));
    }
    if !wide {
        let flags = u32_at(36).unwrap_or(0);
        if flags >> 24 != 5 || flags & 0x400 == 0 || flags & 0x200 != 0 {
            return Err(wrong(what));
        }
    }
    let (offset, size, count) = match wide {
        true => (
            u64_at(32).unwrap_or(0),
            u16_at(54).unwrap_or(0),
            u16_at(56).unwrap_or(0),
        ),
        false => (
            u64::from(u32_at(28).unwrap_or(0)),
            u16_at(42).unwrap_or(0),
            u16_at(44).unwrap_or(0),
        ),
    };
    let minimum = if wide { 56 } else { 32 };
    let end = offset.saturating_add(u64::from(size) * u64::from(count));
    if size < minimum || end > data.len() as u64 {
        return Err(wrong("malformed BusyBox ELF program headers"));
    }
    // PT_DYNAMIC (2) or PT_INTERP (3) means it needs a dynamic loader.
    let dynamic = (0..count).any(|i| {
        matches!(
            u32_at(offset as usize + usize::from(i) * usize::from(size)),
            Some(2 | 3)
        )
    });
    if dynamic {
        return Err(wrong(
            "BusyBox must be static (no DYNAMIC or INTERP segment)",
        ));
    }
    Ok(())
}

/// Binds a board's Cascadia profile to the device tree before assembly: the
/// tree has to name the board.
fn check_compatible(dtb: &Path, root: &Path, board: &str) -> Result<()> {
    let path = root.join("devices").join(board).join("device.toml");
    let file: toml::Table = toml::from_str(&read_text(&path)?)
        .map_err(|e| Error::new(Code::Format, e.message().to_owned()).context(path.display()))?;
    let expected: Vec<&str> = file
        .get("device")
        .and_then(|d| d.get("dt_compatible"))
        .and_then(toml::Value::as_array)
        .map(|names| names.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    let tree = orchard_formats::fdt::Tree::read(&std::fs::read(dtb).context(dtb.display())?)
        .context(dtb.display())?;
    let compatible = tree
        .root
        .properties
        .iter()
        .find(|(name, _)| name == "compatible")
        .map(|(_, value)| value.as_slice());
    let Some(value) = compatible.filter(|v| v.ends_with(&[0])) else {
        return Err(Error::new(
            Code::Format,
            "the Cascadia device tree has no complete compatible list",
        ));
    };
    let named = value[..value.len() - 1]
        .split(|&b| b == 0)
        .any(|name| expected.iter().any(|e| e.as_bytes() == name));
    if !named {
        return Err(invalid(
            "the Cascadia device tree's compatible differs from the selected board",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 header with one program header of `kind`.
    fn elf64(machine: u16, kind: u32) -> Vec<u8> {
        let mut data = vec![0u8; 64 + 56];
        data[..6].copy_from_slice(b"\x7fELF\x02\x01");
        data[18..20].copy_from_slice(&machine.to_le_bytes());
        data[32..40].copy_from_slice(&64u64.to_le_bytes());
        data[54..56].copy_from_slice(&56u16.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());
        data[64..68].copy_from_slice(&kind.to_le_bytes());
        data
    }

    fn elf32(flags: u32, kind: u32) -> Vec<u8> {
        let mut data = vec![0u8; 64 + 32];
        data[..6].copy_from_slice(b"\x7fELF\x01\x01");
        data[18..20].copy_from_slice(&40u16.to_le_bytes());
        data[28..32].copy_from_slice(&52u32.to_le_bytes());
        data[36..40].copy_from_slice(&flags.to_le_bytes());
        data[42..44].copy_from_slice(&32u16.to_le_bytes());
        data[44..46].copy_from_slice(&1u16.to_le_bytes());
        data[52..56].copy_from_slice(&kind.to_le_bytes());
        data
    }

    #[test]
    fn busybox_has_to_be_static_and_of_the_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let check = |data: Vec<u8>, architecture: &str| {
            let path = dir.path().join("busybox");
            std::fs::write(&path, data).unwrap();
            check_busybox(&path, architecture).map_err(|e| e.message)
        };
        assert!(check(elf64(183, 1), "arm64").is_ok());
        assert!(check(elf64(183, 3), "arm64")
            .unwrap_err()
            .contains("must be static"));
        assert!(check(elf64(62, 1), "arm64")
            .unwrap_err()
            .contains("AArch64"));
        assert!(check(b"#!/bin/sh\n".to_vec(), "arm64")
            .unwrap_err()
            .contains("AArch64"));
        let hard_float = 5 << 24 | 0x400;
        assert!(check(elf32(hard_float, 1), "arm").is_ok());
        assert!(check(elf32(5 << 24 | 0x200, 1), "arm")
            .unwrap_err()
            .contains("hard-float"));
        assert!(check(elf32(hard_float, 2), "arm")
            .unwrap_err()
            .contains("must be static"));
        assert!(check(elf64(183, 1), "arm").unwrap_err().contains("ELF32"));
    }

    #[test]
    fn an_overlay_is_regular_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("rootfs");
        assert!(overlay_files(&overlay).unwrap().is_empty());
        std::fs::create_dir_all(overlay.join("etc/a")).unwrap();
        std::fs::write(overlay.join("etc/a/b"), "b").unwrap();
        std::fs::write(overlay.join("top"), "t").unwrap();
        let files = overlay_files(&overlay).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["etc/a/b", "top"]);
        std::os::unix::fs::symlink("top", overlay.join("link")).unwrap();
        assert!(overlay_files(&overlay)
            .unwrap_err()
            .message
            .contains("links in rootfs"));
    }

    #[test]
    fn this_computers_ram_key_is_made_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let first = authorized_keys(dir.path()).unwrap();
        let text = std::fs::read_to_string(&first).unwrap();
        assert!(
            text.starts_with("ssh-ed25519 ") && text.contains("orchard-ram"),
            "{text}"
        );
        let key = dir.path().join("private/ram-ssh/id_ed25519");
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let private = std::fs::read(&key).unwrap();
        authorized_keys(dir.path()).unwrap();
        assert_eq!(std::fs::read(&key).unwrap(), private);
    }
}
