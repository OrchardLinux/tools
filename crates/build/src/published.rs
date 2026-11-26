//! Published components: a build takes orchard's nightly kernel, m1n1 and
//! static tools instead of building them, when they were built from the
//! same state.
//!
//! A component's source key is a hash of the files that decide it: for a
//! platform's kernel the upstream lock, the series and every patch it
//! applies, the config, the toolchain recipes, the descriptor's build
//! fields and the producer version; for m1n1 and the tools their recipes.
//! CI publishes the keys of what it built (`component-keys.json`, listed in
//! `SHA256SUMS`); a build whose data gives the same key downloads that
//! asset, checks it and uses it. Any other key, no network, or
//! `ORCHARD_PUBLISHED=off` (`ORCHARD_KERNEL=local` for kernels only): it is
//! built here.
use crate::components::TOOLS;
use crate::platform::{descriptor, patches};
use crate::{json, recipes, Build, RELEASES};
use orchard_cache::component::Key;
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::{Code, Context, Error, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const KEYS: &str = "component-keys.json";
const SUMS: &str = "SHA256SUMS";
const NIGHTLY: &str = "nightly";
const LIMIT: u64 = 2 << 30;

/// What decides a platform's compiled kernel, from the data's files alone.
pub fn source_inputs(build: &Build, platform: &str) -> Result<Value> {
    let root = &build.root;
    let described = descriptor(root, platform)?;
    let mut paths = vec![
        described.upstream.clone(),
        described.series.clone(),
        described.config.clone(),
        "kernel/Containerfile".to_owned(),
        "kernel/install-toolchain".to_owned(),
    ];
    for patch in patches(root, platform)? {
        paths.push(
            patch
                .strip_prefix(root)
                .unwrap_or(&patch)
                .to_string_lossy()
                .into_owned(),
        );
    }
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    Ok(json!({
        "platform": platform,
        "build": described.build_fields(false),
        "recipes": recipes::object(root, &paths)?,
        "producer": build.contract.kernel_producer,
    }))
}

pub fn source_key(build: &Build, platform: &str) -> Result<String> {
    Ok(json::hash(&source_inputs(build, platform)?))
}

/// The keys of the components every platform shares: `(m1n1, tools)`.
pub fn shared_keys(build: &Build) -> Result<(String, String)> {
    let key = |paths: Vec<String>| -> Result<String> {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        Ok(json::hash(
            &json!({ "recipes": recipes::object(&build.root, &paths)? }),
        ))
    };
    Ok((
        key(build.contract.m1n1.clone())?,
        key(build.contract.tools.clone())?,
    ))
}

/// `component-keys.json` for `platforms`, as CI publishes it.
pub fn keys_document(build: &Build, platforms: &[String]) -> Result<Value> {
    let (m1n1, tools) = shared_keys(build)?;
    let mut rows = serde_json::Map::new();
    for platform in platforms {
        rows.insert(
            platform.clone(),
            json!({"key": source_key(build, platform)?, "asset": format!("{platform}-components.tar.gz")}),
        );
    }
    Ok(json!({
        "schema": 1,
        "platforms": rows,
        "shared": {
            "m1n1": {"key": m1n1, "asset": "m1n1.bin", "files": ["m1n1.bin", "m1n1-idevice.macho", "monitor-stub.macho", "m1n1-armv7.macho"]},
            "tools": {"key": tools, "asset": "tool-components.tar.gz"},
        },
    }))
}

/// A small published file, whole.
pub fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut remote = Http::probe(url, Duration::from_secs(30))?;
    let size = remote.size();
    if size > limit {
        return Err(Error::new(
            Code::TooLarge,
            format!("{url} is larger than expected"),
        ));
    }
    remote.read_at(0, size as usize)
}

/// `name → sha256` of a `SHA256SUMS` file.
pub fn sums(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (hash, name) = (parts.next()?, parts.next()?);
            (parts.next().is_none()
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| (name.trim_start_matches("./").to_owned(), hash.to_owned()))
        })
        .collect()
}

