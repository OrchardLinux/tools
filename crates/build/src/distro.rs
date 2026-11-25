//! A distribution checkout as a project: its own script builds the release.
//!
//! `orchard.toml` with a `[release]` table at the root of a distribution's
//! checkout makes it one. `build` runs its entry as
//!
//! ```text
//! ENTRY --version VERSION --output out/releases/VERSION [--platform NAME ...]
//! ```
//!
//! with `ORCHARD_CHECKOUT` naming orchard's data and producers, `ORCHARD_TOOL`
//! this tool and `ORCHARD_PYTHON` the Python for a `.py` entry. The entry
//! builds what is missing and writes a release directory with the
//! `index.toml` that pins it. The index and every file it pins are then
//! checked, and the build is recorded in `out/releases/latest.json`. The
//! distribution owns how its image is made; orchard owns the release format.
use crate::explain::quote;
use crate::project::{Platforms, Release};
use crate::{invalid, read_text, write_text, Build};
use orchard_cache::component::digest;
use orchard_cache::{Code, Context, Error, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LATEST: &str = "out/releases/latest.json";

/// `dev-YYYYMMDD-HHMMSS` (UTC): a new local version for every build.
pub fn default_version(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86400, seconds % 86400);
    // Days since 1970 as a date of the proleptic Gregorian calendar.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "dev-{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn valid_version(version: &str) -> bool {
    let mut chars = version.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && version.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// What a distribution build is given.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Default: a new `dev-…` version.
    pub version: Option<String>,
    /// For an entry that selects platforms; `None` for one that builds all.
    pub platforms: Option<Platforms>,
    pub kernel_tree: Option<PathBuf>,
    /// A checkout of the Orchard package repository.
    pub packages: Option<PathBuf>,
}

/// A built release.
#[derive(Debug, Clone)]
pub struct Built {
    pub name: String,
    pub version: String,
    pub index: PathBuf,
    /// The call that built it, as a script.
    pub commands: PathBuf,
}

