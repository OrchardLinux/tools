//! Kernel platforms: one kernel build shared by every board that names the
//! platform in its `device.toml`. A platform is described by
//! `platforms/<name>.toml`: its architecture, image, upstream base, patch
//! series, config and the loader its payloads are for.
use crate::{invalid, plain_name, read_text};
use orchard_boards::catalog::Catalog;
use orchard_cache::{Code, Context, Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `[kernel]` table of a platform's descriptor; paths are relative to
/// the data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub platform: String,
    /// `arm64` or `arm`.
    pub arch: String,
    /// The kernel file a payload carries, such as `Image.gz`.
    pub image: String,
    pub dtbs: String,
    pub upstream: String,
    pub series: String,
    pub config: String,
    pub defconfig: String,
    pub branch: String,
    /// `m1n1` or `cascadia`.
    pub loader: String,
}

impl Descriptor {
    /// The fields that decide what a kernel build produces.
    pub fn build_fields(&self, defconfig: bool) -> Value {
        let mut fields = json!({"arch": self.arch, "image": self.image, "dtbs": self.dtbs});
        if defconfig {
            fields["defconfig"] = json!(self.defconfig);
        }
        fields
    }
}

/// The platform and device tree of a board, or a platform by its own name.
pub fn resolve(root: &Path, name: &str) -> Result<(String, Option<String>)> {
    if !plain_name(name) {
        return Err(invalid("invalid device or platform name"));
    }
    if root
        .join("devices")
        .join(name)
        .join("device.toml")
        .is_file()
    {
        let board = Catalog::open(root)?.board(name)?;
        return match (board.platform, board.dtb) {
            (Some(platform), Some(dtb)) => Ok((platform, Some(dtb))),
            _ => Err(invalid(format!(
                "{name} has no kernel yet: its device.toml describes identity only"
            ))),
        };
    }
    if root
        .join("platforms")
        .join(format!("{name}.toml"))
        .is_file()
    {
        return Ok((name.to_owned(), None));
    }
    Err(Error::new(
        Code::NotFound,
        format!("unknown device or platform: {name}"),
    ))
}

/// The platform and device tree of a board.
pub fn layout(root: &Path, board: &str) -> Result<(String, String)> {
    match resolve(root, board)? {
        (platform, Some(dtb)) => Ok((platform, dtb)),
        _ => Err(invalid(format!(
            "{board} is a kernel platform, not a device"
        ))),
    }
}