/// The nightly's row for a component, when it is of the state `key` names:
/// `(tag, asset, sha256)`.
fn offered(
    what: &str,
    key: &str,
    select: &dyn Fn(&Value) -> Option<Value>,
    say: &dyn Fn(&str),
) -> Option<(String, String, String)> {
    let found = (|| -> Result<Option<(String, String, String)>> {
        let pointer: Value = parse(&fetch(
            &format!("{RELEASES}/{NIGHTLY}/latest.json"),
            64 << 10,
        )?)?;
        let tag = pointer["name"]
            .as_str()
            .ok_or_else(|| Error::new(Code::Format, "the nightly pointer has no name"))?;
        let listed = sums(&String::from_utf8_lossy(&fetch(
            &format!("{RELEASES}/{tag}/{SUMS}"),
            1 << 20,
        )?));
        let keys = fetch(&format!("{RELEASES}/{tag}/{KEYS}"), 1 << 20)?;
        if listed.get(KEYS) != Some(&json::sha256(&keys)) {
            say(&format!(
                "The {tag} {KEYS} does not match its {SUMS}; building {what}"
            ));
            return Ok(None);
        }
        let entry = select(&parse(&keys)?);
        let Some(entry) = entry.filter(|e| e["key"] == key) else {
            say(&format!(
                "The {tag} {what} is of another state; building it"
            ));
            return Ok(None);
        };
        let asset = entry["asset"].as_str().unwrap_or("").to_owned();
        match listed.get(&asset) {
            Some(checksum) if !asset.contains('/') => {
                Ok(Some((tag.to_owned(), asset, checksum.clone())))
            }
            _ => Ok(None),
        }
    })();
    match found {
        Ok(found) => found,
        Err(error) => {
            say(&format!(
                "No published {what} ({}); building it",
                error.message
            ));
            None
        }
    }
}

fn parse(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|e| Error::new(Code::Format, e.to_string()))
}

/// The cache entry `name` of the published component `what`, or `None` to
/// build it. `unpack(asset file, stage)` fills the entry from the
/// downloaded, checked asset.
fn published(
    build: &Build,
    name: &str,
    what: &str,
    key: &str,
    select: &dyn Fn(&Value) -> Option<Value>,
    unpack: &dyn Fn(&Path, &Path) -> Result<()>,
) -> Result<Option<PathBuf>> {
    if std::env::var("ORCHARD_PUBLISHED").is_ok_and(|v| v == "off") {
        return Ok(None);
    }
    let offer = if build.cache.dir().join(name).join(key).is_dir() {
        None
    } else {
        match offered(what, key, select, build.say) {
            Some(offer) => Some(offer),
            None => return Ok(None),
        }
    };
    let entry = build.component(
        name,
        &mut || Ok(Key::plain(key)),
        &mut |stage| {
            let (tag, asset, checksum) = offer.clone().expect("an entry that is not there has an offer");
            build.say(format!("Using the {tag} {what}: its source key matches this data"));
            build.explain.note(format!(
                "{what}: downloaded from orchard's {tag} ({asset}, sha256 {checksum}), published for source key \
                 {key}; ORCHARD_PUBLISHED=off builds it instead"
            ));
            let file = build.store.fetch(&format!("{RELEASES}/{tag}/{asset}"), &checksum, LIMIT)?;
            unpack(&file, stage)?;
            if asset.ends_with(".tar.gz") {
                // The entry holds what the archive held; it is fetched again if ever needed.
                let _ = std::fs::remove_file(&file);
            }
            let record = json!({"tag": tag, "asset": asset, "sha256": checksum, "source_key": key});
            crate::write_text(&stage.join("published.json"), &json::pretty(&record))
        },
    )?;
    Ok(Some(entry))
}

