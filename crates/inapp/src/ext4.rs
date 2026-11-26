//! The new root's ext4 filesystem, changed without mounting it: iOS has no
//! ext4, so files go in and out through e2fsprogs' own programs (`debugfs`,
//! `tune2fs`, `resize2fs`, `e2fsck`), which the helper brings along.
use orchard_cache::{Code, Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One name in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The inode's mode, type bits included.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Item {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }

    pub fn is_file(&self) -> bool {
        self.mode & 0o170000 == 0o100000
    }
}

/// A directory's names; `None` for a directory that is not there.
pub type Listing = BTreeMap<String, Option<BTreeMap<String, Item>>>;

/// A change to the filesystem. Paths are absolute, without a trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A directory with this owner and mode, made if it is not there.
    Dir { path: String, mode: u32, uid: u32, gid: u32 },
    /// A file with this content, replacing one that is there. `mtime`
    /// (seconds since 1970) where the time of writing is not what it should say.
    File { path: String, data: Vec<u8>, mode: u32, uid: u32, gid: u32, mtime: Option<u64> },
    /// A symbolic link, replacing what is there.
    Link { path: String, target: String },
}

impl Change {
    pub fn file(path: impl Into<String>, data: impl Into<Vec<u8>>, mode: u32) -> Change {
        Change::File { path: path.into(), data: data.into(), mode, uid: 0, gid: 0, mtime: None }
    }

    fn path(&self) -> &str {
        match self {
            Change::Dir { path, .. } | Change::File { path, .. } | Change::Link { path, .. } => path,
        }
    }
}

pub struct Ext4 {
    tools: PathBuf,
    /// The partition, as e2fsprogs is given it.
    device: String,
    /// A directory of the helper's own for what goes in and out.
    work: PathBuf,
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &path[..at],
    }
}

fn leaf(path: &str) -> &str {
    &path[path.rfind('/').map_or(0, |at| at + 1)..]
}

/// A name as debugfs takes it: in double quotes, which keep its blanks.
fn q(name: &str) -> String {
    format!("\"{name}\"")
}

/// An absolute path debugfs can be given: printable, and without the quote
/// that would end its argument.
fn plain(path: &str) -> Result<()> {
    let ok = path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains("//")
        && path.split('/').skip(1).all(|p| p != "." && p != "..")
        && path.chars().all(|c| !c.is_control() && c != '"');
    if ok {
        Ok(())
    } else {
        Err(Error::new(Code::InvalidInput, format!("not a path the installer writes: {path:?}")))
    }
}

impl Ext4 {
    pub fn new(tools: &Path, device: &str, work: &Path) -> Result<Ext4> {
        std::fs::create_dir_all(work)?;
        Ok(Ext4 { tools: tools.to_owned(), device: device.to_owned(), work: work.to_owned() })
    }

    /// Runs one of the programs on the partition; its output, or what it
    /// said when it failed.
    fn run(&self, tool: &str, args: &[&str], ok: &[i32]) -> Result<(String, String)> {
        let program = self.tools.join(tool);
        let output = Command::new(&program).args(args).output().map_err(|e| Error::from(e).context(program.display()))?;
        let (out, err) = (String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned());
        if !output.status.code().is_some_and(|code| ok.contains(&code)) {
            let last = |text: &str| text.lines().rfind(|l| !l.trim().is_empty()).map(str::to_owned);
            let said = last(&err).or_else(|| last(&out)).unwrap_or_else(|| "no output".into());
            return Err(Error::new(Code::Io, format!("{tool} failed: {said}")));
        }
        Ok((out, err))
    }

    /// The filesystem's identity (`tune2fs -U -L`).
    pub fn identify(&self, uuid: &str, label: &str) -> Result<()> {
        self.run("tune2fs", &["-U", uuid, "-L", label, &self.device], &[0]).map(drop)
    }

    /// Grows the filesystem to `blocks` 4 KiB blocks, checking it first as
    /// resize2fs asks.
    pub fn resize(&self, blocks: u64) -> Result<()> {
        // 1: errors corrected, which an image restored from allocated blocks
        // only has in its free counts.
        self.run("e2fsck", &["-fy", &self.device], &[0, 1])?;
        self.run("resize2fs", &[&self.device, &blocks.to_string()], &[0]).map(drop)
    }

    /// A read-only check; an error names what e2fsck found.
    pub fn check(&self) -> Result<()> {
        self.run("e2fsck", &["-fn", &self.device], &[0]).map(drop)
    }

    /// debugfs with a file of commands. It reports failures as lines on
    /// stderr and goes on, so any line there besides its banner fails.
    /// Commands that take a name are given it in its directory (`cd` first):
    /// debugfs resolves the directory part of `/name` to where it stands.
    fn debugfs(&self, commands: &str, write: bool) -> Result<String> {
        let script = self.work.join("commands");
        std::fs::write(&script, commands)?;
        let mut args = vec!["-f", script.to_str().ok_or_else(|| Error::new(Code::InvalidInput, "work directory is not UTF-8"))?];
        if write {
            args.insert(0, "-w");
        }
        args.push(&self.device);
        let (out, err) = self.run("debugfs", &args, &[0])?;
        if let Some(line) = err.lines().find(|l| !l.trim().is_empty() && !l.starts_with("debugfs ")) {
            return Err(Error::new(Code::Io, format!("debugfs: {line}")));
        }
        Ok(out)
    }