/// Runs the build entry of the distribution at `root` into
/// `out/releases/VERSION`, checks the release and records it.
pub fn build(build: &Build, root: &Path, distro: &Release, request: &Request) -> Result<Built> {
    let version = request
        .version
        .clone()
        .unwrap_or_else(|| default_version(SystemTime::now()));
    if !valid_version(&version) {
        return Err(invalid(
            "a version is letters, digits and . _ + -, starting with a letter or digit",
        ));
    }
    let output = root.join("out/releases").join(&version);
    if std::fs::symlink_metadata(&output).is_ok() {
        return Err(invalid(format!(
            "{} exists; choose another --version",
            output.display()
        )));
    }
    let releases = output.parent().expect("inside out/releases");
    std::fs::create_dir_all(releases).context(releases.display())?;
    let entry = &distro.build;
    let mut argv: Vec<String> = Vec::new();
    if entry.extension().is_some_and(|e| e == "py") {
        argv.push(build.python.to_string_lossy().into_owned());
    }
    argv.extend([
        entry.to_string_lossy().into_owned(),
        "--version".into(),
        version.clone(),
        "--output".into(),
    ]);
    argv.push(output.to_string_lossy().into_owned());
    match (&request.platforms, distro.selects_platform) {
        (Some(_), false) => {
            return Err(invalid(format!(
                "{}'s build entry builds every platform and takes no --platform or --device; update the \
                 distribution (orchard.toml [release] selects-platform = true)",
                distro.name
            )))
        }
        (None, true) => {
            return Err(invalid(format!(
                "{} selects its platforms: choose with --platform, --device, orchard.local.toml or the attached board",
                distro.name
            )))
        }
        (Some(Platforms::All), true) => argv.extend(["--platform".to_owned(), "all".to_owned()]),
        (Some(Platforms::Named(names)), true) => {
            argv.extend(names.iter().flat_map(|name| ["--platform".to_owned(), name.clone()]));
        }
        (None, false) => {}
    }
    build.say(format!("+ {}", argv.join(" ")));
    let tool = std::env::current_exe().context("this program")?;
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(root)
        .env("ORCHARD_PYTHON", &build.python)
        .env("ORCHARD_CHECKOUT", &build.root)
        .env("ORCHARD_CACHE_DIR", build.cache.root())
        // The entry's own `inputs` works with this build's cache and data.
        .env("ORCHARD_CACHE", build.cache.root())
        .env_remove("ORCHARD_DATA")
        .env("ORCHARD_TOOL", &tool);
    let mut shown = String::new();
    if let Some(tree) = &request.kernel_tree {
        // The inputs command, which the entry runs, builds the kernels from this tree.
        command.env("ORCHARD_KERNEL_TREE", tree);
        build.say(format!("Kernel from the tree {}", tree.display()));
        shown.push_str(&format!(
            "ORCHARD_KERNEL_TREE={} ",
            quote(&tree.to_string_lossy())
        ));
    }
    if let Some(packages) = &request.packages {
        // The entry builds its image's [orchard] packages from this checkout.
        command.env("ORCHARD_PACKAGES", packages);
        build.say(format!(
            "Orchard packages: recipes that differ from the published ones are built from {} \
             (packages = false in orchard.local.toml: published only)",
            packages.display()
        ));
        shown.push_str(&format!(
            "ORCHARD_PACKAGES={} ",
            quote(&packages.to_string_lossy())
        ));
    }
    // What ran, as the plain command: the distribution composes the rest itself.
    let commands = releases.join(format!("{version}.commands.sh"));
    let words: Vec<String> = argv.iter().map(|a| quote(a)).collect();
    write_text(
        &commands,
        &format!(
            "#!/bin/sh\n# What orchard build ran for this release (--explain prints it); the build script\n\
             # runs orchard's producers itself, from the data ORCHARD_CHECKOUT names; how each component was\n\
             # got (built, reused, or downloaded from a nightly) is in the inputs document, under \"produced\".\n\
             set -eu\ncd {}\nunset ORCHARD_DATA\n{shown}ORCHARD_CACHE={} ORCHARD_CHECKOUT={} ORCHARD_TOOL={} {}\n",
            quote(&root.to_string_lossy()),
            quote(&build.cache.root().to_string_lossy()),
            quote(&build.root.to_string_lossy()),
            quote(&tool.to_string_lossy()),
            words.join(" ")
        ),
    )?;
    let running = crate::Running::new();
    let status = crate::to_stderr(&mut command)
        .status()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&argv[0]))?;
    drop(running);
    crate::interrupted()?;
    let shown_entry = entry.strip_prefix(root).unwrap_or(entry).display();
    if !status.success() {
        return Err(Error::new(
            Code::Build,
            format!("{shown_entry} failed ({status})"),
        ));
    }
    let index = output.join("index.toml");
    if !index.is_file() {
        return Err(Error::new(
            Code::Build,
            format!("{shown_entry} wrote no {}", index.display()),
        ));
    }
    for (name, built_as) in verify_index(&index)? {
        if name != distro.name {
            return Err(invalid(format!(
                "the release is {name:?}, but orchard.toml names {:?}",
                distro.name
            )));
        }
        if built_as != version {
            return Err(invalid(format!(
                "the release says version {built_as:?}, but was built as {version:?}"
            )));
        }
    }
    let record = json!({"version": version, "index": format!("{version}/index.toml")});
    write_text(&root.join(LATEST), &crate::json::pretty(&record))?;
    build.say(format!("{} {version}: {}", distro.name, index.display()));
    Ok(Built {
        name: distro.name.clone(),
        version,
        index,
        commands,
    })
}

/// The index of a checkout's last build.
pub fn latest(root: &Path) -> Result<PathBuf> {
    let record = root.join(LATEST);
    if !record.is_file() {
        return Err(
            Error::new(Code::NotFound, format!("no build in {}", root.display()))
                .hint("run orchard2 build"),
        );
    }
    let value: serde_json::Value = serde_json::from_str(&read_text(&record)?)
        .map_err(|e| Error::new(Code::Format, e.to_string()))?;
    let index = record
        .parent()
        .expect("inside out/releases")
        .join(value["index"].as_str().unwrap_or(""));
    if !index.is_file() {
        return Err(Error::new(Code::NotFound, "the last build is gone").hint("run orchard2 build"));
    }
    Ok(index)
}

fn format_error(path: &Path, message: impl Into<String>) -> Error {
    Error::new(Code::Format, message.into()).context(path.display())
}

fn load(path: &Path, kind: &str) -> Result<toml::Table> {
    let table: toml::Table =
        toml::from_str(&read_text(path)?).map_err(|e| format_error(path, e.message()))?;
    if table.get("kind").and_then(toml::Value::as_str) != Some(kind) {
        return Err(format_error(path, format!("expected a {kind} document")));
    }
    Ok(table)
}

