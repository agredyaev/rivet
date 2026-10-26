//! Fixed-slot managed child-process runtime.
//!
//! Process state is a fixed SoA retention ring. One process-global state buffer
//! owns all arrays; asynchronous work carries only dense process IDs and slots.

use crate::{
    commands::{CommandRequest, admitted, terminate, terminate_group},
    config::{
        COMMAND_ID_ZERO, CommandId, Config, DEFAULT_TIMEOUT_MS, MAX_PROCESS_OUTPUT_BYTES,
        MAX_RUNNING_PROCESSES, MAX_TIMEOUT_MS, PROCESS_RETENTION_SLOTS, command_name,
    },
    mcp::Fault,
    profile,
};
use rivet::output_buffer::{Buffer, OutputSlice, SliceError};
use serde::Serialize;
use serde_json::json;
use std::{
    io,
    process::Stdio,
    sync::{LazyLock, Mutex},
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

struct ProcessArrays {
    // id == 0 => free.
    // RESULT_RUNNING / RESULT_STOPPING => running.
    // Any other result => completed.
    process_id: [u64; PROCESS_SLOTS],
    process_command: [CommandId; PROCESS_SLOTS],
    process_pgid: [u32; PROCESS_SLOTS],

    // Running => monotonic start ns relative to ProcessState::epoch.
    // Completed => final elapsed ns.
    process_time_ns: [u64; PROCESS_SLOTS],
    process_result: [i32; PROCESS_SLOTS],
    next_id: u64,
}

struct ProcessState {
    arrays: Mutex<ProcessArrays>,
    output_stream: [Mutex<Buffer>; OUTPUT_STREAMS],
    stdin: [AsyncMutex<Option<ChildStdin>>; PROCESS_SLOTS],
    epoch: Instant,
    state_event: watch::Sender<()>,
    stdout_cap: usize,
    stderr_cap: usize,
}

#[derive(Clone, Copy)]
enum InputMode {
    Closed,
    Piped,
}

fn input_default_timeout_ms(input_mode: InputMode) -> u64 {
    match input_mode {
        InputMode::Closed => DEFAULT_TIMEOUT_MS,
        InputMode::Piped => MAX_TIMEOUT_MS,
    }
}

fn process_arrays_new() -> ProcessArrays {
    ProcessArrays {
        process_id: [0; PROCESS_SLOTS],
        process_command: [COMMAND_ID_ZERO; PROCESS_SLOTS],
        process_pgid: [0; PROCESS_SLOTS],
        process_time_ns: [0; PROCESS_SLOTS],
        process_result: [RESULT_RUNNING; PROCESS_SLOTS],
        next_id: 1,
    }
}

fn process_state_new(stdout_cap: usize, stderr_cap: usize) -> ProcessState {
    let (state_event, _) = watch::channel(());
    ProcessState {
        arrays: Mutex::new(process_arrays_new()),
        output_stream: std::array::from_fn(|index| {
            let cap = if index % STREAMS_PER_PROCESS == 0 {
                stdout_cap
            } else {
                stderr_cap
            };
            Mutex::new(Buffer::with_capacity(cap))
        }),
        stdin: std::array::from_fn(|_| AsyncMutex::new(None)),
        epoch: Instant::now(),
        state_event,
        stdout_cap,
        stderr_cap,
    }
}

static PROCESS_STATE: LazyLock<ProcessState> =
    LazyLock::new(|| process_state_new(MAX_PROCESS_OUTPUT_BYTES, MAX_PROCESS_OUTPUT_BYTES));

#[inline]
fn runtime_state() -> &'static ProcessState {
    &PROCESS_STATE
}

#[inline]
fn process_is_running(arrays: &ProcessArrays, slot: usize) -> bool {
    arrays.process_id[slot] != 0
        && matches!(
            arrays.process_result[slot],
            RESULT_RUNNING | RESULT_STOPPING
        )
}

#[inline]
fn process_is_completed(arrays: &ProcessArrays, slot: usize) -> bool {
    arrays.process_id[slot] != 0
        && !matches!(
            arrays.process_result[slot],
            RESULT_RUNNING | RESULT_STOPPING
        )
}

fn process_slot(arrays: &ProcessArrays, id: u64) -> Option<usize> {
    arrays
        .process_id
        .iter()
        .position(|candidate| *candidate == id)
}

fn process_running_count(arrays: &ProcessArrays) -> usize {
    (0..PROCESS_SLOTS)
        .filter(|slot| process_is_running(arrays, *slot))
        .count()
}

