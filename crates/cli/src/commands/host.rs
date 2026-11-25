//! `doctor` and `setup`: what this computer has of what orchard2 uses from
//! it, each with what it is needed for, and getting one of them.
//!
//! orchard2 fetches its own tools when a run needs them, so this is only
//! about the computer: access to USB on Linux, and what a build runs.
//! `doctor` changes nothing; `setup NAME` does one thing and says what it
//! runs before it runs it.
use crate::args::Global;
use crate::output::{Output, Report};
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use serde::Serialize;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::table::{self, Cell, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Ready,
    /// `setup NAME` gets it.
    Missing,
    /// Only the person can: `detail` says what.
    Yours,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// The name `setup` takes.
    pub name: &'static str,
    pub state: State,
    /// Which commands use it, and for what.
    pub needed_for: &'static str,
    /// What was found, or what is missing.
    pub detail: String,
    /// The command that gets it, when orchard2 can.
    pub setup: Option<String>,
}

#[derive(Serialize)]
pub struct Doctor {
    /// Whether nothing is missing.
    ready: bool,
    checks: Vec<Check>,
    /// What `setup` did, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    done: Vec<String>,
}

/// How a check reads in the table: its name, and what it is for.
fn words(name: &str) -> (&'static str, &'static str) {
    match name {
        "usb" => ("USB access", "devices"),
        "python" => ("Python", "building"),
        "git" => ("git", "building"),
        "podman" => ("Podman", "building"),
        "disk" => ("Free space", "building"),
        "ssh" => ("ssh-keygen", "SSH"),
        _ => ("", ""),
    }
}

impl Check {
    /// The table's last two cells: the state, and how to get what is not
    /// there. `detail` is "what is wrong; what to do" where both are known.
    fn cells(&self) -> (String, String) {
        let parts: Vec<&str> = self.detail.split("; ").collect();
        match self.state {
            State::Ready if self.name == "disk" => (format!("ready, {}", self.detail.split(" free").next().unwrap_or("")), String::new()),
            State::Ready => ("ready".into(), String::new()),
            // The command, without what it runs.
            State::Missing => {
                let setup = self.setup.as_deref().unwrap_or("");
                (parts[parts.len() - 1].to_owned(), setup.split_once("  (").map_or(setup, |(command, _)| command).to_owned())
            }
            State::Yours if parts.len() > 1 => (parts[parts.len() - 2].to_owned(), parts[parts.len() - 1].to_owned()),
            State::Yours => (self.detail.clone(), String::new()),
        }
    }
}

impl Report for Doctor {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for line in &self.done {
            let _ = writeln!(text, "{line}");
        }
        // What is for the same thing stands together.
        let mut checks: Vec<&Check> = self.checks.iter().collect();
        checks.sort_by_key(|check| ["devices", "building", "SSH"].iter().position(|p| *p == words(check.name).1));
        let cells: Vec<(String, String)> = checks.iter().map(|check| check.cells()).collect();
        let rows: Vec<Vec<Cell>> = checks
            .iter()
            .zip(&cells)
            .map(|(check, (state, get))| {
                let (name, purpose) = words(check.name);
                let (colour, how) = match check.state {
                    State::Ready => (Style::Board, Style::Plain),
                    State::Missing => (Style::Number, Style::Mode),
                    State::Yours => (Style::Bad, Style::Plain),
                };
                vec![Cell::plain(name), Cell::plain(purpose), Cell::styled(state, colour), Cell::styled(get, how)]
            })
            .collect();
        let _ = writeln!(text, "{}", table::render_list(&["Check", "For", "State", "Get it"], &rows));
        text.into_bytes()
    }
}

fn on_path(tool: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|dir| dir.join(tool)).find(|path| path.is_file())
}

