//! The USB serial console of a RAM payload: one command at a time at its
//! shell prompt, and files as an exact number of raw bytes.
//!
//! Each command is wrapped in two marker lines with a token made for it, so
//! its output and exit status are told apart from whatever else the console
//! prints. Every wait has a deadline; after one that ran out the outcome of
//! the command is unknown, and the caller is told so.
use orchard_cache::{Code, Error, Result};
use std::time::{Duration, Instant};

/// The largest file sent or received this way. SSH takes larger ones.
pub const LIMIT: usize = 16 << 20;

/// What a console is spoken to through: the serial port, or a script in
/// tests.
pub trait Wire {
    /// Writes all of `data` before `deadline`.
    fn send(&mut self, data: &[u8], deadline: Instant) -> Result<()>;
    /// What arrived, waiting for something until `deadline`.
    fn receive(&mut self, deadline: Instant) -> Result<Vec<u8>>;
}

pub struct Console<W: Wire> {
    wire: W,
    pending: Vec<u8>,
}

fn unknown(what: &str) -> Error {
    Error::new(Code::Io, format!("{what}; whether the command ran is not known"))
}

/// Where the shell's prompt ends in `data`: `/ # ` at the start of a line.
fn prompt(data: &[u8]) -> Option<usize> {
    const PROMPT: &[u8] = b"/ # ";
    (0..data.len().saturating_sub(PROMPT.len() - 1))
        .find(|&at| data[at..].starts_with(PROMPT) && (at == 0 || matches!(data[at - 1], b'\r' | b'\n')))
        .map(|at| at + PROMPT.len())
}

