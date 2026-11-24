//! The installer's or launcher's own messages (everything on stderr: the
//! flags, "exploit attempt", "USB port reset", the driver's log) also in a
//! file, `logs/installer-<time>.log` in the cache, each line with the time of
//! day. Started from the app menu there is no terminal to read them in; the
//! Details' Copy includes the file's end.
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// Sends stderr through a pipe to the terminal it had and to the log file.
/// Once per process; None (stderr untouched) where that isn't possible.
pub fn start(name: &str) -> Option<&'static Path> {
    if let Some(path) = PATH.get() {
        return Some(path);
    }
    let dir = orchard_cache::store::default_dir().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{name}-{stamp}.log"));
    let mut file = std::fs::File::create(&path).ok()?;
    // SAFETY: a new pipe; fd 2 is duplicated aside, then replaced by the
    // pipe's write end. The reader thread owns the read end and the copy.
    let (read, terminal) = unsafe {
        let mut fds = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let terminal = libc::dup(2);
        if terminal < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return None;
        }
        libc::close(fds[1]);
        (std::fs::File::from_raw_fd(fds[0]), std::fs::File::from_raw_fd(terminal))
    };
    std::thread::spawn(move || {
        let mut terminal = terminal;
        for line in BufReader::new(read).lines() {
            let Ok(line) = line else { break };
            let _ = writeln!(terminal, "{line}");
            let _ = writeln!(file, "{}  {line}", time_of_day());
            let _ = file.flush();
        }
    });
    let _ = PATH.set(path);
    PATH.get().map(PathBuf::as_path)
}

/// The log file of this run, once started.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// The file's last `n` lines.
pub fn tail(n: usize) -> Vec<String> {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// HH:MM:SS, local time.
fn time_of_day() -> String {
    // SAFETY: localtime_r fills the struct it is given.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    /// In a child process (stderr is replaced for good): a message reaches
    /// both the terminal and the file, the file's line with the time.
    #[test]
    fn stderr_goes_to_the_terminal_and_the_file() {
        if std::env::var_os("ORCHARD_SESSION_LOG_CHILD").is_some() {
            let path = super::start("installer").expect("started");
            eprintln!("hello from the installer");
            std::thread::sleep(std::time::Duration::from_millis(300));
            println!("PATH={}", path.display());
            return;
        }
        let home = std::env::temp_dir().join(format!("orchard-session-log-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "session_log::tests::stderr_goes_to_the_terminal_and_the_file", "--nocapture", "--test-threads=1"])
            .env("ORCHARD_SESSION_LOG_CHILD", "1")
            .env("HOME", &home)
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let path = stdout.lines().find_map(|l| l.split_once("PATH=").map(|(_, p)| p)).unwrap_or_else(|| panic!("child printed no log path:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr)));
        let text = std::fs::read_to_string(path).unwrap();
        let line = text.lines().find(|l| l.ends_with("hello from the installer")).expect("in the file");
        assert_eq!(line.find("  hello"), Some(8), "HH:MM:SS first: {line}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("hello from the installer"), "and on the terminal");
        let _ = std::fs::remove_dir_all(&home);
    }
}
