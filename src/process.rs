//! Fixed-slot managed child-process runtime.
//!
//! The registry is intentionally data-oriented: a fixed 32-slot retention ring stores
//! process metadata in SoA arrays while at most 10 children may run concurrently. Process state is derived from id + result.
//! Captured output buffers are allocated once at startup and reused by slot.

use crate::{
    commands::{CommandRequest, admitted, terminate, terminate_group},
    config::{CommandId, Config, PROCESS_RETENTION_SLOTS},
    mcp::Fault,
    profile,
};
use rivet::output_buffer::{Buffer, OutputSlice, SliceError};
use serde::Serialize;
use serde_json::json;
use std::{
    io,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdin,
    sync::{Mutex as AsyncMutex, watch},
    task::JoinSet,
    time::{Duration, Instant as TokioInstant},
};

const PROCESS_SLOTS: usize = PROCESS_RETENTION_SLOTS;
const STREAMS_PER_PROCESS: usize = 2;
const OUTPUT_STREAMS: usize = PROCESS_SLOTS * STREAMS_PER_PROCESS;
const RESULT_RUNNING: i32 = -1;
const RESULT_STOPPING: i32 = -2;
const RESULT_STOPPED: i32 = -3;
const RESULT_TIMED_OUT: i32 = -4;
const RESULT_NO_CODE: i32 = -5;

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

struct OutputStore {
    streams: [Mutex<Buffer>; OUTPUT_STREAMS],
    stdout_cap: usize,
    stderr_cap: usize,
}

impl OutputStore {
    fn new(stdout_cap: usize, stderr_cap: usize) -> Self {
        Self {
            streams: std::array::from_fn(|index| {
                let cap = if index % STREAMS_PER_PROCESS == 0 {
                    stdout_cap
                } else {
                    stderr_cap
                };
                Mutex::new(Buffer::with_capacity(cap))
            }),
            stdout_cap,
            stderr_cap,
        }
    }

    #[inline]
    fn index(slot: usize, stderr: bool) -> usize {
        slot * STREAMS_PER_PROCESS + usize::from(stderr)
    }

    fn reset(&self, slot: usize) {
        self.streams[Self::index(slot, false)]
            .lock()
            .unwrap()
            .reset();
        self.streams[Self::index(slot, true)]
            .lock()
            .unwrap()
            .reset();
    }

    fn append(&self, slot: usize, stderr: bool, chunk: &[u8]) {
        let cap = if stderr {
            self.stderr_cap
        } else {
            self.stdout_cap
        };
        self.streams[Self::index(slot, stderr)]
            .lock()
            .unwrap()
            .append(chunk, cap);
    }

    fn slice(
        &self,
        slot: usize,
        stderr: bool,
        offset: u64,
        limit: usize,
    ) -> Result<OutputSlice, Fault> {
        let cap = if stderr {
            self.stderr_cap
        } else {
            self.stdout_cap
        };
        self.streams[Self::index(slot, stderr)]
            .lock()
            .unwrap()
            .slice(offset, limit, cap)
            .map_err(|SliceError::OffsetBeyondTotal| {
                Fault::new("INVALID_REQUEST", "output offset is beyond received bytes")
            })
    }

    fn totals(&self, slot: usize) -> (u64, u64, bool) {
        let stdout = self.streams[Self::index(slot, false)].lock().unwrap();
        let stderr = self.streams[Self::index(slot, true)].lock().unwrap();
        (
            stdout.total(),
            stderr.total(),
            stdout.total() > self.stdout_cap as u64 || stderr.total() > self.stderr_cap as u64,
        )
    }

    fn profile_totals(&self, slot: usize) -> (u64, u64, usize, usize, bool, bool) {
        let stdout = self.streams[Self::index(slot, false)].lock().unwrap();
        let stderr = self.streams[Self::index(slot, true)].lock().unwrap();
        (
            stdout.total(),
            stderr.total(),
            stdout.len(),
            stderr.len(),
            stdout.total() > self.stdout_cap as u64,
            stderr.total() > self.stderr_cap as u64,
        )
    }
}

struct Core {
    // id == 0 => free.
    // RESULT_RUNNING / RESULT_STOPPING => running.
    // Any other result => completed.
    id: [u64; PROCESS_SLOTS],
    command: [CommandId; PROCESS_SLOTS],
    pgid: [u32; PROCESS_SLOTS],