/// A platform's published kernel directory for this data's source key, or
/// `None` to compile.
pub fn kernel(build: &Build, platform: &str) -> Result<Option<PathBuf>> {
    if std::env::var("ORCHARD_KERNEL").is_ok_and(|v| v == "local") {
        return Ok(None);
    }
    let entry = published(
        build,
        &format!("kernel-published-{platform}"),
        &format!("{platform} kernel"),
        &source_key(build, platform)?,
        &|keys| keys["platforms"].get(platform).cloned(),
        &|archive, stage| extract(archive, "components/kernel/", &stage.join("kernel")),
    )?;
    Ok(entry.map(|e| e.join("kernel")))
}

/// orchard's published `m1n1.bin` for this data's m1n1 recipe, or `None`
/// to build it.
pub fn m1n1(build: &Build) -> Result<Option<PathBuf>> {
    let entry = published(
        build,
        "m1n1-published",
        "m1n1",
        &shared_keys(build)?.0,
        &|keys| keys["shared"].get("m1n1").cloned(),
        &|file, stage| crate::link_or_copy(file, &stage.join("m1n1.bin")),
    )?;
    Ok(entry.map(|e| e.join("m1n1.bin")))
}

/// The published static tools directory for this data's tool recipes, or
/// `None` to build them.
pub fn tools(build: &Build) -> Result<Option<PathBuf>> {
    published(
        build,
        "tools-published",
        "static tools",
        &shared_keys(build)?.1,
        &|keys| keys["shared"].get("tools").cloned(),
        &|archive, stage| {
            let unpacked = tempfile::Builder::new()
                .prefix("published-")
                .tempdir_in(stage)?;
            extract(archive, "components/boot/", unpacked.path())?;
            for name in TOOLS {
                let tool = unpacked.path().join(name);
                if !tool.is_file() {
                    return Err(Error::new(
                        Code::Format,
                        format!("the published tools lack {name}"),
                    ));
                }
                std::fs::rename(&tool, stage.join(name)).context(tool.display())?;
            }
            Ok(())
        },
    )
}