fn oldest_completed_slot(arrays: &ProcessArrays) -> Option<usize> {
    (0..PROCESS_SLOTS)
        .filter(|slot| process_is_completed(arrays, *slot))
        .min_by_key(|slot| arrays.process_id[*slot])
}

fn clear_process_slot(arrays: &mut ProcessArrays, slot: usize) {
    arrays.process_id[slot] = 0;
}

#[inline]
fn output_index(slot: usize, stderr: bool) -> usize {
    slot * STREAMS_PER_PROCESS + usize::from(stderr)
}

fn output_reset(state: &ProcessState, slot: usize) {
    state.output_stream[output_index(slot, false)]
        .lock()
        .unwrap()
        .reset();
    state.output_stream[output_index(slot, true)]
        .lock()
        .unwrap()
        .reset();
}

fn output_append(state: &ProcessState, slot: usize, stderr: bool, chunk: &[u8]) {
    let cap = if stderr {
        state.stderr_cap
    } else {
        state.stdout_cap
    };
    state.output_stream[output_index(slot, stderr)]
        .lock()
        .unwrap()
        .append(chunk, cap);
}

fn output_slice(
    state: &ProcessState,
    slot: usize,
    stderr: bool,
    offset: u64,
    limit: usize,
) -> Result<OutputSlice, Fault> {
    let cap = if stderr {
        state.stderr_cap
    } else {
        state.stdout_cap
    };
    state.output_stream[output_index(slot, stderr)]
        .lock()
        .unwrap()
        .slice(offset, limit, cap)
        .map_err(|SliceError::OffsetBeyondTotal| {
            Fault::new("INVALID_REQUEST", "output offset is beyond received bytes")
        })
}

fn output_totals(state: &ProcessState, slot: usize) -> (u64, u64, bool) {
    let stdout = state.output_stream[output_index(slot, false)]
        .lock()
        .unwrap();
    let stderr = state.output_stream[output_index(slot, true)]
        .lock()
        .unwrap();
    (
        stdout.total(),
        stderr.total(),
        stdout.total() > state.stdout_cap as u64 || stderr.total() > state.stderr_cap as u64,
    )
}

fn output_profile_totals(
    state: &ProcessState,
    slot: usize,
) -> (u64, u64, usize, usize, bool, bool) {
    let stdout = state.output_stream[output_index(slot, false)]
        .lock()
        .unwrap();
    let stderr = state.output_stream[output_index(slot, true)]
        .lock()
        .unwrap();
    (
        stdout.total(),
        stderr.total(),
        stdout.len(),
        stderr.len(),
        stdout.total() > state.stdout_cap as u64,
        stderr.total() > state.stderr_cap as u64,
    )
}