/// The checked descriptor of a platform, named itself or by one of its
/// boards.
pub fn descriptor(root: &Path, name: &str) -> Result<Descriptor> {
    let platform = resolve(root, name)?.0;
    let path = root.join("platforms").join(format!("{platform}.toml"));
    if !path.is_file() {
        return Err(Error::new(
            Code::NotFound,
            format!("missing platform descriptor: {}", path.display()),
        ));
    }
    let bad = |what: &str| Error::new(Code::Format, what.to_owned()).context(path.display());
    let file: toml::Table = toml::from_str(&read_text(&path)?).map_err(|e| bad(e.message()))?;
    let text = |table: &toml::Table, key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if file.get("schema").and_then(toml::Value::as_integer) != Some(1)
        || text(&file, "kind") != "hardware.platform"
        || text(&file, "name") != platform
    {
        return Err(bad(&format!(
            "expected schema 1, kind \"hardware.platform\", name \"{platform}\""
        )));
    }
    let empty = toml::Table::new();
    let kernel = file
        .get("kernel")
        .and_then(toml::Value::as_table)
        .unwrap_or(&empty);
    let descriptor = Descriptor {
        platform,
        arch: text(kernel, "arch"),
        image: text(kernel, "image"),
        dtbs: text(kernel, "dtbs"),
        upstream: text(kernel, "upstream"),
        series: text(kernel, "series"),
        config: text(kernel, "config"),
        defconfig: text(kernel, "defconfig"),
        branch: text(kernel, "branch"),
        loader: text(kernel, "loader"),
    };
    if !["arm64", "arm"].contains(&descriptor.arch.as_str()) {
        return Err(bad("kernel.arch must be arm64 or arm"));
    }
    let word = |value: &str, extra: &str| {
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    if !word(&descriptor.image, "_.-") {
        return Err(bad("invalid kernel.image"));
    }
    for (key, value) in [
        ("dtbs", &descriptor.dtbs),
        ("upstream", &descriptor.upstream),
        ("series", &descriptor.series),
        ("config", &descriptor.config),
    ] {
        inside(root, value).map_err(|_| bad(&format!("invalid kernel.{key}")))?;
        if key != "dtbs" && !root.join(value).is_file() {
            return Err(bad(&format!("kernel.{key} {value} does not exist")));
        }
    }
    if !descriptor.defconfig.ends_with("_defconfig") || !plain_name(&descriptor.defconfig) {
        return Err(bad("kernel.defconfig must be a <name>_defconfig"));
    }
    if !word(&descriptor.branch, "._/-") {
        return Err(bad("invalid kernel.branch"));
    }
    if !["m1n1", "cascadia"].contains(&descriptor.loader.as_str()) {
        return Err(bad("kernel.loader must be m1n1 or cascadia"));
    }
    Ok(descriptor)
}

/// A path from a descriptor, refused if it leaves the data directory.
fn inside(root: &Path, relative: &str) -> Result<PathBuf> {
    let plain = !relative.is_empty()
        && !relative.starts_with('/')
        && relative
            .split('/')
            .all(|part| !part.is_empty() && part != "..");
    if !plain {
        return Err(invalid("path leaves the data directory"));
    }
    Ok(root.join(relative))
}

/// The platform's patches in the order they apply: the series file names
/// layers under `kernel/`, each layer's patches sorted by path.
pub fn patches(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let series = root.join(descriptor(root, name)?.series);
    let kernel = root.join("kernel");
    let mut found = Vec::new();
    for line in read_text(&series)?.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let layer = inside(&kernel, line)
            .ok()
            .filter(|layer| layer.is_dir())
            .ok_or_else(|| {
                Error::new(
                    Code::Format,
                    format!("invalid patch layer in {}: {line}", series.display()),
                )
            })?;
        let mut layer_patches = Vec::new();
        collect(&layer, &mut layer_patches)?;
        layer_patches.sort();
        found.extend(layer_patches);
    }
    Ok(found)
}

fn collect(directory: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for item in std::fs::read_dir(directory).context(directory.display())? {
        let path = item?.path();
        if path.is_dir() {
            collect(&path, found)?;
        } else if path.extension().is_some_and(|e| e == "patch") {
            found.push(path);
        }
    }
    Ok(())
}

/// `platform → board → device tree` for every board the data describes
/// with a kernel.
pub fn boards_by_platform(root: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let catalog = Catalog::open(root)?;
    let mut known: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for name in catalog.boards()? {
        let Ok(board) = catalog.board(&name) else {
            continue;
        };
        if let (Some(platform), Some(dtb)) = (board.platform, board.dtb) {
            known.entry(platform).or_default().insert(name, dtb);
        }
    }
    Ok(known)
}

/// The boards of one platform.
pub fn boards(root: &Path, platform: &str) -> Result<BTreeMap<String, String>> {
    let mut known = boards_by_platform(root)?;
    known.remove(platform).ok_or_else(|| {
        let names: Vec<&str> = known.keys().map(String::as_str).collect();
        Error::new(
            Code::NotFound,
            format!("unknown platform {platform}; known: {}", names.join(", ")),
        )
    })
}

/// Every platform with a described board whose kernel is of `architecture`
/// (`arm64` or `arm`), sorted.
pub fn platforms(root: &Path, architecture: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for (platform, boards) in boards_by_platform(root)? {
        let first = boards.keys().next().expect("a platform listed for a board");
        if descriptor(root, first)?.arch == architecture {
            found.push(platform);
        }
    }
    Ok(found)
}

