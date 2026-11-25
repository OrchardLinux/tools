//! `orchard build`: what a build needs is looked up in the component cache
//! and made by orchard's producers when it is not there.
//!
//! The producers are orchard's own recipe scripts (`kernel/build`,
//! `boot/m1n1/build`, the assembly scripts …), run as commands from the
//! orchard data directory. This crate decides what is needed, computes the
//! keys, prepares kernel sources, runs the producers and records what ran.
pub mod components;
pub mod distro;
pub mod explain;
pub mod inputs;
pub mod json;
pub mod kernel;
pub mod manage;
pub mod platform;
pub mod project;
pub mod published;
pub mod ram;
pub mod recipes;
pub mod snapshot;
pub mod sources;

use explain::Explain;
use orchard_cache::component::{Components, Key};
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where orchard publishes its builds.
pub const RELEASES: &str = "https://github.com/OrchardLinux/orchard/releases/download";

/// What every step of a build works with.
pub struct Build<'a> {
    /// The orchard data directory: board data, recipes and producers.
    pub root: PathBuf,
    pub cache: Components,
    pub store: &'a Store,
    /// The Python the producers run with.
    pub python: PathBuf,
    /// Tells the person what the build does now.
    pub say: &'a dyn Fn(&str),
    pub explain: Explain,
}

impl<'a> Build<'a> {
    /// A build from the orchard data in `root`, with the cache `store` is in.
    pub fn new(root: &Path, store: &'a Store, say: &'a dyn Fn(&str)) -> Result<Build<'a>> {
        let root = root.canonicalize().context(root.display())?;
        if !root.join("kernel/build").is_file() {
            return Err(Error::new(Code::NotFound, "no kernel/build: not orchard's recipes")
                .context(root.display())
                .hint("point --data at an orchard checkout, or leave it out to use orchard's nightly"));
        }
        let cache_root = store.dir().canonicalize().context(store.dir().display())?;
        Ok(Build {
            root,
            cache: Components::open(&cache_root)?,
            store,
            python: python()?,
            say,
            explain: Explain::default(),
        })
    }

    pub fn say(&self, text: impl AsRef<str>) {
        (self.say)(text.as_ref());
    }

    /// A producer of orchard's, by the name of its file. Producers moved out
    /// of orchard's Python package into `producers/`; data from before that
    /// has them at their old place.
    pub fn producer(&self, name: &str) -> PathBuf {
        self.root.join(self.producer_path(name))
    }

    /// The same, relative to the data directory, as key inputs name it.
    pub fn producer_path(&self, name: &str) -> String {
        let new = format!("producers/{name}");
        if self.root.join(&new).is_file() {
            return new;
        }
        match name {
            "buildenv.py" => "src/orchard/buildenv.py".into(),
            _ => format!("src/orchard/image/{name}"),
        }
    }

    /// Runs a producer and records it. Its output goes to the terminal as
    /// it comes.
    pub fn command<S: AsRef<OsStr>>(&self, argv: &[S], env: &[(&str, &OsStr)]) -> Result<()> {
        let argv: Vec<OsString> = argv.iter().map(|a| a.as_ref().to_owned()).collect();
        let shown: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        self.say(format!("+ {}", shown.join(" ")));
        self.explain.command(&shown);
        let mut command = Command::new(&argv[0]);
        // The recipes keep their compiler cache and kernel trees in the
        // cache they are told about.
        command
            .args(&argv[1..])
            .env("ORCHARD_PYTHON", &self.python)
            .env("ORCHARD_CACHE_DIR", self.cache.root());
        for (name, value) in env {
            command.env(name, value);
        }
        let status = to_stderr(&mut command)
            .status()
            .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&shown[0]))?;
        if !status.success() {
            let error = Error::new(Code::Build, format!("{} failed ({status})", shown[0]));
            return Err(match container_hint() {
                Some(hint) => error.hint(hint),
                None => error,
            });
        }
        Ok(())
    }

    /// The entry `name` for `key` from the component cache, made by
    /// `produce` when it is not there. What produced it is recorded with
    /// it, and listed again when it is reused.
    pub fn component(
        &self,
        name: &str,
        key: &mut dyn FnMut() -> Result<Key>,
        produce: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<PathBuf> {
        let found = self.cache.cached(
            name,
            key,
            &mut |stage| self.explain.producing(stage, || produce(stage)),
            self.say,
        )?;
        if found.reused {
            self.explain.reused(name, &found.path);
        }
        Ok(found.path)
    }

    /// Runs one of orchard's Python producers.
    pub fn python<S: AsRef<OsStr>>(&self, script: &Path, args: &[S]) -> Result<()> {
        let mut argv = vec![
            self.python.clone().into_os_string(),
            script.as_os_str().to_owned(),
        ];
        argv.extend(args.iter().map(|a| a.as_ref().to_owned()));
        self.command(&argv, &[])
    }
}