/// The package manager this computer has, and the command that installs
/// with it. macOS: glu, then Homebrew; Linux: apt, dnf or pacman, with sudo.
fn package_manager() -> Option<(&'static str, Vec<String>)> {
    let words = |list: &[&str]| list.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        let glu = on_path("glu").or_else(|| Some(PathBuf::from("/opt/glustore/bin/glu")).filter(|p| p.is_file()));
        if let Some(glu) = glu {
            return Some(("glu", vec![glu.to_string_lossy().into_owned(), "install".into(), "--yes".into()]));
        }
        return on_path("brew").map(|brew| ("brew", vec![brew.to_string_lossy().into_owned(), "install".into()]));
    }
    [
        ("apt", words(&["sudo", "apt-get", "install", "-y"])),
        ("dnf", words(&["sudo", "dnf", "install", "-y"])),
        ("pacman", words(&["sudo", "pacman", "-S", "--needed", "--noconfirm"])),
    ]
    .into_iter()
    .find(|(_, argv)| on_path(&argv[1]).is_some())
}

/// What a tool's package is called by each package manager.
fn package(tool: &str, manager: &str) -> Option<&'static str> {
    Some(match (tool, manager) {
        ("podman", _) => "podman",
        ("git", _) => "git",
        ("python", "glu" | "brew" | "pacman") => "python",
        ("python", _) => "python3",
        _ => return None,
    })
}

/// The command that installs `tool`'s package, or why there is none.
fn install_command(tool: &str) -> std::result::Result<Vec<String>, String> {
    let Some((manager, mut argv)) = package_manager() else {
        return Err(match cfg!(target_os = "macos") {
            true => format!("install glu or Homebrew (https://brew.sh), which install {tool}"),
            false => format!("install {tool} with your system's package manager"),
        });
    };
    argv.push(package(tool, manager).ok_or_else(|| format!("install {tool} yourself"))?.to_owned());
    Ok(argv)
}

/// A tool a package provides: found, or the package to install.
fn tool(name: &'static str, program: &str, needed_for: &'static str) -> Check {
    match on_path(program) {
        Some(path) => Check { name, state: State::Ready, needed_for, detail: path.display().to_string(), setup: None },
        None => match install_command(name) {
            Ok(argv) => Check {
                name,
                state: State::Missing,
                needed_for,
                detail: format!("{program} is not on PATH"),
                setup: Some(format!("orchard2 setup {name}  ({})", argv.join(" "))),
            },
            Err(how) => Check { name, state: State::Yours, needed_for, detail: format!("{program} is not on PATH; {how}"), setup: None },
        },
    }
}

/// What Podman says about its VM.
enum Machine {
    State(String),
    Absent,
    /// It did not answer in time, as when the VM hangs.
    Silent,
}

fn podman_machine() -> Machine {
    let mut ask = Command::new("podman");
    ask.args(["machine", "inspect", "--format", "{{.State}}"]);
    match orchard_build::within(&mut ask, orchard_build::PODMAN_ANSWERS) {
        Ok(Some(out)) if out.status.success() => Machine::State(String::from_utf8_lossy(&out.stdout).trim().to_owned()),
        Ok(None) => Machine::Silent,
        _ => Machine::Absent,
    }
}