#[inline]
fn process_now_ns(state: &ProcessState) -> u64 {
    state.epoch.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn process_status(arrays: &ProcessArrays, slot: usize) -> Status {
    let completed = process_is_completed(arrays, slot);
    Status {
        running: process_is_running(arrays, slot),
        exit_code: (completed && arrays.process_result[slot] >= 0)
            .then_some(arrays.process_result[slot]),
        timed_out: arrays.process_result[slot] == RESULT_TIMED_OUT,
        stopped: arrays.process_result[slot] == RESULT_STOPPED,
        duration_ms: completed.then_some(arrays.process_time_ns[slot] / 1_000_000),
    }
}

fn reserve_process_slot(
    state: &ProcessState,
    max_running: usize,
    command: CommandId,
) -> Result<(ProcessId, usize), Fault> {
    let mut arrays = state.arrays.lock().unwrap();
    if process_running_count(&arrays) >= max_running {
        return Err(Fault::new("PROCESS_LIMIT", "process capacity is full"));
    }

    let slot = arrays
        .process_id
        .iter()
        .position(|id| *id == 0)
        .or_else(|| oldest_completed_slot(&arrays))
        .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process registry is full"))?;

    output_reset(state, slot);

    let id = arrays.next_id;
    arrays.next_id = arrays
        .next_id
        .checked_add(1)
        .ok_or_else(|| Fault::new("PROCESS_LIMIT", "process IDs exhausted"))?;

    arrays.process_id[slot] = id;
    arrays.process_command[slot] = command;
    arrays.process_pgid[slot] = 0;
    arrays.process_time_ns[slot] = process_now_ns(state);
    arrays.process_result[slot] = RESULT_RUNNING;

    Ok((ProcessId(id), slot))
}

fn release_reserved_process(state: &ProcessState, id: ProcessId, slot: usize) {
    let released = {
        let mut arrays = state.arrays.lock().unwrap();
        if arrays.process_id[slot] == id.0 && process_is_running(&arrays, slot) {
            clear_process_slot(&mut arrays, slot);
            output_reset(state, slot);
            true
        } else {
            false
        }
    };
    if released {
        state.state_event.send_replace(());
    }
}

fn set_process_pgid(state: &ProcessState, id: ProcessId, slot: usize, pgid: u32) {
    let changed = {
        let mut arrays = state.arrays.lock().unwrap();
        if arrays.process_id[slot] == id.0 && process_is_running(&arrays, slot) {
            arrays.process_pgid[slot] = pgid;
            true
        } else {
            false
        }
    };
    if changed {
        state.state_event.send_replace(());
    }
}

fn process_result_for(state: &ProcessState, id: ProcessId, slot: usize) -> i32 {
    let arrays = state.arrays.lock().unwrap();
    if arrays.process_id[slot] == id.0 {
        arrays.process_result[slot]
    } else {
        RESULT_RUNNING
    }
}

fn complete_process(
    state: &ProcessState,
    id: ProcessId,
    slot: usize,
    mut result: i32,
) -> Option<Status> {
    let now_ns = process_now_ns(state);
    let mut arrays = state.arrays.lock().unwrap();
    if arrays.process_id[slot] != id.0 || process_is_completed(&arrays, slot) {
        return None;
    }

    if arrays.process_result[slot] == RESULT_STOPPING && result != RESULT_TIMED_OUT {
        result = RESULT_STOPPED;
    }

    arrays.process_time_ns[slot] = now_ns.saturating_sub(arrays.process_time_ns[slot]);
    arrays.process_result[slot] = result;
    Some(process_status(&arrays, slot))
}

async fn spawn_process(
    config: &Config,
    request: CommandRequest,
    input_mode: InputMode,
) -> Result<ProcessId, Fault> {
    let state = runtime_state();
    let profiling = profile::enabled();
    let profile_command = profiling.then(|| request.command.clone());
    let rss_before_bytes = profile::self_current_rss_bytes();

    let admitted_started = profiling.then(Instant::now);
    let (mut command, timeout_ms, command_id) =
        admitted(config, &request, input_default_timeout_ms(input_mode))?;
    let admitted_us = admitted_started
        .as_ref()
        .map(|started| started.elapsed().as_micros());
    if matches!(input_mode, InputMode::Closed) {
        command.stdin(Stdio::null());
    }

    let registry_started = profiling.then(Instant::now);
    let (id, slot) = reserve_process_slot(state, MAX_RUNNING_PROCESSES, command_id)?;
    let registry_us = registry_started
        .as_ref()
        .map(|started| started.elapsed().as_micros());

    let spawn_started = profiling.then(Instant::now);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            release_reserved_process(state, id, slot);
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
        release_reserved_process(state, id, slot);
        return Err(Fault::new("SPAWN_ERROR", "spawned child has no process ID"));
    };
    set_process_pgid(state, id, slot, pid);

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
        *state.stdin[slot].lock().await = child.stdin.take();
        state.state_event.send_replace(());
    } else {
        let _ = child.stdin.take();
    }

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    tokio::spawn(async move {
        supervise_process(
            id,
            slot,
            pid,
            child,
            stdout,
            stderr,
            timeout_ms,
            profiling,
            profile_command,
            rss_before_bytes,
            spawn_finished_at,
        )
        .await;
    });

    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn supervise_process<Stdout, Stderr>(
    id: ProcessId,
    slot: usize,
    pid: u32,
    mut child: tokio::process::Child,
    stdout: Stdout,
    stderr: Stderr,
    timeout_ms: u64,
    profiling: bool,
    profile_command: Option<String>,
    rss_before_bytes: u64,
    spawn_finished_at: Option<Instant>,
) where
    Stdout: AsyncRead + Unpin + Send + 'static,
    Stderr: AsyncRead + Unpin + Send + 'static,
{
    let state = runtime_state();
    let supervisor_started = profiling.then(Instant::now);
    let supervisor_start_us = spawn_finished_at
        .as_ref()
        .map(|started| started.elapsed().as_micros());
    let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);

    let output_started_at = spawn_finished_at;
    let mut outputs = tokio::spawn(async move {
        let (stdout_result, stderr_result) = tokio::join!(
            drain_process_output(stdout, slot, false, output_started_at),
            drain_process_output(stderr, slot, true, output_started_at)
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
            if process_result_for(state, id, slot) == RESULT_STOPPING {
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

    *state.stdin[slot].lock().await = None;

    let total_us = supervisor_started
        .as_ref()
        .map(|started| started.elapsed().as_micros());
    let spawn_to_complete_us = spawn_finished_at
        .as_ref()
        .map(|started| started.elapsed().as_micros());

    let Some(profile_status) = complete_process(state, id, slot, final_result) else {
        return;
    };
    state.state_event.send_replace(());

    profile::event(|| {
        let (
            stdout_total,
            stderr_total,
            stdout_retained,
            stderr_retained,
            stdout_truncated,
            stderr_truncated,
        ) = output_profile_totals(state, slot);
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
}

pub async fn submit(config: &Config, request: CommandRequest) -> Result<serde_json::Value, Fault> {
    let id = spawn_process(config, request, InputMode::Closed).await?;
    Ok(json!({"process_id": id.0}))
}

pub async fn start(config: &Config, request: CommandRequest) -> Result<serde_json::Value, Fault> {
    let id = spawn_process(config, request, InputMode::Piped).await?;
    Ok(json!({"process_id": id.0}))
}

pub fn list(config: &Config) -> serde_json::Value {
    let state = runtime_state();
    let arrays = state.arrays.lock().unwrap();
    let now_ns = process_now_ns(state);
    let mut slots = [0_usize; PROCESS_SLOTS];
    let mut count = 0;
    for slot in 0..PROCESS_SLOTS {
        if arrays.process_id[slot] != 0 {
            slots[count] = slot;
            count += 1;
        }
    }
    slots[..count].sort_unstable_by_key(|slot| arrays.process_id[*slot]);

    let mut running = Vec::with_capacity(count);
    let mut completed = Vec::with_capacity(count);

    for &slot in &slots[..count] {
        let (stdout_bytes, stderr_bytes, truncated) = output_totals(state, slot);
        if process_is_running(&arrays, slot) {
            running.push(json!({
                "process_id": arrays.process_id[slot],
                "command": command_name(config, arrays.process_command[slot]),
                "duration_ms": now_ns.saturating_sub(arrays.process_time_ns[slot]) / 1_000_000,
                "stdout_bytes": stdout_bytes,
                "stderr_bytes": stderr_bytes,
                "truncated": truncated,
            }));
        } else {
            completed.push(json!({
                "process_id": arrays.process_id[slot],
                "command": command_name(config, arrays.process_command[slot]),
                "duration_ms": arrays.process_time_ns[slot] / 1_000_000,
                "exit_code": (arrays.process_result[slot] >= 0).then_some(arrays.process_result[slot]),
                "stopped": arrays.process_result[slot] == RESULT_STOPPED,
                "timed_out": arrays.process_result[slot] == RESULT_TIMED_OUT,
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

    let state = runtime_state();
    let arrays = state.arrays.lock().unwrap();
    let slot = process_slot(&arrays, id)
        .ok_or_else(|| Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted"))?;
    let stdout = output_slice(state, slot, false, stdout_offset, limit)?;
    let stderr = output_slice(state, slot, true, stderr_offset, limit)?;
    let process_state = if process_is_running(&arrays, slot) {
        "running"
    } else {
        "completed"
    };

    Ok(json!({
        "state": process_state,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

pub async fn input(
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

    let state = runtime_state();
    let mut state_event = state.state_event.subscribe();
    loop {
        let slot = {
            let arrays = state.arrays.lock().unwrap();
            process_slot(&arrays, id).ok_or_else(|| {
                Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
            })?
        };

        let mut stdin = state.stdin[slot].lock().await;
        {
            let arrays = state.arrays.lock().unwrap();
            if arrays.process_id[slot] != id || !process_is_running(&arrays, slot) {
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
            .map_err(|error| {
                Fault::with(
                    "IO_ERROR",
                    "process stdin write failed",
                    json!({"error": error.to_string()}),
                )
            })?;
            return Ok(json!({"bytes_written": text.len()}));
        }

        drop(stdin);
        if state_event.changed().await.is_err() {
            return Err(Fault::new("IO_ERROR", "process state watcher stopped"));
        }
    }
}

pub async fn stop(id: u64) -> Result<serde_json::Value, Fault> {
    let state = runtime_state();
    let mut state_event = state.state_event.subscribe();

    let pgid = loop {
        let pgid = {
            let mut arrays = state.arrays.lock().unwrap();
            let slot = process_slot(&arrays, id).ok_or_else(|| {
                Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
            })?;
            if process_is_completed(&arrays, slot) {
                return Ok(json!({"status": process_status(&arrays, slot)}));
            }
            arrays.process_result[slot] = RESULT_STOPPING;
            arrays.process_pgid[slot]
        };
        if pgid != 0 {
            break pgid;
        }
        if state_event.changed().await.is_err() {
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
            let arrays = state.arrays.lock().unwrap();
            let slot = process_slot(&arrays, id).ok_or_else(|| {
                Fault::new("PROCESS_NOT_FOUND", "process ID is unknown or was evicted")
            })?;
            process_is_completed(&arrays, slot).then(|| process_status(&arrays, slot))
        };
        if let Some(status) = status {
            return Ok(json!({"status": status}));
        }
        if state_event.changed().await.is_err() {
            return Err(Fault::new("IO_ERROR", "process state watcher stopped"));
        }
    }
}

pub async fn shutdown() {
    let state = runtime_state();
    let mut state_event = state.state_event.subscribe();
    let (pgids, count) = loop {
        let mut pgids = [0_u32; PROCESS_SLOTS];
        let mut count = 0_usize;
        let mut waiting_for_pgid = false;
        {
            let mut arrays = state.arrays.lock().unwrap();
            for slot in 0..PROCESS_SLOTS {
                if !process_is_running(&arrays, slot) {
                    continue;
                }
                arrays.process_result[slot] = RESULT_STOPPING;
                let pgid = arrays.process_pgid[slot];
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
        if state_event.changed().await.is_err() {
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
        if process_running_count(&state.arrays.lock().unwrap()) == 0 {
            break;
        }
        if state_event.changed().await.is_err() {
            break;
        }
    }
}

async fn drain_process_output<R: AsyncRead + Unpin>(
    mut reader: R,
    slot: usize,
    stderr: bool,
    spawn_finished_at: Option<Instant>,
) -> io::Result<Option<u64>> {
    let state = runtime_state();
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
        output_append(state, slot, stderr, &chunk[..count]);
    }
    Ok(first_read_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_state_is_derived_from_id_and_result() {
        let mut arrays = process_arrays_new();

        arrays.process_id[3] = 7;
        assert!(process_is_running(&arrays, 3));
        assert!(!process_is_completed(&arrays, 3));
        assert_eq!(process_running_count(&arrays), 1);

        arrays.process_result[3] = 0;
        assert!(!process_is_running(&arrays, 3));
        assert!(process_is_completed(&arrays, 3));
        assert_eq!(process_running_count(&arrays), 0);

        clear_process_slot(&mut arrays, 3);
        assert_eq!(arrays.process_id[3], 0);
        assert!(!process_is_running(&arrays, 3));
        assert!(!process_is_completed(&arrays, 3));
    }

    #[test]
    fn oldest_completed_slot_uses_process_id_order() {
        let mut arrays = process_arrays_new();

        arrays.process_id[2] = 12;
        arrays.process_result[2] = 0;
        arrays.process_id[5] = 9;
        arrays.process_result[5] = RESULT_STOPPED;
        arrays.process_id[7] = 15;
        arrays.process_result[7] = RESULT_RUNNING;

        assert_eq!(oldest_completed_slot(&arrays), Some(5));
    }

    #[test]
    fn output_buffers_reuse_preallocated_capacity() {
        let state = process_state_new(1024, 1024);
        output_append(&state, 2, false, b"abc");

        let before = state.output_stream[output_index(2, false)]
            .lock()
            .unwrap()
            .capacity();
        output_reset(&state, 2);
        let after = state.output_stream[output_index(2, false)]
            .lock()
            .unwrap()
            .capacity();

        assert_eq!(before, 1024);
        assert_eq!(after, 1024);
    }

    #[test]
    fn pgid_and_reservation_rollback_publish_state_changes() {
        let state = process_state_new(4096, 4096);
        {
            let mut arrays = state.arrays.lock().unwrap();
            arrays.process_id[0] = 1;
            arrays.process_result[0] = RESULT_RUNNING;
        }

        let pgid_change = state.state_event.subscribe();
        set_process_pgid(&state, ProcessId(1), 0, 42);
        assert!(pgid_change.has_changed().unwrap());
        assert_eq!(state.arrays.lock().unwrap().process_pgid[0], 42);

        let rollback = state.state_event.subscribe();
        release_reserved_process(&state, ProcessId(1), 0);
        assert!(rollback.has_changed().unwrap());
        assert_eq!(state.arrays.lock().unwrap().process_id[0], 0);
    }

    #[test]
    fn completion_has_no_sequence_or_ready_queue_state() {
        let state = process_state_new(4096, 4096);
        {
            let mut arrays = state.arrays.lock().unwrap();
            arrays.process_id[0] = 1;
            arrays.process_time_ns[0] = process_now_ns(&state);
        }

        let status = complete_process(&state, ProcessId(1), 0, 0).unwrap();

        assert!(!status.running);
        assert_eq!(status.exit_code, Some(0));
        assert!(process_is_completed(&state.arrays.lock().unwrap(), 0));
    }
}