/// The upstream base a platform's series applies to: `url` and `commit`.
pub fn pin(root: &Path, name: &str) -> Result<(String, String)> {
    let lock = root.join(descriptor(root, name)?.upstream);
    let text = read_text(&lock)?;
    let value = |key: &str| {
        text.lines()
            .find_map(|line| {
                line.split_once('=')
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_owned())
            })
            .ok_or_else(|| Error::new(Code::Format, format!("no {key}")).context(lock.display()))
    };
    Ok((value("url")?, value("commit")?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small data directory: two platforms, three boards, one without a
    /// kernel.
    pub fn data() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, text: &str| {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), text).unwrap();
        };
        for (board, platform, chip) in [
            ("aa1", "wide", 0x8010),
            ("bb2", "wide", 0x8015),
            ("cc3", "small", 0x8950),
        ] {
            write(
                &format!("devices/{board}/device.toml"),
                &format!(
                    "[device]\nname = \"{board}\"\napple_products = [\"X{chip},1\"]\ndt_compatible = [\"apple,{board}\"]\n\
                     [match.dfu]\nchip = {chip}\nboard = 1\n[support]\nstatus = \"experimental\"\n\
                     [kernel]\nplatform = \"{platform}\"\ndtb = \"t{chip}-{board}\"\n"
                ),
            );
        }
        write(
            "devices/dd4/device.toml",
            "[device]\nname = \"dd4\"\napple_products = [\"Y1,1\"]\n[match.dfu]\nchip = 1\nboard = 2\n[support]\nstatus = \"identity\"\n",
        );
        for (platform, arch, loader, image) in [
            ("wide", "arm64", "m1n1", "Image.gz"),
            ("small", "arm", "cascadia", "zImage"),
        ] {
            write(
                &format!("platforms/{platform}.toml"),
                &format!(
                    "schema = 1\nkind = \"hardware.platform\"\nname = \"{platform}\"\n[kernel]\narch = \"{arch}\"\n\
                     image = \"{image}\"\ndtbs = \"arch/{arch}/boot/dts/apple\"\nupstream = \"kernel/upstream.lock\"\n\
                     series = \"kernel/series/{platform}\"\nconfig = \"kernel/configs/{platform}/config\"\n\
                     defconfig = \"orchard_{platform}_defconfig\"\nbranch = \"main\"\nloader = \"{loader}\"\n"
                ),
            );
            write(
                &format!("kernel/series/{platform}"),
                "# layers\ncommon\n\nboards/x  # one board\n",
            );
            write(&format!("kernel/configs/{platform}/config"), "CONFIG_X=y\n");
        }
        write(
            "kernel/upstream.lock",
            "url=https://example.org/linux.git\ncommit=0123456789abcdef0123456789abcdef01234567\n",
        );
        write("kernel/common/0002-b.patch", "b");
        write("kernel/common/0001-a.patch", "a");
        write("kernel/common/sub/0000-z.patch", "z");
        write("kernel/common/README", "not a patch");
        write("kernel/boards/x/0001-x.patch", "x");
        write("kernel/build", "#!/bin/sh\n");
        dir
    }

    #[test]
    fn a_board_names_its_platform_and_device_tree() {
        let dir = data();
        assert_eq!(
            layout(dir.path(), "aa1").unwrap(),
            ("wide".into(), "t32784-aa1".into())
        );
        assert_eq!(
            resolve(dir.path(), "small").unwrap(),
            ("small".into(), None)
        );
        assert!(layout(dir.path(), "wide")
            .unwrap_err()
            .message
            .contains("a kernel platform"));
        assert!(resolve(dir.path(), "dd4")
            .unwrap_err()
            .message
            .contains("no kernel yet"));
        assert_eq!(resolve(dir.path(), "zz9").unwrap_err().code, Code::NotFound);
    }

    #[test]
    fn platforms_list_their_boards() {
        let dir = data();
        let known = boards_by_platform(dir.path()).unwrap();
        assert_eq!(known["wide"].keys().collect::<Vec<_>>(), ["aa1", "bb2"]);
        assert_eq!(platforms(dir.path(), "arm64").unwrap(), ["wide"]);
        assert_eq!(platforms(dir.path(), "arm").unwrap(), ["small"]);
        assert!(boards(dir.path(), "other")
            .unwrap_err()
            .message
            .contains("known: small, wide"));
    }

    #[test]
    fn a_descriptor_is_checked_and_patches_come_in_series_order() {
        let dir = data();
        let described = descriptor(dir.path(), "bb2").unwrap();
        assert_eq!(
            (described.platform.as_str(), described.loader.as_str()),
            ("wide", "m1n1")
        );
        assert_eq!(pin(dir.path(), "wide").unwrap().1.len(), 40);
        let names: Vec<String> = patches(dir.path(), "wide")
            .unwrap()
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path().join("kernel"))
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            [
                "common/0001-a.patch",
                "common/0002-b.patch",
                "common/sub/0000-z.patch",
                "boards/x/0001-x.patch"
            ]
        );
        std::fs::remove_file(dir.path().join("kernel/configs/small/config")).unwrap();
        assert!(descriptor(dir.path(), "cc3")
            .unwrap_err()
            .message
            .contains("does not exist"));
    }
}
