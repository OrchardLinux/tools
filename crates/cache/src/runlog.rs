//! The run log: one file per run with everything that happened in order,
//! orchard's own steps and the output of the tools it ran.
//!
//! ```text
//! [000.0] [orchard] Run log, 2026-11-28 11:48:12, orchard 0.2.0
//! [011.2] [palera1n 2.4]  - [11/28/26 11:48:24] <Info>: Checkmate!
//! [021.4] [orchard] USB port reset 1: … off and on again, 10.0 s after the device dropped
//! ```
//!
//! Each line has the seconds since the run started and who said it. A line is
//! written to the file the moment it is said, at the file's end, so a run
//! that is cancelled or killed leaves its log up to there. An ECID is never
//! written: the log can be posted as it is.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    file: Mutex<std::fs::File>,
    started: Instant,
    path: PathBuf,
}

static LOG: OnceLock<Log> = OnceLock::new();
/// Values never written (the device's ECID, in the spellings tools print).
static HIDDEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Starts this process's run log at `path` (added to, where it exists), with
/// `title` in its first line. Once per process: a second start keeps the
/// first. None where the file cannot be written.
pub fn start_at(path: &Path, title: &str) -> Option<&'static Path> {
    if LOG.get().is_none() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        if LOG.set(Log { file: Mutex::new(file), started: Instant::now(), path: path.to_owned() }).is_ok() {
            line("orchard", &format!("Run log, {}, {title}", date_time()));
        }
    }
    self::path()
}

/// Starts the run log as `logs/NAME-TIME.log` in the default cache.
pub fn start(name: &str, title: &str) -> Option<&'static Path> {
    if let Some(path) = path() {
        return Some(path);
    }
    let dir = crate::store::default_dir().ok()?.join("logs");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    start_at(&dir.join(format!("{name}-{stamp}.log")), title)
}

/// The run log's file, once started.
pub fn path() -> Option<&'static Path> {
    LOG.get().map(|l| l.path.as_path())
}

/// Never writes `value` (an ECID) from here on, in any hex spelling.
pub fn hide(value: &str) {
    let digits = value.trim().trim_start_matches("0x").trim_start_matches("0X").trim_start_matches('0').to_ascii_lowercase();
    if digits.len() < 6 {
        return;
    }
    if let Ok(mut hidden) = HIDDEN.lock() {
        if !hidden.contains(&digits) {
            hidden.push(digits);
        }
    }
}

/// `text` as it is written: without the hidden values, and without the hex
/// number behind any "ECID".
fn cleaned(text: &str) -> String {
    let mut out = text.to_owned();
    if let Ok(hidden) = HIDDEN.lock() {
        for value in hidden.iter() {
            while let Some(at) = out.to_ascii_lowercase().find(value.as_str()) {
                out.replace_range(at..at + value.len(), "(hidden)");
            }
        }
    }
    let mut from = 0;
    while let Some(found) = out[from..].to_ascii_lowercase().find("ecid") {
        let label_end = from + found + 4;
        let rest = &out[label_end..];
        let skip = rest.len() - rest.trim_start_matches([':', '=', ' ', '\t']).len();
        let number = &rest[skip..];
        let prefix = if number.starts_with("0x") || number.starts_with("0X") { 2 } else { 0 };
        let digits = number[prefix..].chars().take_while(char::is_ascii_hexdigit).count();
        if digits >= 6 && skip <= 3 {
            let (start, end) = (label_end + skip, label_end + skip + prefix + digits);
            out.replace_range(start..end, "(hidden)");
            from = start + "(hidden)".len();
        } else {
            from = label_end;
        }
    }
    out
}

/// Adds what `source` said, each line of `text` as a line of its own. Does
/// nothing before the log is started.
pub fn line(source: &str, text: &str) {
    let Some(log) = LOG.get() else { return };
    let seconds = log.started.elapsed().as_secs_f64();
    let mut entry = String::new();
    for part in cleaned(text).split(['\n', '\r']).map(str::trim_end).filter(|l| !l.trim().is_empty()) {
        entry.push_str(&format!("[{seconds:05.1}] [{source}] {part}\n"));
    }
    if entry.is_empty() {
        return;
    }
    // One write per entry, at the file's end: nothing is held back.
    if let Ok(mut file) = log.file.lock() {
        let _ = file.write_all(entry.as_bytes());
    }
}

