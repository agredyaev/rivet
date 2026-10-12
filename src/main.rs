//! Rivet's executable entry point.
//!
//! Startup validates roots and command permissions before dispatching to the CLI
//! command or starting the MCP stdio server.
mod commands;
mod config;
mod filesystem;
mod mcp;
mod process;
mod profile;
mod sandbox;
mod session;
mod tunnel;

use std::{collections::BTreeMap, env, path::PathBuf, process::ExitCode, sync::Arc};

#[derive(Default)]
struct PendingCommand {
    executable: Option<String>,
    allow_any_args: bool,
    allowed_subcommands: Vec<String>,
}

impl PendingCommand {
    fn finish(self, name: &str) -> Result<config::CommandConfig, String> {
        let executable = self
            .executable
            .ok_or_else(|| format!("{name}: --allow-command NAME=EXECUTABLE is required"))?;
        let allowed_subcommands = if self.allow_any_args {
            if !self.allowed_subcommands.is_empty() {
                return Err(format!(
                    "{name}: --allow-any-args cannot be combined with --allow-subcommand"
                ));
            }
            None
        } else {
            if self.allowed_subcommands.is_empty() {
                return Err(format!(
                    "{name}: specify --allow-any-args or at least one --allow-subcommand"
                ));
            }
            Some(self.allowed_subcommands)
        };
        Ok(config::CommandConfig {
            executable,
            allowed_subcommands,
        })
    }
}

fn print_usage(action: Option<&str>) {
    match action {
        Some("session") => println!(
            "Usage: rivet session [OPTIONS]\n\nConnect to OpenAI Secure MCP Tunnel. The current directory is the workspace.\nChild commands use the OS sandbox; child network access is disabled by default.\n\nOptions:\n  --network                     Enable child network access\n  --allow-command NAME=EXECUTABLE  Register a command (repeat for each command)\n  --allow-subcommand NAME=VALUE  Allow a first argument (repeat for each value)\n  --allow-any-args NAME          Allow all arguments for a registered command\n  -h, --help                    Print help\n\nWithout command flags, select built-in or custom commands interactively.\nCredentials: CONTROL_PLANE_TUNNEL_ID and CONTROL_PLANE_API_KEY.\nMissing credentials are prompted with terminal echo disabled. Ctrl+C cancels setup.\n\nExample:\n  rivet session --allow-command git=git --allow-subcommand git=status"
        ),
        Some(action) if matches!(action, "serve" | "doctor" | "commands") => {
            let description = match action {
                "serve" => {
                    "Run the MCP server over stdin/stdout. Child commands have no OS sandbox."
                }
                "doctor" => {
                    "Validate stdio roots, commands, root access and stdio descriptors. This does not check the native sandbox or tunnel."
                }
                _ => {
                    "Print registered commands, resolved executable paths and argument permissions."
                }
            };
            println!(
                "Usage: rivet {action} --root PATH [--root PATH]... [OPTIONS]\n\n{description}\n\nOptions:\n  --root PATH                   Allow a filesystem root (required; repeatable)\n  --allow-command NAME=EXECUTABLE  Register a command (repeatable)\n  --allow-subcommand NAME=VALUE  Allow a first argument (repeatable)\n  --allow-any-args NAME          Allow all arguments for a registered command\n  -h, --help                    Print help\n\nExample:\n  rivet {action} --root . --allow-command git=git --allow-subcommand git=status"
            )
        }
        _ => println!(
            "Usage: rivet <COMMAND> [OPTIONS]\n\nCommands:\n  session   Connect the current workspace to OpenAI Secure MCP Tunnel\n  serve     Run an MCP server over stdin/stdout with explicit workspace roots\n  doctor    Check roots and commands for stdio mode\n  commands  Print registered command paths and argument permissions\n\nOptions:\n  -h, --help     Print help\n  -V, --version  Print version\n\nRun `rivet <command> --help` for command options.\nStart an interactive tunnel session with `rivet session`."
        ),
    }
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
        print_usage(None);
        return ExitCode::FAILURE;
    };
    if matches!(action.as_str(), "--version" | "-V") {
        println!("rivet {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if matches!(action.as_str(), "help" | "--help" | "-h") {
        print_usage(None);
        return ExitCode::SUCCESS;
    }
    if action == "session" {
        return session::run(args.collect()).await;
    }
    if !matches!(action.as_str(), "serve" | "doctor" | "commands") {
        eprintln!("unknown command: {action}");
        return ExitCode::FAILURE;
    }
    let mut roots = Vec::new();
    let mut command_args: BTreeMap<String, PendingCommand> = BTreeMap::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage(Some(&action));
                return ExitCode::SUCCESS;
            }
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
            "--allow-command" | "--allow-subcommand" | "--root" => {
                let Some(value) = args.next() else {
                    eprintln!("{arg} requires a value");
                    return ExitCode::FAILURE;
                };
                match arg.as_str() {
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
        .map(|(name, pending)| pending.finish(&name).map(|command| (name, command)))
        .collect::<Result<BTreeMap<_, _>, String>>();
    let command_specs = match command_specs {
        Ok(commands) => commands,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let config = match config::Config::new(roots, command_specs) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("STARTUP_ERROR: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match action.as_str() {
        "doctor" => config.doctor().map(|()| println!("doctor ok")),
        "commands" => {
            for entry in &config.commands {
                let authorization = entry
                    .allowed_subcommands
                    .as_deref()
                    .map(|values| values.join(","))
                    .unwrap_or_else(|| "any args".to_owned());
                println!("{}\t{}\t{authorization}", entry.name, entry.path.display());
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
