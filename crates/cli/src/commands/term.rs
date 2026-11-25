//! The terminal as the T mockups draw it (installer-app.pen, section T):
//! Tokyo Night colors, whole warning and error rows in their color, a live
//! line with a spinner while something runs, and the progress gradient. The
//! runtime's own messages go to the log file, not between these lines.
//! Without a terminal (an agent, a pipe) lines are plain and nothing is live.
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Instant;

/// A question is waiting for its line (Ctrl-C then quits at once).
pub static ASKING: AtomicBool = AtomicBool::new(false);
/// The terminal's fd, for the Ctrl-C handler.
pub static TTY_FD: AtomicI32 = AtomicI32::new(2);
static mut ORIGINAL: Option<libc::termios> = None;

/// Puts the terminal back as it was at the start (from the signal handler).
pub fn restore_on_signal() {
    // SAFETY: ORIGINAL is written once in Term::new before the handler is
    // installed; tcsetattr is async-signal-safe.
    unsafe {
        if let Some(t) = *std::ptr::addr_of!(ORIGINAL) {
            libc::tcsetattr(0, libc::TCSANOW, &t);
        }
    }
}

pub const FG: u32 = 0xc0caf5;
pub const TITLE: u32 = 0xe6e9fa;
pub const SUB: u32 = 0xa9b1d6;
pub const MUTED: u32 = 0x565f89;
pub const BLUE: u32 = 0x7aa2f7;
pub const GREEN: u32 = 0x9ece6a;
pub const YELLOW: u32 = 0xe0af68;
pub const RED: u32 = 0xf7768e;
pub const VIOLET: u32 = 0xbb9af7;
pub const TRACK: u32 = 0x3b4261;

/// A piece of a line in one color.
pub type Span = (String, u32);

pub fn s(text: impl Into<String>, color: u32) -> Span {
    (text.into(), color)
}

/// TEXT in the progress bar's blue-to-violet gradient, one shade per character.
pub fn gradient(text: &str) -> Vec<Span> {
    let n = text.chars().count().max(2) - 1;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let mix = |a: u32, b: u32| (a as f32 + (b as f32 - a as f32) * i as f32 / n as f32).round() as u32;
            (ch.to_string(), (mix(0x7a, 0xbb) << 16) | (mix(0xa2, 0x9a) << 8) | 0xf7)
        })
        .collect()
}

/// A bar of WIDTH cells, FILLED of them in the gradient.
pub fn bar(filled: usize, width: usize) -> Vec<Span> {
    let mut out = gradient(&"█".repeat(filled.max(1)));
    out.truncate(filled);
    out.push(s("░".repeat(width - filled.min(width)), TRACK));
    out
}

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct Term {
    /// The terminal, kept aside before stderr went to the log.
    out: File,
    pub tty: bool,
    color: bool,
    live: Option<(Vec<Span>, Instant)>,
    log: Option<File>,
    /// Where the log goes (--log, else the cache's logs/cli-<time>.log).
    pub log_path: Option<std::path::PathBuf>,
    raw: Option<libc::termios>,
}