/// Every check, in the order they are shown.
fn checks(cache: &Path) -> Vec<Check> {
    let mut found = Vec::new();
    if cfg!(target_os = "linux") {
        const FOR: &str = "every command that works on a device: your account may open it on USB";
        found.push(match orchard_runtime::usb_access::needed() {
            false => Check { name: "usb", state: State::Ready, needed_for: FOR, detail: orchard_runtime::usb_access::RULES.into(), setup: None },
            true => Check {
                name: "usb",
                state: State::Missing,
                needed_for: FOR,
                detail: format!("{} is not there, or not this version's", orchard_runtime::usb_access::RULES),
                setup: Some("orchard2 setup usb  (installs the udev rule; asks for your password)".into()),
            },
        });
    }
    const PYTHON: &str = "build, inputs: orchard's recipes are Python scripts (3.11 or newer)";
    found.push(match orchard_build::python() {
        Ok(python) => Check { name: "python", state: State::Ready, needed_for: PYTHON, detail: python.display().to_string(), setup: None },
        Err(e) => match install_command("python") {
            Ok(argv) => Check {
                name: "python",
                state: State::Missing,
                needed_for: PYTHON,
                detail: e.message,
                setup: Some(format!("orchard2 setup python  ({})", argv.join(" "))),
            },
            Err(how) => Check { name: "python", state: State::Yours, needed_for: PYTHON, detail: format!("{}; {how}", e.message), setup: None },
        },
    });
    found.push(tool("git", "git", "build, inputs: fetch the kernel's source and tell a checkout's state"));
    const PODMAN: &str = "build, inputs: the recipes compile in containers";
    let mut podman = tool("podman", "podman", PODMAN);
    if podman.state == State::Ready && cfg!(target_os = "macos") {
        let missing = match podman_machine() {
            Machine::State(state) if state == "running" => None,
            Machine::State(state) => Some((format!("its VM is {state}"), "podman machine start")),
            Machine::Absent => Some(("it has no VM yet".to_owned(), "podman machine init, then start")),
            Machine::Silent => {
                podman.state = State::Yours;
                podman.detail = "it does not answer; its VM may hang: podman machine stop, then start".into();
                None
            }
        };
        if let Some((detail, how)) = missing {
            podman.state = State::Missing;
            podman.detail = detail;
            podman.setup = Some(format!("orchard2 setup podman  ({how})"));
        }
    }
    found.push(podman);
    const SSH: &str = "install --ssh, and a RAM payload with the USB session: this computer's key";
    found.push(match on_path("ssh-keygen") {
        Some(path) => Check { name: "ssh", state: State::Ready, needed_for: SSH, detail: path.display().to_string(), setup: None },
        None => Check { name: "ssh", state: State::Yours, needed_for: SSH, detail: "ssh-keygen is not on PATH; install OpenSSH".into(), setup: None },
    });
    const DISK: &str = "build: a build that runs out of space halfway leaves broken work behind";
    let needed = orchard_build::manage::min_free();
    let free = orchard_cache::component::free_space(cache).map(|bytes| bytes as f64 / 1e9);
    found.push(match free {
        Ok(free) if free >= needed => Check { name: "disk", state: State::Ready, needed_for: DISK, detail: format!("{free:.0} GB free at {}", cache.display()), setup: None },
        Ok(free) => Check {
            name: "disk",
            state: State::Yours,
            needed_for: DISK,
            detail: format!("{free:.1} GB free of the {needed:.0} a build needs; orchard2 cache prune"),
            setup: None,
        },
        Err(e) => Check { name: "disk", state: State::Yours, needed_for: DISK, detail: e.to_string(), setup: None },
    });
    found
}

fn cache_dir(global: &Global) -> Result<PathBuf> {
    Ok(Store::open(global.cache.as_deref())?.dir().to_owned())
}

fn report(global: &Global, done: Vec<String>) -> Result<Output> {
    let checks = checks(&cache_dir(global)?);
    Ok(Doctor { ready: checks.iter().all(|c| c.state == State::Ready), checks, done }.into())
}

/// What this computer has and lacks; changes nothing.
pub fn doctor(global: &Global) -> Result<Output> {
    report(global, Vec::new())
}

fn run(argv: &[String], done: &mut Vec<String>) -> Result<()> {
    let line = argv.join(" ");
    // Said before it runs: it may ask for a password.
    eprintln!("+ {line}");
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context(&argv[0]))?;
    if !status.success() {
        return Err(Error::new(Code::Refused, format!("{line} failed ({status})")));
    }
    done.push(format!("ran: {line}"));
    Ok(())
}

