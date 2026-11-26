//! Runs an outside tool for a bounded time, in a session of its own that
//! ends with the call.
use super::{Tool, ToolArg};
use orchard_cache::{Code, Error, Result};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicI32, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

/// How often a running tool is looked at.
const POLL: Duration = Duration::from_millis(20);

/// Signal handlers belong to the whole process, so one runner at a time
/// installs them.
static RUNNER: Mutex<()> = Mutex::new(());

/// The signal that arrived while a tool ran, or 0.
static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

extern "C" fn interrupted(signal: libc::c_int) {
    INTERRUPTED.store(signal, Ordering::SeqCst);
}

/// The error for a signal that arrived while a tool ran.
fn interruption() -> Option<Error> {
    match INTERRUPTED.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(Error::new(
            Code::Io,
            format!("tool interrupted by signal {signal}"),
        )),
    }
}

/// Notes SIGINT, SIGTERM and SIGHUP while a tool runs, so that the tool's
/// processes are ended before orchard2 stops. Dropping it puts the earlier
/// handlers back.
struct Signals(Vec<(i32, libc::sigaction)>);

impl Signals {
    fn install() -> Result<Self> {
        INTERRUPTED.store(0, Ordering::SeqCst);
        let mut saved = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: both structs are plain data that sigaction fills or
            // reads, and the handler only stores to an atomic.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut old = std::mem::zeroed();
                action.sa_sigaction = interrupted as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut old) != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                saved.0.push((signal, old));
            }
        }
        Ok(saved)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, old) in &self.0 {
            // SAFETY: `old` is what sigaction returned for this signal.
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}

/// The tool's process and everything it started. Dropping it ends them all,
/// also after a clean exit: palera1n starts checkra1n and only waits for it,
/// and a checkra1n left running takes the next device that enters DFU.
struct Group(Child);

