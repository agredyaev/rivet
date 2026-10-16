//! Direct OpenAI Secure MCP Tunnel transport for Rivet.
//!
//! Product scope is deliberately narrow: one tunnel, one stateless `main`
//! channel, one in-process Rivet MCP runtime.

use crate::{
    config::Config,
    mcp::{Dispatch, Rivet},
    profile,
};
use bytes::Bytes;
use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER},
    redirect::Policy,
};
use rivet::{
    tunnel_policy::{PostClass, backoff_base_ms, classify_post},
    tunnel_wire::{
        CommandType, WireCommand, extract_jsonrpc_id, parse_command, parse_poll,
        parse_response_timeout, valid_tunnel_id,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep, timeout_at},
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const SERVER_INFO: &str =
    r#"{"version":2,"channels":[{"name":"main","stateless":true,"proc_affinity":true}]}"#;
const WIRE_VERSION: &str = "2026-08-25";
const POLL_TIMEOUT_MS: u64 = 15_000;
const MAX_POLL_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 16;
const RESPONSE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
static PROFILE_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize)]
struct ResponseHeaders {
    #[serde(rename = "Content-Type")]
    content_type: [&'static str; 1],
}

#[derive(Serialize)]
struct TunnelResponse<'a> {
    request_id: &'a str,
    channel: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resp_json: Option<&'a RawValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resp_headers: Option<ResponseHeaders>,
    resp_code: u16,
    resp_type: &'static str,
}

struct Client {
    http: reqwest::Client,
    endpoint: String,
}

