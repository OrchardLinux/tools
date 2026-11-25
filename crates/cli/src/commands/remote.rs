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

#[derive(Serialize)]
struct Captured {
    saved: String,
    bytes: usize,
    ecid: String,
    model: Option<String>,
    /// Checks that could not be made or did not hold, none of which stops
    /// the capture.
    notes: Vec<String>,
}

impl Report for Captured {
    fn human(&self) -> Vec<u8> {
        let model = self.model.as_deref().unwrap_or("unknown model");
        let mut text = format!("{} ({} bytes): {model}, ECID {}\n", self.saved, self.bytes, self.ecid);
        for note in &self.notes {
            text.push_str(&format!("Note: {note}\n"));
        }
        text.into_bytes()
    }
}

/// What a captured tree says against what is known of the device: a wrong
/// model or ECID refuses it, the rest are notes.
fn adt_checks(
    unit: &orchard_formats::adt::Unit,
    wanted_ecid: Option<&str>,
    products: Option<&[String]>,
    usb_unit: Option<&str>,
) -> Result<Vec<String>> {
    use orchard_cache::{Code, Error};
    let mut notes = Vec::new();
    if let Some(wanted) = wanted_ecid {
        let wanted = u64::from_str_radix(wanted.trim_start_matches("0x").trim_start_matches("0X"), 16)
            .map_err(|_| Error::new(Code::InvalidInput, "--ecid is not a hex number"))?;
        if wanted != unit.ecid {
            return Err(Error::new(Code::DeviceChanged, format!("the device tree is of ECID {:x}, not the one named", unit.ecid)));
        }
    }
    match (products, unit.model.as_deref()) {
        (Some(products), Some(model)) if !products.iter().any(|p| p == model) => {
            return Err(Error::new(Code::DeviceChanged, format!("the device tree is of a {model}, not of the board the payload says it runs on")));
        }
        (Some(_), Some(_)) => {}
        _ => notes.push("the model was not checked against the board (no board data here, or none named)".to_owned()),
    }
    // The id the payload says on USB is made from the tree's serial number
    // (orchard's usb-identity). Not yet seen to hold on a device: a note.
    match (usb_unit, unit.serial_number.as_deref()) {
        (Some(said), Some(serial)) if orchard_runtime::ssh_keys::unit(serial) != said => {
            notes.push("the unit id the payload says on USB is not the one this tree gives".to_owned());
        }
        (Some(_), Some(_)) => {}
        _ => notes.push("the tree was not checked against the unit id on USB (one of them has none)".to_owned()),
    }
    Ok(notes)
}

pub fn capture_adt(global: &Global, output: &Path) -> Result<Output> {
    use orchard_cache::{Code, Context, Error};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if output.symlink_metadata().is_ok() {
        return Err(Error::new(Code::InvalidInput, "the destination exists already").context(output.display()));
    }
    let (target, tree) = orchard_remote::adt()?;
    let unit = orchard_formats::adt::unit(&tree).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    let store = Store::open(global.cache.as_deref())?;
    let products = target.device.identity.board.as_deref().and_then(|board| {
        let data = orchard_boards::data::board_data::existing(global.data.as_deref(), &store)?;
        Some(orchard_boards::catalog::Catalog::open(&data).ok()?.board(board).ok()?.products)
    });
    let notes = adt_checks(&unit, global.device.ecid.as_deref(), products.as_deref(), target.device.identity.unit.as_deref())?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context(parent.display())?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(output).context(output.display())?;
    file.write_all(&tree).context(output.display())?;
    Ok(Captured { saved: output.display().to_string(), bytes: tree.len(), ecid: format!("{:x}", unit.ecid), model: unit.model, notes }.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_formats::adt::Unit;

    #[test]
    fn a_tree_of_another_device_is_refused_and_the_rest_is_noted() {
        let unit = Unit { ecid: 0xa45d028bbe0a6, model: Some("iPad7,5".into()), serial_number: Some("F4KXY12ZAB".into()) };
        let products = ["iPad7,5".to_owned(), "iPad7,6".to_owned()];
        let said = orchard_runtime::ssh_keys::unit("F4KXY12ZAB");
        // All known and all as they should be: nothing to note.
        assert!(adt_checks(&unit, Some("000A45D028BBE0A6"), Some(&products), Some(&said)).unwrap().is_empty());
        assert!(adt_checks(&unit, Some("0xa45d028bbe0a6"), Some(&products), Some(&said)).unwrap().is_empty());
        // Another ECID or another model: not this device's tree.
        assert!(adt_checks(&unit, Some("1"), Some(&products), Some(&said)).is_err());
        assert!(adt_checks(&unit, None, Some(&["iPad6,11".to_owned()]), Some(&said)).is_err());
        // What cannot be checked, and a unit id that differs, are notes.
        assert_eq!(adt_checks(&unit, None, None, None).unwrap().len(), 2);
        let notes = adt_checks(&unit, None, Some(&products), Some("000000000000")).unwrap();
        assert!(notes.len() == 1 && notes[0].contains("unit id"), "{notes:?}");
    }
}
