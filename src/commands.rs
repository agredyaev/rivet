//! Command admission and process-tree termination.
//!
//! This module applies command, path, environment, and timeout policy before a
//! managed child starts. Arguments go directly to the configured executable.
use crate::{
    config::{CommandId, Config},
    mcp::Fault,
    sandbox,
};
use cap_std::fs::Dir;
use rivet::command_policy::{arguments_valid, subcommand_allowed, timeout_valid};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, fs, io, path::Path, process::Stdio};
use std::{os::fd::AsRawFd, os::unix::process::CommandExt};
use tokio::process::{Child, Command};
use tokio::time::Duration;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn check_argument_path(config: &Config, cwd: &Path, argument: &str) -> Result<(), Fault> {
    let denied = || {
        Fault::with(
            "PATH_DENIED",
            "command argument path is outside allowed roots or cannot be checked",
            json!({"argument": argument}),
        )
    };
    for value in [
        Some(argument),
        argument.split_once('=').map(|(_, value)| value),
    ]
    .into_iter()
    .flatten()
    {
        let value = if value.starts_with('-')
            && !value.starts_with("--")
            && value.get(2..).is_some_and(|path| {
                path.starts_with('/') || path.starts_with("./") || path.starts_with("../")
            }) {
            &value[2..]
        } else if let Some(path) = value.strip_prefix('@') {
            path
        } else {
            value
        };
        let path = Path::new(value);
        let mut candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let mut missing = Vec::new();
        let resolved = loop {
            match fs::canonicalize(&candidate) {
                Ok(mut resolved) => {
                    for name in missing.iter().rev() {
                        resolved.push(name);
                    }
                    break resolved;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if fs::symlink_metadata(&candidate)
                        .is_ok_and(|meta| meta.file_type().is_symlink())
                    {
                        return Err(denied());
                    }
                    missing.push(candidate.file_name().ok_or_else(denied)?.to_os_string());
                    candidate.pop();
                }
                Err(_) => return Err(denied()),
            }
        };
        if config.root_for(&resolved).is_none() {
            return Err(denied());
        }
    }
    Ok(())
}

pub fn admitted(
    config: &Config,
    request: &CommandRequest,
    default_timeout: u64,
) -> Result<(Command, u64, CommandId), Fault> {
    let (command_id, entry) = config
        .command(&request.command)
        .ok_or_else(|| Fault::new("COMMAND_NOT_FOUND", "command is not registered"))?;
    if !subcommand_allowed(entry.allowed_subcommands.as_deref(), &request.args) {
        return Err(Fault::with(
            "SUBCOMMAND_DENIED",
            "subcommand is not allowed",
            json!({"command": request.command, "subcommand": request.args.first()}),
        ));
    }
    if !arguments_valid(&request.args) {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "command argument contains NUL",
        ));
    }
    let timeout_ms = request.timeout_ms.unwrap_or(default_timeout);
    if !timeout_valid(timeout_ms, config.limits.max_timeout_ms) {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "timeout_ms is outside configured limits",
        ));
    }
    let (root, relative) = config.root_for(Path::new(&request.cwd)).ok_or_else(|| {
        Fault::new(
            "PATH_DENIED",
            "cwd is outside allowed roots or is not absolute",
        )
    })?;
    let cwd: Dir = root
        .dir
        .open_dir(if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        })
        .map_err(|_| Fault::new("PATH_DENIED", "cwd is unavailable or escapes allowed roots"))?;
    let canonical_cwd = fs::canonicalize(&request.cwd)
        .map_err(|_| Fault::new("PATH_DENIED", "cwd cannot be checked"))?;
    if !config.sandbox_commands {
        for argument in &request.args {
            check_argument_path(config, &canonical_cwd, argument)?;
        }
    }

    let mut command = if config.sandbox_commands {
        let workspace = config
            .roots
            .first()
            .ok_or_else(|| Fault::new("SANDBOX_ERROR", "sandbox workspace is unavailable"))?;
        sandbox::command(
            &entry.path,
            &request.args,
            &workspace.path,
            &canonical_cwd,
            config.sandbox_network,
        )
        .map_err(|error| {
            Fault::with(
                "SANDBOX_ERROR",
                "failed to prepare sandbox",
                json!({"error": error}),
            )
        })?
    } else {
        let mut command = std::process::Command::new(&entry.path);
        command.args(&request.args);
        command
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    command.env_clear();
    for name in &config.environment.pass {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in &request.env {
        if !config.environment.allow_override.contains(name) {
            return Err(Fault::with(
                "COMMAND_DENIED",
                "environment override is not allowed",
                json!({"name": name}),
            ));
        }
        if value.contains('\0') {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "environment value contains NUL",
            ));
        }
        command.env(name, value);
    }
    if config.sandbox_commands {
        sandbox::apply_environment(&mut command, &config.roots[0].path);
    }

    command.process_group(0);
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(cwd.as_raw_fd()) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }

    Ok((Command::from(command), timeout_ms, command_id))
}

pub fn kill_group(pid: u32, signal: i32) -> io::Result<()> {
    let code = unsafe { libc::killpg(pid as i32, signal) };
    if code == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_exists(pid: u32) -> io::Result<bool> {
    let code = unsafe { libc::killpg(pid as i32, 0) };
    if code == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

pub async fn terminate_group(pid: u32) -> io::Result<()> {
    kill_group(pid, libc::SIGTERM)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);

    loop {
        if !process_group_exists(pid)? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    if let Err(error) = kill_group(pid, libc::SIGKILL)
        && error.raw_os_error() != Some(libc::EPERM)
    {
        return Err(error);
    }
    Ok(())
}

pub async fn terminate(child: &mut Child, pid: u32) -> io::Result<()> {
    terminate_group(pid).await?;
    let _ = child.wait().await?;
    Ok(())
}