    // Running => monotonic start ns relative to ProcessTable::epoch.
    // Completed => final elapsed ns.
    time_ns: [u64; PROCESS_SLOTS],

    result: [i32; PROCESS_SLOTS],

    next_id: u64,
}

impl Core {
    fn new() -> Self {
        Self {
            id: [0; PROCESS_SLOTS],
            command: [CommandId::ZERO; PROCESS_SLOTS],
            pgid: [0; PROCESS_SLOTS],
            time_ns: [0; PROCESS_SLOTS],
            result: [RESULT_RUNNING; PROCESS_SLOTS],
            next_id: 1,
        }
    }

    #[inline]
    fn is_running(&self, slot: usize) -> bool {
        self.id[slot] != 0 && matches!(self.result[slot], RESULT_RUNNING | RESULT_STOPPING)
    }

    #[inline]
    fn is_completed(&self, slot: usize) -> bool {
        self.id[slot] != 0 && !matches!(self.result[slot], RESULT_RUNNING | RESULT_STOPPING)
    }

    fn slot(&self, id: u64) -> Option<usize> {
        self.id.iter().position(|candidate| *candidate == id)
    }

    fn running_count(&self) -> usize {
        (0..PROCESS_SLOTS)
            .filter(|slot| self.is_running(*slot))
            .count()
    }

    fn oldest_completed_slot(&self) -> Option<usize> {
        (0..PROCESS_SLOTS)
            .filter(|slot| self.is_completed(*slot))
            .min_by_key(|slot| self.id[*slot])
    }

    fn clear_slot(&mut self, slot: usize) {
        self.id[slot] = 0;
    }
}

enum InputMode {
    Closed,
    Piped,
}

pub struct ProcessTable {
    core: Mutex<Core>,
    output: OutputStore,
    stdin: [AsyncMutex<Option<ChildStdin>>; PROCESS_SLOTS],
    epoch: Instant,
    state_event: watch::Sender<()>,
}

impl ProcessTable {
    pub fn new(config: &Config) -> Self {
        let (state_event, _) = watch::channel(());
        Self {
            core: Mutex::new(Core::new()),
            output: OutputStore::new(
                config.limits.max_stdout_bytes,
                config.limits.max_stderr_bytes,
            ),
            stdin: std::array::from_fn(|_| AsyncMutex::new(None)),
            epoch: Instant::now(),
            state_event,
        }
    }

    #[cfg(test)]
    fn test_new() -> Self {
        let (state_event, _) = watch::channel(());
        Self {
            core: Mutex::new(Core::new()),
            output: OutputStore::new(4096, 4096),
            stdin: std::array::from_fn(|_| AsyncMutex::new(None)),
            epoch: Instant::now(),
            state_event,
        }
    }

