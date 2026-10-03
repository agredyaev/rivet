//! Rivet's executable entry point.
//!
//! Startup loads and validates configuration before dispatching to the CLI
//! command or starting the MCP stdio server.
mod commands;
mod config;
mod filesystem;
mod mcp;
mod process;
mod session;
mod tunnel;

use std::{collections::BTreeMap, env, path::PathBuf, process::ExitCode, sync::Arc};

#[derive(Default)]
struct PendingCommand {
    executable: Option<String>,
    allow_any_args: bool,
    allowed_subcommands: Vec<String>,
}

fn assignment<'b>(argument: &str, value: &'b str) -> Result<(&'b str, &'b str), String> {
    let Some((name, setting)) = value.split_once('=') else {
        return Err(format!("{argument} requires NAME=VALUE"));
    };
    if name.is_empty() || setting.is_empty() {
        return Err(format!("{argument} requires nonempty NAME and VALUE"));
    }
    Ok((name, setting))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(action) = args.next() else {
        eprintln!(
            "usage: rivet <serve|doctor|commands|config-check> [--config PATH] --root PATH... [--allow-command NAME=EXECUTABLE]... [--allow-subcommand NAME=VALUE]... [--allow-any-args NAME]..."
        );
        return ExitCode::FAILURE;
    };
    if action == "session-mcp" {
        let executable = match env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("Cannot locate Rivet executable: {error}");
                return ExitCode::FAILURE;
            }
        };
        let status = match std::process::Command::new(executable)
            .arg("serve")
            .args(args)
            .env_remove("CONTROL_PLANE_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("OPENAI_ADMIN_KEY")
            .status()
        {
            Ok(status) => status,
            Err(error) => {
                eprintln!("Could not start Rivet MCP server: {error}");
                return ExitCode::FAILURE;
            }
        };
        return if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if action == "session" {
        return session::run(args.collect());
    }
    let mut path = PathBuf::from("./rivet.toml");
    let mut roots = Vec::new();
    let mut command_args: BTreeMap<String, PendingCommand> = BTreeMap::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-any-args" => {
                let Some(name) = args.next() else {
                    eprintln!("--allow-any-args requires a command name");
                    return ExitCode::FAILURE;
                };
                let pending = command_args.entry(name).or_default();
                if pending.allow_any_args {
                    eprintln!("--allow-any-args was repeated for the same command");
                    return ExitCode::FAILURE;
                }
                pending.allow_any_args = true;
            }
            "--allow-command" | "--allow-subcommand" | "--config" | "--root" => {
                let Some(value) = args.next() else {
                    eprintln!("{arg} requires a value");
                    return ExitCode::FAILURE;
                };
                match arg.as_str() {
                    "--config" => path = PathBuf::from(value),
                    "--root" => roots.push(PathBuf::from(value)),
                    "--allow-command" => {
                        let (name, executable) = match assignment(&arg, &value) {
                            Ok(pair) => pair,
                            Err(error) => {
                                eprintln!("{error}");
                                return ExitCode::FAILURE;
                            }
                        };
                        let pending = command_args.entry(name.to_owned()).or_default();
                        if pending.executable.replace(executable.to_owned()).is_some() {
                            eprintln!("duplicate --allow-command for {name}");
                            return ExitCode::FAILURE;
                        }
                    }
                    "--allow-subcommand" => {
                        let (name, subcommand) = match assignment(&arg, &value) {
                            Ok(pair) => pair,
                            Err(error) => {
                                eprintln!("{error}");
                                return ExitCode::FAILURE;
                            }
                        };
                        command_args
                            .entry(name.to_owned())
                            .or_default()
                            .allowed_subcommands
                            .push(subcommand.to_owned());
                    }
                    _ => unreachable!(),
                }
            }
            _ => {
                eprintln!("unknown argument: {arg}");
                return ExitCode::FAILURE;
            }
        }
    }

    let command_specs = command_args
        .into_iter()
        .map(|(name, pending)| {
            let executable = pending
                .executable
                .ok_or_else(|| format!("{name}: --allow-command NAME=EXECUTABLE is required"))?;
            let allowed_subcommands =
                if pending.allow_any_args && pending.allowed_subcommands.is_empty() {
                    None
                } else {
                    Some(pending.allowed_subcommands)
                };
            Ok((
                name,
                config::CommandConfig {
                    executable,
                    allow_any_args: pending.allow_any_args,
                    allowed_subcommands,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, String>>();
    let command_specs = match command_specs {
        Ok(commands) => commands,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let config = match config::Config::load(&path, roots, command_specs) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("CONFIG_ERROR: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match action.as_str() {
        "config-check" => {
            println!("configuration valid");
            Ok(())
        }
        "doctor" => config.doctor().map(|()| println!("doctor ok")),
        "commands" => {
            for (name, entry) in &config.commands {
                let authorization = if entry.allow_any_args {
                    "any args".to_owned()
                } else {
                    entry.allowed_subcommands.join(",")
                };
                println!("{name}\t{}\t{authorization}", entry.path.display());
            }
            Ok(())
        }
        "serve" => mcp::serve(config).await,
        _ => Err(format!("unknown command: {action}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