fn profile_rpc_label(
    raw: &RawValue,
    command: &WireCommand,
) -> (Option<String>, Option<String>, Option<String>) {
    if !profile::enabled() {
        return (None, None, None);
    }

    #[derive(Deserialize)]
    struct ProfileCommand {
        #[serde(default)]
        created_at: Option<String>,
    }

    let created_at = serde_json::from_str::<ProfileCommand>(raw.get())
        .ok()
        .and_then(|command| command.created_at);
    let Some(jsonrpc) = command.jsonrpc.as_deref() else {
        return (created_at, None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(jsonrpc.get()) else {
        return (created_at, None, None);
    };
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let tool = if method.as_deref() == Some("tools/call") {
        value
            .pointer("/params/name")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    (created_at, method, tool)
}

impl Client {
    fn new(base_url: String, tunnel_id: String, api_key: String) -> Result<Arc<Self>, String> {
        let mut headers = HeaderMap::new();
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| "runtime API key cannot be represented as an HTTP header".to_owned())?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        headers.insert("X-Tunnel-Client-Name", HeaderValue::from_static("rivet"));
        headers.insert(
            "X-Tunnel-Client-Version",
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        );
        headers.insert(
            "X-Tunnel-Client-Wire-Protocol-Version",
            HeaderValue::from_static(WIRE_VERSION),
        );
        headers.insert(
            "X-Tunnel-MCP-Server-Info",
            HeaderValue::from_static(SERVER_INFO),
        );
        headers.insert(
            "X-Tunnel-Client-Instance-Id",
            HeaderValue::from_str(&instance_id())
                .map_err(|_| "client instance ID cannot be represented as an HTTP header")?,
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .build()
            .map_err(|error| error.to_string())?;

        Ok(Arc::new(Self {
            http,
            endpoint: format!("{}/v1/tunnels/{tunnel_id}", base_url.trim_end_matches('/')),
        }))
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.endpoint)
    }

    async fn validate(&self) -> Result<(), String> {
        let response = self
            .http
            .get(&self.endpoint)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|error| format!("tunnel metadata request failed: {error}"))?;
        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err("tunnel authentication or authorization failed".into())
            }
            status => Err(format!("tunnel metadata request returned HTTP {status}")),
        }
    }

    async fn poll_loop(self: &Arc<Self>, runtime: Arc<Rivet>) -> Result<(), String> {
        let mut in_flight = JoinSet::new();
        let mut failures = 0_u32;
        let poll_url = self.url("/poll");

        loop {
            while in_flight.len() >= MAX_IN_FLIGHT {
                if let Some(result) = in_flight.join_next().await {
                    report_task(result);
                }
            }
            while let Some(result) = in_flight.try_join_next() {
                report_task(result);
            }

            let available = MAX_IN_FLIGHT.saturating_sub(in_flight.len()).clamp(1, 25);
            let profiling = profile::enabled();
            let poll_started = profiling.then(Instant::now);
            let response = match self
                .http
                .get(format!(
                    "{poll_url}?limit={available}&timeout_ms={POLL_TIMEOUT_MS}"
                ))
                .timeout(Duration::from_millis(POLL_TIMEOUT_MS + 5_000))
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    failures = failures.saturating_add(1);
                    eprintln!("Tunnel poll transport error: {error}");
                    sleep(backoff(failures)).await;
                    continue;
                }
            };

            // reqwest resolves send() once response headers are available. The
            // protocol timeout is anchored here, before body decoding.
            let received_at = Instant::now();
            let received_wall_ms = profile::wall_ms();
            let poll_http_us = poll_started.map(|started| started.elapsed().as_micros());
            let status = response.status();

            if status == StatusCode::NO_CONTENT {
                failures = 0;
                continue;
            }
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(format!(
                    "tunnel poll authorization failed with HTTP {status}"
                ));
            }
            if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                failures = failures.saturating_add(1);
                let delay = retry_after(response.headers())
                    .unwrap_or_default()
                    .max(backoff(failures));
                sleep(delay).await;
                continue;
            }
            if status != StatusCode::OK {
                return Err(format!("tunnel poll returned HTTP {status}"));
            }

            failures = 0;
            let body_started = profiling.then(Instant::now);
            let body = read_limited(response, MAX_POLL_BODY_BYTES).await?;
            let envelope =
                parse_poll(&body).map_err(|error| format!("invalid tunnel poll JSON: {error}"))?;
            profile::event(|| {
                serde_json::json!({
                    "event": "poll",
                    "received_wall_ms": received_wall_ms,
                    "http_us": poll_http_us,
                    "body_us": body_started.map(|started| started.elapsed().as_micros()),
                    "commands": envelope.commands.len(),
                    "in_flight_before_spawn": in_flight.len(),
                    "available": available,
                    "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                    "children_peak_rss_bytes": profile::children_peak_rss_bytes()
                })
            });

            // poll.limit is a hint, not a protocol execution cap. Every command
            // returned in this bounded poll body is admitted immediately.
            for raw in envelope.commands {
                let client = Arc::clone(self);
                let runtime = runtime.clone();
                in_flight.spawn(async move {
                    process_command(client, runtime, raw, received_at, received_wall_ms).await
                });
            }
        }
    }

    async fn post_terminal(
        &self,
        command: &WireCommand,
        body: Bytes,
        deadline: Option<Instant>,
        profile_seq: u64,
    ) -> Result<(), String> {
        let response_url = self.url("/response");
        let shard_token = HeaderValue::from_str(&command.shard_token)
            .map_err(|_| "tunnel shard token cannot be represented as an HTTP header")?;
        let mut attempt = 0_u32;
        loop {
            if expired(deadline) {
                return Ok(());
            }

            attempt = attempt.saturating_add(1);
            let attempt_started = profile::enabled().then(Instant::now);
            let request = self
                .http
                .post(&response_url)
                .header("X-Tunnel-Shard-Token", shard_token.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone());

            let response = if let Some(deadline) = deadline {
                match timeout_at(deadline, request.send()).await {
                    Ok(result) => result,
                    Err(_) => return Ok(()),
                }
            } else {
                request.timeout(RESPONSE_ATTEMPT_TIMEOUT).send().await
            };

            match response {
                Ok(response) => {
                    let status = response.status();
                    profile::event(|| {
                        let processing_ms = response
                            .headers()
                            .get("openai-processing-ms")
                            .and_then(|value| value.to_str().ok());
                        let server_timing = response
                            .headers()
                            .get("server-timing")
                            .and_then(|value| value.to_str().ok());
                        serde_json::json!({
                            "event": "post_attempt",
                            "seq": profile_seq,
                            "attempt": attempt,
                            "elapsed_us": attempt_started
                                .as_ref()
                                .map(|started| started.elapsed().as_micros()),
                            "status": status.as_u16(),
                            "http_version": format!("{:?}", response.version()),
                            "openai_processing_ms": processing_ms,
                            "server_timing": server_timing,
                            "self_peak_rss_bytes": profile::self_peak_rss_bytes()
                        })
                    });

                    match classify_post(status.as_u16()) {
                        PostClass::Accepted | PostClass::Terminal => return Ok(()),
                        PostClass::Auth => {
                            return Err(format!(
                                "terminal response authorization failed with HTTP {status}"
                            ));
                        }
                        PostClass::Fail => {
                            return Err(format!(
                                "terminal response was rejected with HTTP {status}"
                            ));
                        }
                        PostClass::Retry => {
                            let delay = retry_after(response.headers())
                                .unwrap_or_default()
                                .max(backoff(attempt));
                            if !sleep_with_deadline(delay, deadline).await {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(error) => {
                    profile::event(|| {
                        serde_json::json!({
                            "event": "post_attempt",
                            "seq": profile_seq,
                            "attempt": attempt,
                            "elapsed_us": attempt_started
                                .as_ref()
                                .map(|started| started.elapsed().as_micros()),
                            "transport_error": true,
                            "self_peak_rss_bytes": profile::self_peak_rss_bytes()
                        })
                    });
                    eprintln!("Terminal response transport error: {error}");
                    if !sleep_with_deadline(backoff(attempt), deadline).await {
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn process_command(
    client: Arc<Client>,
    runtime: Arc<Rivet>,
    raw: Box<RawValue>,
    received_at: Instant,
    received_wall_ms: u128,
) -> Result<(), String> {
    let command = match parse_command(raw.as_ref()) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("Dropping malformed tunnel command: {error}");
            return Ok(());
        }
    };

    if command.request_id.is_empty() || command.shard_token.is_empty() {
        eprintln!("Dropping tunnel command with missing correlation fields");
        return Ok(());
    }

    let profiling = profile::enabled();
    let seq = if profiling {
        PROFILE_SEQ.fetch_add(1, Ordering::Relaxed)
    } else {
        0
    };
    let rss_before_bytes = profile::self_current_rss_bytes();
    let (created_at, rpc_method, tool_name) = profile_rpc_label(raw.as_ref(), &command);
    let start_delay_us = profiling.then(|| received_at.elapsed().as_micros());
    let dispatch_started = profiling.then(Instant::now);

    let deadline = command
        .response_timeout
        .as_ref()
        .and_then(parse_response_timeout)
        .and_then(|duration| received_at.checked_add(duration));
    if expired(deadline) {
        return Ok(());
    }

    let terminal = match command.command_type {
        CommandType::SessionTermination => {
            encode_response(&command, None, 204, "session_termination_response")?
        }
        CommandType::Jsonrpc if command.channel.as_deref().unwrap_or("main") != "main" => {
            synthetic_error(&command, "client_internal", 502)?
        }
        CommandType::Jsonrpc => match command.jsonrpc.as_deref() {
            Some(jsonrpc) => dispatch_jsonrpc(&runtime, &command, jsonrpc, deadline, seq).await?,
            None => synthetic_error(&command, "protocol", 502)?,
        },
        CommandType::Other => synthetic_error(&command, "protocol", 502)?,
    };

    let dispatch_us = dispatch_started.map(|started| started.elapsed().as_micros());
    let request_bytes = command
        .jsonrpc
        .as_deref()
        .map(|value| value.get().len())
        .unwrap_or(0);
    let response_bytes = terminal.len();
    if terminal.is_empty() || expired(deadline) {
        profile::event(|| {
            serde_json::json!({
                "event": "command",
                "seq": seq,
                "rpc_method": rpc_method,
                "tool": tool_name,
                "created_at": created_at,
                "received_wall_ms": received_wall_ms,
                "start_delay_us": start_delay_us,
                "dispatch_us": dispatch_us,
                "request_bytes": request_bytes,
                "response_bytes": response_bytes,
                "post_us": null,
                "total_us": received_at.elapsed().as_micros(),
                "result": "dropped",
                "self_rss_before_bytes": rss_before_bytes,
                "self_rss_after_bytes": profile::self_current_rss_bytes(),
                "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                "children_peak_rss_bytes": profile::children_peak_rss_bytes()
            })
        });
        return Ok(());
    }

    let post_started = profiling.then(Instant::now);
    let result = client
        .post_terminal(&command, Bytes::from(terminal), deadline, seq)
        .await;
    profile::event(|| {
        serde_json::json!({
            "event": "command",
            "seq": seq,
            "rpc_method": rpc_method,
            "tool": tool_name,
            "created_at": created_at,
            "received_wall_ms": received_wall_ms,
            "start_delay_us": start_delay_us,
            "dispatch_us": dispatch_us,
            "request_bytes": request_bytes,
            "response_bytes": response_bytes,
            "post_us": post_started.map(|started| started.elapsed().as_micros()),
            "total_us": received_at.elapsed().as_micros(),
            "result": if result.is_ok() { "ok" } else { "error" },
            "self_rss_before_bytes": rss_before_bytes,
            "self_rss_after_bytes": profile::self_current_rss_bytes(),
            "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
            "children_peak_rss_bytes": profile::children_peak_rss_bytes()
        })
    });
    result
}

async fn dispatch_jsonrpc(
    runtime: &Rivet,
    command: &WireCommand,
    jsonrpc: &RawValue,
    deadline: Option<Instant>,
    profile_seq: u64,
) -> Result<Vec<u8>, String> {
    let future = runtime.dispatch_jsonrpc_profiled(jsonrpc.get().as_bytes(), Some(profile_seq));
    let result = if let Some(deadline) = deadline {
        match timeout_at(deadline, future).await {
            Ok(result) => result,
            Err(_) => return Ok(Vec::new()),
        }
    } else {
        future.await
    };

    match result {
        Ok(Dispatch::Response(raw)) => {
            encode_response(command, Some(raw.as_ref()), 200, "jsonrpc_response")
        }
        Ok(Dispatch::NotificationAck) => encode_response(command, None, 204, "notify_ack"),
        Err(_) => synthetic_error(command, "client_internal", 502),
    }
}

fn synthetic_error(
    command: &WireCommand,
    source: &'static str,
    status: u16,
) -> Result<Vec<u8>, String> {
    let id = command
        .jsonrpc
        .as_deref()
        .and_then(extract_jsonrpc_id)
        .unwrap_or(Value::Null);
    let value = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": "Bad Gateway",
            "data": {
                "tunnel_failure": {
                    "version": 1,
                    "source": source,
                    "upstream_response_received": false
                }
            }
        }
    });
    let raw = serde_json::value::to_raw_value(&value).map_err(|error| error.to_string())?;
    encode_response(command, Some(raw.as_ref()), status, "jsonrpc_response")
}

fn encode_response(
    command: &WireCommand,
    jsonrpc: Option<&RawValue>,
    status: u16,
    response_type: &'static str,
) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&TunnelResponse {
        request_id: &command.request_id,
        channel: command.channel.as_deref().unwrap_or("main"),
        resp_json: jsonrpc,
        resp_headers: jsonrpc.map(|_| ResponseHeaders {
            content_type: ["application/json"],
        }),
        resp_code: status,
        resp_type: response_type,
    })
    .map_err(|error| error.to_string())
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(60)));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(
        date.duration_since(SystemTime::now())
            .ok()?
            .min(Duration::from_secs(60)),
    )
}

