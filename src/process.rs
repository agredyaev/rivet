//! Lifecycle and bounded output capture for managed child processes.
//!
//! A bounded registry owns active and retained processes. Supervisors move
//! completed process IDs into a completion queue when the OS reports exit.
use crate::{
    commands::{CommandRequest, admitted, terminate},
    config::Config,
    mcp::Fault,
};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
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

struct EntryRecord {
    process: Arc<ProcessEntry>,
    active: bool,
    completion_sequence: Option<u64>,
}

struct TableState {
    entries: HashMap<ProcessId, EntryRecord>,
    ready: VecDeque<ProcessId>,
    next_process: u64,
    next_completion: u64,
    running: usize,
}

enum InputMode {
    Closed,
    Piped,
}

pub struct ProcessTable {
    state: Arc<Mutex<TableState>>,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TableState {
                entries: HashMap::new(),
                ready: VecDeque::new(),
                next_process: 1,
                next_completion: 1,
                running: 0,
            })),
        }
    }

    fn get(&self, id: u64) -> Result<Arc<ProcessEntry>, Fault> {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(&ProcessId(id))
            .map(|record| record.process.clone())
            .ok_or_else(|| Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted"))
    }

    fn remove_ready_locked(state: &mut TableState, id: ProcessId) {
        state.ready.retain(|queued| *queued != id);
    }

    fn release_active_locked(state: &mut TableState, id: ProcessId) {
        let active = state.entries.get(&id).is_some_and(|record| record.active);
        if !active {
            return;
        }
        state.running = state.running.saturating_sub(1);
        if let Some(record) = state.entries.get_mut(&id) {
            record.active = false;
        }
    }

    fn remove_entry_locked(state: &mut TableState, id: ProcessId) {
        Self::release_active_locked(state, id);
        Self::remove_ready_locked(state, id);
        state.entries.remove(&id);
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

        let mut state = self.state.lock().unwrap();
        if state.running >= config.limits.max_running_processes {
            return Err(Fault::new("PROCESS_LIMIT", "process capacity is full"));
        }

        if state.entries.len() >= config.limits.max_running_processes
            && let Some(oldest) = state.ready.front().copied()
        {
            Self::remove_entry_locked(&mut state, oldest);
        }
        if state.entries.len() >= config.limits.max_running_processes {
            return Err(Fault::new("PROCESS_LIMIT", "process registry is full"));
        }

        let id = ProcessId(state.next_process);
        state.next_process = state
            .next_process
            .checked_add(1)
            .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process IDs exhausted"))?;
        state.running += 1;
        state.entries.insert(
            id,
            EntryRecord {
                process: entry.clone(),
                active: true,
                completion_sequence: None,
            },
        );
        Ok((id, entry))
    }

    fn spawn(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
        default_timeout_ms: u64,
        input_mode: InputMode,
    ) -> Result<ProcessId, Fault> {
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
                let mut state = self.state.lock().unwrap();
                Self::remove_entry_locked(&mut state, id);
                return Err(Fault::with(
                    "SPAWN_ERROR",
                    "failed to spawn process",
                    json!({"error": error.to_string()}),
                ));
            }
        };
        let Some(pid) = child.id() else {
            let _ = child.start_kill();
            let mut state = self.state.lock().unwrap();
            Self::remove_entry_locked(&mut state, id);
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
        let state = self.state.clone();
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

            let mut state = state.lock().unwrap();
            status_tx.send_replace(status);
            ProcessTable::release_active_locked(&mut state, id);
            if state.entries.contains_key(&id) {
                let sequence = state.next_completion;
                state.next_completion = state.next_completion.saturating_add(1);
                if let Some(record) = state.entries.get_mut(&id) {
                    record.completion_sequence = Some(sequence);
                }
                state.ready.push_back(id);
            }
        });

        Ok(id)
    }

    pub fn run(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let id = self.spawn(
            config.clone(),
            request,
            config.limits.default_timeout_ms,
            InputMode::Closed,
        )?;
        Ok(json!({"state": "submitted", "process_id": id.0}))
    }

    pub fn start(
        &self,
        config: Arc<Config>,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let id = self.spawn(
            config.clone(),
            request,
            config.limits.max_timeout_ms,
            InputMode::Piped,
        )?;
        Ok(json!({"process_id": id.0}))
    }

    pub fn list(&self) -> serde_json::Value {
        let state = self.state.lock().unwrap();
        let mut rows: Vec<_> = state
            .entries
            .iter()
            .map(|(id, record)| {
                let status = record.process.status.borrow().clone();
                let duration_ms = status
                    .duration_ms
                    .unwrap_or_else(|| record.process.started.elapsed().as_millis() as u64);
                json!({
                    "process_id": id.0,
                    "command": record.process.command,
                    "status": status,
                    "duration_ms": duration_ms,
                    "completion_sequence": record.completion_sequence,
                })
            })
            .collect();
        rows.sort_by_key(|row| row["process_id"].as_u64().unwrap_or(0));
        json!({
            "processes": rows,
            "running": state.running,
            "ready": state.ready.len(),
        })
    }

    pub fn ready(
        &self,
        after_sequence: u64,
        limit: usize,
        output_limit: usize,
        max_results: usize,
        max_output: usize,
    ) -> Result<serde_json::Value, Fault> {
        if limit == 0 || limit > max_results {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "ready result limit is outside configured bounds",
            ));
        }
        if output_limit > max_output {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "ready output_limit is outside configured bounds",
            ));
        }

        let state = self.state.lock().unwrap();
        let oldest_available_sequence = state.ready.front().and_then(|id| {
            state
                .entries
                .get(id)
                .and_then(|record| record.completion_sequence)
        });
        let latest_sequence = state.next_completion.saturating_sub(1);
        let gap = match oldest_available_sequence {
            Some(oldest) => after_sequence.saturating_add(1) < oldest,
            None => after_sequence < latest_sequence,
        };

        let mut rows = Vec::new();
        let mut next_after_sequence = after_sequence;
        for id in &state.ready {
            if rows.len() >= limit {
                break;
            }
            let Some(record) = state.entries.get(id) else {
                continue;
            };
            let Some(sequence) = record.completion_sequence else {
                continue;
            };
            if sequence <= after_sequence {
                continue;
            }
            let stdout = record.process.stdout.lock().unwrap();
            let stderr = record.process.stderr.lock().unwrap();
            let stdout_slice = (output_limit > 0)
                .then(|| stdout.slice(0, output_limit))
                .transpose()?;
            let stderr_slice = (output_limit > 0)
                .then(|| stderr.slice(0, output_limit))
                .transpose()?;
            let status = record.process.status.borrow().clone();
            rows.push(json!({
                "completion_sequence": sequence,
                "process_id": id.0,
                "command": record.process.command,
                "status": status,
                "stdout": stdout_slice.as_ref().map(|slice| &slice.text),
                "stderr": stderr_slice.as_ref().map(|slice| &slice.text),
                "stdout_bytes": stdout.total,
                "stderr_bytes": stderr.total,
                "stdout_next_offset": stdout_slice.as_ref().map(|slice| slice.next_offset),
                "stderr_next_offset": stderr_slice.as_ref().map(|slice| slice.next_offset),
                "truncated": stdout.truncated || stderr.truncated,
            }));
            next_after_sequence = sequence;
        }

        Ok(json!({
            "processes": rows,
            "next_after_sequence": next_after_sequence,
            "oldest_available_sequence": oldest_available_sequence,
            "latest_sequence": latest_sequence,
            "gap": gap,
        }))
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
        let entries: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .entries
            .values()
            .filter(|record| record.active)
            .map(|record| record.process.clone())
            .collect();
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
