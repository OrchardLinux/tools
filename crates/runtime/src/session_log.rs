//! The installer's or launcher's run log (`orchard_cache::runlog`):
//! `logs/<date>-<time>-installer.log` in the cache, with everything on stderr (the
//! flags, "exploit attempt", the driver's log) as orchard's own lines next
//! to the tools' output. Started from the app menu there is no terminal to
//! read them in; the Details' Copy includes the file's end.
use std::path::Path;

/// Starts the run log and sends stderr into it and on to the terminal it
/// had. Once per process; None (stderr untouched) where that isn't possible.
pub fn start(name: &str) -> Option<&'static Path> {
    let path = orchard_cache::runlog::start(name, &format!("orchard {} ({name})", env!("CARGO_PKG_VERSION")))?;
    orchard_cache::runlog::capture_stderr(true);
    Some(path)
}

/// The log file of this run, once started.
pub fn path() -> Option<&'static Path> {
    orchard_cache::runlog::path()
}

/// The file's last `n` lines.
pub fn tail(n: usize) -> Vec<String> {
    orchard_cache::runlog::tail(n)
}

#[cfg(test)]
mod tests {
    /// In a child process (stderr is replaced for good): a message reaches
    /// both the terminal and the file, the file's line with the time and
    /// who said it.
    #[test]
    fn stderr_goes_to_the_terminal_and_the_file() {
        if std::env::var_os("ORCHARD_SESSION_LOG_CHILD").is_some() {
            let path = super::start("installer").expect("started");
            eprintln!("hello from the installer");
            orchard_cache::runlog::release_stderr();
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
        assert!(line.starts_with("[000.") && line.contains("] [orchard] hello"), "time and source first: {line}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("hello from the installer"), "and on the terminal");
        let _ = std::fs::remove_dir_all(&home);
    }
}