impl Term {
    /// Takes over stderr: the runtime's messages go to LOG (or are dropped),
    /// this writes to the terminal it had.
    pub fn new(log: Option<&std::path::Path>) -> Term {
        let tty = std::io::IsTerminal::is_terminal(&std::io::stderr()) && std::io::IsTerminal::is_terminal(&std::io::stdin());
        let color = tty && std::env::var_os("NO_COLOR").is_none();
        // SAFETY: duplicates fd 2 for our own use, then points fd 2 at the log.
        let out = unsafe { File::from_raw_fd(libc::dup(2)) };
        // Without --log, as the app: the cache's logs/cli-<time>.log.
        let log_path = log.map(|p| p.to_owned()).or_else(|| {
            let dir = orchard_cache::store::default_dir().ok()?.join("logs");
            std::fs::create_dir_all(&dir).ok()?;
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            Some(dir.join(format!("cli-{stamp}.log")))
        });
        let log_file = log_path.as_deref().and_then(|p| File::create(p).ok());
        let target = match &log_file {
            Some(f) => f.try_clone().ok(),
            None => File::options().write(true).open("/dev/null").ok(),
        };
        if let Some(t) = target {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::dup2(t.as_raw_fd(), 2);
            }
        }
        {
            use std::os::fd::AsRawFd;
            TTY_FD.store(out.as_raw_fd(), Ordering::SeqCst);
        }
        if tty {
            // SAFETY: once, before any signal handler reads it.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) == 0 {
                    *std::ptr::addr_of_mut!(ORIGINAL) = Some(t);
                }
            }
        }
        Term { out, tty, color, live: None, log: log_file, log_path, raw: None }
    }

    fn paint(&self, spans: &[Span]) -> String {
        let mut line = String::new();
        for (text, rgb) in spans {
            if self.color {
                line.push_str(&format!("\x1b[38;2;{};{};{}m{text}", rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff));
            } else {
                line.push_str(text);
            }
        }
        if self.color {
            line.push_str("\x1b[0m");
        }
        line
    }

    fn clear_live(&mut self) {
        if self.tty && self.live.is_some() {
            let _ = write!(self.out, "\r\x1b[2K");
        }
    }

    /// A permanent line.
    pub fn line(&mut self, spans: Vec<Span>) {
        self.clear_live();
        let text = self.paint(&spans);
        let _ = writeln!(self.out, "{text}");
        if let Some(log) = self.log.as_mut() {
            let plain: String = spans.iter().map(|(t, _)| t.as_str()).collect();
            let _ = writeln!(log, "{plain}");
        }
        self.draw_live();
    }

    /// Takes back the last LINES permanent lines, to draw them anew in place.
    pub fn erase(&mut self, lines: usize) {
        if self.tty && lines > 0 {
            self.clear_live();
            let _ = write!(self.out, "\x1b[{lines}A\r\x1b[J");
        }
    }

    pub fn blank(&mut self) {
        self.line(vec![]);
    }

    /// The live line (a spinner turns in front of it); None ends it.
    pub fn live(&mut self, spans: Option<Vec<Span>>) {
        self.clear_live();
        let started = self.live.as_ref().map(|(_, t)| *t).unwrap_or_else(Instant::now);
        self.live = spans.map(|sp| (sp, started));
        self.draw_live();
    }

    pub fn live_restart(&mut self, spans: Vec<Span>) {
        self.clear_live();
        self.live = Some((spans, Instant::now()));
        self.draw_live();
    }

    /// Seconds the live line has been showing.
    pub fn live_secs(&self) -> u64 {
        self.live.as_ref().map(|(_, t)| t.elapsed().as_secs()).unwrap_or(0)
    }

    pub fn tick(&mut self) {
        if self.live.is_some() {
            self.clear_live();
            self.draw_live();
        }
    }

    fn draw_live(&mut self) {
        if !self.tty {
            return;
        }
        if let Some((spans, started)) = &self.live {
            let frame = FRAMES[(started.elapsed().as_millis() / 100) as usize % FRAMES.len()];
            let mut all = vec![s(format!("{frame} "), VIOLET)];
            all.extend(spans.iter().cloned());
            let secs = started.elapsed().as_secs();
            if secs >= 3 {
                all.push(s(format!("  {}:{:02}", secs / 60, secs % 60), MUTED));
            }
            let text = self.paint(&all);
            let _ = write!(self.out, "{text}");
            let _ = self.out.flush();
        }
    }

    /// Asks for a line; None without a terminal.
    pub fn ask(&mut self, prompt: Vec<Span>, secret: bool) -> Option<String> {
        if !self.tty {
            return None;
        }
        self.keys(false);
        self.clear_live();
        let live = self.live.take();
        let text = self.paint(&prompt);
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
        let mut line = String::new();
        ASKING.store(true, Ordering::SeqCst);
        let read = if secret { self.read_secret(&mut line) } else { std::io::stdin().read_line(&mut line).map(|_| ()) };
        ASKING.store(false, Ordering::SeqCst);
        if let Some(log) = self.log.as_mut() {
            let plain: String = prompt.iter().map(|(t, _)| t.as_str()).collect();
            let _ = writeln!(log, "{plain}{}", if secret { "(hidden)" } else { line.trim() });
        }
        self.live = live;
        read.ok()?;
        Some(line.trim_end_matches(['\r', '\n']).to_owned())
    }

    /// Lets the person choose one of ROWS with the arrow keys (or j and k, or
    /// its number) and Enter, starting at SELECTED; None when they leave
    /// (Esc, q) or without a terminal. The rows stay, the chosen one marked.
    pub fn pick(&mut self, rows: &[Vec<Span>], selected: usize, hint: &str) -> Option<usize> {
        if !self.tty || rows.is_empty() {
            return None;
        }
        self.keys(false);
        self.clear_live();
        let live = self.live.take();
        let saved = cbreak()?;
        let mut at = selected.min(rows.len() - 1);
        let mut drawn = false;
        ASKING.store(true, Ordering::SeqCst);
        let chosen = loop {
            if drawn {
                let _ = write!(self.out, "\x1b[{}A", rows.len() + 1);
            }
            drawn = true;
            for (i, row) in rows.iter().enumerate() {
                let mut all = vec![match i == at {
                    true => s("  ❯ ", BLUE),
                    false => s("    ", MUTED),
                }];
                // The row under the mark reads as the title does.
                all.extend(row.iter().map(|(text, color)| match (i == at, *color) {
                    (true, FG) => (text.clone(), TITLE),
                    (false, FG) => (text.clone(), SUB),
                    _ => (text.clone(), *color),
                }));
                let text = self.paint(&all);
                let _ = writeln!(self.out, "\r\x1b[2K{text}");
            }
            let text = self.paint(&[s(format!("  {hint}"), MUTED)]);
            let _ = writeln!(self.out, "\r\x1b[2K{text}");
            let _ = self.out.flush();
            // Read from the terminal itself: Rust's stdin keeps what follows a
            // byte to itself, and an arrow key is Esc, [ and a letter at once.
            let key = match terminal_byte() {
                None => break None,
                Some(0x1b) if !waiting_byte() => Key::Leave,
                Some(0x1b) => sequence(&mut terminal_byte),
                Some(byte) => key_of(byte),
            };
            match moved(at, rows.len(), key) {
                Move::To(next) => at = next,
                Move::Take(index) => break Some(index),
                Move::Leave => break None,
            }
        };
        ASKING.store(false, Ordering::SeqCst);
        restore(saved);
        // The hint goes; the list stays as it was left.
        let _ = write!(self.out, "\x1b[1A\r\x1b[2K");
        let _ = self.out.flush();
        if let Some(log) = self.log.as_mut() {
            for (i, row) in rows.iter().enumerate() {
                let plain: String = row.iter().map(|(t, _)| t.as_str()).collect();
                let mark = if chosen == Some(i) { ">" } else { " " };
                let _ = writeln!(log, "  {mark} {plain}");
            }
        }
        self.live = live;
        chosen
    }

    /// A secret line, a dot per character: Backspace takes one back, Ctrl-U
    /// all. Ctrl-C still quits (the signal handler, ISIG stays on).
    fn read_secret(&mut self, line: &mut String) -> std::io::Result<()> {
        let Some(saved) = cbreak() else {
            return std::io::stdin().read_line(line).map(|_| ());
        };
        let dot = self.paint(&[s("•", BLUE)]);
        let mut bytes = Vec::new();
        let result = loop {
            let mut b = [0u8; 1];
            match std::io::stdin().read(&mut b) {
                Ok(0) => break Ok(()),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            }
            match b[0] {
                b'\r' | b'\n' => break Ok(()),
                0x7f | 0x08 => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if let Some(ch) = text.chars().last() {
                        bytes.truncate(bytes.len() - ch.len_utf8());
                        let _ = write!(self.out, "\x08 \x08");
                    }
                }
                0x15 => {
                    let n = String::from_utf8_lossy(&bytes).chars().count();
                    let _ = write!(self.out, "{}", "\x08 \x08".repeat(n));
                    bytes.clear();
                }
                0x1b => {}
                c if c < 0x20 => {}
                c => {
                    bytes.push(c);
                    // A dot when a character is complete (UTF-8 lead or ASCII).
                    if std::str::from_utf8(&bytes).is_ok() {
                        let _ = write!(self.out, "{dot}");
                    }
                }
            }
            let _ = self.out.flush();
        };
        restore(saved);
        let _ = writeln!(self.out);
        line.push_str(&String::from_utf8_lossy(&bytes));
        result
    }

    /// Single keys without Enter (while waiting for DFU mode), or back to lines.
    pub fn keys(&mut self, on: bool) {
        if !self.tty {
            return;
        }
        match (on, self.raw.is_some()) {
            (true, false) => {
                // SAFETY: termios on stdin, restored by keys(false) or drop.
                unsafe {
                    let mut t: libc::termios = std::mem::zeroed();
                    if libc::tcgetattr(0, &mut t) == 0 {
                        let saved = t;
                        t.c_lflag &= !(libc::ICANON | libc::ECHO);
                        t.c_cc[libc::VMIN] = 0;
                        t.c_cc[libc::VTIME] = 0;
                        libc::tcsetattr(0, libc::TCSANOW, &t);
                        self.raw = Some(saved);
                    }
                }
            }
            (false, true) => restore(self.raw.take().unwrap()),
            _ => {}
        }
    }

    /// A key pressed since the last call, in key mode.
    pub fn key(&mut self) -> Option<char> {
        self.raw?;
        let mut b = [0u8; 1];
        match std::io::stdin().read(&mut b) {
            Ok(1) => Some(b[0] as char),
            _ => None,
        }
    }
}

