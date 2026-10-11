//! Direct OpenAI Secure MCP Tunnel transport for Rivet.
//!
//! Product scope is deliberately narrow: one tunnel, one stateless `main`
//! channel, one in-process Rivet MCP runtime.

use crate::{
    config::Config,
    mcp::{Dispatch, Runtime},
};
use bytes::Bytes;
use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER},
    redirect::Policy,
};
use rivet::tunnel_wire::{
    WireCommand, extract_jsonrpc_id, parse_command, parse_poll, parse_response_timeout,
    valid_tunnel_id,
};
use serde::Serialize;
use serde_json::{Value, value::RawValue};
use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::Arc,
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

#[derive(Clone)]
struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    http: reqwest::Client,
    base_url: String,
    tunnel_id: String,
}

enum PostClass {
    Accepted,
    Terminal,
    Retry,
    Auth,
    Fail,
}

impl Client {
    fn new(base_url: String, tunnel_id: String, api_key: String) -> Result<Self, String> {
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

        Ok(Self {
            inner: Arc::new(ClientInner {
                http,
                base_url: base_url.trim_end_matches('/').to_owned(),
                tunnel_id,
            }),
        })
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/v1/tunnels/{}{}",
            self.inner.base_url, self.inner.tunnel_id, suffix
        )
    }

    async fn validate(&self) -> Result<(), String> {
        let response = self
            .inner
            .http
            .get(self.url(""))
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

    async fn poll_loop(&self, runtime: Runtime) -> Result<(), String> {
        let mut in_flight = JoinSet::new();
        let mut failures = 0_u32;

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
            let response = match self
                .inner
                .http
                .get(format!(
                    "{}?limit={available}&timeout_ms={POLL_TIMEOUT_MS}",
                    self.url("/poll")
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
            let body = read_limited(response, MAX_POLL_BODY_BYTES).await?;
            let envelope =
                parse_poll(&body).map_err(|error| format!("invalid tunnel poll JSON: {error}"))?;

            // poll.limit is a hint, not a protocol execution cap. Every command
            // returned in this bounded poll body is admitted immediately.
            for raw in envelope.commands {
                let client = self.clone();
                let runtime = runtime.clone();
                in_flight
                    .spawn(async move { process_command(client, runtime, raw, received_at).await });
            }
        }
    }

    async fn post_terminal(
        &self,
        command: &WireCommand,
        body: Bytes,
        deadline: Option<Instant>,
    ) -> Result<(), String> {
        let mut attempt = 0_u32;
        loop {
            if expired(deadline) {
                return Ok(());
            }

            attempt = attempt.saturating_add(1);
            let request = self
                .inner
                .http
                .post(self.url("/response"))
                .header("X-Tunnel-Shard-Token", &command.shard_token)
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
                Ok(response) => match classify_post(response.status()) {
                    PostClass::Accepted | PostClass::Terminal => return Ok(()),
                    PostClass::Auth => {
                        return Err(format!(
                            "terminal response authorization failed with HTTP {}",
                            response.status()
                        ));
                    }
                    PostClass::Fail => {
                        return Err(format!(
                            "terminal response was rejected with HTTP {}",
                            response.status()
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
                },
                Err(error) => {
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
    client: Client,
    runtime: Runtime,
    raw: Box<RawValue>,
    received_at: Instant,
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

    let deadline = command
        .response_timeout
        .as_ref()
        .and_then(parse_response_timeout)
        .and_then(|duration| received_at.checked_add(duration));
    if expired(deadline) {
        return Ok(());
    }

    let terminal = match command.command_type.as_str() {
        "session_termination" => {
            encode_response(&command, None, 204, "session_termination_response")?
        }
        "jsonrpc" if command.channel != "main" => {
            synthetic_error(&command, "client_internal", 502)?
        }
        "jsonrpc" => match command.jsonrpc.as_deref() {
            Some(jsonrpc) => dispatch_jsonrpc(&runtime, &command, jsonrpc, deadline).await?,
            None => synthetic_error(&command, "protocol", 502)?,
        },
        _ => synthetic_error(&command, "protocol", 502)?,
    };

    if terminal.is_empty() || expired(deadline) {
        return Ok(());
    }
    client
        .post_terminal(&command, Bytes::from(terminal), deadline)
        .await
}

async fn dispatch_jsonrpc(
    runtime: &Runtime,
    command: &WireCommand,
    jsonrpc: &RawValue,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, String> {
    // The tunnel headers remain preserved in WireCommand. The Rivet backend is
    // in-process and stateless, so no downstream HTTP header hop is created.
    let _ = &command.headers;
    let future = runtime.dispatch_jsonrpc(jsonrpc.get().as_bytes());
    let result = if let Some(deadline) = deadline {
        match timeout_at(deadline, future).await {
            Ok(result) => result,
            Err(_) => return Ok(Vec::new()),
        }
    } else {
        future.await
    };

    match result {
        Ok(Dispatch::Response(bytes)) => {
            let text = String::from_utf8(bytes).map_err(|_| "MCP response is not UTF-8")?;
            let raw = RawValue::from_string(text).map_err(|error| error.to_string())?;
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
    let raw = RawValue::from_string(value.to_string()).map_err(|error| error.to_string())?;
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
        channel: &command.channel,
        resp_json: jsonrpc,
        resp_headers: jsonrpc.map(|_| ResponseHeaders {
            content_type: ["application/json"],
        }),
        resp_code: status,
        resp_type: response_type,
    })
    .map_err(|error| error.to_string())
}

fn classify_post(status: StatusCode) -> PostClass {
    match status {
        StatusCode::OK => PostClass::Accepted,
        StatusCode::NOT_FOUND => PostClass::Terminal,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => PostClass::Auth,
        StatusCode::REQUEST_TIMEOUT
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => PostClass::Retry,
        _ => PostClass::Fail,
    }
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
    let shift = attempt.saturating_sub(1).min(6);
    let base = 200_u64.saturating_mul(1_u64 << shift).min(10_000);
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

fn prompt_line(prompt: &str) -> Result<String, String> {
    print!("  {prompt}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| error.to_string())?;
    Ok(value.trim().to_owned())
}

fn secret(prompt: &str) -> Result<String, String> {
    print!("  {prompt}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    {
        use std::os::fd::AsRawFd;

        let stdin = io::stdin();
        let fd = stdin.as_raw_fd();
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: fd is the live stdin terminal descriptor and `original` points
        // to valid writable storage for one termios value.
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(format!(
                "could not read terminal settings: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: tcgetattr succeeded and initialized the value.
        let original = unsafe { original.assume_init() };
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        // SAFETY: both fd and the termios pointer are valid for this call.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
            return Err(format!(
                "could not disable terminal echo: {}",
                io::Error::last_os_error()
            ));
        }

        let mut value = String::new();
        let read = stdin
            .read_line(&mut value)
            .map_err(|error| error.to_string());
        // Restore terminal state even when the read itself failed.
        // SAFETY: fd and the saved termios value remain valid here.
        let restore = unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &original) };
        println!();
        if restore != 0 {
            return Err(format!(
                "could not restore terminal echo: {}",
                io::Error::last_os_error()
            ));
        }
        read?;
        Ok(value.trim().to_owned())
    }
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

fn credentials() -> Result<(String, String, String), String> {
    let interactive = io::stdin().is_terminal();
    let tunnel_id = match env::var("CONTROL_PLANE_TUNNEL_ID") {
        Ok(value) if !value.is_empty() => value,
        _ if interactive => prompt_line("Tunnel ID: ")?,
        _ => return Err("CONTROL_PLANE_TUNNEL_ID is required without a terminal".into()),
    };
    if !valid_tunnel_id(&tunnel_id) {
        return Err("invalid tunnel ID format".into());
    }
    let api_key = match env::var("CONTROL_PLANE_API_KEY") {
        Ok(value) if !value.is_empty() => value,
        _ if interactive => secret("Runtime API key (input hidden): ")?,
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
    let (base_url, tunnel_id, api_key) = credentials()?;
    let client = Client::new(base_url, tunnel_id, api_key)?;
    let runtime = Runtime::new(config);

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
        assert!(matches!(classify_post(StatusCode::OK), PostClass::Accepted));
        assert!(matches!(
            classify_post(StatusCode::NOT_FOUND),
            PostClass::Terminal
        ));
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(matches!(classify_post(status), PostClass::Retry));
        }
        assert!(matches!(
            classify_post(StatusCode::UNAUTHORIZED),
            PostClass::Auth
        ));
        assert!(matches!(
            classify_post(StatusCode::BAD_REQUEST),
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

        let config = Config::native_session(vec![root.clone()], BTreeMap::new(), false).unwrap();
        let runtime = Runtime::new(Arc::new(config));
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
