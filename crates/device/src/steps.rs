//! The steps of a method: run once and in order against the session, or,
//! for a plan, recorded by name without touching anything.
//!
//! A method's function is written once against [`Run`]. A plan runs it with
//! no session: each step is recorded and its action skipped. A real run
//! runs the same function after a plan of it, so the progress, the result
//! and an error all count the same steps. A step that fails ends the method
//! with an error that says which step it was and what had been done before.
use crate::host::{Connect, DfuMode, M1n1Proxy, PongoShell, RecoveryMode};
use crate::session::Session;
use orchard_cache::{Error, Result};
use orchard_usb::dfu::{Pwned, Status};
use orchard_usb::pongo::Output;
use orchard_usb::Sent;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    /// The device accepted a command that makes it start something else.
    Acknowledged,
    /// The device stopped answering during such a command. The wait that
    /// follows shows whether it ran.
    Unconfirmed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Acknowledged => "acknowledged",
            Outcome::Unconfirmed => "unconfirmed",
        }
    }

    pub fn of(sent: Sent) -> Outcome {
        match sent {
            Sent::Acknowledged => Outcome::Acknowledged,
            Sent::Unconfirmed => Outcome::Unconfirmed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub name: String,
    pub outcome: Outcome,
}

/// One method's steps, run against a session or recorded for a plan.
pub struct Run<'a, D: Connect> {
    session: Option<&'a mut Session<D>>,
    /// The step names the plan of this run gave; empty for a plan.
    planned: Vec<String>,
    /// Plan: the names recorded. Run: the steps done.
    names: Vec<String>,
    done: Vec<Step>,
    progress: &'a mut dyn FnMut(&str),
}

impl<'a, D: Connect> Run<'a, D> {
    /// A run that records step names and touches nothing.
    pub fn plan(progress: &'a mut dyn FnMut(&str)) -> Run<'a, D> {
        Run {
            session: None,
            planned: Vec::new(),
            names: Vec::new(),
            done: Vec::new(),
            progress,
        }
    }

    /// A run against `session`, whose plan named `planned`; `progress`
    /// hears each step as it starts.
    pub fn new(
        session: &'a mut Session<D>,
        planned: Vec<String>,
        progress: &'a mut dyn FnMut(&str),
    ) -> Run<'a, D> {
        Run {
            session: Some(session),
            planned,
            names: Vec::new(),
            done: Vec::new(),
            progress,
        }
    }

    pub fn planning(&self) -> bool {
        self.session.is_none()
    }

    /// The session, in a real run.
    pub fn session(&mut self) -> Option<&mut Session<D>> {
        self.session.as_deref_mut()
    }

    /// Runs the next step, or records it in a plan, where it gives
    /// `T::default()`.
    pub fn step<T: Default>(
        &mut self,
        name: impl Into<String>,
        run: impl FnOnce(&mut Session<D>) -> Result<(Outcome, T)>,
    ) -> Result<T> {
        self.step_with_hint(name, None, run)
    }

    /// The same, with `hint` as a progress line of its own after the step's
    /// name, for whoever watches the step run.
    pub fn step_with_hint<T: Default>(
        &mut self,
        name: impl Into<String>,
        hint: Option<&str>,
        run: impl FnOnce(&mut Session<D>) -> Result<(Outcome, T)>,
    ) -> Result<T> {
        let name = name.into();
        let Some(session) = self.session.as_deref_mut() else {
            self.names.push(name);
            return Ok(T::default());
        };
        (self.progress)(&name);
        if let Some(hint) = hint {
            (self.progress)(hint);
        }
        match run(session) {
            Ok((outcome, value)) => {
                self.done.push(Step { name, outcome });
                Ok(value)
            }
            Err(e) => Err(self.stopped(&name, e)),
        }
    }

    /// Says where the method stopped and what it had done by then.
    fn stopped(&self, name: &str, e: Error) -> Error {
        let at = self.done.len();
        let done = match at {
            0 => "nothing had been done to the device".to_owned(),
            _ => {
                let names: Vec<&str> = self.done.iter().map(|s| s.name.as_str()).collect();
                format!("completed before it: {}", names.join("; "))
            }
        };
        let hint = match &e.hint {
            Some(own) => format!("{own}; {done}"),
            None => done,
        };
        let of = self.planned.len().max(at + 1);
        let step = format!("step {} of {of} ({name})", at + 1);
        Error {
            hint: Some(hint),
            ..e.context(step)
        }
    }

    /// The names a plan recorded.
    pub fn names(self) -> Vec<String> {
        self.names
    }

    /// The steps a real run did.
    pub fn finish(self) -> Vec<Step> {
        self.done
    }

    pub fn dfu(&mut self) -> Result<Box<dyn DfuMode>> {
        match self.session() {
            Some(session) => session.dfu(),
            None => Ok(Box::new(Unused)),
        }
    }

    pub fn recovery(&mut self) -> Result<Box<dyn RecoveryMode>> {
        match self.session() {
            Some(session) => session.recovery(),
            None => Ok(Box::new(Unused)),
        }
    }

    pub fn pongo(&mut self) -> Result<Box<dyn PongoShell>> {
        match self.session() {
            Some(session) => session.pongo(),
            None => Ok(Box::new(Unused)),
        }
    }

    pub fn m1n1(&mut self) -> Result<Box<dyn M1n1Proxy>> {
        match self.session() {
            Some(session) => session.m1n1(),
            None => Ok(Box::new(Unused)),
        }
    }
}

/// The handle a plan opens: its steps never run, so nothing calls it.
struct Unused;

type UsbResult<T> = std::result::Result<T, orchard_usb::Error>;
const PLANNED: &str = "a plan runs no step";

impl DfuMode for Unused {
    fn status(&mut self, _: Duration) -> UsbResult<Status> {
        unreachable!("{PLANNED}")
    }
    fn clear_status(&mut self, _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn abort(&mut self, _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn reset(&mut self) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn send(&mut self, _: &[u8], _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn start_pwned(&mut self, _: Pwned, _: &[u8], _: Duration) -> UsbResult<Sent> {
        unreachable!("{PLANNED}")
    }
}

impl RecoveryMode for Unused {
    fn send(&mut self, _: &[u8], _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn command(&mut self, _: &str, _: Duration) -> UsbResult<Sent> {
        unreachable!("{PLANNED}")
    }
    fn getenv(&mut self, _: &str, _: Duration) -> UsbResult<String> {
        unreachable!("{PLANNED}")
    }
}

impl PongoShell for Unused {
    fn output(&mut self, _: Duration) -> UsbResult<Output> {
        unreachable!("{PLANNED}")
    }
    fn send(&mut self, _: &[u8], _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn command(&mut self, _: &str, _: Duration) -> UsbResult<Sent> {
        unreachable!("{PLANNED}")
    }
}

impl M1n1Proxy for Unused {
    fn ping(&mut self, _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn call(&mut self, _: u64, _: &[u64], _: Duration) -> UsbResult<u64> {
        unreachable!("{PLANNED}")
    }
    fn read(&mut self, _: u64, _: usize, _: Duration) -> UsbResult<Vec<u8>> {
        unreachable!("{PLANNED}")
    }
    fn write(&mut self, _: u64, _: &[u8], _: Duration) -> UsbResult<()> {
        unreachable!("{PLANNED}")
    }
    fn take_console(&mut self) -> Vec<u8> {
        unreachable!("{PLANNED}")
    }
}