/// The file a receipt pins, inside `bundle`: a plain relative path with no
/// links on the way, of the pinned size and SHA-256.
fn verify_file(receipt: &toml::Value, origin: &Path, bundle: &Path) -> Result<PathBuf> {
    let Some(relative) = receipt.get("file").and_then(toml::Value::as_str) else {
        return Err(format_error(origin, "a local release pins files, not URLs"));
    };
    let normal = !relative.contains(['\\', ':'])
        && !relative.chars().any(|c| c.is_control())
        && relative
            .split('/')
            .all(|part| !["", ".", ".."].contains(&part));
    if !normal {
        return Err(format_error(
            origin,
            format!("{relative}: expected a normalized relative path"),
        ));
    }
    let mut path = bundle.to_owned();
    for part in relative.split('/') {
        path.push(part);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink()) {
            return Err(format_error(&path, "linked release file refused"));
        }
    }
    let size = std::fs::metadata(&path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len());
    let pinned = receipt.get("bytes").and_then(toml::Value::as_integer);
    if size.is_none() || size.map(|s| s as i64) != pinned {
        return Err(format_error(&path, "missing file or byte count differs"));
    }
    if Some(digest(&path)?.as_str()) != receipt.get("sha256").and_then(toml::Value::as_str) {
        return Err(format_error(&path, "SHA-256 differs"));
    }
    Ok(path)
}

/// Every file a release manifest pins.
fn manifest_receipts(manifest: &toml::Table) -> Vec<&toml::Value> {
    let mut found = Vec::new();
    for name in [
        "root",
        "boot",
        "pongo",
        "logo",
        "m1n1",
        "install_tools",
        "board_data",
    ] {
        found.extend(manifest.get(name));
    }
    found.extend(manifest.get("root").and_then(|root| root.get("map")));
    for table in ["cascadia", "platforms"] {
        for row in manifest
            .get(table)
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|t| t.values())
        {
            match table {
                "cascadia" => found.push(row),
                _ => found.extend(["boot", "layer"].iter().filter_map(|name| row.get(name))),
            }
        }
    }
    found
}

/// Checks a local release: the index, each manifest it pins and every file
/// those pin. Returns each manifest's distro and version.
pub fn verify_index(index: &Path) -> Result<Vec<(String, String)>> {
    let bundle = index.parent().expect("an index is in a directory");
    let document = load(index, "orchard.index")?;
    let text = |table: &toml::Table, key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut pinned: Vec<&toml::Value> = document.get("release").into_iter().collect();
    pinned.extend(
        document
            .get("releases")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|t| t.values()),
    );
    if document.get("release").is_none() {
        return Err(format_error(index, "no [release]"));
    }
    let mut found = Vec::new();
    for receipt in pinned {
        let path = verify_file(receipt, index, bundle)?;
        let manifest = load(&path, "orchard.install")?;
        if text(&manifest, "distro") != text(&document, "distro") {
            return Err(format_error(
                &path,
                "the manifest does not match the index's distro",
            ));
        }
        for file in manifest_receipts(&manifest) {
            verify_file(file, &path, bundle)?;
        }
        found.push((text(&manifest, "distro"), text(&manifest, "version")));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_development_version_is_the_utc_time() {
        let at = |seconds: u64| default_version(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "dev-19700101-000000");
        assert_eq!(at(951_782_400), "dev-20000229-000000");
        assert_eq!(at(1_795_564_799), "dev-20261124-235959");
        assert!(valid_version(&at(1_795_564_799)));
        assert!(!valid_version("-x") && !valid_version("a/b") && !valid_version(""));
    }

    fn release(dir: &Path) -> PathBuf {
        let pin = |name: &str, data: &[u8]| {
            std::fs::write(dir.join(name), data).unwrap();
            format!(
                "{{ file = \"{name}\", bytes = {}, sha256 = \"{}\" }}",
                data.len(),
                crate::json::sha256(data)
            )
        };
        let manifest = format!(
            "schema = 2\nkind = \"orchard.install\"\ndistro = \"demo\"\nversion = \"1.0\"\nroot = {}\npongo = {}\n\
             [platforms.wide]\nboards = [\"aa1\"]\nlayer = {}\n",
            pin("root.blocks.zst", b"root"),
            pin("pongo.bin", b"pongo"),
            pin("platform-wide.tar.gz", b"layer"),
        );
        let index = format!(
            "schema = 2\nkind = \"orchard.index\"\ndistro = \"demo\"\nrelease = {}\n",
            pin("release.toml", manifest.as_bytes())
        );
        std::fs::write(dir.join("index.toml"), index).unwrap();
        dir.join("index.toml")
    }

    #[test]
    fn a_release_is_checked_down_to_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let index = release(dir.path());
        assert_eq!(
            verify_index(&index).unwrap(),
            [("demo".to_owned(), "1.0".to_owned())]
        );
        std::fs::write(dir.path().join("platform-wide.tar.gz"), b"other").unwrap();
        assert!(verify_index(&index)
            .unwrap_err()
            .message
            .contains("SHA-256 differs"));
        std::fs::write(dir.path().join("platform-wide.tar.gz"), b"longer layer").unwrap();
        assert!(verify_index(&index)
            .unwrap_err()
            .message
            .contains("byte count differs"));
        std::fs::remove_file(dir.path().join("pongo.bin")).unwrap();
        assert!(verify_index(&index)
            .unwrap_err()
            .message
            .contains("pongo.bin"));
    }

    #[test]
    fn a_manifest_of_another_distro_or_an_edited_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let index = release(dir.path());
        let text = std::fs::read_to_string(&index)
            .unwrap()
            .replace("distro = \"demo\"", "distro = \"other\"");
        std::fs::write(&index, text).unwrap();
        assert!(verify_index(&index)
            .unwrap_err()
            .message
            .contains("does not match the index's distro"));
        let index = release(dir.path());
        let manifest = dir.path().join("release.toml");
        let edited = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.0", "2.0");
        std::fs::write(&manifest, edited).unwrap();
        assert!(verify_index(&index)
            .unwrap_err()
            .message
            .contains("SHA-256 differs"));
    }

    /// A build entry that writes a release of `name` and records what it
    /// was given.
    const ENTRY: &str = r#"