fn backoff(attempt: u32) -> Duration {
    let base = backoff_base_ms(attempt);
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64 % ((base / 5).max(1) + 1))
        .unwrap_or(0);
    Duration::from_millis(base + jitter)
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

async fn sleep_with_deadline(delay: Duration, deadline: Option<Instant>) -> bool {
    if let Some(deadline) = deadline {
        let now = Instant::now();
        if now >= deadline || now.checked_add(delay).is_none_or(|wake| wake >= deadline) {
            return false;
        }
    }
    sleep(delay).await;
    true
}

async fn read_limited(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(cap),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read tunnel body: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > cap {
            return Err(format!("tunnel response exceeds {cap} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn report_task(result: Result<Result<(), String>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("Tunnel command failed: {error}"),
        Err(error) => eprintln!("Tunnel command task failed: {error}"),
    }
}

fn instance_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("rivet-{}-{nanos}", std::process::id())
}

async fn read_hidden(prompt: &str) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    use tokio::signal::unix::{SignalKind, signal};

    struct TerminalEcho {
        fd: libc::c_int,
        original: libc::termios,
        flags: libc::c_int,
        restored: bool,
    }
    impl TerminalEcho {
        fn restore(&mut self) -> Result<(), String> {
            // SAFETY: stdin remains open and original came from tcgetattr.
            if unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) } != 0 {
                return Err(format!(
                    "could not restore terminal echo: {}",
                    io::Error::last_os_error()
                ));
            }
            // SAFETY: restore the file status flags saved from this live descriptor.
            if unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) } < 0 {
                return Err(format!(
                    "could not restore stdin flags: {}",
                    io::Error::last_os_error()
                ));
            }
            self.restored = true;
            Ok(())
        }
    }
    impl Drop for TerminalEcho {
        fn drop(&mut self) {
            if !self.restored {
                let _ = self.restore();
            }
        }
    }

    let mut interrupt = signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
    let stdin = io::stdin();
    let fd = stdin.as_raw_fd();
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: fd is live and original points to writable termios storage.
    if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
        return Err(format!(
            "could not read terminal settings: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: tcgetattr initialized the value.
    let original = unsafe { original.assume_init() };
    // SAFETY: fcntl inspects the live stdin descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let mut terminal = TerminalEcho {
        fd,
        original,
        flags,
        restored: false,
    };
    let mut hidden = original;
    hidden.c_lflag &= !libc::ECHO;
    // SAFETY: fd and the termios pointer are valid.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
        return Err(format!(
            "could not disable terminal echo: {}",
            io::Error::last_os_error()
        ));
    }
    // A signal can flush terminal input between poll and read; nonblocking reads avoid that race.
    // SAFETY: fd is live and flags were obtained from this descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    print!("  {prompt}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut value = Vec::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = interrupt.recv() => break Err("Session setup cancelled: interrupted".to_owned()),
            _ = sleep(Duration::from_millis(25)) => {
                // Keep canonical terminal editing, including partial lines submitted with Ctrl+D.
                let mut ready = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                // SAFETY: ready points to one initialized pollfd; the call does not block.
                let status = unsafe { libc::poll(&mut ready, 1, 0) };
                if status < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted { continue; }
                    break Err(error.to_string());
                }
                if status > 0 {
                    let mut bytes = [0_u8; 4096];
                    // SAFETY: bytes is writable for its length and fd remains open.
                    let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
                    if count < 0 {
                        let error = io::Error::last_os_error();
                        if matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) { continue; }
                        break Err(error.to_string());
                    }
                    if count == 0 { break Err("Session setup cancelled: input closed".to_owned()); }
                    value.extend_from_slice(&bytes[..count as usize]);
                    if value.ends_with(b"\n") {
                        break String::from_utf8(value).map(|value| value.trim().to_owned()).map_err(|error| error.to_string());
                    }
                }
            }
        }
    };
    terminal.restore()?;
    println!();
    result
}

