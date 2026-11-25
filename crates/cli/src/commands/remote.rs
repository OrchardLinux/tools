//! `shell`, `exec`, `push`, `pull`: a running RAM payload or installed
//! system, through `orchard-remote`.
use crate::args::{Global, ReachArgs};
use crate::output::{Output, Report};
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_remote::{Copied, Reach};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn reach(args: &ReachArgs) -> Reach {
    Reach { user: args.user.clone(), host: args.host.clone(), ram: args.ram }
}

fn cache(global: &Global) -> Result<PathBuf> {
    Ok(Store::open(global.cache.as_deref())?.dir().to_owned())
}

/// A status the device's command ended with, as this command's own.
fn status(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(255))
}

pub fn shell(global: &Global, args: &ReachArgs) -> Result<ExitCode> {
    orchard_remote::shell(&cache(global)?, &reach(args)).map(status)
}

pub fn exec(global: &Global, args: &ReachArgs, command: &[String]) -> Result<ExitCode> {
    orchard_remote::exec(&cache(global)?, &reach(args), command).map(status)
}

#[derive(Serialize)]
struct Transfer {
    from: String,
    to: String,
    size: u64,
    sha256: String,
}

impl Report for Transfer {
    fn human(&self) -> Vec<u8> {
        format!("{} -> {} ({} bytes, sha256 {})\n", self.from, self.to, self.size, &self.sha256[..16]).into_bytes()
    }
}

fn copied(done: Copied) -> Output {
    Transfer { from: done.from, to: done.to, size: done.size, sha256: done.sha256 }.into()
}

pub fn push(global: &Global, args: &ReachArgs, local: &Path, remote: &str, serial: bool) -> Result<Output> {
    orchard_remote::push(&cache(global)?, &reach(args), local, remote, serial).map(copied)
}

pub fn pull(global: &Global, args: &ReachArgs, remote: &str, local: &Path, serial: bool) -> Result<Output> {
    orchard_remote::pull(&cache(global)?, &reach(args), remote, local, serial).map(copied)
}

#[derive(Serialize)]
struct Access {
    user: String,
    keys: usize,
}

impl Report for Access {
    fn human(&self) -> Vec<u8> {
        let keys = if self.keys == 1 { "1 key".to_owned() } else { format!("{} keys", self.keys) };
        format!("SSH access for {} added from this computer ({keys}). Try: orchard2 shell\n", self.user).into_bytes()
    }
}

/// One line typed at the terminal; `hidden` turns the echo off for it.
/// Without a terminal, the next line of standard input.
fn typed(prompt: &str, hidden: bool) -> Result<String> {
    use std::io::{BufRead, Write};
    use std::os::fd::AsRawFd;
    let mut line = String::new();
    let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        std::io::stdin().lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    };
    (&tty).write_all(prompt.as_bytes())?;
    // SAFETY: termios calls on a descriptor this owns; the settings are put back.
    let before = unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        (hidden && libc::tcgetattr(tty.as_raw_fd(), &mut settings) == 0).then(|| {
            let before = settings;
            settings.c_lflag &= !libc::ECHO;
            libc::tcsetattr(tty.as_raw_fd(), libc::TCSAFLUSH, &settings);
            before
        })
    };
    let read = std::io::BufReader::new(&tty).read_line(&mut line);
    if let Some(before) = before {
        // SAFETY: as above.
        unsafe { libc::tcsetattr(tty.as_raw_fd(), libc::TCSAFLUSH, &before) };
        (&tty).write_all(b"\n")?;
    }
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

pub fn ssh_access(global: &Global, user: Option<&str>) -> Result<Output> {
    use orchard_cache::{Code, Error};
    let cache = cache(global)?;
    let (target, remembered) = orchard_remote::installed(&cache)?;
    let user = match user.map(str::to_owned).or(remembered) {
        Some(user) => user,
        None => typed("User on the device: ", false)?.trim().to_owned(),
    };
    if !orchard_remote::valid_user(&user) {
        return Err(Error::new(Code::InvalidInput, format!("{user:?} is not a user name")));
    }
    let password = typed(&format!("Password of {user} on the device: "), true)?;
    if password.is_empty() {
        return Err(Error::new(Code::InvalidInput, "no password was given"));
    }
    let keys = orchard_runtime::ssh_keys::keys(&cache)?;
    orchard_remote::grant(&target, &user, &password, &keys)?;
    // The user for shell and its kin; and the unit's host key may be new
    // since an earlier install, so it is learned again.
    orchard_runtime::ssh_keys::installed(&cache, &user, target.device.identity.unit.as_deref())?;
    Ok(Access { user, keys: keys.len() }.into())
}