/// Unpacks the regular files and directories under `prefix` of a gzipped
/// component tarball into `into`, without `prefix`. Anything else under it
/// is refused.
pub fn extract(archive: &Path, prefix: &str, into: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let bad = |what: &str| Error::new(Code::Format, what.to_owned()).context(archive.display());
    let mut tar =
        flate2::read::GzDecoder::new(std::fs::File::open(archive).context(archive.display())?);
    let (mut long, mut found) = (None::<String>, false);
    loop {
        let mut header = [0u8; 512];
        tar.read_exact(&mut header)
            .map_err(|_| bad("truncated archive"))?;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let field = |range: std::ops::Range<usize>| {
            let bytes = &header[range];
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
        };
        let size = u64::from_str_radix(&field(124..136), 8)
            .map_err(|_| bad("unreadable archive entry size"))?;
        let padded = size.div_ceil(512) * 512;
        let mut name = match (field(345..500), field(0..100)) {
            (prefix, name) if prefix.is_empty() => name,
            (prefix, name) => format!("{prefix}/{name}"),
        };
        let kind = header[156];
        if kind == b'g' {
            // A global header, which GitHub's archives start with.
            std::io::copy(&mut (&mut tar).take(padded), &mut std::io::sink())?;
            continue;
        }
        if matches!(kind, b'x' | b'L') {
            let mut data = vec![0; padded as usize];
            tar.read_exact(&mut data)
                .map_err(|_| bad("truncated archive"))?;
            let text = String::from_utf8_lossy(&data[..size as usize]).into_owned();
            long = match kind {
                b'L' => Some(text.trim_end_matches('\0').to_owned()),
                _ => text
                    .lines()
                    .find_map(|line| line.split_once(" path=").map(|(_, path)| path.to_owned()))
                    .or(long),
            };
            continue;
        }
        if let Some(path) = long.take() {
            name = path;
        }
        let name = name.trim_start_matches("./").to_owned();
        let inside = name.strip_prefix(prefix).map(str::to_owned);
        let Some(relative) = inside else {
            std::io::copy(&mut (&mut tar).take(padded), &mut std::io::sink())?;
            continue;
        };
        let inside =
            !Path::new(&relative).is_absolute() && relative.split('/').all(|part| part != "..");
        if !inside || !matches!(kind, b'0' | 0 | b'5') {
            return Err(bad(&format!("unexpected member {name}")));
        }
        found = true;
        let target = into.join(&relative);
        if kind == b'5' {
            std::fs::create_dir_all(&target).context(target.display())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).context(parent.display())?;
        }
        let mut file = std::fs::File::create(&target).context(target.display())?;
        if std::io::copy(&mut (&mut tar).take(size), &mut file)? != size {
            return Err(bad("truncated archive"));
        }
        std::io::copy(&mut (&mut tar).take(padded - size), &mut std::io::sink())?;
        let executable = u32::from_str_radix(&field(100..108), 8).unwrap_or(0) & 0o111 != 0;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
            .context(target.display())?;
    }
    if !found {
        return Err(bad(&format!("no {prefix} in the archive")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gzipped tar with regular files and one entry of another type.
    pub fn tarball(path: &Path, files: &[(&str, &[u8], u32)], link: Option<&str>) {
        use std::io::Write;
        let mut tar = Vec::new();
        let mut entry = |name: &str, data: &[u8], mode: u32, kind: u8| {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
            header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            header[156] = kind;
            header[148..156].copy_from_slice(b"        ");
            let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            tar.extend_from_slice(&header);
            tar.extend_from_slice(data);
            tar.extend(std::iter::repeat_n(
                0,
                data.len().div_ceil(512) * 512 - data.len(),
            ));
        };
        for (name, data, mode) in files {
            entry(name, data, *mode, b'0');
        }
        if let Some(name) = link {
            entry(name, b"", 0o777, b'2');
        }
        tar.extend([0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap();
    }

    #[test]
    fn nothing_is_written_outside_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        let outside = dir.path().join("outside");
        let absolute = format!("components/kernel/{}", outside.display());
        for name in [absolute.as_str(), "components/kernel/../../outside"] {
            tarball(&archive, &[(name, b"x", 0o644)], None);
            let into = dir.path().join("into/deep");
            let error = extract(&archive, "components/kernel/", &into).unwrap_err();
            assert_eq!(error.code, Code::Format, "{name}");
            assert!(!outside.exists(), "{name}");
        }
    }

    #[test]
    fn only_the_named_part_of_a_bundle_is_unpacked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        let big = vec![7u8; 1500];
        tarball(
            &archive,
            &[
                ("components/kernel/Image.gz", &big, 0o644),
                ("components/kernel/lib/tool", b"x", 0o755),
                ("components/boot/busybox", b"b", 0o755),
            ],
            None,
        );
        let into = dir.path().join("out");
        extract(&archive, "components/kernel/", &into).unwrap();
        assert_eq!(std::fs::read(into.join("Image.gz")).unwrap(), big);
        assert_eq!(
            std::fs::metadata(into.join("lib/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(!into.join("busybox").exists());
        assert!(extract(&archive, "components/other/", &into)
            .unwrap_err()
            .message
            .contains("no components/other/"));
    }

    #[test]
    fn a_link_in_a_bundle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        tarball(
            &archive,
            &[("components/kernel/a", b"a", 0o644)],
            Some("components/kernel/link"),
        );
        let error = extract(&archive, "components/kernel/", &dir.path().join("out")).unwrap_err();
        assert!(
            error
                .message
                .contains("unexpected member components/kernel/link"),
            "{error}"
        );
    }

    #[test]
    fn checksum_lists_are_read_by_name() {
        let hash = "a".repeat(64);
        let listed = sums(&format!(
            "{hash}  ./pongo.bin\nshort  x\n{hash} m1n1.bin extra\n"
        ));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed["pongo.bin"], hash);
    }
}