/// Sends a child's standard output to this process's standard error: what
/// producers print is progress, and standard output is the result's.
pub(crate) fn to_stderr(command: &mut Command) -> &mut Command {
    use std::os::fd::AsFd;
    match std::io::stderr().as_fd().try_clone_to_owned() {
        Ok(stderr) => command.stdout(stderr),
        Err(_) => command,
    }
}

/// What to do when a producer failed because its containers cannot run:
/// the recipes build in Podman unless the host is a native builder.
fn container_hint() -> Option<&'static str> {
    if std::env::var("ORCHARD_BUILD_NATIVE").is_ok_and(|v| v == "1") {
        return None;
    }
    let podman = |args: &[&str]| {
        Command::new("podman")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let Ok(version) = podman(&["--version"]) else {
        return Some("the recipes build in Podman containers, and podman is not installed");
    };
    if !version.status.success() {
        return Some("the recipes build in Podman containers, and podman does not run");
    }
    if cfg!(target_os = "macos") {
        let state = podman(&["machine", "inspect", "--format", "{{.State}}"]).ok()?;
        if String::from_utf8_lossy(&state.stdout).trim() != "running" {
            return Some("the Podman VM is not running: podman machine start");
        }
    }
    None
}

/// The Python that runs the producers: `ORCHARD_PYTHON`, else the newest
/// `python3.N` on PATH, else `python3`. The producers need 3.11.
pub fn python() -> Result<PathBuf> {
    let mut candidates: Vec<OsString> = Vec::new();
    match std::env::var_os("ORCHARD_PYTHON") {
        Some(given) if !given.is_empty() => candidates.push(given),
        _ => {
            candidates.push("python3".into());
            candidates.extend(
                (11..=19)
                    .rev()
                    .map(|minor| OsString::from(format!("python3.{minor}"))),
            );
        }
    }
    let mut found = None;
    for candidate in &candidates {
        let version = Command::new(candidate)
            .args([
                "-c",
                "import sys; print(sys.version_info[0], sys.version_info[1], sys.executable)",
            ])
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(output) = version else { continue };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut parts = text.trim().splitn(3, ' ');
        let (Some(major), Some(minor), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let version = (
            major.parse::<u32>().unwrap_or(0),
            minor.parse::<u32>().unwrap_or(0),
        );
        if version >= (3, 11) {
            return Ok(PathBuf::from(path));
        }
        found.get_or_insert(format!(
            "{} is Python {major}.{minor}",
            candidate.to_string_lossy()
        ));
    }
    let what = found.unwrap_or_else(|| "no python3 on PATH".into());
    Err(Error::new(Code::NotFound, format!("orchard's producers need Python 3.11 or newer; {what}"))
        .hint("install a current Python (glu install python, or your system's package), or set ORCHARD_PYTHON"))
}

/// Runs a command for its output; the error carries what it wrote to stderr.
pub(crate) fn output(command: &mut Command) -> Result<Vec<u8>> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&program))?;
    if !output.status.success() {
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.trim().lines().last().unwrap_or("").to_owned();
        return Err(Error::new(
            Code::Build,
            format!("{program} {} failed: {last}", args.join(" ")),
        ));
    }
    Ok(output.stdout)
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::new(Code::InvalidInput, message)
}

/// A board, platform or project name as orchard writes them.
pub(crate) fn plain_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub(crate) fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).context(path.display())
}

pub(crate) fn write_text(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).context(path.display())
}

/// Copies a directory tree. With `follow`, a link is copied as what it
/// points at; otherwise as the link it is.
pub(crate) fn copy_tree(from: &Path, to: &Path, follow: bool) -> Result<()> {
    std::fs::create_dir_all(to).context(to.display())?;
    for item in std::fs::read_dir(from).context(from.display())? {
        let item = item?;
        let (source, target) = (item.path(), to.join(item.file_name()));
        let meta = std::fs::symlink_metadata(&source).context(source.display())?;
        if meta.is_symlink() && !follow {
            let link = std::fs::read_link(&source).context(source.display())?;
            std::os::unix::fs::symlink(link, &target).context(target.display())?;
        } else if source.is_dir() {
            copy_tree(&source, &target, follow)?;
        } else {
            copy_file(&source, &target)?;
        }
    }
    Ok(())
}

/// Puts a downloaded file into a cache entry without a second copy on disk:
/// as a hard link where the filesystem has them.
pub(crate) fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ()).context(from.display())
}

/// Copies a file with its mode and modification time.
pub(crate) fn copy_file(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).context(from.display())?;
    let modified = std::fs::metadata(from)
        .and_then(|m| m.modified())
        .context(from.display())?;
    std::fs::File::options()
        .write(true)
        .open(to)
        .and_then(|f| f.set_modified(modified))
        .context(to.display())
}
