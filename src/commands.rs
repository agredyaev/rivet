//! Command admission and process-tree termination.
//
//! This module applies command, path, environment, and timeout policy before a
//! managed child starts. Arguments go directly to the configured executable.
use crate::{
    config::{CommandId, Config, MAX_TIMEOUT_MS, command_executable, command_id, root_for},
    mcp::Fault,
    sandbox,
};
use cap_std::fs::Dir;
use rivet::command_policy::{arguments_valid, timeout_valid};
use serde::Deserialize;
use serde_json::json;
use std::{fs, io, path::Path, process::Stdio};
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
}

pub fn admitted(
    config: &Config,
    request: &CommandRequest,
    default_timeout: u64,
) -> Result<(Command, u64, CommandId), Fault> {
    let command_id = command_id(config, &request.command)
        .ok_or_else(|| Fault::new("COMMAND_NOT_FOUND", "command is not registered"))?;
    if !arguments_valid(&request.args) {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "command argument contains NUL",
        ));
    }
    let timeout_ms = request.timeout_ms.unwrap_or(default_timeout);
    if !timeout_valid(timeout_ms, MAX_TIMEOUT_MS) {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "timeout_ms is outside built-in limits",
        ));
    }
    let (root, relative) = root_for(config, Path::new(&request.cwd)).ok_or_else(|| {
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
    let mut command = sandbox::command_with_paths(
        command_executable(config, command_id),
        &request.args,
        &config.root.path,
        &canonical_cwd,
        &config.command_path_grant,
        config.network,
    )
    .map_err(|error| {
        Fault::with(
            "SANDBOX_ERROR",
            "failed to prepare sandbox",
            json!({"error": error}),
        )
    })?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    command.env_clear();
    if let Some(path) = &config.runtime_path {
        command.env("PATH", path);
    }
    for index in 0..config.env_name.len() {
        command.env(&config.env_name[index], &config.env_value[index]);
    }
    sandbox::apply_environment(&mut command, &config.root.path);

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
