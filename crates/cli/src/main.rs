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
    // Install and launch stream their own output and exit codes.
    match &cli.command {
        args::Command::Install(a) => return commands::install::install(cli.global.json, a),
        args::Command::Launch(a) => return commands::install::launch(cli.global.json, a),
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
        args::Command::Session { .. } => unreachable!("handled above"),
        args::Command::Devices(a) => commands::devices::run(&cli.global, &a),
        args::Command::Wait { mode, timeout } => {
            commands::devices::wait(&cli.global, mode.into(), timeout.timeout)
        }
        args::Command::Prep {
            path,
            firmware,
            plan,
            timeout,
        } => {
            let way = commands::Way {
                path: &path,
                firmware: &firmware,
            };
            commands::prep::run(&cli.global, &way, plan, timeout)
        }
        args::Command::Load {
            file,
            payload,
            path,
            firmware,
            wait,
            plan,
            timeout,
        } => {
            let way = commands::Way {
                path: &path,
                firmware: &firmware,
            };
            let wait = wait.map(Into::into);
            commands::load::run(&cli.global, &file, &payload, &way, wait, plan, timeout)
        }
        args::Command::Boot {
            boot_entry,
            path,
            firmware,
            list,
            dtb_name,
            no_splash,
            wait,
            plan,
            timeout,
        } => {
            let options = orchard_device::boot::Options {
                entry: boot_entry,
                dtb_name,
                splash: !no_splash,
                wait: wait.map(Into::into),
                list,
            };
            let way = commands::Way {
                path: &path,
                firmware: &firmware,
            };
            commands::boot::run(&cli.global, options, &way, plan, timeout)
        }
        args::Command::Fw(a) => commands::fw::run(&cli.global, &a.command),
        args::Command::Boards => commands::boards::list(&cli.global),
        args::Command::Board { name } => commands::boards::show(&cli.global, &name),
        args::Command::Data { command } => commands::data::run(&cli.global, command.as_ref()),
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
