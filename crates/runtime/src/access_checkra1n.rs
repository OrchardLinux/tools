//! The checkra1n access route, for iOS 12 to 14: checkra1n starts iOS from
//! DFU mode, and every checkra1n start runs an SSH server inside iOS
//! (dropbear, port 44, root). Over the cable one SSH connection then
//!
//!   1. removes what an interrupted earlier session left behind,
//!   2. copies the app and its helper to `/private/var/ResizeAPFS`,
//!   3. has the helper put the app on the Home Screen,
//!   4. runs the helper as root while the person resizes in the app,
//!   5. afterwards stops the helper and takes the app off again, once the
//!      helper says no resize runs.
//!
//! Everything goes to the data partition, never the system partition, and
//! only `sh`, `mkdir`, `cat`, `chmod`, `rm` and `kill` are used on the
//! device. The sequence and the device-side commands are resize-apfs's
//! `scripts/checkra1n_route.py`.
use crate::exec::Executor;
use crate::resize_app::Older;
use crate::usbmux;
use orchard_cache::{Code, Error, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Root-owned and readable by everyone (the app runs as mobile), on the
/// data partition. The helper accepts only an app directly in this folder.
const SESSION: &str = "/private/var/ResizeAPFS";
const APP: &str = "/private/var/ResizeAPFS/ResizeAPFS.app";
const HELPER: &str = "/private/var/ResizeAPFS/resize-helper";
const PIDFILE: &str = "/private/var/ResizeAPFS/helper.pid";
const HELPER_LOG: &str = "/private/var/ResizeAPFS/helper.log";
/// Where earlier releases put them.
const OLD_APP: &str = "/Applications/ResizeAPFS.app";
const OLD_SESSION: &str = "/var/root/ResizeAPFS/session";
const SSH_PORT: u16 = 44;
/// checkra1n's SSH server takes root with iOS's default password.
const PASSWORD: &str = "alpine";
/// checkra1n's own run, then iOS's start until its SSH server answers.
const BOOT: Duration = Duration::from_secs(120 + 300);

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::Refused, message.into())
}

/// The one device running iOS on USB, by its UDID.
fn udid() -> Result<Option<String>> {
    let mut usb: Vec<_> = usbmux::devices(Path::new(usbmux::SOCKET))?.into_iter().filter(|d| d.usb).collect();
    match usb.len() {
        0 => Ok(None),
        1 => Ok(Some(usb.remove(0).udid)),
        _ => Err(fail("more than one device running iOS is connected; connect only the one to resize")),
    }
}

/// The device whose SSH server sends its greeting over the cable: that, not
/// checkra1n's exit, is the start having worked (its command line goes on
/// waiting for the next DFU device).
fn ssh_answers() -> Result<Option<String>> {
    let Some(udid) = udid()? else { return Ok(None) };
    let Ok(mut stream) = usbmux::connect(Path::new(usbmux::SOCKET), &udid, SSH_PORT) else { return Ok(None) };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut greeting = [0u8; 4];
    Ok((stream.read_exact(&mut greeting).is_ok() && &greeting == b"SSH-").then_some(udid))
}

