//! Lifecycle and bounded output capture for managed child processes.
//!
//! The table owns every child that can outlive one MCP request. Each output
//! stream stores a capped prefix while tracking total bytes for offset reads.
use crate::{
    commands::{CommandRequest, admitted, terminate},
    config::Config,
    mcp::Fault,
};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::HashMap,
    io,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdin,
    sync::{Mutex as AsyncMutex, oneshot, watch},
    time::{Duration, Instant as TokioInstant},
};

const TERMINATION_SETTLE_MS: u64 = 2_000;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct ProcessId(u64);

#[derive(Clone, Serialize)]
pub struct Status {
    pub running: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stopped: bool,
    pub duration_ms: Option<u64>,
}

struct Buffer {
    bytes: Vec<u8>,
    total: u64,
    truncated: bool,
}
impl Buffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            truncated: false,
        }
    }

    fn append(&mut self, chunk: &[u8], cap: usize) {
        self.total += chunk.len() as u64;
        let count = chunk.len().min(cap.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&chunk[..count]);
        self.truncated |= count < chunk.len();
    }

    fn slice(&self, offset: u64, limit: usize) -> Result<OutputSlice, Fault> {
        if offset > self.total {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "output offset is beyond received bytes",
            ));
        }
        let start = (offset as usize).min(self.bytes.len());
        let end = start.saturating_add(limit).min(self.bytes.len());
        let next_offset =
            if offset as usize >= self.bytes.len() && self.total > self.bytes.len() as u64 {
                self.total
            } else {
                end as u64
            };
        Ok(OutputSlice {
            text: String::from_utf8_lossy(&self.bytes[start..end]).into_owned(),
            next_offset,
            total_bytes: self.total,
            truncated: self.truncated,
        })
    }
}

#[derive(Serialize)]
struct OutputSlice {
    text: String,
    next_offset: u64,
    total_bytes: u64,
    truncated: bool,
}

struct ProcessEntry {
    command: String,
    stdout: Arc<Mutex<Buffer>>,
    stderr: Arc<Mutex<Buffer>>,
    stdin: AsyncMutex<Option<ChildStdin>>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    status: watch::Receiver<Status>,
    started: Instant,
}

enum InputMode {
    Closed,
    Piped,
}

struct SpawnedProcess {
    id: ProcessId,
    entry: Arc<ProcessEntry>,
    timeout_ms: u64,
}