/// A key of the list of choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    /// The row with this number, from 1.
    Number(usize),
    Leave,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
enum Move {
    To(usize),
    Take(usize),
    Leave,
}

fn key_of(byte: u8) -> Key {
    match byte {
        b'\r' | b'\n' => Key::Enter,
        b'k' | b'K' | 0x10 => Key::Up,
        b'j' | b'J' | 0x0e => Key::Down,
        b'1'..=b'9' => Key::Number(usize::from(byte - b'0')),
        b'q' | b'Q' | 0x04 => Key::Leave,
        _ => Key::Other,
    }
}

/// The key of a sequence after its Esc, read to its end: a key with a
/// modifier or a number in it (Shift and an arrow, Page Up) is more bytes,
/// and a digit of it is not the number of a row.
fn sequence(next: &mut dyn FnMut() -> Option<u8>) -> Key {
    let last = match next() {
        Some(b'O') => next(),
        Some(b'[') => {
            let first = next();
            let mut byte = first;
            while !matches!(byte, Some(0x40..=0x7e) | None) {
                byte = next();
            }
            first
        }
        _ => None,
    };
    match last {
        Some(b'A') => Key::Up,
        Some(b'B') => Key::Down,
        _ => Key::Other,
    }
}

/// Where a key takes the mark from row AT of COUNT; the ends don't wrap.
fn moved(at: usize, count: usize, key: Key) -> Move {
    match key {
        Key::Up => Move::To(at.saturating_sub(1)),
        Key::Down => Move::To((at + 1).min(count - 1)),
        Key::Enter => Move::Take(at),
        Key::Number(n) if n <= count => Move::Take(n - 1),
        Key::Number(_) | Key::Other => Move::To(at),
        Key::Leave => Move::Leave,
    }
}