    /// The names in each of `dirs`.
    pub fn list(&self, dirs: &BTreeSet<String>) -> Result<Listing> {
        let mut commands = String::new();
        for dir in dirs {
            if dir != "/" {
                plain(dir)?;
            }
            commands.push_str(&format!("ls -p {}\n", q(dir)));
        }
        let script = self.work.join("commands");
        std::fs::write(&script, &commands)?;
        // A directory that is not there is an answer here, not a failure.
        let (out, _) = self.run("debugfs", &["-f", &script.display().to_string(), &self.device], &[0])?;
        let mut listing: Listing = dirs.iter().map(|d| (d.clone(), None)).collect();
        let mut current: Option<String> = None;
        for line in out.lines() {
            if let Some(dir) = line.strip_prefix("debugfs: ls -p ") {
                current = Some(dir.trim().trim_matches('"').to_owned());
                continue;
            }
            // /inode/mode/uid/gid/name/size/
            let fields: Vec<&str> = line.split('/').collect();
            let (Some(dir), true) = (&current, fields.len() >= 7 && fields[0].is_empty()) else { continue };
            let (Ok(mode), Ok(uid), Ok(gid)) = (u32::from_str_radix(fields[2], 8), fields[3].parse(), fields[4].parse()) else { continue };
            listing.entry(dir.clone()).or_default().get_or_insert_with(BTreeMap::new).insert(fields[5].to_owned(), Item { mode, uid, gid });
        }
        Ok(listing)
    }