pub struct ProcessTable {
    entries: Mutex<HashMap<ProcessId, Arc<ProcessEntry>>>,
    next: Mutex<u64>,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            next: Mutex::new(1),
        }
    }

    fn get(&self, id: u64) -> Result<Arc<ProcessEntry>, Fault> {
        self.entries
            .lock()
            .unwrap()
            .get(&ProcessId(id))
            .cloned()
            .ok_or_else(|| Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted"))
    }

    fn insert_entry(
        &self,
        config: &Config,
        command: String,
        stop_tx: oneshot::Sender<()>,
        status_rx: watch::Receiver<Status>,
    ) -> Result<(ProcessId, Arc<ProcessEntry>), Fault> {
        let entry = Arc::new(ProcessEntry {
            command,
            stdout: Arc::new(Mutex::new(Buffer::new())),
            stderr: Arc::new(Mutex::new(Buffer::new())),
            stdin: AsyncMutex::new(None),
            stop: Mutex::new(Some(stop_tx)),
            status: status_rx,
            started: Instant::now(),
        });
        let mut entries = self.entries.lock().unwrap();

        if entries.len() >= config.limits.max_running_processes
            && let Some(oldest) = entries
                .iter()
                .filter(|(_, e)| !e.status.borrow().running)
                .min_by_key(|(_, e)| e.started)
                .map(|(id, _)| *id)
        {
            entries.remove(&oldest);
        }
        if entries.len() >= config.limits.max_running_processes {
            return Err(Fault::new("PROCESS_LIMIT", "process table is full"));
        }

        let mut next = self.next.lock().unwrap();
        let id = ProcessId(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process IDs exhausted"))?;
        entries.insert(id, entry.clone());
        Ok((id, entry))
    }

    fn spawn(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
        default_timeout_ms: u64,
        input_mode: InputMode,
    ) -> Result<SpawnedProcess, Fault> {
        let command_name = request.command.clone();
        let (mut command, timeout_ms) = admitted(&config, &request, default_timeout_ms)?;
        if matches!(input_mode, InputMode::Closed) {
            command.stdin(Stdio::null());
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        let (status_tx, status_rx) = watch::channel(Status {
            running: true,
            exit_code: None,
            timed_out: false,
            stopped: false,
            duration_ms: None,
        });
        let (id, entry) = self.insert_entry(&config, command_name, stop_tx, status_rx)?;

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.entries.lock().unwrap().remove(&id);
                return Err(Fault::with(
                    "SPAWN_ERROR",
                    "failed to spawn process",
                    json!({"error": error.to_string()}),
                ));
            }
        };
        let Some(pid) = child.id() else {
            let _ = child.start_kill();
            self.entries.lock().unwrap().remove(&id);
            return Err(Fault::new("SPAWN_ERROR", "spawned child has no process ID"));
        };

        if matches!(input_mode, InputMode::Piped) {
            *entry
                .stdin
                .try_lock()
                .expect("new process stdin lock must be uncontended") = child.stdin.take();
        } else {
            let _ = child.stdin.take();
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let out_task = tokio::spawn(drain_into(
            stdout,
            entry.stdout.clone(),
            config.limits.max_stdout_bytes,
        ));
        let err_task = tokio::spawn(drain_into(
            stderr,
            entry.stderr.clone(),
            config.limits.max_stderr_bytes,
        ));

        let supervisor_entry = entry.clone();
        tokio::spawn(async move {
            let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
            let mut stop_rx = stop_rx;
            let (mut status, needs_stop) = tokio::select! {
                result = child.wait() => (
                    Status {
                        running: false,
                        exit_code: result.ok().and_then(|s| s.code()),
                        timed_out: false,
                        stopped: false,
                        duration_ms: None,
                    },
                    false,
                ),
                _ = &mut stop_rx => (
                    Status {
                        running: false,
                        exit_code: None,
                        timed_out: false,
                        stopped: true,
                        duration_ms: None,
                    },
                    true,
                ),
                _ = tokio::time::sleep_until(deadline) => (
                    Status {
                        running: false,
                        exit_code: None,
                        timed_out: true,
                        stopped: false,
                        duration_ms: None,
                    },
                    true,
                ),
            };

            if needs_stop {
                let _ = terminate(&mut child, pid).await;
            }

            let mut outputs = Box::pin(async {
                let _ = tokio::join!(out_task, err_task);
            });
            if !needs_stop {
                tokio::select! {
                    _ = &mut outputs => {},
                    _ = &mut stop_rx => {
                        status.stopped = true;
                        let _ = terminate(&mut child, pid).await;
                        outputs.await;
                    },
                    _ = tokio::time::sleep_until(deadline) => {
                        status.timed_out = true;
                        let _ = terminate(&mut child, pid).await;
                        outputs.await;
                    },
                }
            } else {
                outputs.await;
            }

            *supervisor_entry.stdin.lock().await = None;
            status.duration_ms = Some(supervisor_entry.started.elapsed().as_millis() as u64);
            status_tx.send_replace(status);
        });

        Ok(SpawnedProcess {
            id,
            entry,
            timeout_ms,
        })
    }

    fn run_value(
        entry: &ProcessEntry,
        id: ProcessId,
        state: &str,
        retain_process_id: bool,
        config: &Config,
    ) -> Result<serde_json::Value, Fault> {
        let stdout = entry
            .stdout
            .lock()
            .unwrap()
            .slice(0, config.limits.max_stdout_bytes)?;
        let stderr = entry
            .stderr
            .lock()
            .unwrap()
            .slice(0, config.limits.max_stderr_bytes)?;
        let status = entry.status.borrow().clone();
        let duration_ms = status
            .duration_ms
            .unwrap_or_else(|| entry.started.elapsed().as_millis() as u64);

        Ok(json!({
            "state": state,
            "process_id": retain_process_id.then_some(id.0),
            "exit_code": status.exit_code,
            "stdout": stdout.text,
            "stderr": stderr.text,
            "stdout_offset": stdout.next_offset,
            "stderr_offset": stderr.next_offset,
            "duration_ms": duration_ms,
            "timed_out": status.timed_out,
            "truncated": stdout.truncated || stderr.truncated,
        }))
    }

    pub async fn run(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let foreground_wait_ms = config.limits.foreground_wait_ms;
        let spawned = self.spawn(
            config.clone(),
            request,
            config.limits.default_timeout_ms,
            InputMode::Closed,
        )?;

        let wait_ms = if spawned.timeout_ms <= foreground_wait_ms {
            spawned.timeout_ms.saturating_add(TERMINATION_SETTLE_MS)
        } else {
            foreground_wait_ms
        };
        let mut status = spawned.entry.status.clone();
        if status.borrow().running {
            let _ = tokio::time::timeout(Duration::from_millis(wait_ms), async {
                while status.borrow().running {
                    if status.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }

        let final_status = status.borrow().clone();
        if final_status.running {
            return Self::run_value(&spawned.entry, spawned.id, "running", true, &config);
        }

        let value = Self::run_value(&spawned.entry, spawned.id, "completed", false, &config)?;
        self.entries.lock().unwrap().remove(&spawned.id);

        if final_status.timed_out {
            return Err(Fault::with(
                "COMMAND_TIMEOUT",
                "command timed out",
                json!({
                    "exit_code": value["exit_code"],
                    "stdout": value["stdout"],
                    "stderr": value["stderr"],
                    "duration_ms": value["duration_ms"],
                    "timed_out": true,
                    "truncated": value["truncated"],
                }),
            ));
        }
        Ok(value)
    }

    pub fn start(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let spawned = self.spawn(
            config.clone(),
            request,
            config.limits.max_timeout_ms,
            InputMode::Piped,
        )?;
        Ok(json!({"process_id": spawned.id.0}))
    }

    pub fn list(&self) -> serde_json::Value {
        let entries = self.entries.lock().unwrap();
        let mut rows: Vec<_> = entries
            .iter()
            .map(|(id, entry)| {
                let status = entry.status.borrow().clone();
                let duration_ms = status
                    .duration_ms
                    .unwrap_or_else(|| entry.started.elapsed().as_millis() as u64);
                json!({
                    "process_id": id.0,
                    "command": entry.command,
                    "status": status,
                    "duration_ms": duration_ms,
                })
            })
            .collect();
        rows.sort_by_key(|row| row["process_id"].as_u64().unwrap_or(0));
        json!({"processes": rows})
    }

    pub fn read(
        &self,
        id: u64,
        stdout_offset: u64,
        stderr_offset: u64,
        limit: usize,
        max: usize,
    ) -> Result<serde_json::Value, Fault> {
        if limit == 0 || limit > max {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "output limit is outside configured bounds",
            ));
        }
        let entry = self.get(id)?;
        let stdout = entry.stdout.lock().unwrap().slice(stdout_offset, limit)?;
        let stderr = entry.stderr.lock().unwrap().slice(stderr_offset, limit)?;
        let status = entry.status.borrow().clone();
        Ok(json!({"stdout": stdout, "stderr": stderr, "status": status}))
    }

    pub async fn input(
        &self,
        id: u64,
        text: &str,
        max: usize,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, Fault> {
        if text.len() > max {
            return Err(Fault::new(
                "OUTPUT_LIMIT",
                "input exceeds max_file_read_bytes",
            ));
        }
        let entry = self.get(id)?;
        let mut stdin = entry.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| Fault::new("IO_ERROR", "process stdin is closed"))?;
        tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            pipe.write_all(text.as_bytes()),
        )
        .await
        .map_err(|_| Fault::new("COMMAND_TIMEOUT", "process stdin write timed out"))?
        .map_err(|e| {
            Fault::with(
                "IO_ERROR",
                "process stdin write failed",
                json!({"error": e.to_string()}),
            )
        })?;
        Ok(json!({"bytes_written": text.len()}))
    }

    pub async fn stop(&self, id: u64) -> Result<serde_json::Value, Fault> {
        let entry = self.get(id)?;
        if let Some(sender) = entry.stop.lock().unwrap().take() {
            let _ = sender.send(());
        }
        let mut status = entry.status.clone();
        while status.borrow().running {
            status
                .changed()
                .await
                .map_err(|_| Fault::new("IO_ERROR", "process watcher stopped"))?;
        }
        Ok(json!({"status": status.borrow().clone()}))
    }

    pub async fn shutdown(&self) {
        let entries: Vec<_> = self.entries.lock().unwrap().values().cloned().collect();
        for entry in &entries {
            if let Some(sender) = entry.stop.lock().unwrap().take() {
                let _ = sender.send(());
            }
        }
        for entry in entries {
            let mut status = entry.status.clone();
            while status.borrow().running {
                if status.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn drain_into<R: AsyncRead + Unpin>(
    mut reader: R,
    buffer: Arc<Mutex<Buffer>>,
    cap: usize,
) -> io::Result<()> {
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        buffer.lock().unwrap().append(&chunk[..count], cap);
    }
    Ok(())
}
