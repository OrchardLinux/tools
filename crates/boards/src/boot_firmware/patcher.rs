//! Runs a bundled upstream iBoot patcher on one image.
//!
//! The patchers are third-party C programs (see `vendor/`). They are compiled
//! and embedded at build time, then run as a child process per image: writing
//! the binary to a private temp directory, feeding it the image, and reading
//! the result back.
use orchard_cache::{Code, Context, Error, Result};
use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub enum Patcher {
    /// hBootPatcher, for 64-bit iBoot, which comes as IMG4.
    Arm64,
    /// iBoot32Patcher, for 32-bit iBoot, which comes as IMG3.
    Arm32,
}

impl Patcher {
    fn executable(&self) -> &'static [u8] {
        match self {
            Self::Arm64 => include_bytes!(concat!(env!("OUT_DIR"), "/hbootpatcher")),
            Self::Arm32 => include_bytes!(concat!(env!("OUT_DIR"), "/iboot32patcher")),
        }
    }

    /// The SHA-256 of the bundled program: what it makes of an image can
    /// change whenever the program does.
    pub fn sha256(&self) -> String {
        orchard_cache::store::sha256(self.executable())
    }

    pub fn patch(&self, data: &[u8], bootargs: &str) -> Result<Vec<u8>> {
        if data.len() < 0x4000 {
            return Err(Error::new(Code::Format, "iBoot image is too small"));
        }
        let executable = self.executable();
        let dir = tempfile::tempdir()?;
        let tool = dir.path().join("patcher");
        let input = dir.path().join("input");
        let output = dir.path().join("output");
        fs::write(&tool, executable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o700))?;
        }
        fs::write(&input, data)?;
        let log = fs::File::create(dir.path().join("log"))?;
        let mut command = Command::new(tool);
        match self {
            Self::Arm64 => {
                command.arg("-airs").arg(&input).arg(&output);
            }
            Self::Arm32 => {
                command.arg(&input).arg(&output).arg("-b").arg(bootargs);
            }
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(match result {
                        Err(e) => e.into(),
                        _ => Error::new(Code::Timeout, "iBoot patcher exceeded 30 seconds"),
                    });
                }
            }
        };
        if !status.success() {
            let mut log = String::new();
            fs::File::open(dir.path().join("log"))?
                .take(4096)
                .read_to_string(&mut log)?;
            let log = log.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut message = format!("iBoot patcher exited with {status}");
            if !log.is_empty() {
                message.push_str(": ");
                message.push_str(&log);
            }
            return Err(Error::new(Code::Format, message)
                .hint("the image may be from a different iBoot build than the data expects"));
        }
        let mut bytes = Vec::new();
        fs::File::open(&output)
            .context("iBoot patcher output")?
            .take(data.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != data.len() {
            return Err(Error::new(Code::Format, "iBoot patcher changed image size"));
        }
        Ok(bytes)
    }
}