/// A marker line `NAME` after a line break: where the break starts and where
/// the line ends. With `status`, the line is `NAME:CODE` and the code comes
/// back too. `bare` takes only `\n` as the end, which binary data follows.
fn marker(data: &[u8], name: &[u8], status: bool, bare: bool) -> Option<(usize, usize, u8)> {
    let mut from = 0;
    while let Some(found) = find(&data[from..], name) {
        let at = from + found;
        from = at + 1;
        let start = match (at.checked_sub(1).map(|i| data[i]), at.checked_sub(2).map(|i| data[i])) {
            (Some(b'\n'), Some(b'\r')) if !bare => at - 2,
            (Some(b'\n'), _) => at - 1,
            _ => continue,
        };
        let mut end = at + name.len();
        let mut code = 0u32;
        if status {
            if data.get(end) != Some(&b':') {
                continue;
            }
            let digits = data[end + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
            if !(1..=3).contains(&digits) {
                continue;
            }
            code = data[end + 1..end + 1 + digits].iter().fold(0, |n, b| n * 10 + u32::from(b - b'0'));
            end += 1 + digits;
        }
        end = match (data.get(end), data.get(end + 1)) {
            (Some(b'\r'), Some(b'\n')) if !bare => end + 2,
            (Some(b'\n'), _) => end + 1,
            _ => continue,
        };
        return Some((start, end, code.min(255) as u8));
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A word as the device's shell reads it back unchanged.
pub fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

/// A token no output will hold by accident.
fn token() -> String {
    let mut bytes = [0u8; 12];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes));
    if read.is_err() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        bytes[..8].copy_from_slice(&(now as u64).to_le_bytes());
        bytes[8..].copy_from_slice(&std::process::id().to_le_bytes());
    }
    format!("OMA_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

impl<W: Wire> Console<W> {
    pub fn new(wire: W) -> Console<W> {
        Console { wire, pending: Vec::new() }
    }

    /// Reads until `found` matches what is pending, and takes it out up to
    /// the match's end. Returns what was taken and the match.
    fn until<T>(&mut self, deadline: Instant, limit: usize, found: impl Fn(&[u8]) -> Option<(usize, T)>) -> Result<(Vec<u8>, T)> {
        loop {
            if let Some((end, value)) = found(&self.pending) {
                let taken = self.pending.drain(..end).collect();
                return Ok((taken, value));
            }
            if self.pending.len() > limit {
                return Err(unknown("the console answered with more than expected"));
            }
            let more = self.wire.receive(deadline)?;
            self.pending.extend(more);
        }
    }

    fn exact(&mut self, size: usize, deadline: Instant) -> Result<Vec<u8>> {
        while self.pending.len() < size {
            let more = self.wire.receive(deadline)?;
            self.pending.extend(more);
        }
        Ok(self.pending.drain(..size).collect())
    }

    /// A fresh prompt, then the command line.
    fn start(&mut self, line: &str, deadline: Instant) -> Result<()> {
        self.wire.send(b"\r", deadline)?;
        self.until(deadline, 65536, |data| prompt(data).map(|end| (end, ())))?;
        self.wire.send(line.as_bytes(), deadline)
    }

    /// Runs `command` in the payload's shell: its exit status and output.
    pub fn run(&mut self, command: &str, timeout: Duration) -> Result<(u8, String)> {
        let deadline = Instant::now() + timeout;
        let token = token();
        let (begin, done) = (format!("{token}_BEGIN"), format!("{token}_DONE"));
        self.start(
            &format!("printf '\\n%s\\n' {begin}; sh -c {}; code=$?; printf '\\n%s:%s\\n' {done} \"$code\"\r", quote(command)),
            deadline,
        )?;
        let (raw, (start, code)) = self.until(deadline, 65536, |data| {
            marker(data, done.as_bytes(), true, false).map(|(start, end, code)| (end, (start, code)))
        })?;
        let Some((_, body, _)) = marker(&raw[..start + 2.min(raw.len() - start)], begin.as_bytes(), false, false).filter(|m| m.1 <= start) else {
            return Err(unknown("the console's answer lacks its markers"));
        };
        Ok((code, String::from_utf8_lossy(&raw[body..start]).into_owned()))
    }

    /// Runs `command`, which writes exactly `size` raw bytes, and returns them.
    pub fn capture_exact(&mut self, command: &str, size: usize, timeout: Duration) -> Result<Vec<u8>> {
        if !(1..=LIMIT).contains(&size) {
            return Err(Error::new(Code::TooLarge, "a serial transfer is 1 byte to 16 MiB"));
        }
        let deadline = Instant::now() + timeout;
        let token = token();
        let (begin, done) = (format!("{token}_BEGIN"), format!("{token}_DONE"));
        // Without output processing: it would turn a line feed into two bytes.
        self.start(
            &format!("stty -echo -opost; printf '\\n%s\\n' {begin}; {command}; code=$?; printf '\\n%s:%s\\n' {done} \"$code\"; stty echo opost\r"),
            deadline,
        )?;
        self.until(deadline, 65536, |data| marker(data, begin.as_bytes(), false, true).map(|(_, end, _)| (end, ())))?;
        let data = self.exact(size, deadline)?;
        let (_, (start, code)) = self.until(deadline, 4096, |data| {
            marker(data, done.as_bytes(), true, true).map(|(start, end, code)| (end, (start, code)))
        })?;
        if start != 0 || code != 0 {
            return Err(unknown("the device failed to send the file, or sent more than its size"));
        }
        Ok(data)
    }

    /// Writes `data` to the file `to` (already quoted for the shell) on the
    /// device: `head` reads exactly its length from the console. The caller
    /// checks what arrived by its checksum.
    ///
    /// Nothing is sent unless something is reading it. A file that cannot
    /// be opened would leave the shell at its prompt, where the file's bytes
    /// would run as commands: so the shell opens it first and says BEGIN only
    /// then, and without a BEGIN the data stays here.
    pub fn send_file(&mut self, to: &str, data: &[u8], timeout: Duration) -> Result<()> {
        if !(1..=LIMIT).contains(&data.len()) {
            return Err(Error::new(Code::TooLarge, "a serial transfer is 1 byte to 16 MiB"));
        }
        let deadline = Instant::now() + timeout;
        let token = token();
        let (begin, done) = (format!("{token}_BEGIN"), format!("{token}_DONE"));
        // Raw, so no byte is echoed, edited or taken for flow control. `true`
        // and not `:`, which as a special builtin can end a shell when its
        // redirection fails.
        self.start(
            &format!(
                "stty raw -echo; if true > {to} 2>/dev/null; then printf '\\n%s\\n' {begin}; head -c {} > {to}; code=$?; else code=1; fi; \
                 stty sane; printf '\\n%s:%s\\n' {done} \"$code\"\r",
                data.len()
            ),
            deadline,
        )?;
        // Whichever comes: BEGIN, and `head` reads; or DONE alone, and it
        // never started.
        let (_, reading) = self.until(deadline, 65536, |pending| {
            let begun = marker(pending, begin.as_bytes(), false, false).map(|(_, end, _)| (end, true));
            begun.or_else(|| marker(pending, done.as_bytes(), true, false).map(|(_, end, _)| (end, false)))
        })?;
        if !reading {
            return Err(Error::new(Code::Refused, "the device cannot write the file there; nothing was sent"));
        }
        self.wire.send(data, deadline)?;
        let (_, code) = self.until(deadline, 65536, |pending| {
            marker(pending, done.as_bytes(), true, false).map(|(_, end, code)| (end, code))
        })?;
        match code {
            0 => Ok(()),
            _ => Err(unknown("the device failed to take the file")),
        }
    }
}

/// The serial port itself: exclusive, raw, 115200 baud.
#[cfg(unix)]
pub struct Tty {
    file: std::fs::File,
    previous: libc::termios,
}

#[cfg(unix)]
impl Tty {
    pub fn open(path: &std::path::Path) -> Result<Tty> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
        let name = path.to_string_lossy();
        let known = match std::env::consts::OS {
            "macos" => name.starts_with("/dev/cu."),
            _ => name.strip_prefix("/dev/ttyACM").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        };
        if !known {
            return Err(Error::new(Code::NotFound, "the device's USB serial port is not there"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| Error::from(e).context(&name))?;
        if !file.metadata()?.file_type().is_char_device() {
            return Err(Error::new(Code::Format, "not a serial port").context(&name));
        }
        let fd = file.as_raw_fd();
        // One user at a time: a second one would take bytes of the answer.
        // SAFETY: flock, ioctl and termios calls on a descriptor this owns.
        unsafe {
            if libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) != 0 || libc::ioctl(fd, libc::TIOCEXCL as _) != 0 {
                return Err(Error::new(Code::Locked, "the device's USB serial port is in use").context(&name));
            }
            let mut settings: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut settings) != 0 {
                return Err(Error::from(std::io::Error::last_os_error()).context(&name));
            }
            let previous = settings;
            settings.c_iflag = 0;
            settings.c_oflag = 0;
            settings.c_lflag = 0;
            settings.c_cflag = libc::CS8 | libc::CREAD | libc::CLOCAL;
            libc::cfsetispeed(&mut settings, libc::B115200);
            libc::cfsetospeed(&mut settings, libc::B115200);
            settings.c_cc[libc::VMIN] = 0;
            settings.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSANOW, &settings) != 0 {
                return Err(Error::from(std::io::Error::last_os_error()).context(&name));
            }
            Ok(Tty { file, previous })
        }
    }

    /// Waits until the port can be read or written, or the deadline.
    fn ready(&self, events: libc::c_short, deadline: Instant) -> bool {
        use std::os::fd::AsRawFd;
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd { fd: self.file.as_raw_fd(), events, revents: 0 };
        // SAFETY: poll on one descriptor this owns.
        !left.is_zero() && unsafe { libc::poll(&mut fd, 1, left.as_millis().min(i32::MAX as u128) as i32) } > 0
    }
}