    /// A file's content, or `None` when it is not there.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        plain(path)?;
        let out = self.work.join("read");
        let _ = std::fs::remove_file(&out);
        let dir = parent(path).to_owned();
        let there = self.list(&BTreeSet::from([dir.clone()]))?.remove(&dir).flatten().is_some_and(|names| names.get(leaf(path)).is_some_and(Item::is_file));
        if !there {
            return Ok(None);
        }
        self.debugfs(&format!("dump {} {}\n", q(path), q(&out.display().to_string())), false)?;
        let data = std::fs::read(&out)?;
        let _ = std::fs::remove_file(&out);
        Ok(Some(data))
    }

    /// Applies the changes in order. Directories a change needs above it are
    /// made (root's, 0755).
    pub fn apply(&self, changes: &[Change]) -> Result<()> {
        // What is there, for every directory a change touches or needs.
        let mut ask = BTreeSet::new();
        for change in changes {
            plain(change.path())?;
            let mut dir = parent(change.path());
            loop {
                ask.insert(dir.to_owned());
                if dir == "/" {
                    break;
                }
                dir = parent(dir);
            }
        }
        let listing = self.list(&ask)?;
        // Names known to exist, kept current as the script makes more.
        let mut dirs: BTreeSet<String> = listing.iter().filter(|(_, names)| names.is_some()).map(|(d, _)| d.clone()).collect();
        let mut names: BTreeSet<String> = BTreeSet::new();
        for (dir, items) in &listing {
            for name in items.iter().flat_map(|items| items.keys()) {
                names.insert(if dir == "/" { format!("/{name}") } else { format!("{dir}/{name}") });
            }
        }
        let staging = self.work.join("staging");
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let mut script = String::new();
        let own = |script: &mut String, path: &str, mode: u32, uid: u32, gid: u32| {
            let path = q(path);
            script.push_str(&format!("sif {path} mode 0{mode:o}\nsif {path} uid {uid}\nsif {path} gid {gid}\n"));
        };
        let make_dirs = |script: &mut String, dirs: &mut BTreeSet<String>, names: &BTreeSet<String>, dir: &str| -> Result<()> {
            let mut missing = Vec::new();
            let mut at = dir;
            while !dirs.contains(at) {
                if names.contains(at) {
                    return Err(Error::new(Code::Format, format!("{at} is in the way of a directory")));
                }
                missing.push(at.to_owned());
                at = parent(at);
            }
            for path in missing.into_iter().rev() {
                script.push_str(&format!("cd {}\nmkdir {}\n", q(parent(&path)), q(leaf(&path))));
                own(script, &path, 0o040755, 0, 0);
                dirs.insert(path);
            }
            Ok(())
        };
        for (index, change) in changes.iter().enumerate() {
            let path = change.path();
            make_dirs(&mut script, &mut dirs, &names, parent(path))?;
            match change {
                Change::Dir { mode, uid, gid, .. } => {
                    if !dirs.contains(path) {
                        if names.contains(path) {
                            return Err(Error::new(Code::Format, format!("{path} is in the way of a directory")));
                        }
                        script.push_str(&format!("cd {}\nmkdir {}\n", q(parent(path)), q(leaf(path))));
                        dirs.insert(path.to_owned());
                    }
                    own(&mut script, path, 0o040000 | (mode & 0o7777), *uid, *gid);
                }
                Change::File { data, mode, uid, gid, mtime, .. } => {
                    if dirs.contains(path) {
                        return Err(Error::new(Code::Format, format!("{path} is a directory")));
                    }
                    if names.contains(path) {
                        script.push_str(&format!("cd {}\nrm {}\n", q(parent(path)), q(leaf(path))));
                    }
                    let source = staging.join(index.to_string());
                    std::fs::write(&source, data)?;
                    script.push_str(&format!("cd {}\nwrite {} {}\n", q(parent(path)), q(&source.display().to_string()), q(leaf(path))));
                    own(&mut script, path, 0o100000 | (mode & 0o7777), *uid, *gid);
                    if let Some(mtime) = mtime {
                        script.push_str(&format!("sif {} mtime @{mtime}\n", q(path)));
                    }
                }
                Change::Link { target, .. } => {
                    if target.is_empty() || target.chars().any(|c| c.is_control() || c == '"') {
                        return Err(Error::new(Code::InvalidInput, format!("not a link target the installer writes: {target:?}")));
                    }
                    if names.contains(path) {
                        script.push_str(&format!("cd {}\nrm {}\n", q(parent(path)), q(leaf(path))));
                    }
                    script.push_str(&format!("cd {}\nsymlink {} {}\n", q(parent(path)), q(leaf(path)), q(target)));
                }
            }
            names.insert(path.to_owned());
        }
        let result = self.debugfs(&script, true).map(drop);
        let _ = std::fs::remove_dir_all(&staging);
        result
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The e2fsprogs built for this computer (`ios-installer/build-e2fsprogs.sh host`).
    pub fn host_tools() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ios-installer/out/e2fsprogs-host");
        dir.join("debugfs").is_file().then_some(dir)
    }

    /// An ext4 filesystem of `mib` MiB, made by this computer's mke2fs, in a
    /// file twice as large, so it can grow.
    pub fn image(dir: &Path, mib: u64) -> Option<PathBuf> {
        let path = dir.join("root.img");
        std::fs::File::create(&path).ok()?.set_len(mib << 21).ok()?;
        let blocks = (mib << 20) / 4096;
        let made = Command::new("mke2fs").args(["-q", "-t", "ext4", "-b", "4096", "-m", "0", "-E", "nodiscard"]).arg(&path).arg(blocks.to_string()).output().ok()?;
        made.status.success().then_some(path)
    }

    #[test]
    fn files_directories_and_links_go_in_and_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let (Some(tools), Some(image)) = (host_tools(), image(dir.path(), 64)) else {
            eprintln!("skipped: needs ios-installer/out/e2fsprogs-host and mke2fs");
            return;
        };
        let fs = Ext4::new(&tools, image.to_str().unwrap(), &dir.path().join("work")).unwrap();
        fs.apply(&[Change::file("/etc/passwd", "root:x:0:0::/root:/bin/sh\n", 0o644)]).unwrap();
        fs.apply(&[
            Change::file("/etc/passwd", "tablet:x:1000:1000::/home/tablet:/bin/sh\n", 0o644),
            Change::File { path: "/etc/orchard/pin".into(), data: b"tablet:$6$x\n".to_vec(), mode: 0o640, uid: 0, gid: 1000, mtime: Some(1_700_000_000) },
            Change::Dir { path: "/var/lib/orchard/pin-tally".into(), mode: 0o700, uid: 1000, gid: 1000 },
            Change::Link { path: "/etc/localtime".into(), target: "../usr/share/zoneinfo/Europe/Berlin".into() },
        ])
        .unwrap();
        assert_eq!(fs.read("/etc/passwd").unwrap().unwrap(), b"tablet:x:1000:1000::/home/tablet:/bin/sh\n");
        assert_eq!(fs.read("/etc/shadow").unwrap(), None);
        let listing = fs.list(&BTreeSet::from(["/etc/orchard".to_string(), "/var/lib/orchard".to_string(), "/nowhere".to_string()])).unwrap();
        assert_eq!(listing["/etc/orchard"].as_ref().unwrap()["pin"], Item { mode: 0o100640, uid: 0, gid: 1000 });
        assert_eq!(listing["/var/lib/orchard"].as_ref().unwrap()["pin-tally"], Item { mode: 0o040700, uid: 1000, gid: 1000 });
        assert!(listing["/nowhere"].is_none());
        fs.identify("0f0e0d0c-0b0a-4908-8706-050403020100", "omarchy-a").unwrap();
        fs.resize(20_000).unwrap();
        fs.check().unwrap();
        fs.apply(&[Change::file("/usr/share/alsa/J81 Speakers.conf", "ucm", 0o644)]).unwrap();
        assert_eq!(fs.read("/usr/share/alsa/J81 Speakers.conf").unwrap().unwrap(), b"ucm");
        assert!(fs.apply(&[Change::file("/etc/pass\"wd", "", 0o644)]).is_err());
        assert!(fs.apply(&[Change::file("/etc/passwd/x", "", 0o644)]).is_err(), "a file is in the way");
    }
}
