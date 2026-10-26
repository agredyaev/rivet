//! Rivet's executable entry point.
mod commands;
mod config;
mod filesystem;
mod mcp;
mod process;
mod profile;
mod sandbox;
mod session;
mod tunnel;

use std::{env, process::ExitCode};

fn print_usage(action: Option<&str>) {
    match action {
        Some("session") => println!(
            "Usage: rivet session [OPTIONS]\n\nConnect the current workspace to OpenAI Secure MCP Tunnel.\nChild commands always use the native OS sandbox; child network access is disabled by default.\n\nOptions:\n  --network                        Enable child network access\n  --allow-command NAME=EXECUTABLE Register a command (repeatable)\n  --allow-command-path PATH        Allow child commands read/write access to PATH (repeatable)\n  --pass-env NAME                  Pass one parent environment variable to child commands (repeatable)\n  -h, --help                       Print help\n\nWithout command flags, select built-in or custom commands interactively.\nCredentials: CONTROL_PLANE_TUNNEL_ID and CONTROL_PLANE_API_KEY.\nMissing credentials are prompted with terminal echo disabled. Ctrl+C cancels setup.\n\nExample:\n  rivet session --allow-command git=git --pass-env FRED_API_KEY"
        ),
        _ => println!(
            "Usage: rivet <COMMAND> [OPTIONS]\n\nCommands:\n  session   Connect the current workspace to OpenAI Secure MCP Tunnel\n\nOptions:\n  -h, --help     Print help\n  -V, --version  Print version\n\nRun rivet session --help for session options."
        ),
    }
}

fn assignment<'a>(argument: &str, value: &'a str) -> Result<(&'a str, &'a str), String> {
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

    eprintln!("unknown command: {action}");
    ExitCode::FAILURE
}