#[cfg(unix)]
impl Drop for Tty {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: puts back what tcgetattr gave.
        unsafe {
            libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.previous);
        }
    }
}

#[cfg(unix)]
impl Wire for Tty {
    fn send(&mut self, mut data: &[u8], deadline: Instant) -> Result<()> {
        use std::io::Write;
        while !data.is_empty() {
            if !self.ready(libc::POLLOUT, deadline) {
                return Err(unknown("writing to the serial console took too long"));
            }
            match self.file.write(data) {
                Ok(0) => return Err(unknown("the serial console went away")),
                Ok(sent) => data = &data[sent..],
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                Err(_) => return Err(unknown("the serial console went away")),
            }
        }
        Ok(())
    }

    fn receive(&mut self, deadline: Instant) -> Result<Vec<u8>> {
        use std::io::Read;
        if !self.ready(libc::POLLIN, deadline) {
            return Err(unknown("the serial console did not answer in time"));
        }
        let mut block = [0u8; 4096];
        match self.file.read(&mut block) {
            Ok(0) => Err(unknown("the serial console went away")),
            Ok(n) => Ok(block[..n].to_vec()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => Ok(Vec::new()),
            Err(_) => Err(unknown("the serial console went away")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A console that answers as a shell would: it reads the token out of
    /// each command line and prints what `answer` gives between the markers.
    pub struct Script {
        pub sent: Vec<u8>,
        ready: Vec<u8>,
        answer: Box<dyn FnMut(&str) -> (Vec<u8>, u8)>,
        /// Line ends as a console with output processing prints them.
        crlf: bool,
        /// Raw bytes still expected from `send_file`.
        taking: usize,
        done: Option<String>,
    }

    impl Script {
        pub fn new(crlf: bool, answer: impl FnMut(&str) -> (Vec<u8>, u8) + 'static) -> Script {
            Script { sent: Vec::new(), ready: Vec::new(), answer: Box::new(answer), crlf, taking: 0, done: None }
        }
    }

    impl Wire for Script {
        fn send(&mut self, data: &[u8], _: Instant) -> Result<()> {
            self.sent.extend(data);
            if self.taking > 0 {
                self.taking -= data.len().min(self.taking);
                if self.taking == 0 {
                    let done = self.done.take().unwrap();
                    self.ready.extend(format!("\r\n{done}:0\r\n").into_bytes());
                }
                return Ok(());
            }
            if data == b"\r" {
                self.ready.extend(b"\r\n/ # ");
                return Ok(());
            }
            let line = String::from_utf8_lossy(data).into_owned();
            let token = &line[line.find("OMA_").unwrap()..][..28];
            let nl = if self.crlf && !line.contains("-opost") { "\r\n" } else { "\n" };
            // The echo of the line, as a terminal gives it.
            self.ready.extend(line.as_bytes());
            self.ready.extend(format!("{nl}{token}_BEGIN{nl}").into_bytes());
            if let Some(size) = line.split("head -c ").nth(1).and_then(|rest| rest.split(' ').next()).and_then(|n| n.parse().ok()) {
                // As the shell does it: the file is opened before BEGIN is
                // printed, and one under /ro cannot be.
                if line.contains("true > /ro/") {
                    self.ready.truncate(self.ready.len() - format!("{nl}{token}_BEGIN{nl}").len());
                    self.ready.extend(format!("{nl}{token}_DONE:1{nl}").into_bytes());
                    return Ok(());
                }
                self.taking = size;
                self.done = Some(format!("{token}_DONE"));
                return Ok(());
            }
            let (output, code) = (self.answer)(&line);
            self.ready.extend(output);
            self.ready.extend(format!("{nl}{token}_DONE:{code}{nl}").into_bytes());
            Ok(())
        }

        fn receive(&mut self, _: Instant) -> Result<Vec<u8>> {
            if self.ready.is_empty() {
                return Err(unknown("the serial console did not answer in time"));
            }
            // In small pieces, as a serial port delivers.
            let n = self.ready.len().min(7);
            Ok(self.ready.drain(..n).collect())
        }
    }

    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn a_command_gives_its_status_and_output_between_its_markers() {
        for crlf in [true, false] {
            let mut console = Console::new(Script::new(crlf, |line| {
                assert!(line.contains("sh -c 'echo it'\"'\"'s'"), "{line}");
                (b"it's\n".to_vec(), 3)
            }));
            let (code, output) = console.run("echo it's", LONG).unwrap();
            assert_eq!((code, output.as_str()), (3, "it's\n"));
            // A second command starts from a fresh prompt.
            assert_eq!(console.run("echo it's", LONG).unwrap().0, 3);
        }
    }

    #[test]
    fn output_that_looks_like_a_marker_is_not_one() {
        let mut console = Console::new(Script::new(false, |_| (b"OMA_x_DONE:0\nmore\n".to_vec(), 0)));
        let (code, output) = console.run("true", LONG).unwrap();
        assert_eq!((code, output.as_str()), (0, "OMA_x_DONE:0\nmore\n"));
    }

    #[test]
    fn a_file_comes_as_exactly_its_bytes() {
        let file: Vec<u8> = (0..=255).cycle().take(1000).collect();
        let sent = file.clone();
        let mut console = Console::new(Script::new(true, move |_| (sent.clone(), 0)));
        assert_eq!(console.capture_exact("cat f", 1000, LONG).unwrap(), file);
        // One byte more than was said is refused, and so is a failed command.
        let more = file.clone();
        let mut console = Console::new(Script::new(true, move |_| (more.clone(), 0)));
        assert!(console.capture_exact("cat f", 999, LONG).is_err());
        let failed = file.clone();
        let mut console = Console::new(Script::new(true, move |_| (failed.clone(), 1)));
        assert!(console.capture_exact("cat f", 1000, LONG).is_err());
        assert!(console.capture_exact("cat f", 0, LONG).is_err());
    }

    #[test]
    fn a_file_goes_as_exactly_its_bytes() {
        let mut console = Console::new(Script::new(true, |_| (Vec::new(), 0)));
        console.send_file("/tmp/f.part", b"\x03\x04\r\n\x7f", LONG).unwrap();
        assert!(console.wire.sent.ends_with(b"\x03\x04\r\n\x7f"));
        assert!(console.send_file("/tmp/f.part", b"", LONG).is_err());
    }

    #[test]
    fn a_file_that_cannot_be_written_is_never_sent() {
        // Its bytes would otherwise arrive at the shell's prompt, as commands.
        let mut console = Console::new(Script::new(true, |_| (Vec::new(), 0)));
        let data = b"rm -rf /important\n";
        let e = console.send_file("/ro/f.part", data, LONG).unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(e.message.contains("nothing was sent"), "{}", e.message);
        let sent = &console.wire.sent;
        assert!(!sent.windows(data.len()).any(|w| w == data), "the data went out");
        // The console is usable afterwards.
        assert_eq!(console.run("true", LONG).unwrap().0, 0);
    }

    #[test]
    fn a_silent_console_ends_with_the_outcome_unknown() {
        struct Silent;
        impl Wire for Silent {
            fn send(&mut self, _: &[u8], _: Instant) -> Result<()> {
                Ok(())
            }
            fn receive(&mut self, _: Instant) -> Result<Vec<u8>> {
                Err(unknown("the serial console did not answer in time"))
            }
        }
        let e = Console::new(Silent).run("true", Duration::from_millis(10)).unwrap_err();
        assert!(e.message.contains("not known"), "{}", e.message);
    }

    #[test]
    fn words_are_quoted_for_the_shell() {
        assert_eq!(quote("a/b-c.d"), "a/b-c.d");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("it's"), "'it'\"'\"'s'");
    }
}
