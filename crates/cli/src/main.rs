//! The command line: read arguments, call the core, print the result.
mod args;
mod commands;
mod output;
mod table;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = args::Cli::parse();
    // A session ends with its command's exit status and prints nothing of
    // its own.
    if let args::Command::Session { timeout, command } = &cli.command {
        return match commands::session::run(&cli.global, *timeout, command) {
            Ok(status) => status,
            Err(e) => output::finish(&cli.global, Err(e)),
        };
    }
    // A shell and a command on the device end with their own status.
    let on_device = match &cli.command {
        args::Command::Shell { reach } => Some(commands::remote::shell(&cli.global, reach)),
        args::Command::Exec { reach, command } => Some(commands::remote::exec(&cli.global, reach, command)),
        _ => None,
    };
    if let Some(ended) = on_device {
        return match ended {
            Ok(status) => status,
            Err(e) => output::finish(&cli.global, Err(e)),
        };
    }
    // Install and launch stream their own output and exit codes.
    match &cli.command {
        args::Command::Install(a) => return commands::install::install(&cli.global, a),
        args::Command::Launch(a) => return commands::install::launch(&cli.global, a),
        _ => {}
    }
    let result = match cli.command {
        args::Command::Install(_) | args::Command::Launch(_) => unreachable!("handled above"),
        args::Command::Build(args) => commands::build::build(&cli.global, &args),
        args::Command::Inputs(args) => commands::build::inputs(&cli.global, &args),
        args::Command::ComponentKeys { output, platform } => {
            commands::build::component_keys(&cli.global, &output, &platform)
        }
        args::Command::Run(args) => commands::run::run(&cli.global, &args),
        args::Command::Session { .. } | args::Command::Shell { .. } | args::Command::Exec { .. } => {
            unreachable!("handled above")
        }
        args::Command::CaptureAdt { output } => commands::remote::capture_adt(&cli.global, &output),
        args::Command::SshAccess { user } => commands::remote::ssh_access(&cli.global, user.as_deref()),
        args::Command::Push { local, remote, reach, serial } => {
            commands::remote::push(&cli.global, &reach, &local, &remote, serial)
        }
        args::Command::Pull { remote, local, reach, serial } => {
            commands::remote::pull(&cli.global, &reach, &remote, &local, serial)
        }
        args::Command::Devices(a) => commands::devices::run(&cli.global, &a),
        args::Command::Wait { mode, timeout } => {
            commands::devices::wait(&cli.global, mode.into(), timeout.timeout)
        }
        args::Command::Fw(a) => commands::fw::run(&cli.global, &a.command),
        args::Command::Boards => commands::boards::list(&cli.global),
        args::Command::Board { name } => commands::boards::show(&cli.global, &name),
        args::Command::Data { command } => commands::data::run(&cli.global, command.as_ref()),
        args::Command::Doctor => commands::host::doctor(&cli.global),
        args::Command::Setup { names } => commands::host::setup(&cli.global, &names),
        args::Command::Cache { command } => {
            let command = command.unwrap_or(args::CacheCommand::Ls);
            commands::cache::run(&cli.global, &command)
        }
        args::Command::Dfu { timeout, command } => {
            commands::dfu::run(&cli.global, timeout.timeout, &command)
        }
        args::Command::Recovery { timeout, command } => {
            commands::recovery::run(&cli.global, timeout.timeout, &command)
        }
        args::Command::Pongo { timeout, command } => {
            commands::pongo::run(&cli.global, timeout.timeout, &command)
        }
        args::Command::M1n1 { timeout, command } => {
            commands::m1n1::run(&cli.global, timeout.timeout, &command)
        }
    };
    output::finish(&cli.global, result)
}
