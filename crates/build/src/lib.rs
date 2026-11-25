//! `orchard build`: what a build needs is looked up in the component cache
//! and made by orchard's producers when it is not there.
//!
//! The producers are orchard's own recipe scripts (`kernel/build`,
//! `boot/m1n1/build`, the assembly scripts …), run as commands from the
//! orchard data directory. This crate decides what is needed, computes the
//! keys, prepares kernel sources, runs the producers and records what ran.
pub mod components;
pub mod contract;
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
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
/// Commands of the build that run now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Set when Ctrl-C came while no command ran: nothing returns from it.
static ALONE: AtomicBool = AtomicBool::new(false);
/// What the build stages outside the cache, to remove when it is ended.
static STAGED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

extern "C" fn on_interrupt(_: libc::c_int) {
    if INTERRUPTS.fetch_add(1, Ordering::SeqCst) >= 1 {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(130) };
    }
    if RUNNING.load(Ordering::SeqCst) == 0 {
        ALONE.store(true, Ordering::SeqCst);
    }
}

/// A directory the build stages in a project, removed when Ctrl-C ends the
/// build while it lives. In the cache, the next build removes what is left.
pub(crate) struct Staged(PathBuf);

impl Staged {
    pub(crate) fn new(path: &Path) -> Staged {
        if let Ok(mut staged) = STAGED.lock() {
            staged.push(path.to_owned());
        }
        Staged(path.to_owned())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Ok(mut staged) = STAGED.lock() {
            staged.retain(|path| *path != self.0);
        }
    }
}

/// Whether the person interrupted the build.
pub fn was_interrupted() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

/// While it lives, Ctrl-C is the running command's to act on first.
pub(crate) struct Running;

impl Running {
    pub(crate) fn new() -> Running {
        RUNNING.fetch_add(1, Ordering::SeqCst);
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The first Ctrl-C ends the running producer, which the terminal signals
/// too, and the build returns and removes what it staged; the second ends
/// the process. With no command running (a download, a copy, waiting for
/// another build) nothing would act on the first, so the build ends then:
/// what it staged in the project is removed, and what it staged in the
/// cache goes with the next build.
pub fn catch_interrupts() {
    static WATCHED: std::sync::Once = std::sync::Once::new();
    WATCHED.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(50));
            if ALONE.load(Ordering::SeqCst) {
                let staged = STAGED.lock().map(|s| s.clone()).unwrap_or_default();
                for path in staged {
                    let _ = std::fs::remove_dir_all(path);
                }
                std::process::exit(130);
            }
        });
    });
    // SAFETY: installs a handler that only uses async-signal-safe calls.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_interrupt as *const () as libc::sighandler_t,
        );
    }
}

/// An error once the person interrupted the build.
pub(crate) fn interrupted() -> Result<()> {
    match INTERRUPTS.load(Ordering::SeqCst) {
        0 => Ok(()),
        _ => Err(Error::new(Code::Build, "interrupted")),
    }
}

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
    /// What the recipes state about themselves.
    pub contract: contract::Contract,
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
            cache: Components::open(&cache_root)?,
            store,
            python: python()?,
            contract: contract::read(&root)?,
            root,
            say,
            explain: Explain::default(),
        })
    }

    pub fn say(&self, text: impl AsRef<str>) {
        (self.say)(text.as_ref());
    }

    /// A producer of orchard's, by the name of its file.
    pub fn producer(&self, name: &str) -> PathBuf {
        self.root.join(contract::producer_path(&self.root, name))
    }

    /// Runs a producer and records it. Its output goes to the terminal as
    /// it comes.
    pub fn command<S: AsRef<OsStr>>(&self, argv: &[S], env: &[(&str, &OsStr)]) -> Result<()> {
        let argv: Vec<OsString> = argv.iter().map(|a| a.as_ref().to_owned()).collect();
        let shown: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        interrupted()?;
        self.say(format!("+ {}", shown.join(" ")));
        // The recipes keep their compiler cache and kernel trees in the
        // cache they are told about.
        let mut given: Vec<(&str, &OsStr)> = vec![
            ("ORCHARD_PYTHON", self.python.as_os_str()),
            ("ORCHARD_CACHE_DIR", self.cache.root().as_os_str()),
        ];
        given.extend_from_slice(env);
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        let mut recorded = inherited();
        for (name, value) in given {
            command.env(name, value);
            recorded.retain(|(known, _)| known != name);
            recorded.push((name.to_owned(), value.to_string_lossy().into_owned()));
        }
        self.explain.command(&shown, &recorded);
        let running = Running::new();
        let status = to_stderr(&mut command)
            .status()
            .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&shown[0]))?;
        drop(running);
        interrupted()?;
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
    let podman = |args: &[&str]| within(Command::new("podman").args(args), PODMAN_ANSWERS);
    let version = match podman(&["--version"]) {
        Ok(Some(version)) => version,
        Ok(None) => return Some(NO_ANSWER),
        Err(_) => {
            return Some("the recipes build in Podman containers, and podman is not installed")
        }
    };
    if !version.status.success() {
        return Some("the recipes build in Podman containers, and podman does not run");
    }
    if cfg!(target_os = "macos") {
        let Some(state) = podman(&["machine", "inspect", "--format", "{{.State}}"]).ok()? else {
            return Some(NO_ANSWER);
        };
        if String::from_utf8_lossy(&state.stdout).trim() != "running" {
            return Some("the Podman VM is not running: podman machine start");
        }
    }
    None
}

/// How long Podman gets to say what state it is in.
pub(crate) const PODMAN_ANSWERS: Duration = Duration::from_secs(20);
const NO_ANSWER: &str = "podman does not answer; its VM may hang: podman machine stop, then start";

/// Runs a command that is asked something, for at most `limit`: its output,
/// or `None` when it was ended for not finishing. For commands that print
/// little; a build's producers run as long as they need.
pub(crate) fn within(command: &mut Command, limit: Duration) -> std::io::Result<Option<Output>> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let end = Instant::now() + limit;
    while child.try_wait()?.is_none() {
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().map(Some)
}

/// The variables of this process that producers read and that change what
/// they do, as a build's record names them.
fn inherited() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| {
            name.starts_with("ORCHARD_BUILD_")
                || ["ORCHARD_KERNEL_JOBS", "ORCHARD_CCACHE"].contains(&name.as_str())
        })
        .collect();
    found.sort();
    found
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
    let running = Running::new();
    let output = command
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&program))?;
    drop(running);
    interrupted()?;
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
    // Opened for reading: the copy is read-only when the original is.
    std::fs::File::open(to)
        .and_then(|f| f.set_modified(modified))
        .context(to.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_that_does_not_answer_is_ended() {
        let started = Instant::now();
        let hung = within(Command::new("sleep").arg("30"), Duration::from_millis(200)).unwrap();
        assert!(hung.is_none() && started.elapsed() < Duration::from_secs(10));
        let answered = within(Command::new("echo").arg("running"), Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&answered.stdout).trim(), "running");
        assert!(within(
            &mut Command::new("no-such-program-here"),
            Duration::from_secs(1)
        )
        .is_err());
    }
}
