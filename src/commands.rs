//! Command admission and execution.
//!
//! This module applies the command registry, root, environment, timeout, and
//! output limits before starting a child process. Arguments are passed to the
//! configured executable directly; the OS process group/tree is managed here.
use crate::{config::Config, mcp::Fault};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{collections::BTreeMap, fs, io, path::Path, process::Stdio, sync::Arc, time::Instant};
#[cfg(unix)]
use std::{os::fd::AsRawFd, os::unix::process::CommandExt};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    time::{Duration, Instant as TokioInstant},
};

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

#[derive(Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub truncated: bool,
}

fn check_argument_path(config: &Config, cwd: &Path, argument: &str) -> Result<(), Fault> {
    let denied = || {
        Fault::with(
            "PATH_DENIED",
            "command argument path is outside allowed roots or cannot be checked",
            json!({"argument": argument}),
        )
    };
    // Reject path-like values outside the roots when they can be resolved.
    // This is a conservative check; it cannot sandbox paths interpreted inside
    // an executable's own config files or embedded argument syntax.
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
    // Do not leak the server's full environment into a launched program.
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
        // SAFETY: the closure only uses async-signal-safe fchdir and an owned directory fd.
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
    // SAFETY: kill receives a process-group ID created by process_group(0), with a fixed signal.
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
    // macOS returns EPERM when only already-dead group members remain after SIGTERM.
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

pub async fn drain<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(cap.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < count;
    }
    Ok((bytes, truncated))
}

fn collected(
    result: Result<io::Result<(Vec<u8>, bool)>, tokio::task::JoinError>,
    stream: &str,
) -> Result<(Vec<u8>, bool), Fault> {
    result
        .map_err(|e| {
            Fault::with(
                "IO_ERROR",
                "output reader failed",
                json!({"stream": stream, "error": e.to_string()}),
            )
        })?
        .map_err(|e| {
            Fault::with(
                "IO_ERROR",
                "output read failed",
                json!({"stream": stream, "error": e.to_string()}),
            )
        })
}

pub async fn run(config: Arc<Config>, request: CommandRequest) -> Result<CommandResult, Fault> {
    let (mut command, timeout_ms) = admitted(&config, &request, config.limits.default_timeout_ms)?;
    command.stdin(Stdio::null());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| {
        Fault::with(
            "SPAWN_ERROR",
            "failed to spawn command",
            json!({"error": e.to_string()}),
        )
    })?;
    let pid = child
        .id()
        .ok_or_else(|| Fault::new("SPAWN_ERROR", "spawned child has no process ID"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Fault::new("SPAWN_ERROR", "missing stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Fault::new("SPAWN_ERROR", "missing stderr pipe"))?;
    let mut out = tokio::spawn(drain(stdout, config.limits.max_stdout_bytes));
    let mut err = tokio::spawn(drain(stderr, config.limits.max_stderr_bytes));
    let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
    let wait = tokio::time::timeout_at(deadline, child.wait()).await;
    let (exit_code, timed_out) = match wait {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(error)) => {
            let _ = terminate(&mut child, pid).await;
            return Err(Fault::with(
                "IO_ERROR",
                "failed to wait for command",
                json!({"error": error.to_string()}),
            ));
        }
        Err(_) => {
            terminate(&mut child, pid).await.map_err(|e| {
                Fault::with(
                    "IO_ERROR",
                    "failed to terminate timed-out command",
                    json!({"error": e.to_string()}),
                )
            })?;
            (None, true)
        }
    };
    // Descendants may keep pipes open after the leader exits; the same deadline still applies.
    let output = if timed_out {
        None
    } else {
        tokio::time::timeout_at(deadline, async { ((&mut out).await, (&mut err).await) })
            .await
            .ok()
    };
    let ((stdout, out_truncated), (stderr, err_truncated), timed_out) = match output {
        Some((out_result, err_result)) => (
            collected(out_result, "stdout")?,
            collected(err_result, "stderr")?,
            false,
        ),
        None => {
            if !timed_out {
                terminate(&mut child, pid).await.map_err(|e| {
                    Fault::with(
                        "IO_ERROR",
                        "failed to terminate command group",
                        json!({"error": e.to_string()}),
                    )
                })?;
            }
            (
                collected(out.await, "stdout")?,
                collected(err.await, "stderr")?,
                true,
            )
        }
    };
    let result = CommandResult {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
        truncated: out_truncated || err_truncated,
    };
    if timed_out {
        return Err(Fault::with(
            "COMMAND_TIMEOUT",
            "command timed out",
            json!({"exit_code": result.exit_code, "stdout": result.stdout, "stderr": result.stderr, "duration_ms": result.duration_ms, "timed_out": true, "truncated": result.truncated}),
        ));
    }
    Ok(result)
}