/// Runs checkra1n until the device's SSH server answers, and returns the
/// device's UDID.
fn start_ios(executor: &Executor) -> Result<String> {
    let (pin, release) = crate::flags::checkra1n_pin().ok_or_else(|| Error::new(Code::NotFound, "checkra1n publishes no build for this computer"))?;
    let checkra1n = executor.palera1n(pin, release)?;
    let (log, log_path) = executor.log_file("checkra1n")?;
    let mut child = Command::new(&checkra1n).arg("--cli").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
    let until = Instant::now() + BOOT;
    let outcome = loop {
        std::thread::sleep(Duration::from_secs(3));
        match ssh_answers() {
            Ok(Some(udid)) => break Ok(udid),
            Err(e) => break Err(e),
            Ok(None) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                break Err(fail(format!("checkra1n failed ({status}); its log: {}", log_path.display())));
            }
        }
        if Instant::now() > until {
            break Err(fail("checkra1n's SSH server did not come up. Unlock the device; if it stays in iOS without the checkra1n app, start again"));
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

/// What one command on the device gave.
struct Ran {
    /// The exit status; None when the command did not end in time.
    code: Option<i32>,
    stderr: String,
}

/// One SSH connection to the device's server, used for every command
/// (ControlMaster).
struct Ssh {
    dir: PathBuf,
    base: Vec<String>,
}

impl Ssh {
    fn open(udid: &str) -> Result<Ssh> {
        // A short folder: the control socket's path must stay under 104 bytes.
        let dir = PathBuf::from(format!("/tmp/orchard-resize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let askpass = dir.join("askpass");
        std::fs::write(&askpass, format!("#!/bin/sh\necho {PASSWORD}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700))?;
        }
        let me = std::env::current_exe()?;
        let proxy = format!("'{}' usbmux-proxy {SSH_PORT} {udid}", me.display().to_string().replace('\'', "'\\''"));
        let base: Vec<String> = [
            "-T",
            &format!("-oProxyCommand={proxy}"),
            &format!("-oControlPath={}", dir.join("ssh.sock").display()),
            "-oPreferredAuthentications=password,keyboard-interactive",
            "-oPubkeyAuthentication=no",
            // Each checkra1n start may present a new host key; nothing is saved.
            "-oStrictHostKeyChecking=no",
            "-oUserKnownHostsFile=/dev/null",
            "-oLogLevel=ERROR",
            "-oServerAliveInterval=15",
            "-oConnectTimeout=20",
            "root@iphone",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let ssh = Ssh { dir, base };
        let mut master = ssh.command();
        master.args(["-oControlMaster=yes", "-oControlPersist=yes", "-N", "-f"]);
        let opened = Ssh::wait(master, None, Duration::from_secs(60))?;
        match opened.code {
            Some(0) => Ok(ssh),
            _ => Err(fail(format!("checkra1n's SSH server did not accept the connection: {}", opened.stderr.trim()))),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("ssh");
        command
            .args(&self.base)
            .env("SSH_ASKPASS", self.dir.join("askpass"))
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()));
        command
    }

    fn wait(mut command: Command, stdin: Option<&[u8]>, most: Duration) -> Result<Ran> {
        let mut child = command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // The device reads as it writes: a write error is the command's to report.
            let _ = pipe.write_all(data);
        }
        let until = Instant::now() + most;
        let code = loop {
            match child.try_wait()? {
                Some(status) => break status.code(),
                None if Instant::now() > until => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Ran { code: None, stderr: String::new() });
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        Ok(Ran { code, stderr })
    }

    /// Runs `script` on the device.
    fn sh(&self, script: &str, stdin: Option<&[u8]>, most: Duration) -> Result<Ran> {
        let mut command = self.command();
        command.arg(script);
        Ssh::wait(command, stdin, most)
    }

    /// The same, and a failure is an error naming `what`.
    fn must(&self, script: &str, what: &str, stdin: Option<&[u8]>) -> Result<()> {
        let ran = self.sh(script, stdin, Duration::from_secs(300))?;
        match ran.code {
            Some(0) => Ok(()),
            _ => Err(fail(format!("{what} failed on the device{}", if ran.stderr.trim().is_empty() { ".".into() } else { format!(": {}", ran.stderr.trim()) }))),
        }
    }

    fn put(&self, data: &[u8], remote: &str, mode: u32) -> Result<()> {
        let name = remote.rsplit('/').next().unwrap_or(remote);
        self.must(&format!("cat > {} && chmod {mode:o} {}", quote(remote), quote(remote)), &format!("Copying {name}"), Some(data))
    }
}

impl Drop for Ssh {
    fn drop(&mut self) {
        let mut close = self.command();
        close.args(["-O", "exit"]);
        let _ = Ssh::wait(close, None, Duration::from_secs(30));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Executables and libraries: a release ZIP carries no execute bits.
fn is_macho(data: &[u8]) -> bool {
    matches!(data.get(..4), Some([0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe] | [0xca, 0xfe, 0xba, 0xbf]))
}

/// Undoes a session that ended before its cleanup, in `folder` with its app
/// at `app`. Safe when there is none.
fn leftover(folder: &str, app: &str) -> String {
    let (pidfile, helper) = (format!("{folder}/helper.pid"), format!("{folder}/resize-helper"));
    format!(
        "if [ -f {pidfile} ]; then kill \"$(cat {pidfile})\" 2>/dev/null; rm -f {pidfile}; fi; \
         if [ -x {helper} ] && [ -d {app} ]; then {helper} --unregister-app {app}; fi; rm -rf {app} {folder}"
    )
}

/// Whether a resize the helper started still runs: false only when the
/// helper clearly says none does; None when the device could not be asked.
/// The helper outlives a dropped connection, so silence is not "done".
fn busy(ssh: &Ssh) -> Result<Option<bool>> {
    Ok(match ssh.sh(&format!("{HELPER} --operation-active"), None, Duration::from_secs(45))?.code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    })
}

/// How taking the app off went.
enum Left {
    Removed,
    /// A resize is running: nothing was stopped.
    Busy,
    /// The device did not answer: nothing was stopped, or the next try
    /// finds the helper stopped and removes the app then.
    Unanswered,
}

/// Stops the helper and removes the app, unless a resize runs by then: the
/// device asks the helper again first, so nothing is stopped mid-resize.
fn finish(ssh: &Ssh) -> Result<Left> {
    let stop = format!(
        "if {HELPER} --operation-active; then exit 3; fi; \
         if [ -f {PIDFILE} ]; then pid=\"$(cat {PIDFILE})\"; kill \"$pid\" 2>/dev/null; i=0; \
         while kill -0 \"$pid\" 2>/dev/null && [ $i -lt 20 ]; do sleep 1; i=$((i+1)); done; \
         kill -9 \"$pid\" 2>/dev/null; rm -f {PIDFILE}; fi"
    );
    match ssh.sh(&stop, None, Duration::from_secs(90))?.code {
        Some(0) => {}
        Some(3) => return Ok(Left::Busy),
        _ => return Ok(Left::Unanswered),
    }
    let removed = ssh.sh(&format!("{HELPER} --unregister-app {APP}; rm -rf {SESSION}"), None, Duration::from_secs(90))?;
    match removed.code {
        Some(0) => Ok(Left::Removed),
        // SSH itself failed, or no answer in time.
        Some(255) | None => Ok(Left::Unanswered),
        Some(_) => Err(fail(format!("Removing the app failed on the device: {}", removed.stderr.trim()))),
    }
}

/// Starts iOS on the device in DFU mode with checkra1n, puts the app there
/// for one session, and takes it off again once `done` returns: `done`
/// waits for the person to say they have finished, and is asked again when
/// a resize still runs. `say` is told each step.
pub fn run(executor: &Executor, app: &Older, say: &dyn Fn(&str), done: &mut dyn FnMut() -> Result<()>) -> Result<()> {
    if !app.app.iter().any(|(path, _)| path == "ResizeAPFS") {
        return Err(Error::new(Code::Format, "the release's app for iOS 12 to 14 has no executable"));
    }
    say("Starting iOS with checkra1n");
    let udid = start_ios(executor)?;
    say("iOS is up; copying Resize APFS");
    let ssh = Ssh::open(&udid)?;
    ssh.must(&format!("{}; {}", leftover(OLD_SESSION, OLD_APP), leftover(SESSION, APP)), "Removing an earlier session", None)?;
    // 755: the app runs as mobile and must read its bundle; the helper trusts
    // it only while nobody but root can change it.
    ssh.must(&format!("umask 022; mkdir -p {SESSION} && chmod 755 {SESSION}"), "Preparing the session folder", None)?;
    ssh.put(&app.helper, HELPER, 0o755)?;
    let mut folders: Vec<String> = vec![APP.to_owned()];
    for (path, _) in &app.app {
        let mut at = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last().map(|(_, parents)| parents).unwrap_or_default() {
            at = format!("{at}/{part}");
            let folder = format!("{APP}{at}");
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
    }
    ssh.must(&format!("umask 022; mkdir -p {}", folders.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ")), "Creating the app folder", None)?;
    for (path, data) in &app.app {
        ssh.put(data, &format!("{APP}/{path}"), if is_macho(data) { 0o755 } else { 0o644 })?;
    }
    ssh.must(&format!("{HELPER} --register-app {APP}"), "Adding the app to the Home Screen", None)?;
    // Detached from the connection: a locked device that sleeps stops
    // answering, and that must not take the helper, and a running resize,
    // with it.
    ssh.must(
        &format!("cd /; trap '' HUP; {HELPER} --session --allow-writes --ui-path {APP}/ResizeAPFS </dev/null >{HELPER_LOG} 2>&1 & echo $! > {PIDFILE}"),
        "Starting the helper",
        None,
    )?;
    say("Resize APFS is on the Home Screen. Unlock the device and resize in the app.");

    // Cleanup never follows silence: a device that does not answer is asked
    // again, without a time limit.
    done()?;
    let mut waiting = false;
    loop {
        let left = match busy(&ssh)? {
            None => Left::Unanswered,
            Some(true) => Left::Busy,
            Some(false) => finish(&ssh)?,
        };
        match left {
            Left::Removed => return Ok(()),
            Left::Unanswered => {
                if !waiting {
                    say("Waiting for the device to answer. Unlock it if it is locked.");
                    waiting = true;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
            Left::Busy => {
                waiting = false;
                say("A resize is still running. Wait until the app shows it has finished.");
                done()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_are_quoted_and_executables_told_apart() {
        assert_eq!(quote("/a b/it's"), "'/a b/it'\\''s'");
        assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0]));
        assert!(is_macho(&[0xca, 0xfe, 0xba, 0xbe]));
        assert!(!is_macho(b"<?xml"));
        assert!(!is_macho(&[0xcf]));
    }

    #[test]
    fn a_leftover_session_is_undone_with_its_own_helper() {
        let script = leftover(SESSION, APP);
        assert!(script.contains("kill \"$(cat /private/var/ResizeAPFS/helper.pid)\""));
        assert!(script.contains("/private/var/ResizeAPFS/resize-helper --unregister-app /private/var/ResizeAPFS/ResizeAPFS.app"));
        assert!(script.ends_with("rm -rf /private/var/ResizeAPFS/ResizeAPFS.app /private/var/ResizeAPFS"));
    }
}
