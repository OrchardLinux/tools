//! SSH access to an installed system that was installed without it.
//!
//! Every installed system offers a login on its USB serial console. This
//! signs in there once, as the system's user with their password, and adds
//! this computer's keys to that user's `authorized_keys`, as the installer
//! does when SSH is allowed. The password goes only to that login, over the
//! cable; it is not kept and not logged.
use crate::serial::{marker, quote, token, unknown, Wire};
use orchard_cache::{Code, Error, Result};
use std::time::{Duration, Instant};

/// What the console shows next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Login,
    Password,
    /// Someone's shell, left open.
    Shell,
}

fn refused() -> Error {
    Error::new(Code::Refused, "the system refused the login: check the user name and password")
}

/// A login session on an installed system's serial console.
pub struct Login<W: Wire> {
    wire: W,
    pending: Vec<u8>,
    /// Signed in: from then on "Permission denied" is a command's own word,
    /// not the login's.
    signed_in: bool,
}

/// The command that adds `keys` to the user's `authorized_keys`, each once.
pub fn append_keys(keys: &[String]) -> String {
    let lines: String = keys
        .iter()
        .map(|key| {
            let key = quote(key);
            format!("grep -qxF {key} ~/.ssh/authorized_keys 2>/dev/null || echo {key} >> ~/.ssh/authorized_keys; ")
        })
        .collect();
    // A file whose last line has no end would get the first new key glued
    // onto that line, and both keys would stop working: end it first.
    let ended = "if [ -s ~/.ssh/authorized_keys ] && [ -n \"$(tail -c1 ~/.ssh/authorized_keys)\" ]; then echo >> ~/.ssh/authorized_keys; fi; ";
    format!("umask 077; mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && {ended}{lines}chmod 600 ~/.ssh/authorized_keys")
}

impl<W: Wire> Login<W> {
    pub fn new(wire: W) -> Login<W> {
        Login { wire, pending: Vec::new(), signed_in: false }
    }

    /// Reads until `found` matches what is pending and takes it out up to
    /// there. A refusal that shows first ends it.
    fn until<T>(&mut self, deadline: Instant, found: impl Fn(&[u8]) -> Option<(usize, T)>) -> Result<T> {
        loop {
            let said = String::from_utf8_lossy(&self.pending);
            if !self.signed_in && ["Login incorrect", "Authentication failure", "Permission denied"].iter().any(|no| said.contains(no)) {
                self.pending.clear();
                return Err(refused());
            }
            if let Some((end, value)) = found(&self.pending) {
                self.pending.drain(..end);
                return Ok(value);
            }
            if self.pending.len() > 65536 {
                return Err(unknown("the console answered with more than expected"));
            }
            let more = self.wire.receive(deadline)?;
            self.pending.extend(more);
        }
    }

    /// Waits for one of `wanted` to be the last thing on the console.
    fn shows(&mut self, wanted: &[Shown], deadline: Instant) -> Result<Shown> {
        self.until(deadline, |data| {
            let end = |tail: &[u8]| data.ends_with(tail);
            let shown = if end(b"login: ") {
                Shown::Login
            } else if end(b"assword: ") || end(b"assword:") {
                Shown::Password
            } else if [b"$ ", b"# ", b"% ", b"> "].iter().any(|tail| end(*tail)) {
                Shown::Shell
            } else {
                return None;
            };
            wanted.contains(&shown).then_some((data.len(), shown))
        })
    }

    /// Signs in as `user`. Afterwards the console is their shell.
    pub fn login(&mut self, user: &str, password: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        // A fresh line brings up the login prompt, or a shell left open.
        self.wire.send(b"\r", deadline)?;
        let soon = deadline.min(Instant::now() + Duration::from_secs(3));
        let mut first = match self.shows(&[Shown::Login, Shown::Password, Shown::Shell], soon) {
            Ok(shown) => shown,
            Err(e) if e.code == Code::Refused || Instant::now() >= deadline => return Err(e),
            // Nothing known stands there. A login would: so it is a shell
            // with a prompt of its own (starship's, say), and a shell prints
            // what it is asked to. Should it be a slow login after all, it
            // takes the line for a user name and asks for a password.
            Err(_) => {
                let token = token();
                self.wire.send(format!("printf '\\n%s\\n' {token}\r").as_bytes(), deadline)?;
                self.until(deadline, |data| {
                    let printed = marker(data, token.as_bytes(), false, false).map(|(_, end, _)| (end, Shown::Shell));
                    let asks = (data.ends_with(b"assword: ") || data.ends_with(b"assword:")).then_some((data.len(), Shown::Password));
                    printed.or(asks)
                })?
            }
        };
        if first == Shown::Shell {
            // Someone's shell is open on the console: end it, sign in anew.
            self.wire.send(b"exit\r", deadline)?;
            first = self.shows(&[Shown::Login], deadline)?;
        }
        if first == Shown::Password {
            // A password prompt left from before: fail it and start over.
            self.wire.send(b"\r", deadline)?;
            self.pending.clear();
            match self.shows(&[Shown::Login], deadline) {
                Ok(_) => {}
                // That empty password was refused, as meant.
                Err(e) if e.code == Code::Refused => {
                    self.shows(&[Shown::Login], deadline)?;
                }
                Err(e) => return Err(e),
            }
        }
        self.wire.send(format!("{user}\r").as_bytes(), deadline)?;
        self.shows(&[Shown::Password], deadline)?;
        self.wire.send(format!("{password}\r").as_bytes(), deadline)?;
        // The prompt is the user's own: wait for a line this session prints.
        let token = token();
        loop {
            self.wire.send(format!("printf '\\n%s\\n' {token}\r").as_bytes(), deadline)?;
            let soon = deadline.min(Instant::now() + Duration::from_secs(3));
            match self.until(soon, |data| marker(data, token.as_bytes(), false, false).map(|(_, end, _)| (end, ()))) {
                Ok(()) => {
                    self.signed_in = true;
                    return Ok(());
                }
                Err(e) if e.code == Code::Refused => return Err(e),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => {}
            }
        }
    }

