//! `orchard.toml`: what a project builds, one file for RAM payloads and
//! distributions, and `orchard.local.toml`: a developer's own choices for
//! one checkout, never committed.
//!
//! ```toml
//! name = "minimal-boot"           # a RAM payload: init and rootfs/ beside this file
//! [payload]
//! transport = false               # leave the USB session helper out
//! modules = true                  # stage the kernel's modules
//! firmware = true                 # add the booted board's firmware at run
//!
//! name = "minimal-linux"          # a distribution: its own script builds the release
//! [release]
//! build = "tools/build.py"        # relative to this file
//! selects-platform = true         # the script takes --platform
//! ```
use crate::platform::{boards_by_platform, descriptor};
use crate::{invalid, plain_name, read_text};
use orchard_cache::{Code, Context, Error, Result};
use std::path::{Path, PathBuf};

pub const NAME: &str = "orchard.toml";
pub const LOCAL: &str = "orchard.local.toml";

/// What a project file says the project is.
#[derive(Debug, Clone, PartialEq)]
pub enum Project {
    Payload(Payload),
    Release(Release),
}

/// How a RAM payload is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payload {
    /// The USB session helper (network, SSH) is part of the payload.
    pub transport: bool,
    /// The kernel's modules are staged, without loading them.
    pub modules: bool,
    /// `run` adds the booted board's firmware.
    pub firmware: bool,
}

impl Default for Payload {
    fn default() -> Payload {
        Payload {
            transport: true,
            modules: false,
            firmware: false,
        }
    }
}

/// A distribution: its own script builds the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The distro name of its index.
    pub name: String,
    /// The build entry.
    pub build: PathBuf,
    /// Whether the entry takes `--platform`.
    pub selects_platform: bool,
}

fn load(path: &Path) -> Result<toml::Table> {
    toml::from_str(&read_text(path)?)
        .map_err(|e| Error::new(Code::Format, e.message().to_owned()).context(path.display()))
}

fn unknown(path: &Path, table: &toml::Table, allowed: &[&str]) -> Result<()> {
    let mut extra: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    extra.sort_unstable();
    if extra.is_empty() {
        return Ok(());
    }
    Err(invalid(format!(
        "{}: unknown settings {}; expected {}",
        path.display(),
        extra.join(", "),
        allowed.join(", ")
    )))
}

/// The project file of `directory`: a directory without one is a payload
/// with every setting at its default.
pub fn read(directory: &Path) -> Result<Project> {
    let path = directory.join(NAME);
    if !path.is_file() {
        // Settings in a file nothing reads would be lost without a word.
        if let Some(old) = ["payload.toml", "distro.toml"]
            .iter()
            .find(|old| directory.join(old).is_file())
        {
            return Err(invalid(format!(
                "{} is a project file from before {NAME}, which orchard reads instead",
                directory.join(old).display()
            ))
            .hint(format!(
                "write its settings as {NAME} (name = \"...\", then [payload] or [release]) and remove {old}"
            )));
        }
        return Ok(Project::Payload(Payload::default()));
    }
    let file = load(&path)?;
    unknown(&path, &file, &["name", "payload", "release"])?;
    if file.contains_key("payload") && file.contains_key("release") {
        return Err(invalid(format!(
            "{}: a project is a payload or a release, not both",
            path.display()
        )));
    }
    let name = file.get("name").and_then(toml::Value::as_str).unwrap_or("");
    if name.is_empty() {
        return Err(invalid(format!(
            "{}: name = \"...\" names the project",
            path.display()
        )));
    }
    let kind = if file.contains_key("release") {
        "release"
    } else {
        "payload"
    };
    let empty = toml::Value::Table(toml::Table::new());
    let table = file
        .get(kind)
        .unwrap_or(&empty)
        .as_table()
        .ok_or_else(|| invalid(format!("{}: [{kind}] must be a table", path.display())))?;
    if kind == "payload" {
        unknown(&path, table, &["transport", "modules", "firmware"])?;
        let flag = |key: &str, default: bool| match table.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| invalid(format!("{}: {key} must be true or false", path.display()))),
        };
        let defaults = Payload::default();
        return Ok(Project::Payload(Payload {
            transport: flag("transport", defaults.transport)?,
            modules: flag("modules", defaults.modules)?,
            firmware: flag("firmware", defaults.firmware)?,
        }));
    }
    unknown(&path, table, &["build", "selects-platform"])?;
    let selects_platform = match table.get("selects-platform") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            invalid(format!(
                "{}: selects-platform must be true or false",
                path.display()
            ))
        })?,
    };
    let distro = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c);
    if !name.chars().all(distro) || name.starts_with(['.', '_', '-']) {
        return Err(invalid(format!(
            "{}: name must be the distro name of its index, e.g. \"omarchy-a\"",
            path.display()
        )));
    }
    let build = table
        .get("build")
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let inside = !build.is_empty()
        && !build.contains('\\')
        && build
            .split('/')
            .all(|part| !["", ".", ".."].contains(&part));
    if !inside {
        return Err(invalid(format!(
            "{}: build must be a path inside the checkout, e.g. \"tools/build\"",
            path.display()
        )));
    }
    let entry = directory.join(build);
    let meta = std::fs::symlink_metadata(&entry)
        .ok()
        .filter(|m| m.is_file());
    let Some(meta) = meta else {
        return Err(invalid(format!(
            "{}: the build entry {build} is not a file",
            path.display()
        )));
    };
    use std::os::unix::fs::PermissionsExt;
    if entry.extension().is_none_or(|e| e != "py") && meta.permissions().mode() & 0o111 == 0 {
        return Err(invalid(format!(
            "{}: the build entry {build} is not executable",
            path.display()
        )));
    }
    Ok(Project::Release(Release {
        name: name.to_owned(),
        build: entry,
        selects_platform,
    }))
}

