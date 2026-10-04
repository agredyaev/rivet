//! Lifecycle and bounded output capture for managed child processes.
//!
//! The registry retains bounded process state. Execution capacity tracks only
//! live processes and keeps foreground capacity available for short MCP calls.
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Foreground,
    Background,
}

struct EntryRecord {
    process: Arc<ProcessEntry>,
    slot: Option<SlotKind>,
}

struct TableState {
    entries: HashMap<ProcessId, EntryRecord>,
    next: u64,
    running_total: usize,
    running_background: usize,
}

enum InputMode {
    Closed,
    Piped,
}

enum Admission {
    Foreground,
    Background,
}

enum Promotion {
    Promoted,
    Completed,
    Full,
}

struct SpawnedProcess {
    id: ProcessId,
    entry: Arc<ProcessEntry>,
    timeout_ms: u64,
}

pub struct ProcessTable {
    state: Arc<Mutex<TableState>>,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TableState {
                entries: HashMap::new(),
                next: 1,
                running_total: 0,
                running_background: 0,
            })),
        }
    }

    fn background_limit(config: &Config) -> usize {
        config
            .limits
            .max_running_processes
            .saturating_sub(config.limits.foreground_process_reserve)
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

    fn release_slot_locked(state: &mut TableState, id: ProcessId) {
        let slot = state.entries.get(&id).and_then(|record| record.slot);
        match slot {
            Some(SlotKind::Foreground) => {
                state.running_total = state.running_total.saturating_sub(1);
            }
            Some(SlotKind::Background) => {
                state.running_total = state.running_total.saturating_sub(1);
                state.running_background = state.running_background.saturating_sub(1);
            }
            None => return,
        }
        if let Some(record) = state.entries.get_mut(&id) {
            record.slot = None;
        }
    }

    fn remove_entry_locked(state: &mut TableState, id: ProcessId) {
        Self::release_slot_locked(state, id);
        state.entries.remove(&id);
    }

    fn insert_entry(
        &self,
        config: &Config,
        command: String,
        stop_tx: oneshot::Sender<()>,
        status_rx: watch::Receiver<Status>,
        admission: Admission,
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
        if state.running_total >= config.limits.max_running_processes {
            return Err(Fault::new("PROCESS_LIMIT", "process capacity is full"));
        }
        if matches!(admission, Admission::Background)
            && state.running_background >= Self::background_limit(config)
        {
            return Err(Fault::new(
                "PROCESS_LIMIT",
                "background process capacity is full",
            ));
        }

        if state.entries.len() >= config.limits.max_running_processes
            && let Some(oldest) = state
                .entries
                .iter()
                .filter(|(_, record)| record.slot.is_none())
                .min_by_key(|(_, record)| record.process.started)
                .map(|(id, _)| *id)
        {
            state.entries.remove(&oldest);
        }

        let id = ProcessId(state.next);
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process IDs exhausted"))?;

        let slot = match admission {
            Admission::Foreground => SlotKind::Foreground,
            Admission::Background => SlotKind::Background,
        };
        state.running_total += 1;
        if slot == SlotKind::Background {
            state.running_background += 1;
        }
        state.entries.insert(
            id,
            EntryRecord {
                process: entry.clone(),
                slot: Some(slot),
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
        admission: Admission,
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
        let (id, entry) =
            self.insert_entry(&config, command_name, stop_tx, status_rx, admission)?;

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
            status_tx.send_replace(status);

            let mut state = state.lock().unwrap();
            ProcessTable::release_slot_locked(&mut state, id);
        });

        Ok(SpawnedProcess {
            id,
            entry,
            timeout_ms,
        })
    }

    fn promote_to_background(&self, id: ProcessId, config: &Config) -> Result<Promotion, Fault> {
        let mut state = self.state.lock().unwrap();
        let record = state.entries.get(&id).ok_or_else(|| {
            Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
        })?;
        let slot = record.slot;
        let running = record.process.status.borrow().running;

        if !running || slot.is_none() {
            Self::release_slot_locked(&mut state, id);
            return Ok(Promotion::Completed);
        }
        if slot == Some(SlotKind::Background) {
            return Ok(Promotion::Promoted);
        }
        if state.running_background >= Self::background_limit(config) {
            return Ok(Promotion::Full);
        }

        state.running_background += 1;
        state
            .entries
            .get_mut(&id)
            .expect("process entry exists")
            .slot = Some(SlotKind::Background);
        Ok(Promotion::Promoted)
    }

    fn request_stop(&self, id: ProcessId) {
        let entry = self
            .state
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|record| record.process.clone());
        if let Some(entry) = entry
            && let Some(sender) = entry.stop.lock().unwrap().take()
        {
            let _ = sender.send(());
        }
    }

    fn remove_completed(&self, id: ProcessId) {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .get(&id)
            .is_some_and(|record| record.slot.is_none())
        {
            state.entries.remove(&id);
        }
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

    fn completed_run_value(
        &self,
        spawned: &SpawnedProcess,
        config: &Config,
    ) -> Result<serde_json::Value, Fault> {
        let final_status = spawned.entry.status.borrow().clone();
        let value = Self::run_value(&spawned.entry, spawned.id, "completed", false, config)?;
        self.remove_completed(spawned.id);

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
            Admission::Foreground,
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

        if !status.borrow().running {
            return self.completed_run_value(&spawned, &config);
        }

        match self.promote_to_background(spawned.id, &config)? {
            Promotion::Promoted => {
                Self::run_value(&spawned.entry, spawned.id, "running", true, &config)
            }
            Promotion::Completed => self.completed_run_value(&spawned, &config),
            Promotion::Full => {
                if !spawned.entry.status.borrow().running {
                    return self.completed_run_value(&spawned, &config);
                }
                self.request_stop(spawned.id);
                Err(Fault::with(
                    "PROCESS_LIMIT",
                    "background process capacity is full",
                    json!({"foreground_wait_ms": foreground_wait_ms}),
                ))
            }
        }
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
            Admission::Background,
        )?;
        Ok(json!({"process_id": spawned.id.0}))
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
                })
            })
            .collect();
        rows.sort_by_key(|row| row["process_id"].as_u64().unwrap_or(0));
        json!({
            "processes": rows,
            "running_total": state.running_total,
            "running_background": state.running_background,
        })
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