fn control_plane_base_url(value: Option<&str>, allow_unsafe: bool) -> Result<String, String> {
    match value {
        Some(value) if !value.is_empty() && value != DEFAULT_BASE_URL => {
            if !allow_unsafe {
                return Err(
                    "CONTROL_PLANE_BASE_URL override requires RIVET_ALLOW_UNSAFE_CONTROL_PLANE=1"
                        .into(),
                );
            }
            Ok(value.to_owned())
        }
        _ => Ok(DEFAULT_BASE_URL.to_owned()),
    }
}

async fn credentials() -> Result<(String, String, String), String> {
    let interactive = io::stdin().is_terminal();
    let tunnel_id = match env::var("CONTROL_PLANE_TUNNEL_ID") {
        Ok(value) if !value.is_empty() => value,
        _ if interactive => read_hidden("Tunnel ID (input hidden): ").await?,
        _ => return Err("CONTROL_PLANE_TUNNEL_ID is required without a terminal".into()),
    };
    if !valid_tunnel_id(&tunnel_id) {
        return Err("invalid tunnel ID format".into());
    }
    let api_key = match env::var("CONTROL_PLANE_API_KEY") {
        Ok(value) if !value.is_empty() => value,
        _ if interactive => read_hidden("Runtime API key (input hidden): ").await?,
        _ => return Err("CONTROL_PLANE_API_KEY is required without a terminal".into()),
    };
    if api_key.is_empty() {
        return Err("runtime API key cannot be empty".into());
    }
    let base_url_override = env::var("CONTROL_PLANE_BASE_URL").ok();
    let base_url = control_plane_base_url(
        base_url_override.as_deref(),
        env::var("RIVET_ALLOW_UNSAFE_CONTROL_PLANE").as_deref() == Ok("1"),
    )?;
    Ok((base_url, tunnel_id, api_key))
}

