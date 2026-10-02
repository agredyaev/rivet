mod commands;
mod config;
mod filesystem;
mod mcp;
mod process;

use std::{env, path::PathBuf, process::ExitCode, sync::Arc};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(action) = args.next() else {
        eprintln!(
            "usage: rivet <serve|doctor|commands|config-check> [--config PATH] [--root PATH]..."
        );
        return ExitCode::FAILURE;
    };
    let mut path = PathBuf::from("./rivet.toml");
    let mut roots = Vec::new();
    while let Some(arg) = args.next() {
        if arg != "--config" && arg != "--root" {
            eprintln!("unknown argument: {arg}");
            return ExitCode::FAILURE;
        }
        let Some(value) = args.next() else {
            eprintln!("{arg} requires a path");
            return ExitCode::FAILURE;
        };
        if arg == "--config" {
            path = PathBuf::from(value);
        } else {
            roots.push(PathBuf::from(value));
        }
    }
    let config = match config::Config::load(&path, roots) {
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
                println!("{name}\t{}", entry.path.display());
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