/// The log's last `n` lines.
pub fn tail(n: usize) -> Vec<String> {
    let Some(text) = path().and_then(|p| std::fs::read(p).ok()) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// What `stderr` was before the log took it, and whether the reader is done.
struct Captured {
    terminal: libc::c_int,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

static CAPTURED: Mutex<Option<Captured>> = Mutex::new(None);

/// Sends everything this process writes to stderr into the run log, as
/// orchard's own lines, and with `echo` also on to where stderr went before.
/// False where that isn't possible (stderr stays as it is).
pub fn capture_stderr(echo: bool) -> bool {
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;
    let Ok(mut captured) = CAPTURED.lock() else { return false };
    if captured.is_some() {
        return true;
    }
    if LOG.get().is_none() {
        return false;
    }
    // SAFETY: a new pipe; fd 2 is duplicated aside, then replaced by the
    // pipe's write end. The reader thread owns the read end.
    let (read, terminal) = unsafe {
        let mut fds = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return false;
        }
        let terminal = libc::dup(2);
        if terminal < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return false;
        }
        libc::close(fds[1]);
        (std::fs::File::from_raw_fd(fds[0]), terminal)
    };
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = done.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(read);
        let mut bytes = Vec::new();
        while reader.read_until(b'\n', &mut bytes).is_ok_and(|n| n > 0) {
            if echo {
                // SAFETY: `terminal` stays open for the process's life.
                unsafe {
                    libc::write(terminal, bytes.as_ptr().cast(), bytes.len());
                }
            }
            line("orchard", &String::from_utf8_lossy(&bytes));
            bytes.clear();
        }
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    *captured = Some(Captured { terminal, done });
    true
}

/// Gives stderr back and waits (briefly) until what was written to it has
/// reached the log and the terminal: before the process ends.
pub fn release_stderr() {
    let Some(captured) = CAPTURED.lock().ok().and_then(|mut c| c.take()) else { return };
    // SAFETY: puts the earlier stderr back on fd 2, which closes the pipe's
    // write end, so the reader sees its end.
    unsafe {
        libc::dup2(captured.terminal, 2);
    }
    let until = Instant::now() + std::time::Duration::from_millis(500);
    while !captured.done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// YYYY-MM-DD HH:MM:SS, local time.
fn date_time() -> String {
    // SAFETY: localtime_r fills the struct it is given.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ecid_is_never_written() {
        hide("0x001A2B3C4D5E6F");
        assert_eq!(cleaned("device 1a2b3c4d5e6f at 3-3"), "device (hidden) at 3-3");
        assert_eq!(cleaned("found 0x1A2B3C4D5E6F"), "found 0x(hidden)");
        assert_eq!(cleaned("CPID:8015 ECID:00AABBCCDDEEFF11 IBFL:3C"), "CPID:8015 ECID:(hidden) IBFL:3C");
        assert_eq!(cleaned("ecid = 0xAABBCCDDEEFF11, ECID unknown"), "ecid = (hidden), ECID unknown");
    }

    /// In a child process (the log and stderr are the process's own): lines
    /// are in the file at once, in order, each with its time and source, and
    /// stderr reaches both the file and the terminal.
    #[test]
    fn lines_are_in_the_file_as_soon_as_they_are_said() {
        if let Some(path) = std::env::var_os("ORCHARD_RUNLOG_CHILD") {
            let path = PathBuf::from(path);
            std::fs::write(&path, "an earlier run\n").unwrap();
            start_at(&path, "test").expect("started");
            line("palera1n 2.4", "\x20- Checkmate!\r\nsecond line\n");
            let now = std::fs::read_to_string(&path).unwrap();
            assert!(now.starts_with("an earlier run\n[000.0] [orchard] Run log, "), "added to: {now}");
            assert!(now.ends_with("] [palera1n 2.4]  - Checkmate!\n[000.0] [palera1n 2.4] second line\n"), "at once: {now}");
            assert!(capture_stderr(true));
            eprintln!("hello from orchard");
            release_stderr();
            assert!(std::fs::read_to_string(&path).unwrap().ends_with("] [orchard] hello from orchard\n"));
            return;
        }
        let path = std::env::temp_dir().join(format!("orchard-runlog-{}.log", std::process::id()));
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runlog::tests::lines_are_in_the_file_as_soon_as_they_are_said", "--nocapture", "--test-threads=1"])
            .env("ORCHARD_RUNLOG_CHILD", &path)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}\n{}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr.contains("hello from orchard"), "and on the terminal: {stderr}");
        let _ = std::fs::remove_file(&path);
    }
}