    #[inline]
    fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }

    fn status_locked(core: &Core, slot: usize) -> Status {
        let completed = core.is_completed(slot);
        Status {
            running: core.is_running(slot),
            exit_code: (completed && core.result[slot] >= 0).then_some(core.result[slot]),
            timed_out: core.result[slot] == RESULT_TIMED_OUT,
            stopped: core.result[slot] == RESULT_STOPPED,
            duration_ms: completed.then_some(core.time_ns[slot] / 1_000_000),
        }
    }

    fn reserve_slot(
        &self,
        max_running: usize,
        command: CommandId,
    ) -> Result<(ProcessId, usize), Fault> {
        let mut core = self.core.lock().unwrap();
        if core.running_count() >= max_running {
            return Err(Fault::new("PROCESS_LIMIT", "process capacity is full"));
        }

        let slot = core
            .id
            .iter()
            .position(|id| *id == 0)
            .or_else(|| core.oldest_completed_slot())
            .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process registry is full"))?;

        self.output.reset(slot);

        let id = core.next_id;
        core.next_id = core
            .next_id
            .checked_add(1)
            .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process IDs exhausted"))?;

        core.id[slot] = id;
        core.command[slot] = command;
        core.pgid[slot] = 0;
        core.time_ns[slot] = self.now_ns();
        core.result[slot] = RESULT_RUNNING;

        Ok((ProcessId(id), slot))
    }

    fn release_reserved(&self, id: ProcessId, slot: usize) {
        let released = {
            let mut core = self.core.lock().unwrap();
            if core.id[slot] == id.0 && core.is_running(slot) {
                core.clear_slot(slot);
                self.output.reset(slot);
                true
            } else {
                false
            }
        };
        if released {
            self.state_event.send_replace(());
        }
    }

    fn set_pgid(&self, id: ProcessId, slot: usize, pgid: u32) {
        let changed = {
            let mut core = self.core.lock().unwrap();
            if core.id[slot] == id.0 && core.is_running(slot) {
                core.pgid[slot] = pgid;
                true
            } else {
                false
            }
        };
        if changed {
            self.state_event.send_replace(());
        }
    }

    fn result_for(&self, id: ProcessId, slot: usize) -> i32 {
        let core = self.core.lock().unwrap();
        if core.id[slot] == id.0 {
            core.result[slot]
        } else {
            RESULT_RUNNING
        }
    }

    fn complete(&self, id: ProcessId, slot: usize, mut result: i32) -> Option<Status> {
        let now_ns = self.now_ns();
        let mut core = self.core.lock().unwrap();
        if core.id[slot] != id.0 || core.is_completed(slot) {
            return None;
        }

        if core.result[slot] == RESULT_STOPPING && result != RESULT_TIMED_OUT {
            result = RESULT_STOPPED;
        }

        core.time_ns[slot] = now_ns.saturating_sub(core.time_ns[slot]);
        core.result[slot] = result;
        Some(Self::status_locked(&core, slot))
    }

    async fn spawn(
        self: &Arc<Self>,
        config: &Config,
        request: CommandRequest,
        default_timeout_ms: u64,
        input_mode: InputMode,
    ) -> Result<ProcessId, Fault> {
        let profiling = profile::enabled();
        let profile_command = profiling.then(|| request.command.clone());
        let rss_before_bytes = profile::self_current_rss_bytes();

        let admitted_started = profiling.then(Instant::now);
        let (mut command, timeout_ms, command_id) = admitted(config, &request, default_timeout_ms)?;
        let admitted_us = admitted_started
            .as_ref()
            .map(|started| started.elapsed().as_micros());
        if matches!(input_mode, InputMode::Closed) {
            command.stdin(Stdio::null());
        }

        let registry_started = profiling.then(Instant::now);
        let (id, slot) = self.reserve_slot(config.limits.max_running_processes, command_id)?;
        let registry_us = registry_started
            .as_ref()
            .map(|started| started.elapsed().as_micros());

        let spawn_started = profiling.then(Instant::now);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.release_reserved(id, slot);
                return Err(Fault::with(
                    "SPAWN_ERROR",
                    "failed to spawn process",
                    json!({"error": error.to_string()}),
                ));
            }
        };
        let spawn_us = spawn_started
            .as_ref()
            .map(|started| started.elapsed().as_micros());
        let spawn_finished_at = profiling.then(Instant::now);

        let Some(pid) = child.id() else {
            let _ = child.start_kill();
            self.release_reserved(id, slot);
            return Err(Fault::new("SPAWN_ERROR", "spawned child has no process ID"));
        };
        self.set_pgid(id, slot, pid);

        profile::event(|| {
            json!({
                "event": "process_spawn",
                "process_id": id.0,
                "command": profile_command.as_deref(),
                "admitted_us": admitted_us,
                "registry_us": registry_us,
                "spawn_us": spawn_us,
                "self_rss_before_bytes": rss_before_bytes,
                "self_rss_after_spawn_bytes": profile::self_current_rss_bytes(),
                "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                "children_peak_rss_bytes": profile::children_peak_rss_bytes()
            })
        });

        if matches!(input_mode, InputMode::Piped) {
            *self.stdin[slot].lock().await = child.stdin.take();
            self.state_event.send_replace(());
        } else {
            let _ = child.stdin.take();
        }

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let table = Arc::clone(self);

        tokio::spawn(async move {
            let supervisor_started = profiling.then(Instant::now);
            let supervisor_start_us = spawn_finished_at
                .as_ref()
                .map(|started| started.elapsed().as_micros());
            let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);

            let output_table = Arc::clone(&table);
            let output_started_at = spawn_finished_at;
            let mut outputs = tokio::spawn(async move {
                let (stdout_result, stderr_result) = tokio::join!(
                    drain_into(stdout, &output_table, slot, false, output_started_at,),
                    drain_into(stderr, &output_table, slot, true, output_started_at,)
                );
                (stdout_result.ok().flatten(), stderr_result.ok().flatten())
            });

            let first_event_us;
            let first_event_kind;
            let mut terminate_us = 0_u128;
            let mut final_result;

            tokio::select! {
                status = child.wait() => {
                    first_event_us = supervisor_started
                        .as_ref()
                        .map(|started| started.elapsed().as_micros());
                    if table.result_for(id, slot) == RESULT_STOPPING {
                        first_event_kind = "stop";
                        final_result = RESULT_STOPPED;
                    } else {
                        first_event_kind = "exit";
                        final_result = status
                            .ok()
                            .and_then(|status| status.code())
                            .unwrap_or(RESULT_NO_CODE);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    first_event_us = supervisor_started
                        .as_ref()
                        .map(|started| started.elapsed().as_micros());
                    first_event_kind = "timeout";
                    final_result = RESULT_TIMED_OUT;
                    let terminate_started = profiling.then(Instant::now);
                    let _ = terminate(&mut child, pid).await;
                    terminate_us = terminate_started
                        .as_ref()
                        .map(|started| started.elapsed().as_micros())
                        .unwrap_or(0);
                }
            }

            let output_join_started = profiling.then(Instant::now);
            let (stdout_first_read_us, stderr_first_read_us) = if final_result == RESULT_TIMED_OUT {
                outputs.await.unwrap_or((None, None))
            } else {
                tokio::select! {
                    result = &mut outputs => result.unwrap_or((None, None)),
                    _ = tokio::time::sleep_until(deadline) => {
                        final_result = RESULT_TIMED_OUT;
                        let terminate_started = profiling.then(Instant::now);
                        let _ = terminate_group(pid).await;
                        terminate_us = terminate_us.saturating_add(
                            terminate_started
                                .as_ref()
                                .map(|started| started.elapsed().as_micros())
                                .unwrap_or(0)
                        );
                        outputs.await.unwrap_or((None, None))
                    }
                }
            };
            let output_join_us = output_join_started
                .as_ref()
                .map(|started| started.elapsed().as_micros());

            *table.stdin[slot].lock().await = None;

            let total_us = supervisor_started
                .as_ref()
                .map(|started| started.elapsed().as_micros());
            let spawn_to_complete_us = spawn_finished_at
                .as_ref()
                .map(|started| started.elapsed().as_micros());

            let Some(profile_status) = table.complete(id, slot, final_result) else {
                return;
            };
            table.state_event.send_replace(());

            profile::event(|| {
                let (
                    stdout_total,
                    stderr_total,
                    stdout_retained,
                    stderr_retained,
                    stdout_truncated,
                    stderr_truncated,
                ) = table.output.profile_totals(slot);
                json!({
                "event": "process_complete",
                "process_id": id.0,
                "command": profile_command.as_deref(),
                "supervisor_start_us": supervisor_start_us,
                "first_event_kind": first_event_kind,
                "first_event_us": first_event_us,
                "spawn_to_first_event_us": first_event_us.zip(supervisor_start_us)
                    .map(|(event, start)| event.saturating_add(start)),
                "stdout_first_read_us": stdout_first_read_us,
                "stderr_first_read_us": stderr_first_read_us,
                "terminate_us": terminate_us,
                "output_join_us": output_join_us,
                "spawn_to_complete_us": spawn_to_complete_us,
                "total_us": total_us,
                "stdout_total_bytes": stdout_total,
                "stderr_total_bytes": stderr_total,
                "stdout_retained_bytes": stdout_retained,
                "stderr_retained_bytes": stderr_retained,
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
                "status": profile_status,
                "self_rss_before_bytes": rss_before_bytes,
                "self_rss_after_bytes": profile::self_current_rss_bytes(),
                "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                "children_peak_rss_bytes": profile::children_peak_rss_bytes()
                })
            });
        });

        Ok(id)
    }

    pub async fn submit(
        self: &Arc<Self>,
        config: &Config,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let id = self
            .spawn(
                config,
                request,
                config.limits.default_timeout_ms,
                InputMode::Closed,
            )
            .await?;
        Ok(json!({"process_id": id.0}))
    }

    pub async fn start(
        self: &Arc<Self>,
        config: &Config,
        request: CommandRequest,
    ) -> Result<serde_json::Value, Fault> {
        let id = self
            .spawn(
                config,
                request,
                config.limits.max_timeout_ms,
                InputMode::Piped,
            )
            .await?;
        Ok(json!({"process_id": id.0}))
    }

    pub fn list(&self, config: &Config) -> serde_json::Value {
        let core = self.core.lock().unwrap();
        let now_ns = self.now_ns();
        let mut slots = [0_usize; PROCESS_SLOTS];
        let mut count = 0;
        for slot in 0..PROCESS_SLOTS {
            if core.id[slot] != 0 {
                slots[count] = slot;
                count += 1;
            }
        }
        slots[..count].sort_unstable_by_key(|slot| core.id[*slot]);

        let mut running = Vec::with_capacity(count);
        let mut completed = Vec::with_capacity(count);

        for &slot in &slots[..count] {
            let (stdout_bytes, stderr_bytes, truncated) = self.output.totals(slot);
            if core.is_running(slot) {
                running.push(json!({
                    "process_id": core.id[slot],
                    "command": config.command_name(core.command[slot]),
                    "duration_ms": now_ns.saturating_sub(core.time_ns[slot]) / 1_000_000,
                    "stdout_bytes": stdout_bytes,
                    "stderr_bytes": stderr_bytes,
                    "truncated": truncated,
                }));
            } else {
                completed.push(json!({
                    "process_id": core.id[slot],
                    "command": config.command_name(core.command[slot]),
                    "duration_ms": core.time_ns[slot] / 1_000_000,
                    "exit_code": (core.result[slot] >= 0).then_some(core.result[slot]),
                    "stopped": core.result[slot] == RESULT_STOPPED,
                    "timed_out": core.result[slot] == RESULT_TIMED_OUT,
                    "stdout_bytes": stdout_bytes,
                    "stderr_bytes": stderr_bytes,
                    "truncated": truncated,
                }));
            }
        }

        json!({
            "running": running,
            "completed": completed,
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

        let core = self.core.lock().unwrap();
        let slot = core.slot(id).ok_or_else(|| {
            Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
        })?;
        let stdout = self.output.slice(slot, false, stdout_offset, limit)?;
        let stderr = self.output.slice(slot, true, stderr_offset, limit)?;
        let state = if core.is_running(slot) {
            "running"
        } else {
            "completed"
        };

        Ok(json!({
            "state": state,
            "stdout": stdout,
            "stderr": stderr,
        }))
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

        let mut state = self.state_event.subscribe();
        loop {
            let slot = {
                let core = self.core.lock().unwrap();
                core.slot(id).ok_or_else(|| {
                    Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
                })?
            };

            let mut stdin = self.stdin[slot].lock().await;
            {
                let core = self.core.lock().unwrap();
                if core.id[slot] != id || !core.is_running(slot) {
                    return Err(Fault::new(
                        "PROCESS_NOT_FOUND",
                        "process ID is unknown, completed, or was evicted",
                    ));
                }
            }

            if let Some(pipe) = stdin.as_mut() {
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
                return Ok(json!({"bytes_written": text.len()}));
            }

            drop(stdin);
            if state.changed().await.is_err() {
                return Err(Fault::new("IO_ERROR", "process state watcher stopped"));
            }
        }
    }

    pub async fn stop(&self, id: u64) -> Result<serde_json::Value, Fault> {
        let mut state = self.state_event.subscribe();

        let pgid = loop {
            let pgid = {
                let mut core = self.core.lock().unwrap();
                let slot = core.slot(id).ok_or_else(|| {
                    Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
                })?;
                if core.is_completed(slot) {
                    return Ok(json!({"status": Self::status_locked(&core, slot)}));
                }
                core.result[slot] = RESULT_STOPPING;
                core.pgid[slot]
            };
            if pgid != 0 {
                break pgid;
            }
            if state.changed().await.is_err() {
                return Err(Fault::new("IO_ERROR", "process state watcher stopped"));
            }
        };

        terminate_group(pgid).await.map_err(|error| {
            Fault::with(
                "IO_ERROR",
                "failed to terminate process group",
                json!({"error": error.to_string()}),
            )
        })?;

        loop {
            let status = {
                let core = self.core.lock().unwrap();
                let slot = core.slot(id).ok_or_else(|| {
                    Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
                })?;
                core.is_completed(slot)
                    .then(|| Self::status_locked(&core, slot))
            };
            if let Some(status) = status {
                return Ok(json!({"status": status}));
            }
            if state.changed().await.is_err() {
                return Err(Fault::new("IO_ERROR", "process state watcher stopped"));
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut state = self.state_event.subscribe();
        let (pgids, count) = loop {
            let mut pgids = [0_u32; PROCESS_SLOTS];
            let mut count = 0_usize;
            let mut waiting_for_pgid = false;
            {
                let mut core = self.core.lock().unwrap();
                for slot in 0..PROCESS_SLOTS {
                    if !core.is_running(slot) {
                        continue;
                    }
                    core.result[slot] = RESULT_STOPPING;
                    let pgid = core.pgid[slot];
                    if pgid == 0 {
                        waiting_for_pgid = true;
                    } else {
                        pgids[count] = pgid;
                        count += 1;
                    }
                }
            }
            if !waiting_for_pgid {
                break (pgids, count);
            }
            if state.changed().await.is_err() {
                return;
            }
        };

        let mut tasks = JoinSet::new();
        for pgid in pgids.into_iter().take(count) {
            tasks.spawn(async move {
                let _ = terminate_group(pgid).await;
            });
        }
        while tasks.join_next().await.is_some() {}

        loop {
            if self.core.lock().unwrap().running_count() == 0 {
                break;
            }
            if state.changed().await.is_err() {
                break;
            }
        }
    }
}

async fn drain_into<R: AsyncRead + Unpin>(
    mut reader: R,
    table: &ProcessTable,
    slot: usize,
    stderr: bool,
    spawn_finished_at: Option<Instant>,
) -> io::Result<Option<u64>> {
    let mut chunk = [0_u8; 8192];
    let mut first_read_us = None;
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }

        if first_read_us.is_none()
            && let Some(spawn_finished_at) = spawn_finished_at
        {
            first_read_us = Some(
                spawn_finished_at
                    .elapsed()
                    .as_micros()
                    .min(u64::MAX as u128) as u64,
            );
        }
        table.output.append(slot, stderr, &chunk[..count]);
    }
    Ok(first_read_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_state_is_derived_from_id_and_result() {
        let mut core = Core::new();

        core.id[3] = 7;
        assert!(core.is_running(3));
        assert!(!core.is_completed(3));
        assert_eq!(core.running_count(), 1);

        core.result[3] = 0;
        assert!(!core.is_running(3));
        assert!(core.is_completed(3));
        assert_eq!(core.running_count(), 0);

        core.clear_slot(3);
        assert_eq!(core.id[3], 0);
        assert!(!core.is_running(3));
        assert!(!core.is_completed(3));
    }

    #[test]
    fn oldest_completed_slot_uses_process_id_order() {
        let mut core = Core::new();

        core.id[2] = 12;
        core.result[2] = 0;
        core.id[5] = 9;
        core.result[5] = RESULT_STOPPED;
        core.id[7] = 15;
        core.result[7] = RESULT_RUNNING;

        assert_eq!(core.oldest_completed_slot(), Some(5));
    }

    #[test]
    fn output_buffers_reuse_preallocated_capacity() {
        let output = OutputStore::new(1024, 1024);
        output.append(2, false, b"abc");

        let before = output.streams[OutputStore::index(2, false)]
            .lock()
            .unwrap()
            .capacity();
        output.reset(2);
        let after = output.streams[OutputStore::index(2, false)]
            .lock()
            .unwrap()
            .capacity();

        assert_eq!(before, 1024);
        assert_eq!(after, 1024);
    }

    #[test]
    fn pgid_and_reservation_rollback_publish_state_changes() {
        let table = ProcessTable::test_new();
        {
            let mut core = table.core.lock().unwrap();
            core.id[0] = 1;
            core.result[0] = RESULT_RUNNING;
        }

        let pgid_change = table.state_event.subscribe();
        table.set_pgid(ProcessId(1), 0, 42);
        assert!(pgid_change.has_changed().unwrap());
        assert_eq!(table.core.lock().unwrap().pgid[0], 42);

        let rollback = table.state_event.subscribe();
        table.release_reserved(ProcessId(1), 0);
        assert!(rollback.has_changed().unwrap());
        assert_eq!(table.core.lock().unwrap().id[0], 0);
    }

    #[test]
    fn completion_has_no_sequence_or_ready_queue_state() {
        let table = ProcessTable::test_new();
        {
            let mut core = table.core.lock().unwrap();
            core.id[0] = 1;
            core.time_ns[0] = table.now_ns();
        }

        let status = table.complete(ProcessId(1), 0, 0).unwrap();

        assert!(!status.running);
        assert_eq!(status.exit_code, Some(0));
        assert!(table.core.lock().unwrap().is_completed(0));
    }
}
