//! Command admission and process-tree termination.
//!
//! This module applies command, path, environment, and timeout policy before a
//! managed child starts. Arguments go directly to the configured executable.
use crate::{config::Config, mcp::Fault};
use cap_std::fs::Dir;
use serde::Deserialize;
use serde_json::json;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{collections::BTreeMap, fs, io, path::Path, process::Stdio};
#[cfg(unix)]
use std::{os::fd::AsRawFd, os::unix::process::CommandExt};
use tokio::process::{Child, Command};
#[cfg(unix)]
use tokio::time::Duration;

#[derive(Clone, Deserialize, schemars::JsonSchema)]
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
) -> Result<(Command, u64), Fault> {
    let entry = config
        .commands
        .get(&request.command)
        .ok_or_else(|| Fault::new("COMMAND_NOT_FOUND", "command is not registered"))?;
    if !entry.allow_any_args
        && !entry
            .allowed_subcommands
            .iter()
            .any(|s| request.args.first() == Some(s))
    {
        return Err(Fault::with(
            "SUBCOMMAND_DENIED",
            "subcommand is not allowed",
            json!({"command": request.command, "subcommand": request.args.first()}),
        ));
    }
    if request.args.iter().any(|arg| arg.contains('\0')) {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "command argument contains NUL",
        ));
    }
    let timeout_ms = request.timeout_ms.unwrap_or(default_timeout);
    if timeout_ms == 0 || timeout_ms > config.limits.max_timeout_ms {
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
    for argument in &request.args {
        check_argument_path(config, &canonical_cwd, argument)?;
    }

    let mut command = std::process::Command::new(&entry.path);
    command
        .args(&request.args)
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

    #[cfg(unix)]
    {
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
    }

    #[cfg(windows)]
    {
        drop(cwd);
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        command.current_dir(canonical_cwd);
    }

    Ok((Command::from(command), timeout_ms))
}

#[cfg(unix)]
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

#[cfg(unix)]
pub async fn terminate(child: &mut Child, pid: u32) -> io::Result<()> {
    kill_group(pid, libc::SIGTERM)?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    if let Err(error) = kill_group(pid, libc::SIGKILL)
        && error.raw_os_error() != Some(libc::EPERM)
    {
        return Err(error);
    }
    child.wait().await.map(|_| ())
}

#[cfg(windows)]
pub async fn terminate(child: &mut Child, pid: u32) -> io::Result<()> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if !status.success() && child.try_wait()?.is_none() {
        return Err(io::Error::other("taskkill failed to stop the process tree"));
    }
    child.wait().await.map(|_| ())
}