import hashlib, os, pathlib, sys
args = sys.argv[1:]
version, out = args[args.index('--version') + 1], pathlib.Path(args[args.index('--output') + 1])
if os.environ.get('FAIL'):
    sys.exit(3)
out.mkdir(parents=True)
def pin(name, data):
    (out / name).write_bytes(data)
    return '{ file = "%s", bytes = %d, sha256 = "%s" }' % (name, len(data), hashlib.sha256(data).hexdigest())
manifest = 'schema = 2\nkind = "orchard.install"\ndistro = "NAME"\nversion = "%s"\nroot = %s\n' % (version, pin('root', b'r'))
(out / 'index.toml').write_text('schema = 2\nkind = "orchard.index"\ndistro = "NAME"\nrelease = %s\n'
                                % pin('release.toml', manifest.encode()))
given = [os.environ['ORCHARD_CHECKOUT'], os.environ['ORCHARD_TOOL'], os.environ.get('ORCHARD_KERNEL_TREE', '-'), ' '.join(args[4:])]
(out / 'given').write_text('\n'.join(given))
"#;

    #[test]
    fn a_distributions_entry_builds_its_release() {
        if crate::python().is_err() {
            return;
        }
        let data = crate::platform::tests::data();
        let (cache, checkout) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let store = orchard_cache::store::Store::open(Some(cache.path())).unwrap();
        let say = |_: &str| {};
        let build = Build::new(data.path(), &store, &say).unwrap();
        let root = checkout.path().canonicalize().unwrap();
        std::fs::write(root.join("build.py"), ENTRY.replace("NAME", "demo")).unwrap();
        let distro = |name: &str, selects_platform: bool| Release {
            name: name.to_owned(),
            build: root.join("build.py"),
            selects_platform,
        };
        let request = Request {
            version: Some("1.0".into()),
            platforms: Some(Platforms::Named(vec!["small".into(), "wide".into()])),
            kernel_tree: Some(PathBuf::from("/trees/linux")),
            packages: None,
        };
        let built = super::build(&build, &root, &distro("demo", true), &request).unwrap();
        assert_eq!(built.index, root.join("out/releases/1.0/index.toml"));
        assert_eq!(latest(&root).unwrap(), built.index);
        let given = std::fs::read_to_string(root.join("out/releases/1.0/given")).unwrap();
        let given: Vec<&str> = given.lines().collect();
        assert_eq!(given[0], build.root.to_string_lossy());
        assert!(Path::new(given[1]).is_file());
        assert_eq!(
            &given[2..],
            ["/trees/linux", "--platform small --platform wide"]
        );
        let commands = std::fs::read_to_string(&built.commands).unwrap();
        assert!(
            commands.contains("ORCHARD_KERNEL_TREE=/trees/linux ORCHARD_CACHE="),
            "{commands}"
        );

        // A version is built once; a development version is new every time.
        assert!(super::build(&build, &root, &distro("demo", true), &request)
            .unwrap_err()
            .message
            .contains("exists"));
        // The release has to be the distribution's own.
        let other = Request {
            version: Some("2.0".into()),
            ..request.clone()
        };
        let error = super::build(&build, &root, &distro("other", true), &other).unwrap_err();
        assert!(
            error.message.contains("orchard.toml names \"other\""),
            "{error}"
        );
        // An entry that builds every platform takes no selection, and one that selects needs one.
        let every = Request {
            version: Some("3.0".into()),
            ..request.clone()
        };
        assert!(super::build(&build, &root, &distro("demo", false), &every)
            .unwrap_err()
            .message
            .contains("takes no --platform"));
        let none = Request {
            version: Some("4.0".into()),
            platforms: None,
            ..request.clone()
        };
        assert!(super::build(&build, &root, &distro("demo", true), &none)
            .unwrap_err()
            .message
            .contains("selects its platforms"));
        assert_eq!(latest(&root).unwrap(), built.index);
    }
}