    /// Runs `command` in the signed-in shell: its exit status.
    pub fn run(&mut self, command: &str, timeout: Duration) -> Result<u8> {
        let deadline = Instant::now() + timeout;
        let done = format!("{}_DONE", token());
        self.wire.send(format!("sh -c {}; printf '\\n%s:%s\\n' {done} \"$?\"\r", quote(command)).as_bytes(), deadline)?;
        self.until(deadline, |data| marker(data, done.as_bytes(), true, false).map(|(_, end, code)| (end, code)))
    }

    /// Ends the session; the console shows its login again.
    pub fn logout(&mut self) {
        self.signed_in = false;
        let _ = self.wire.send(b"exit\r", Instant::now() + Duration::from_secs(5));
    }

    /// Signs in, adds `keys` to the user's `authorized_keys` and signs out.
    pub fn grant(&mut self, user: &str, password: &str, keys: &[String]) -> Result<()> {
        self.login(user, password, Duration::from_secs(30))?;
        let code = self.run(&append_keys(keys), Duration::from_secs(30));
        self.logout();
        match code? {
            0 => Ok(()),
            code => Err(Error::new(Code::Refused, format!("adding the keys failed on the device (exit {code})"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A getty, a login and a shell, as far as this speaks to them.
    struct Getty {
        state: &'static str,
        user: String,
        password: &'static str,
        ready: Vec<u8>,
        typed: Vec<u8>,
        ran: Vec<String>,
        /// The shell's prompt.
        prompt: &'static str,
    }

    impl Getty {
        fn new(state: &'static str) -> Getty {
            Getty { state, user: String::new(), password: "secret", ready: Vec::new(), typed: Vec::new(), ran: Vec::new(), prompt: "[tablet@ipad ~]$ " }
        }

        fn say(&mut self, text: &str) {
            self.ready.extend(text.as_bytes());
        }

        fn line(&mut self, line: String) {
            match self.state {
                "login" if line.is_empty() => self.say("\r\nipad login: "),
                "shell" if line.is_empty() => {
                    let prompt = self.prompt;
                    self.say(&format!("\r\n{prompt}"));
                }
                "login" => {
                    self.user = line;
                    self.state = "password";
                    self.say("Password: ");
                }
                "password" => {
                    if self.user == "tablet" && line == self.password {
                        self.state = "shell";
                        let prompt = self.prompt;
                        self.say(&format!("\r\nLast login: today\r\n{prompt}"));
                    } else {
                        self.state = "login";
                        self.say("\r\nLogin incorrect\r\n\r\nipad login: ");
                    }
                }
                _ if line == "exit" => {
                    self.state = "login";
                    self.say("logout\r\n\r\nipad login: ");
                }
                _ => {
                    // Echo, then what the line prints.
                    self.say(&format!("{line}\r\n"));
                    if let Some(token) = line.strip_prefix("printf '\\n%s\\n' ") {
                        self.say(&format!("\r\n{token}\r\n"));
                    } else if let Some((command, rest)) = line.split_once("; printf '\\n%s:%s\\n' ") {
                        self.ran.push(command.to_owned());
                        let token = rest.split(' ').next().unwrap();
                        let code = if command.contains("fail") { 1 } else { 0 };
                        if command.contains("denied") {
                            self.say("chmod: /x: Permission denied\r\n");
                        }
                        self.say(&format!("\r\n{token}:{code}\r\n"));
                    }
                    let prompt = self.prompt;
                    self.say(prompt);
                }
            }
        }
    }

    impl Wire for Getty {
        fn send(&mut self, data: &[u8], _: Instant) -> Result<()> {
            for &byte in data {
                if byte == b'\r' {
                    let line = String::from_utf8(std::mem::take(&mut self.typed)).unwrap();
                    self.line(line);
                } else {
                    self.typed.push(byte);
                }
            }
            Ok(())
        }

        fn receive(&mut self, _: Instant) -> Result<Vec<u8>> {
            if self.ready.is_empty() {
                return Err(unknown("the serial console did not answer in time"));
            }
            // In pieces, as a serial port gives them.
            let n = self.ready.len().min(5);
            Ok(self.ready.drain(..n).collect())
        }
    }

    fn keys() -> Vec<String> {
        vec!["ssh-ed25519 AAAAone".into(), "ssh-ed25519 AAAAtwo".into()]
    }

    #[test]
    fn the_keys_are_added_at_the_login_and_the_session_is_ended() {
        let mut login = Login::new(Getty::new("login"));
        login.grant("tablet", "secret", &keys()).unwrap();
        assert_eq!(login.wire.state, "login", "signed out again");
        let ran = &login.wire.ran[0];
        assert!(ran.contains("authorized_keys") && ran.contains("AAAAone") && ran.contains("AAAAtwo"), "{ran}");
    }

    #[test]
    fn a_shell_left_open_is_ended_first() {
        let mut login = Login::new(Getty::new("shell"));
        login.grant("tablet", "secret", &keys()).unwrap();
        assert_eq!(login.wire.ran.len(), 1);
    }

    #[test]
    fn a_wrong_password_is_said_and_no_key_is_added() {
        let mut login = Login::new(Getty::new("login"));
        let e = login.grant("tablet", "wrong", &keys()).unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(e.message.contains("user name and password"), "{}", e.message);
        assert!(login.wire.ran.is_empty());
        assert_eq!(login.grant("other", "secret", &keys()).unwrap_err().code, Code::Refused);
    }

    #[test]
    fn a_password_prompt_left_from_before_is_failed_first() {
        let mut getty = Getty::new("password");
        getty.user = "someone".into();
        // What stands on the console: the prompt comes again on a fresh line
        // only after the empty password was refused.
        let mut login = Login::new(getty);
        login.wire.say("Password: ");
        login.grant("tablet", "secret", &keys()).unwrap();
        assert_eq!(login.wire.ran.len(), 1);
    }

    #[test]
    fn a_shell_with_a_prompt_of_its_own_is_known_by_what_it_prints() {
        // starship's, which Omarchy uses: no $ or # to know it by.
        let mut getty = Getty::new("shell");
        getty.prompt = "\u{276f} ";
        let mut login = Login::new(getty);
        login.grant("tablet", "secret", &keys()).unwrap();
        assert_eq!((login.wire.state, login.wire.ran.len()), ("login", 1));
    }

    #[test]
    fn a_commands_permission_denied_is_not_a_wrong_password() {
        let mut login = Login::new(Getty::new("login"));
        login.login("tablet", "secret", Duration::from_secs(60)).unwrap();
        assert_eq!(login.run("echo denied; fail", Duration::from_secs(60)).unwrap(), 1);
    }

    #[test]
    fn a_last_line_without_its_end_is_ended_before_a_key_is_added() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join(".ssh/authorized_keys");
        let run = |keys: &[String]| {
            let done = std::process::Command::new("sh").arg("-c").arg(append_keys(keys)).env("HOME", home.path()).status().unwrap();
            assert!(done.success());
            std::fs::read_to_string(&file).unwrap()
        };
        // No file yet.
        assert_eq!(run(&keys()[..1]), "ssh-ed25519 AAAAone\n");
        // Someone's key, with no line end after it.
        std::fs::write(&file, "ssh-ed25519 AAAAone\nssh-rsa THEIRS").unwrap();
        assert_eq!(run(&keys()), "ssh-ed25519 AAAAone\nssh-rsa THEIRS\nssh-ed25519 AAAAtwo\n");
        // Again: nothing is added twice, and no empty line appears.
        assert_eq!(run(&keys()), "ssh-ed25519 AAAAone\nssh-rsa THEIRS\nssh-ed25519 AAAAtwo\n");
    }

    #[test]
    fn keys_are_quoted_and_added_once() {
        let command = append_keys(&["ssh-ed25519 AAAA'; rm -rf ~".into()]);
        assert!(command.contains("grep -qxF 'ssh-ed25519 AAAA'\"'\"'; rm -rf ~' ~/.ssh/authorized_keys"), "{command}");
        assert!(command.starts_with("umask 077; ") && command.ends_with("chmod 600 ~/.ssh/authorized_keys"));
    }
}