pub async fn run(config: Arc<Config>) -> Result<(), String> {
    let (base_url, tunnel_id, api_key) = credentials().await?;
    let client = Client::new(base_url, tunnel_id, api_key)?;
    let runtime = Arc::new(Rivet::new(config));

    println!("  Connecting to OpenAI Secure MCP Tunnel...");
    client.validate().await?;
    println!(
        "  ✓ Credentials accepted\n  Direct Rivet tunnel is running. Press Ctrl+C to stop it."
    );

    let result = tokio::select! {
        result = client.poll_loop(runtime.clone()) => result,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| error.to_string())?;
            Ok(())
        }
    };
    runtime.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        path::Path,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    const TEST_TUNNEL_ID: &str = "tunnel_0123456789abcdefghijklmnopqrstuv";

    struct HttpRequest {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    #[derive(Default)]
    struct MockData {
        metadata_headers: Option<BTreeMap<String, String>>,
        first_poll_path: Option<String>,
        post_bodies: BTreeMap<String, Vec<Vec<u8>>>,
        shard_tokens: BTreeMap<String, Vec<String>>,
    }

    struct MockServer {
        base_url: String,
        state: Arc<Mutex<MockData>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn start(poll_body: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let state = Arc::new(Mutex::new(MockData::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let state_for_thread = state.clone();
            let stop_for_thread = stop.clone();
            let tunnel_path = format!("/v1/tunnels/{TEST_TUNNEL_ID}");
            let poll_path = format!("{tunnel_path}/poll");
            let response_path = format!("{tunnel_path}/response");

            let thread = thread::spawn(move || {
                while !stop_for_thread.load(Ordering::Relaxed) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("mock tunnel accept failed: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    let request = read_request(&mut stream).unwrap();

                    if request.method == "GET" && request.path == tunnel_path {
                        state_for_thread.lock().unwrap().metadata_headers = Some(request.headers);
                        write_response(&mut stream, 200, &[], b"{}").unwrap();
                        continue;
                    }

                    if request.method == "GET" && request.path.starts_with(&poll_path) {
                        let (first, complete) = {
                            let mut state = state_for_thread.lock().unwrap();
                            let first = state.first_poll_path.is_none();
                            if first {
                                state.first_poll_path = Some(request.path.clone());
                            }
                            let retry_done = state
                                .post_bodies
                                .get("req_retry")
                                .is_some_and(|attempts| attempts.len() == 2);
                            (first, state.post_bodies.len() == 15 && retry_done)
                        };
                        if first {
                            write_response(&mut stream, 200, &[], &poll_body).unwrap();
                        } else if complete {
                            write_response(&mut stream, 400, &[], b"{}").unwrap();
                        } else {
                            thread::sleep(Duration::from_millis(5));
                            write_response(&mut stream, 204, &[], b"").unwrap();
                        }
                        continue;
                    }

                    if request.method == "POST" && request.path == response_path {
                        let value: Value = serde_json::from_slice(&request.body).unwrap();
                        let request_id = value["request_id"].as_str().unwrap().to_owned();
                        let shard = request
                            .headers
                            .get("x-tunnel-shard-token")
                            .cloned()
                            .unwrap_or_default();
                        let attempt = {
                            let mut state = state_for_thread.lock().unwrap();
                            let attempt = {
                                let attempts =
                                    state.post_bodies.entry(request_id.clone()).or_default();
                                attempts.push(request.body);
                                attempts.len()
                            };
                            state
                                .shard_tokens
                                .entry(request_id.clone())
                                .or_default()
                                .push(shard);
                            attempt
                        };
                        if request_id == "req_retry" && attempt == 1 {
                            write_response(&mut stream, 503, &[("Retry-After", "0")], b"").unwrap();
                        } else if request_id == "req_404" {
                            write_response(&mut stream, 404, &[], b"").unwrap();
                        } else {
                            write_response(&mut stream, 200, &[], b"").unwrap();
                        }
                        continue;
                    }

                    write_response(&mut stream, 404, &[], b"").unwrap();
                }
            });

            Self {
                base_url: format!("http://{address}"),
                state,
                stop,
                thread: Some(thread),
            }
        }

        fn stop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut request_line = line.split_whitespace();
        let method = request_line.next().unwrap_or_default().to_owned();
        let path = request_line.next().unwrap_or_default().to_owned();

        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }

        let length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        Ok(HttpRequest {
            method,
            path,
            headers,
            body,
        })
    }

    fn write_response(
        stream: &mut TcpStream,
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> io::Result<()> {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            404 => "Not Found",
            503 => "Service Unavailable",
            _ => "Error",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )?;
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        write!(stream, "\r\n")?;
        stream.write_all(body)?;
        stream.flush()
    }

    fn poll_body(target: &Path) -> Vec<u8> {
        let mut commands = vec![serde_json::json!({
            "request_id": "req_retry",
            "shard_token": "shard_retry",
            "command_type": "jsonrpc",
            "channel": "main",
            "headers": {},
            "response_timeout": "5s",
            "jsonrpc": {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "replace_text",
                    "arguments": {
                        "path": target,
                        "old": "before",
                        "new": "after",
                        "expected_occurrences": 1
                    }
                }
            }
        })];
        commands.push(serde_json::json!({
            "request_id": "req_404",
            "shard_token": "shard_404",
            "command_type": "session_termination",
            "channel": "main",
            "response_timeout": "5s"
        }));
        commands.push(serde_json::json!({
            "request_id": "req_expired",
            "shard_token": "shard_expired",
            "command_type": "session_termination",
            "channel": "main",
            "response_timeout": "0s"
        }));
        for index in 0..13 {
            commands.push(serde_json::json!({
                "request_id": format!("req_{index}"),
                "shard_token": format!("shard_{index}"),
                "command_type": "session_termination",
                "channel": "main",
                "response_timeout": "5s"
            }));
        }
        assert_eq!(commands.len(), MAX_IN_FLIGHT);
        serde_json::to_vec(&serde_json::json!({"commands": commands})).unwrap()
    }

    #[test]
    fn terminal_retry_set_matches_protocol() {
        assert!(matches!(
            classify_post(StatusCode::OK.as_u16()),
            PostClass::Accepted
        ));
        assert!(matches!(
            classify_post(StatusCode::NOT_FOUND.as_u16()),
            PostClass::Terminal
        ));
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(matches!(classify_post(status.as_u16()), PostClass::Retry));
        }
        assert!(matches!(
            classify_post(StatusCode::UNAUTHORIZED.as_u16()),
            PostClass::Auth
        ));
        assert!(matches!(
            classify_post(StatusCode::BAD_REQUEST.as_u16()),
            PostClass::Fail
        ));
    }

    #[test]
    fn unsafe_control_plane_override_requires_explicit_opt_in() {
        assert_eq!(
            control_plane_base_url(None, false).unwrap(),
            DEFAULT_BASE_URL
        );
        assert_eq!(
            control_plane_base_url(Some(DEFAULT_BASE_URL), false).unwrap(),
            DEFAULT_BASE_URL
        );
        assert!(control_plane_base_url(Some("http://127.0.0.1:1"), false).is_err());
        assert_eq!(
            control_plane_base_url(Some("http://127.0.0.1:1"), true).unwrap(),
            "http://127.0.0.1:1"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_tunnel_e2e_retries_without_replaying_dispatch() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            env::temp_dir().join(format!("rivet-tunnel-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let target = root.join("state.txt");
        fs::write(&target, "before").unwrap();

        let config = Config::new(root.clone(), BTreeMap::new(), Vec::new(), false).unwrap();
        let runtime = Arc::new(Rivet::new(Arc::new(config)));
        let mut server = MockServer::start(poll_body(&target));
        let client = Client::new(
            server.base_url.clone(),
            TEST_TUNNEL_ID.to_owned(),
            "test-key".to_owned(),
        )
        .unwrap();

        client.validate().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), client.poll_loop(runtime.clone()))
            .await
            .expect("mock tunnel did not terminate")
            .unwrap_err();
        assert!(error.contains("HTTP 400"), "{error}");
        runtime.shutdown().await;
        server.stop();

        assert_eq!(fs::read_to_string(&target).unwrap(), "after");
        let state = server.state.lock().unwrap();
        let metadata = state.metadata_headers.as_ref().unwrap();
        assert_eq!(metadata.get("authorization").unwrap(), "Bearer test-key");
        assert_eq!(metadata.get("x-tunnel-client-name").unwrap(), "rivet");
        assert_eq!(
            metadata
                .get("x-tunnel-client-wire-protocol-version")
                .unwrap(),
            WIRE_VERSION
        );
        assert_eq!(
            metadata.get("x-tunnel-mcp-server-info").unwrap(),
            SERVER_INFO
        );
        assert!(
            metadata
                .get("x-tunnel-client-instance-id")
                .is_some_and(|value| value.starts_with("rivet-"))
        );
        assert!(
            state
                .first_poll_path
                .as_ref()
                .is_some_and(|path| path.contains("limit=16&timeout_ms=15000"))
        );

        let retry = &state.post_bodies["req_retry"];
        assert_eq!(retry.len(), 2);
        assert_eq!(
            retry[0], retry[1],
            "POST retry must reuse the terminal body"
        );
        assert_eq!(
            state.shard_tokens["req_retry"],
            ["shard_retry", "shard_retry"]
        );
        assert_eq!(state.post_bodies["req_404"].len(), 1);
        assert_eq!(state.shard_tokens["req_404"], ["shard_404"]);
        assert!(!state.post_bodies.contains_key("req_expired"));
        assert_eq!(state.post_bodies.len(), MAX_IN_FLIGHT - 1);

        drop(state);
        fs::remove_dir_all(root).unwrap();
    }
}