/// The distribution checkout `directory` is in: the nearest project file up
/// to its repository's root, when that is a release.
pub fn find_release(directory: &Path) -> Result<Option<(PathBuf, Release)>> {
    let mut at = Some(directory);
    while let Some(parent) = at {
        if parent.join(NAME).is_file() {
            return Ok(match read(parent)? {
                Project::Release(release) => Some((parent.to_owned(), release)),
                // The nearest project is a payload.
                Project::Payload(_) => None,
            });
        }
        if parent.join(".git").exists() {
            break;
        }
        at = parent.parent();
    }
    Ok(None)
}

/// A checkout's own choices: sticky defaults for `--device`, `--platform`,
/// `--kernel-tree` and `--packages`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Local {
    /// The file they come from.
    pub path: Option<PathBuf>,
    pub device: Option<String>,
    /// A platform name, or `all`.
    pub platform: Option<String>,
    pub kernel_tree: Option<PathBuf>,
    /// A checkout of the package repository; `Some(None)` is `false`:
    /// published packages only.
    pub packages: Option<Option<PathBuf>>,
}

/// The nearest `orchard.local.toml` from `directory` up to its repository's
/// root.
pub fn local(directory: &Path) -> Result<Local> {
    let mut at = Some(directory);
    while let Some(parent) = at {
        let path = parent.join(LOCAL);
        if path.exists() {
            return read_local(&path);
        }
        if parent.join(".git").exists() {
            break;
        }
        at = parent.parent();
    }
    Ok(Local::default())
}

fn read_local(path: &Path) -> Result<Local> {
    let file = load(path)?;
    unknown(
        path,
        &file,
        &["device", "platform", "kernel-tree", "packages"],
    )?;
    let base = path.parent().unwrap_or(Path::new("."));
    let name = |key: &str| match file.get(key) {
        None => Ok(None),
        Some(value) => match value.as_str().filter(|v| plain_name(v)) {
            Some(value) => Ok(Some(value.to_owned())),
            None => Err(invalid(format!("{}: invalid {key} name", path.display()))),
        },
    };
    let kernel_tree = match file.get("kernel-tree") {
        None => None,
        Some(value) => match value.as_str().filter(|v| !v.is_empty()) {
            Some(value) => Some(absolute(base, value)),
            None => {
                return Err(invalid(format!(
                    "{}: kernel-tree must be a path",
                    path.display()
                )))
            }
        },
    };
    let packages = match file.get("packages") {
        None => None,
        Some(toml::Value::Boolean(false)) => Some(None),
        Some(value) => match value.as_str().filter(|v| !v.is_empty()) {
            Some(value) => Some(Some(absolute(base, value))),
            None => {
                return Err(invalid(format!(
                    "{}: packages must be a path, or false",
                    path.display()
                )))
            }
        },
    };
    Ok(Local {
        path: Some(path.to_owned()),
        device: name("device")?,
        platform: name("platform")?,
        kernel_tree,
        packages,
    })
}

/// A path as written in a settings file, with `~` for the home directory,
/// relative to the file.
fn absolute(base: &Path, value: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (value.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => base.join(value),
    }
}