impl Group {
    /// Starts `command` as the leader of a session of its own. A process
    /// group of ours would be a background job of the terminal, which stops
    /// it as soon as it reads from the terminal or changes it.
    fn start(command: &mut Command) -> std::io::Result<Group> {
        command.stdin(Stdio::null());
        // SAFETY: setsid is async-signal-safe, and nothing else runs
        // between fork and exec.
        unsafe {
            command.pre_exec(|| match libc::setsid() {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        Ok(Group(command.spawn()?))
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        // SAFETY: the child leads its own session, so the group with its
        // id holds only what it started.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

/// Runs `command` until it ends, for `timeout` at most. A signal to
/// orchard2 in that time ends the tool and is the error returned.
fn run(command: &mut Command, timeout: Duration) -> Result<()> {
    let _runner = RUNNER
        .lock()
        .map_err(|_| Error::new(Code::Io, "tool runner: poisoned lock"))?;
    let signals = Signals::install()?;
    let result = supervise(command, timeout);
    // A signal can arrive just as the tool ends. It is read once the earlier
    // handlers are back, so every signal either shows here or acts as usual.
    drop(signals);
    match interruption() {
        Some(error) => Err(error),
        None => result,
    }
}

/// Starts `command` and waits for its exit, the time limit or a signal.
/// Whatever the command started is gone when this returns.
fn supervise(command: &mut Command, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    let mut group = Group::start(command)?;
    loop {
        if let Some(error) = interruption() {
            return Err(error);
        }
        if let Some(status) = group.0.try_wait()? {
            return match status.success() {
                true => Ok(()),
                false => Err(Error::new(Code::Io, format!("tool exited with {status}"))),
            };
        }
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(Error::new(Code::Timeout, "tool time limit is up"));
        }
        std::thread::sleep(POLL.min(left));
    }
}

/// Writes the tool and its input files into `dir` and builds the command
/// that runs it. Arguments are passed as they are, with no shell between.
fn stage(tool: &Tool<'_>, dir: &Path) -> Result<Command> {
    let executable = dir.join("tool");
    std::fs::write(&executable, tool.executable)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let mut files = Vec::new();
    for (index, bytes) in tool.files.iter().enumerate() {
        let path = dir.join(format!("input-{index}"));
        std::fs::write(&path, bytes)?;
        files.push(path);
    }
    let mut command = Command::new(&executable);
    for arg in tool.args {
        match arg {
            ToolArg::Text(text) => {
                command.arg(text);
            }
            ToolArg::File(index) => {
                let path = files.get(*index).ok_or_else(|| {
                    Error::new(
                        Code::InvalidInput,
                        format!("{}: no input file {index}", tool.name),
                    )
                })?;
                command.arg(path);
            }
        }
    }
    Ok(command)
}

/// Fixed-size secret reply over a private socket, never a file or a log.
pub(super) fn reply(
    tool: &Tool<'_>,
    timeout: Duration,
    expected: usize,
    inherit: &dyn Fn(&mut Command) -> Result<()>,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let dir = tempfile::tempdir_in(tool.dir)?;
    let mut command = stage(tool, dir.path())?;
    let (reader, writer) = UnixStream::pair()?;
    reader.set_read_timeout(Some(Duration::from_secs(1)))?;
    command
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null());
    inherit(&mut command)?;
    run(&mut command, timeout).map_err(|e| {
        e.context(tool.name)
            .hint("device operation may have run; inspect USB before retrying")
    })?;
    drop(command);
    let mut bytes = Vec::new();
    reader.take(expected as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() != expected {
        return Err(Error::new(
            Code::Format,
            "invalid device key reply (values suppressed)",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell command with `args` as `$1`, `$2` and so on.
    fn sh(script: &str, args: &[&Path]) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script, "test"]).args(args);
        command
    }

    const LIMIT: Duration = Duration::from_secs(2);

    #[test]
    fn private_reply_only_accepts_the_expected_byte_count() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Tool {
            name: "reply-fixture",
            executable: b"#!/bin/sh\nprintf reply\nprintf diagnostic >&2\n",
            args: &[],
            files: &[],
            dir: dir.path(),
        };
        assert_eq!(reply(&tool, LIMIT, 5, &|_| Ok(())).unwrap(), b"reply");
        for expected in [4, 6] {
            let error = reply(&tool, LIMIT, expected, &|_| Ok(())).unwrap_err();
            assert_eq!(error.code, Code::Format);
            assert_eq!(
                error.message,
                "invalid device key reply (values suppressed)"
            );
        }
    }

    #[test]
    fn arguments_reach_the_tool_as_written_and_files_as_paths() {
        let dir = tempfile::tempdir().unwrap();
        let script = "test \"$1\" = 'literal ; $value' \
                      && test \"$(cat \"$2\")\" = second \
                      && test \"$(cat \"$3\")\" = first";
        let tool = Tool {
            name: "shell",
            executable: b"",
            args: &[
                ToolArg::Text("-c"),
                ToolArg::Text(script),
                ToolArg::Text("test"),
                ToolArg::Text("literal ; $value"),
                ToolArg::File(1),
                ToolArg::File(0),
            ],
            files: &[b"first", b"second"],
            dir: dir.path(),
        };
        let staged = stage(&tool, dir.path()).unwrap();
        // macOS does not run a copy of a system binary, so the staged
        // arguments go to the shell where it is.
        run(Command::new("/bin/sh").args(staged.get_args()), LIMIT).unwrap();
    }

    #[test]
    fn the_executable_is_staged_for_its_owner_alone() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Tool {
            name: "tool",
            executable: b"bytes",
            args: &[],
            files: &[],
            dir: dir.path(),
        };
        let staged = stage(&tool, dir.path()).unwrap();
        let path = Path::new(staged.get_program());
        assert_eq!(std::fs::read(path).unwrap(), b"bytes");
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn a_file_argument_needs_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Tool {
            name: "tool",
            executable: b"",
            args: &[ToolArg::File(1)],
            files: &[b"only"],
            dir: dir.path(),
        };
        let error = stage(&tool, dir.path()).unwrap_err();
        assert_eq!(error.code, Code::InvalidInput);
        assert_eq!(error.message, "tool: no input file 1");
    }

    #[test]
    fn a_clean_exit_is_a_success() {
        run(&mut sh("exit 0", &[]), LIMIT).unwrap();
    }

    #[test]
    fn a_failing_exit_is_an_error() {
        let error = run(&mut sh("exit 7", &[]), LIMIT).unwrap_err();
        assert_eq!(error.code, Code::Io);
        assert!(error.message.starts_with("tool exited with"));
    }

    #[test]
    fn a_tool_past_its_limit_is_stopped() {
        let started = Instant::now();
        let error = run(&mut sh("sleep 10", &[]), Duration::from_millis(80)).unwrap_err();
        assert_eq!(error.code, Code::Timeout);
        assert!(started.elapsed() < LIMIT);
    }

    #[test]
    fn the_tool_leads_a_session_of_its_own() {
        let group = Group::start(&mut sh("sleep 5", &[])).unwrap();
        let pid = group.0.id() as i32;
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
    }

    #[test]
    fn what_the_tool_started_ends_with_it() {
        // The tool exits cleanly, fails, or runs into its limit.
        for end in ["exit 0", "exit 7", "wait"] {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("survived");
            let script = format!("(sleep 0.3; touch \"$1\") & {end}");
            let _ = run(&mut sh(&script, &[&marker]), Duration::from_millis(80));
            std::thread::sleep(Duration::from_millis(400));
            assert!(!marker.exists(), "a descendant survived {end}");
        }
    }

    /// The other half of `a_signal_ends_the_tool_and_what_it_started`, which
    /// starts this test binary again with the variable set: signal handlers
    /// are the process's, so the signalled runner needs a process to itself.
    /// On its own this test does nothing.
    #[test]
    fn signalled_runner() {
        let Some(dir) = std::env::var_os("ORCHARD_TOOL_SIGNAL_TEST") else {
            return;
        };
        let script = "(sleep 0.3; touch survived) & touch ready; wait";
        let error = run(sh(script, &[]).current_dir(dir), Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.code, Code::Io);
        assert!(error.message.starts_with("tool interrupted by signal"));
    }

    #[test]
    fn a_signal_ends_the_tool_and_what_it_started() {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let dir = tempfile::tempdir().unwrap();
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "host::tool::tests::signalled_runner"])
                .env("ORCHARD_TOOL_SIGNAL_TEST", dir.path())
                .process_group(0)
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let mut runner = Group(child);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.path().join("ready").exists() {
                assert!(Instant::now() < deadline, "the runner did not start");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(unsafe { libc::kill(runner.0.id() as i32, signal) }, 0);
            let status = loop {
                if let Some(status) = runner.0.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "the runner did not stop");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(status.success(), "the runner's own assertions failed");
            std::thread::sleep(Duration::from_millis(400));
            assert!(!dir.path().join("survived").exists());
        }
    }
}
