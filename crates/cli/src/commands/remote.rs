//! `shell`, `exec`, `push`, `pull`: a running RAM payload or installed
//! system, through `orchard-remote`.
use crate::args::{Global, ReachArgs};
use crate::output::{Output, Report};
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_remote::{Copied, Reach};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn reach(args: &ReachArgs) -> Reach {
    Reach { user: args.user.clone(), host: args.host.clone(), ram: args.ram }
}

fn cache(global: &Global) -> Result<PathBuf> {
    Ok(Store::open(global.cache.as_deref())?.dir().to_owned())
}

/// A status the device's command ended with, as this command's own.
fn status(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(255))
}

pub fn shell(global: &Global, args: &ReachArgs) -> Result<ExitCode> {
    orchard_remote::shell(&cache(global)?, &reach(args)).map(status)
}

pub fn exec(global: &Global, args: &ReachArgs, command: &[String]) -> Result<ExitCode> {
    orchard_remote::exec(&cache(global)?, &reach(args), command).map(status)
}

#[derive(Serialize)]
struct Transfer {
    from: String,
    to: String,
    size: u64,
    sha256: String,
}

impl Report for Transfer {
    fn human(&self) -> Vec<u8> {
        format!("{} -> {} ({} bytes, sha256 {})\n", self.from, self.to, self.size, &self.sha256[..16]).into_bytes()
    }
}

fn copied(done: Copied) -> Output {
    Transfer { from: done.from, to: done.to, size: done.size, sha256: done.sha256 }.into()
}

pub fn push(global: &Global, args: &ReachArgs, local: &Path, remote: &str, serial: bool) -> Result<Output> {
    orchard_remote::push(&cache(global)?, &reach(args), local, remote, serial).map(copied)
}

pub fn pull(global: &Global, args: &ReachArgs, remote: &str, local: &Path, serial: bool) -> Result<Output> {
    orchard_remote::pull(&cache(global)?, &reach(args), remote, local, serial).map(copied)
}