/// `--kernel-tree`, else the checkout's own setting.
pub fn kernel_tree(directory: &Path, given: Option<&Path>) -> Result<Option<PathBuf>> {
    match given {
        Some(tree) => Ok(Some(tree.canonicalize().context(tree.display())?)),
        None => Ok(local(directory)?.kernel_tree),
    }
}

/// `--packages`, else the checkout's setting, else a `packages` checkout
/// beside the distribution's. A checkout of the Orchard package repository
/// has `tools/pkgrepo` and `packages/`.
pub fn packages(directory: &Path, given: Option<&Path>) -> Result<Option<PathBuf>> {
    let is_checkout =
        |path: &Path| path.join("tools/pkgrepo").is_file() && path.join("packages").is_dir();
    let (found, origin) = match given {
        Some(path) => (path.to_owned(), "--packages".to_owned()),
        None => {
            let settings = local(directory)?;
            match settings.packages {
                Some(None) => return Ok(None),
                Some(Some(path)) => (
                    path,
                    format!("{}: packages", settings.path.unwrap_or_default().display()),
                ),
                None => {
                    let sibling = directory.parent().map(|p| p.join("packages"));
                    return Ok(sibling.filter(|path| is_checkout(path)));
                }
            }
        }
    };
    if !is_checkout(&found) {
        return Err(invalid(format!(
            "{origin}: {} is not a checkout of the Orchard package repository \
             (OrchardLinux/packages: tools/pkgrepo, packages/)",
            found.display()
        )));
    }
    // The build entry runs in the distribution's directory.
    Ok(Some(found.canonicalize().context(found.display())?))
}

/// What a build is asked to cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platforms {
    All,
    Named(Vec<String>),
}

/// What the command line and the attached device say about the target.
#[derive(Debug, Clone, Default)]
pub struct Choice<'a> {
    pub platforms: &'a [String],
    pub device: Option<&'a str>,
    /// The board of the one device attached in DFU or Pongo, if any.
    pub attached: Option<&'a str>,
    /// Why this computer cannot see USB devices at all, if it can't.
    pub usb: Option<&'a str>,
}