/// The next byte typed, waited for; None when the terminal is gone.
fn terminal_byte() -> Option<u8> {
    let mut byte = 0u8;
    loop {
        // SAFETY: reads one byte from stdin into a byte this function owns.
        match unsafe { libc::read(0, std::ptr::addr_of_mut!(byte).cast(), 1) } {
            1 => return Some(byte),
            0 => return None,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

/// Whether more input follows at once: the rest of a key's sequence.
fn waiting_byte() -> bool {
    let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    // SAFETY: poll on stdin with one descriptor.
    unsafe { libc::poll(&mut fd, 1, 50) > 0 }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.keys(false);
        self.clear_live();
    }
}

/// Keys one at a time without echo (signals still on); the old settings back.
fn cbreak() -> Option<libc::termios> {
    // SAFETY: termios on stdin.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return None;
        }
        let saved = t;
        t.c_lflag &= !(libc::ICANON | libc::ECHO);
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(0, libc::TCSANOW, &t);
        Some(saved)
    }
}

fn restore(t: libc::termios) {
    // SAFETY: puts back what tcgetattr gave.
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, &t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_moves_within_the_list_and_keys_choose() {
        assert_eq!(moved(0, 3, Key::Up), Move::To(0));
        assert_eq!(moved(0, 3, Key::Down), Move::To(1));
        assert_eq!(moved(2, 3, Key::Down), Move::To(2));
        assert_eq!(moved(1, 3, Key::Enter), Move::Take(1));
        assert_eq!(moved(0, 3, key_of(b'3')), Move::Take(2));
        assert_eq!(moved(1, 3, key_of(b'4')), Move::To(1));
        assert_eq!(moved(1, 3, key_of(b'x')), Move::To(1));
        assert_eq!(moved(1, 3, key_of(b'q')), Move::Leave);
        let key = |bytes: &'static [u8]| {
            let mut bytes = bytes.iter().copied();
            let key = sequence(&mut || bytes.next());
            (key, bytes.len())
        };
        assert_eq!(key(b"[A"), (Key::Up, 0));
        assert_eq!(key(b"OB"), (Key::Down, 0));
        // Shift and Up, Page Up: read to the end, and no row 2 or 5.
        assert_eq!(key(b"[1;2A"), (Key::Other, 0));
        assert_eq!(key(b"[5~"), (Key::Other, 0));
        assert_eq!((key_of(b'j'), key_of(b'k'), key_of(b'\r')), (Key::Down, Key::Up, Key::Enter));
    }
}
