//! The device's disk: its partition table read and written, and the new
//! partitions written to. A regular file stands in for the disk in tests;
//! its partitions are files beside it, named as the device names its nodes
//! (`disk` and `disks2`), so that e2fsprogs is given a plain path either way.
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::gpt::{self, Table};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Sector size of a disk kept in a regular file.
const FILE_SECTOR: u64 = 4096;
// <sys/disk.h>, which the iPhoneOS SDK lacks.
const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;
const DKIOCSYNCHRONIZECACHE: libc::c_ulong = 0x2000_6416;
/// How long iOS gets to show a new partition's node.
const NODE_WAIT: Duration = Duration::from_secs(20);

pub struct Disk {
    pub path: PathBuf,
    pub sector: u64,
    pub bytes: u64,
    device: bool,
}

fn io(e: std::io::Error, what: impl std::fmt::Display) -> Error {
    Error::from(e).context(what)
}

/// (sector size, bytes) of an open disk or partition node.
fn geometry(file: &File) -> Result<(u64, u64)> {
    let (mut size, mut count) = (0u32, 0u64);
    let fd = file.as_raw_fd();
    // SAFETY: both requests write one integer of the size passed.
    let ok = unsafe { libc::ioctl(fd, DKIOCGETBLOCKSIZE, &mut size) == 0 && libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &mut count) == 0 };
    if !ok || size == 0 {
        return Err(Error::new(Code::Io, "the disk does not say its size"));
    }
    Ok((size as u64, size as u64 * count))
}

fn sync(file: &File, device: bool) -> Result<()> {
    if !device {
        return Ok(file.sync_all()?);
    }
    // As resize-apfs flushes a disk node: plain fsync (Rust's sync_all asks
    // for F_FULLFSYNC, which a device node refuses), then the disk's cache.
    let fd = file.as_raw_fd();
    // SAFETY: an open descriptor; the cache request takes no argument.
    unsafe {
        if libc::fsync(fd) != 0 {
            let e = std::io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP | libc::ENOTTY)) {
                return Err(io(e, "flushing the disk"));
            }
        }
        if libc::ioctl(fd, DKIOCSYNCHRONIZECACHE) != 0 {
            return Err(io(std::io::Error::last_os_error(), "flushing the disk's cache"));
        }
    }
    Ok(())
}

impl Disk {
    pub fn open(path: &Path) -> Result<Disk> {
        let file = File::open(path).map_err(|e| io(e, path.display()))?;
        let kind = file.metadata()?.file_type();
        let device = kind.is_block_device() || kind.is_char_device();
        let (sector, bytes) = if device { geometry(&file)? } else { (FILE_SECTOR, file.metadata()?.len()) };
        Ok(Disk { path: path.to_owned(), sector, bytes, device })
    }

    fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let file = File::open(&self.path).map_err(|e| io(e, self.path.display()))?;
        let mut data = vec![0u8; len];
        file.read_exact_at(&mut data, offset).map_err(|e| io(e, "reading the disk"))?;
        Ok(data)
    }

    fn table(&self, lba: u64) -> Result<Table> {
        let header = self.read(lba * self.sector, self.sector as usize)?;
        let (entries_lba, count) = gpt::entries_of(&header)?;
        let entries = self.read(entries_lba * self.sector, count as usize * 128)?;
        Ok(gpt::parse(&header, &entries)?)
    }

    /// Both copies of the partition table, which must agree.
    pub fn tables(&self) -> Result<(Table, Table)> {
        let primary = self.table(1).context("the partition table")?;
        let backup = self.table(primary.other_lba).context("the partition table's backup")?;
        if !primary.agrees_with(&backup) {
            return Err(Error::new(Code::Format, "the two copies of the partition table disagree"));
        }
        Ok((primary, backup))
    }

    /// Writes a changed table, the backup copy first, and reads it back.
    pub fn write_tables(&self, primary: &Table, backup: &Table, changed: &gpt::Changed) -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true);
        if self.device {
            // The disk is in use by iOS: a shared lock, as resize-apfs's
            // partition table writer takes it.
            options.custom_flags(libc::O_SHLOCK | libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options.open(&self.path).map_err(|e| io(e, "opening the disk for its partition table"))?;
        let pad = |bytes: &[u8]| {
            let mut out = bytes.to_vec();
            out.resize(out.len().div_ceil(self.sector as usize) * self.sector as usize, 0);
            out
        };
        let entries = pad(&changed.entries);
        for (lba, bytes) in [
            (backup.entries_lba, &entries),
            (backup.my_lba, &changed.backup_header),
            (primary.entries_lba, &entries),
            (primary.my_lba, &changed.primary_header),
        ] {
            file.write_all_at(bytes, lba * self.sector).map_err(|e| io(e, "writing the partition table"))?;
        }
        sync(&file, self.device)?;
        drop(file);
        let (now, _) = self.tables()?;
        if now.entries != changed.entries {
            return Err(Error::new(Code::Io, "the partition table read back differs from the one written"));
        }
        Ok(())
    }

    /// Partition `number` of this disk, opened for writing. On a device, its
    /// node once iOS shows it with the planned size.
    pub fn part(&self, number: u32, blocks: u64) -> Result<Part> {
        let bytes = blocks * self.sector;
        let node = PathBuf::from(format!("{}s{number}", self.path.display()));
        if !self.device {
            let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&node).map_err(|e| io(e, node.display()))?;
            file.set_len(bytes)?;
            return Ok(Part { tool_path: node.display().to_string(), file, bytes, device: false });
        }
        let deadline = Instant::now() + NODE_WAIT;
        loop {
            let found = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOFOLLOW).open(&node);
            match found.map_err(Error::from).and_then(|file| Ok((geometry(&file)?.1, file))) {
                Ok((size, file)) if size == bytes => {
                    return Ok(Part { tool_path: node.display().to_string(), file, bytes, device: true });
                }
                result if Instant::now() >= deadline => {
                    let why = match result {
                        Ok((size, _)) => format!("{} has {size} bytes, not the planned {bytes}", node.display()),
                        Err(e) => format!("{}: {e}", node.display()),
                    };
                    return Err(Error::new(Code::Io, format!("iOS did not show the new partition: {why}"))
                        .hint("Restart the device and try again."));
                }
                _ => std::thread::sleep(Duration::from_millis(250)),
            }
        }
    }
}

/// A partition being written.
pub struct Part {
    /// The node, as e2fsprogs is given it.
    pub tool_path: String,
    file: File,
    pub bytes: u64,
    device: bool,
}

impl Part {
    pub fn write_at(&self, data: &[u8], offset: u64) -> Result<()> {
        if offset + data.len() as u64 > self.bytes {
            return Err(Error::new(Code::TooLarge, "a write reaches past the partition"));
        }
        self.file.write_all_at(data, offset).map_err(|e| io(e, "writing the partition"))
    }

    pub fn read_at(&self, len: usize, offset: u64) -> Result<Vec<u8>> {
        let mut data = vec![0u8; len];
        self.file.read_exact_at(&mut data, offset).map_err(|e| io(e, "reading the partition"))?;
        Ok(data)
    }

    pub fn sync(&self) -> Result<()> {
        sync(&self.file, self.device)
    }
}

/// What a partition holds, by its first 4 KiB: `ext4`, `FAT32`, `APFS`.
pub fn filesystem(first: &[u8]) -> Option<&'static str> {
    if first.len() >= 1082 && first[1080..1082] == [0x53, 0xef] {
        Some("ext4")
    } else if first.len() >= 90 && &first[82..90] == b"FAT32   " {
        Some("FAT32")
    } else if first.len() >= 36 && &first[32..36] == b"NXSB" {
        Some("APFS")
    } else {
        None
    }
}

impl Disk {
    /// What partition `number`, which starts at `first`, holds, and the name
    /// its filesystem has.
    pub fn describe(&self, number: u32, first: u64) -> (Option<&'static str>, Option<String>) {
        let head = if self.device {
            self.read(first * self.sector, 4096)
        } else {
            std::fs::File::open(format!("{}s{number}", self.path.display())).map_err(Error::from).and_then(|file| {
                let mut head = vec![0u8; 4096];
                file.read_exact_at(&mut head, 0)?;
                Ok(head)
            })
        };
        let Ok(head) = head else { return (None, None) };
        let kind = filesystem(&head);
        let label = match kind {
            Some("ext4") => Some(&head[1144..1160]),
            Some("FAT32") => Some(&head[71..82]),
            _ => None,
        };
        let label = label.map(|raw| String::from_utf8_lossy(raw).trim_matches(['\0', ' ']).to_owned()).filter(|l| !l.is_empty());
        (kind, label)
    }
}