fn listing(root: &Path) -> Result<String> {
    Ok(boards_by_platform(root)?
        .iter()
        .map(|(name, boards)| {
            format!(
                "{name}: {}",
                boards.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; "))
}

/// The platforms to build: `--platform` (repeatable, or `all`), else the
/// platform of `--device`, else the checkout's settings, else the attached
/// board's. A build never means every platform unless `all` is asked for.
pub fn platforms(directory: &Path, root: &Path, choice: &Choice) -> Result<Platforms> {
    let known = boards_by_platform(root)?;
    let mut chosen: Vec<String> = choice.platforms.to_vec();
    let mut origin = "--platform".to_owned();
    if chosen.is_empty() && choice.device.is_none() {
        let settings = local(directory)?;
        if let Some(platform) = settings.platform {
            origin = settings.path.unwrap_or_default().display().to_string();
            if let (Some(board), Some(boards)) = (&settings.device, known.get(&platform)) {
                if !boards.contains_key(board) {
                    return Err(invalid(format!(
                        "{origin}: device {board} is not a board of platform {platform}"
                    )));
                }
            }
            chosen = vec![platform];
        }
    }
    if chosen.is_empty() {
        return Ok(Platforms::Named(vec![platform(directory, root, choice)?]));
    }
    if chosen.iter().any(|name| name == "all") {
        if chosen.len() > 1 {
            return Err(invalid(format!(
                "{origin}: all builds every platform; it combines with no other"
            )));
        }
        return Ok(Platforms::All);
    }
    chosen.sort();
    chosen.dedup();
    let missing: Vec<&str> = chosen
        .iter()
        .filter(|n| !known.contains_key(*n))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = known.keys().map(String::as_str).collect();
        return Err(invalid(format!(
            "unknown platform {} ({origin}); known: {}",
            missing.join(", "),
            names.join(", ")
        )));
    }
    Ok(Platforms::Named(chosen))
}

/// One platform, for a RAM payload.
pub fn platform(directory: &Path, root: &Path, choice: &Choice) -> Result<String> {
    let known = boards_by_platform(root)?;
    if let Some(name) = choice.platforms.first() {
        if choice.platforms.len() > 1 || name == "all" {
            return Err(invalid(
                "a RAM payload is built for one platform: give --platform once",
            ));
        }
        if !known.contains_key(name) {
            let names: Vec<&str> = known.keys().map(String::as_str).collect();
            return Err(invalid(format!(
                "unknown platform {name}; known: {}",
                names.join(", ")
            )));
        }
        return Ok(name.clone());
    }
    let settings = local(directory)?;
    if choice.device.is_none() {
        if let Some(name) = &settings.platform {
            return match platforms(directory, root, &Choice::default())? {
                Platforms::Named(names) => Ok(names[0].clone()),
                Platforms::All => Err(invalid(format!(
                    "{}: a payload is built for one platform; name it instead of {name}",
                    settings.path.unwrap_or_default().display()
                ))),
            };
        }
    }
    let board = choice
        .device
        .map(str::to_owned)
        .or(settings.device)
        .or(choice.attached.map(str::to_owned));
    let Some(board) = board else {
        if let Some(why) = choice.usb {
            return Err(invalid(format!(
                "{why}\nTo build without a device, pass --device BOARD or --platform PLATFORM, or set \
                 device in {LOCAL}."
            )));
        }
        return Err(invalid(format!(
            "which platform? Pass --device BOARD or --platform PLATFORM, set device in {LOCAL}, \
             or connect the device. Platforms and their boards: {}",
            listing(root)?
        )));
    };
    match known.iter().find(|(_, boards)| boards.contains_key(&board)) {
        Some((name, _)) => Ok(name.clone()),
        None => Err(invalid(format!(
            "no hardware support describes {board}. Platforms and their boards: {}",
            listing(root)?
        ))),
    }
}

/// The board a build is for, when one is chosen: `--device`, else the
/// checkout's setting, else the attached board, each only if it is a board
/// of `platform`.
pub fn board(
    directory: &Path,
    root: &Path,
    platform: &str,
    choice: &Choice,
) -> Result<Option<String>> {
    let boards = crate::platform::boards(root, platform)?;
    let settings = local(directory)?;
    let chosen = choice
        .device
        .map(str::to_owned)
        .or(settings.device)
        .or(choice.attached.map(str::to_owned));
    let loader = descriptor(root, platform)?.loader;
    match chosen {
        Some(board) if boards.contains_key(&board) => Ok(Some(board)),
        Some(board) if choice.device.is_some() => Err(invalid(format!(
            "{board} is not a board of platform {platform}"
        ))),
        _ if loader == "cascadia" => Err(invalid("select the Cascadia board with --device BOARD")),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::tests::data;

    fn project(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NAME), text).unwrap();
        dir
    }

    #[test]
    fn a_project_file_from_before_is_refused_not_ignored() {
        for old in ["payload.toml", "distro.toml"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(old), "modules = true\n").unwrap();
            let error = read(dir.path()).unwrap_err();
            assert_eq!(error.code, Code::InvalidInput);
            assert!(error.message.contains(old), "{}", error.message);
            // With the project file there, the old one is only a leftover.
            std::fs::write(dir.path().join(NAME), "name = \"x\"\n").unwrap();
            read(dir.path()).unwrap();
        }
    }

    #[test]
    fn a_directory_without_a_project_file_is_a_default_payload() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read(dir.path()).unwrap(),
            Project::Payload(Payload::default())
        );
        let dir = project("name = \"x\"\n[payload]\ntransport = false\nmodules = true\n");
        let expected = Payload {
            transport: false,
            modules: true,
            firmware: false,
        };
        assert_eq!(read(dir.path()).unwrap(), Project::Payload(expected));
    }

    #[test]
    fn a_project_file_is_checked() {
        for (text, problem) in [
            ("[payload]\n", "names the project"),
            ("name = \"x\"\n[payload]\n[release]\n", "not both"),
            ("name = \"x\"\nother = 1\n", "unknown settings other"),
            (
                "name = \"x\"\n[payload]\ntransport = \"no\"\n",
                "must be true or false",
            ),
            (
                "name = \"x\"\n[payload]\nboard = \"j71b\"\n",
                "unknown settings board",
            ),
            (
                "name = \"x\"\n[release]\nbuild = \"../build\"\n",
                "inside the checkout",
            ),
            (
                "name = \"x\"\n[release]\nbuild = \"missing\"\n",
                "is not a file",
            ),
            ("name = \"X\"\n[release]\nbuild = \"b\"\n", "distro name"),
        ] {
            let dir = project(text);
            let error = read(dir.path()).unwrap_err();
            assert!(error.message.contains(problem), "{text}: {error}");
        }
    }

    #[test]
    fn a_release_is_found_from_inside_its_checkout() {
        let dir = project(
            "name = \"distro\"\n[release]\nbuild = \"build.py\"\nselects-platform = true\n",
        );
        std::fs::write(dir.path().join("build.py"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let (root, release) = find_release(&dir.path().join("a/b")).unwrap().unwrap();
        assert_eq!(root, dir.path());
        assert!(release.selects_platform && release.name == "distro");
        // A payload nearer than the release is the project.
        std::fs::write(dir.path().join("a").join(NAME), "name = \"p\"\n").unwrap();
        assert_eq!(find_release(&dir.path().join("a/b")).unwrap(), None);
        // The search stops at the repository's root.
        std::fs::remove_file(dir.path().join("a").join(NAME)).unwrap();
        std::fs::create_dir(dir.path().join("a/.git")).unwrap();
        assert_eq!(find_release(&dir.path().join("a/b")).unwrap(), None);
    }

    #[test]
    fn local_settings_are_sticky_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(
            dir.path().join(LOCAL),
            "device = \"aa1\"\nkernel-tree = \"../linux\"\npackages = false\n",
        )
        .unwrap();
        let settings = local(&dir.path().join("sub")).unwrap();
        assert_eq!(settings.device.as_deref(), Some("aa1"));
        assert_eq!(settings.kernel_tree, Some(dir.path().join("../linux")));
        assert_eq!(settings.packages, Some(None));
        assert_eq!(packages(dir.path(), None).unwrap(), None);
        std::fs::write(dir.path().join(LOCAL), "orchard = \"x\"\n").unwrap();
        assert!(local(dir.path())
            .unwrap_err()
            .message
            .contains("unknown settings orchard"));
    }

    #[test]
    fn the_platform_comes_from_the_most_specific_choice() {
        let data = data();
        let dir = tempfile::tempdir().unwrap();
        let (here, root) = (dir.path(), data.path());
        let choose =
            |platforms: &[&str], device: Option<&'static str>, attached: Option<&'static str>| {
                let platforms: Vec<String> = platforms.iter().map(|p| (*p).to_owned()).collect();
                let choice = Choice {
                    platforms: &platforms,
                    device,
                    attached,
                    usb: None,
                };
                (
                    platform(here, root, &choice),
                    self::platforms(here, root, &choice),
                )
            };
        assert!(choose(&[], None, None)
            .0
            .unwrap_err()
            .message
            .contains("which platform?"));
        assert_eq!(choose(&[], None, Some("cc3")).0.unwrap(), "small");
        assert_eq!(choose(&[], Some("aa1"), Some("cc3")).0.unwrap(), "wide");
        assert_eq!(choose(&["small"], Some("aa1"), None).0.unwrap(), "small");
        assert!(choose(&["all"], None, None).0.is_err());
        assert_eq!(choose(&["all"], None, None).1.unwrap(), Platforms::All);
        assert_eq!(
            choose(&["wide", "small", "wide"], None, None).1.unwrap(),
            Platforms::Named(vec!["small".into(), "wide".into()])
        );
        assert!(choose(&["nope"], None, None)
            .1
            .unwrap_err()
            .message
            .contains("known: small, wide"));
        assert!(choose(&[], Some("zz9"), None)
            .0
            .unwrap_err()
            .message
            .contains("no hardware support describes zz9"));

        // The checkout's settings come before the attached board.
        std::fs::write(here.join(LOCAL), "platform = \"small\"\n").unwrap();
        assert_eq!(choose(&[], None, Some("aa1")).0.unwrap(), "small");
        assert_eq!(choose(&[], Some("aa1"), None).0.unwrap(), "wide");
        std::fs::write(here.join(LOCAL), "platform = \"wide\"\ndevice = \"cc3\"\n").unwrap();
        assert!(choose(&[], None, None)
            .1
            .unwrap_err()
            .message
            .contains("not a board of platform wide"));
    }

    #[test]
    fn the_board_is_the_chosen_one_of_the_platform() {
        let data = data();
        let dir = tempfile::tempdir().unwrap();
        let pick =
            |platform: &str, device: Option<&'static str>, attached: Option<&'static str>| {
                let choice = Choice {
                    platforms: &[],
                    device,
                    attached,
                    usb: None,
                };
                board(dir.path(), data.path(), platform, &choice)
            };
        assert_eq!(
            pick("wide", Some("bb2"), None).unwrap().as_deref(),
            Some("bb2")
        );
        assert_eq!(
            pick("wide", None, Some("bb2")).unwrap().as_deref(),
            Some("bb2")
        );
        assert_eq!(pick("wide", None, None).unwrap(), None);
        // An attached board of another platform does not choose.
        assert_eq!(pick("wide", None, Some("cc3")).unwrap(), None);
        assert!(pick("wide", Some("cc3"), None).is_err());
        assert!(pick("small", None, None)
            .unwrap_err()
            .message
            .contains("Cascadia board"));
    }
}
