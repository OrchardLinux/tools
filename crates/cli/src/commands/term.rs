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
        let log_file = log.and_then(|p| File::create(p).ok());
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
        Term { out, tty, color, live: None, log: log_file, raw: None }
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
        let saved = if secret { echo(false) } else { None };
        let mut line = String::new();
        ASKING.store(true, Ordering::SeqCst);
        let read = std::io::stdin().read_line(&mut line);
        ASKING.store(false, Ordering::SeqCst);
        if let Some(t) = saved {
            restore(t);
            let _ = writeln!(self.out);
        }
        if let Some(log) = self.log.as_mut() {
            let plain: String = prompt.iter().map(|(t, _)| t.as_str()).collect();
            let _ = writeln!(log, "{plain}{}", if secret { "(hidden)" } else { line.trim() });
        }
        self.live = live;
        read.ok()?;
        Some(line.trim_end_matches(['\r', '\n']).to_owned())
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

impl Drop for Term {
    fn drop(&mut self) {
        self.keys(false);
        self.clear_live();
    }
}

fn echo(on: bool) -> Option<libc::termios> {
    // SAFETY: termios on stdin.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return None;
        }
        let saved = t;
        if on {
            t.c_lflag |= libc::ECHO;
        } else {
            t.c_lflag &= !libc::ECHO;
        }
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
