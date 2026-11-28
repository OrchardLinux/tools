//! The one place that prints results and errors and picks the exit status.
//!
//! Every command returns a [`Report`]: a struct whose JSON form comes from
//! its type and whose text form is a method on the same value, so both show
//! the same data. JSON conventions: snake_case field names, sizes as byte
//! counts named `size`, lists wrapped in an object under a plural name.
use crate::args::Global;
use orchard_cache::Error;
use serde::Serialize;
use std::io::Write;
use std::process::ExitCode;

pub trait Report: Serialize {
    /// What people see: text, or raw bytes for commands like `fw cat`.
    fn human(&self) -> Vec<u8>;
}

/// A finished report, in both forms.
pub struct Output {
    json: serde_json::Value,
    human: Vec<u8>,
}

impl<R: Report> From<R> for Output {
    fn from(report: R) -> Output {
        // Serializing plain data structs cannot fail.
        Output {
            json: serde_json::to_value(&report).expect("report serializes"),
            human: report.human(),
        }
    }
}

/// Tells people what a long command is doing now. The result still comes
/// once, at the end; with `--json` it is all there is.
pub fn progress(global: &Global, text: &str) {
    if !global.json {
        eprintln!("{text}");
    }
}

pub fn finish(global: &Global, result: Result<Output, Error>) -> ExitCode {
    match result {
        Ok(out) if global.json => {
            // The result goes to stdout, which the run log does not see.
            orchard_cache::runlog::line("orchard", &format!("Result: {}", out.json));
            orchard_cache::runlog::release_stderr();
            println!("{}", out.json);
            ExitCode::SUCCESS
        }
        Ok(out) => {
            orchard_cache::runlog::line("orchard", &format!("Result: {}", String::from_utf8_lossy(&out.human)));
            orchard_cache::runlog::release_stderr();
            let mut stdout = std::io::stdout().lock();
            match stdout.write_all(&out.human).and_then(|_| stdout.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(e) => {
            if global.json {
                println!("{}", serde_json::json!({ "error": e }));
            } else {
                eprintln!("error: {e}");
                if let Some(log) = orchard_cache::runlog::path() {
                    eprintln!("log: {}", log.display());
                }
            }
            orchard_cache::runlog::release_stderr();
            // Ctrl-C ended it: the status a shell gives an interrupted command.
            match orchard_build::was_interrupted() {
                true => ExitCode::from(130),
                false => ExitCode::from(e.code.exit_status()),
            }
        }
    }
}