/// Gets the named things, one by one; without a name, shows what there is.
pub fn setup(global: &Global, names: &[String]) -> Result<Output> {
    let cache = cache_dir(global)?;
    let known = checks(&cache);
    let mut done = Vec::new();
    for name in names {
        let Some(check) = known.iter().find(|c| c.name == name) else {
            let names: Vec<&str> = known.iter().map(|c| c.name).collect();
            return Err(Error::new(Code::InvalidInput, format!("no {name} to set up; there are: {}", names.join(", "))));
        };
        match check.state {
            State::Ready => done.push(format!("{name}: ready already ({})", check.detail)),
            State::Yours => return Err(Error::new(Code::Unsupported, format!("{name}: {}", check.detail)).hint("orchard2 cannot do this one for you")),
            State::Missing => match name.as_str() {
                "usb" => {
                    eprintln!("+ install {} (asks for your password)", orchard_runtime::usb_access::RULES);
                    orchard_runtime::usb_access::request()?;
                    done.push(format!("installed {}; reconnect a device that is plugged in", orchard_runtime::usb_access::RULES));
                }
                "podman" if on_path("podman").is_some() => {
                    // Installed, but macOS runs its containers in a VM.
                    if matches!(podman_machine(), Machine::Absent) {
                        run(&["podman".into(), "machine".into(), "init".into()], &mut done)?;
                    }
                    run(&["podman".into(), "machine".into(), "start".into()], &mut done)?;
                }
                tool => {
                    let argv = install_command(tool).map_err(|how| Error::new(Code::Unsupported, how))?;
                    run(&argv, &mut done)?;
                    if tool == "podman" && cfg!(target_os = "macos") {
                        run(&["podman".into(), "machine".into(), "init".into()], &mut done)?;
                        run(&["podman".into(), "machine".into(), "start".into()], &mut done)?;
                    }
                }
            },
        }
    }
    report(global, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_named_as_each_manager_names_them() {
        assert_eq!(package("python", "apt"), Some("python3"));
        assert_eq!(package("python", "glu"), Some("python"));
        assert_eq!(package("podman", "dnf"), Some("podman"));
        assert_eq!(package("ssh", "apt"), None);
    }

    #[test]
    fn every_check_says_what_it_is_for_and_a_missing_one_how_to_get_it() {
        let dir = std::env::temp_dir();
        let found = checks(&dir);
        let names: Vec<&str> = found.iter().map(|c| c.name).collect();
        for name in ["python", "git", "podman", "ssh", "disk"] {
            assert!(names.contains(&name), "{name}");
        }
        assert_eq!(names.contains(&"usb"), cfg!(target_os = "linux"));
        for check in &found {
            assert!(!check.needed_for.is_empty() && !check.detail.is_empty(), "{}", check.name);
            assert_eq!(check.state == State::Missing, check.setup.is_some(), "{}", check.name);
        }
        let text = String::from_utf8(Doctor { ready: false, checks: found, done: vec!["ran: x".into()] }.human()).unwrap();
        assert!(text.starts_with("ran: x\n┌"), "{text}");
        assert!(text.contains("│ Check ") && text.contains("│ git ") && text.contains("│ building │"), "{text}");
    }

    #[test]
    fn a_check_reads_as_a_state_and_how_to_get_it() {
        let check = |state, detail: &str, setup: Option<&str>| Check { name: "podman", state, needed_for: "x", detail: detail.into(), setup: setup.map(str::to_owned) };
        assert_eq!(check(State::Ready, "/usr/bin/podman", None).cells(), ("ready".into(), String::new()));
        assert_eq!(
            check(State::Missing, "its VM is stopped", Some("orchard2 setup podman  (podman machine start)")).cells(),
            ("its VM is stopped".into(), "orchard2 setup podman".into())
        );
        assert_eq!(
            check(State::Yours, "it does not answer; its VM may hang: podman machine stop, then start", None).cells(),
            ("it does not answer".into(), "its VM may hang: podman machine stop, then start".into())
        );
        let disk = Check { name: "disk", state: State::Ready, needed_for: "x", detail: "30 GB free at /cache".into(), setup: None };
        assert_eq!(disk.cells().0, "ready, 30 GB");
    }
}
