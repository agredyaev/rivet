//! MCP stdio adapter and tool request routing.
//!
//! Handlers delegate command, filesystem, and process policy to their modules
//! and convert the results into MCP responses.
use crate::{commands::CommandRequest, config::Config, filesystem, process::ProcessTable};
use rmcp::{
    RoleServer, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ClientJsonRpcMessage, ContentBlock, Implementation, ServerCapabilities,
        ServerConfig,
    },
    schemars::JsonSchema,
    service::serve_directly,
    tool, tool_handler, tool_router,
    transport::OneshotTransport,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

#[derive(Debug, Serialize)]
pub struct Fault {
    code: &'static str,
    message: String,
    details: Value,
}
impl Fault {
    pub fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }
    pub fn with(code: &'static str, message: &str, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}

fn result(value: Result<Value, Fault>) -> CallToolResult {
    match value {
        Ok(value) => {
            let mut output = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
            output.structured_content = Some(value);
            output
        }
        Err(error) => {
            let value = serde_json::to_value(error).expect("serializable error");
            let mut output = CallToolResult::error(vec![ContentBlock::text(value.to_string())]);
            output.structured_content = Some(value);
            output
        }
    }
}

#[derive(Clone)]
struct Rivet {
    config: Arc<Config>,
    processes: Arc<ProcessTable>,
    mutation: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub(crate) struct Runtime {
    server: Arc<Rivet>,
    processes: Arc<ProcessTable>,
}

pub(crate) enum Dispatch {
    Response(Vec<u8>),
    NotificationAck,
}

impl Runtime {
    pub(crate) fn new(config: Arc<Config>) -> Self {
        let processes = Arc::new(ProcessTable::new());
        Self {
            server: Arc::new(Rivet {
                config,
                processes: processes.clone(),
                mutation: Arc::new(Mutex::new(())),
            }),
            processes,
        }
    }

    pub(crate) async fn dispatch_jsonrpc(&self, input: &[u8]) -> Result<Dispatch, String> {
        let message: ClientJsonRpcMessage = serde_json::from_slice(input)
            .map_err(|error| format!("invalid MCP JSON-RPC: {error}"))?;
        match message {
            ClientJsonRpcMessage::Request(request) => {
                let (transport, mut receiver) =
                    OneshotTransport::<RoleServer>::new(ClientJsonRpcMessage::Request(request));
                let service =
                    serve_directly::<RoleServer, _, _, _, _>(self.server.clone(), transport, None);
                let response = receiver
                    .recv()
                    .await
                    .ok_or("MCP service closed without a response")?;
                let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
                let _ = service.waiting().await;
                Ok(Dispatch::Response(bytes))
            }
            ClientJsonRpcMessage::Notification(_) => Ok(Dispatch::NotificationAck),
            _ => Err("unsupported inbound MCP JSON-RPC message".into()),
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.processes.shutdown().await;
    }
}

// Build the route table once instead of reconstructing it for each request.
static ROUTER: LazyLock<ToolRouter<Rivet>> = LazyLock::new(Rivet::tool_router);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunCommandRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
    timeout_ms: Option<u64>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    wait_ms: Option<u64>,
    output_limit: Option<usize>,
}

impl RunCommandRequest {
    fn into_command(self) -> CommandRequest {
        CommandRequest {
            command: self.command,
            args: self.args,
            cwd: self.cwd,
            timeout_ms: self.timeout_ms,
            env: self.env,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunCommandsRequest {
    commands: Vec<CommandRequest>,
    wait_ms: Option<u64>,
    output_limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProcessOutputRequest {
    process_id: u64,
    stdout_offset: Option<u64>,
    stderr_offset: Option<u64>,
    limit: Option<usize>,
    wait_ms: Option<u64>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadyProcessesRequest {
    after_sequence: Option<u64>,
    limit: Option<usize>,
    output_limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProcessInputRequest {
    process_id: u64,
    text: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProcessStopRequest {
    process_id: u64,
}

#[tool_router]
impl Rivet {
    #[tool(description = "List effective allowed roots for file paths and command cwd")]
    fn list_roots(&self) -> CallToolResult {
        let roots: Vec<_> = self
            .config
            .roots
            .iter()
            .map(|root| root.path.display().to_string())
            .collect();
        result(Ok(json!({"roots": roots})))
    }

    #[tool(description = "List registered commands and their argument authorization")]
    fn list_commands(&self) -> CallToolResult {
        let commands: Vec<_> = self
            .config
            .commands
            .iter()
            .map(|(name, entry)| {
                if entry.allow_any_args {
                    json!({"name": name, "allow_any_args": true})
                } else {
                    json!({"name": name, "allowed_subcommands": entry.allowed_subcommands})
                }
            })
            .collect();
        result(Ok(json!({"commands": commands})))
    }

    #[tool(
        description = "Run a registered command. Fast commands complete inline; slower commands return a managed process handle."
    )]
    async fn run_command(
        &self,
        Parameters(request): Parameters<RunCommandRequest>,
    ) -> CallToolResult {
        let max_output = self
            .config
            .limits
            .max_stdout_bytes
            .max(self.config.limits.max_stderr_bytes);
        let wait_ms = request.wait_ms.unwrap_or(250);
        if wait_ms > 5_000 {
            return result(Err(Fault::new(
                "INVALID_REQUEST",
                "wait_ms must not exceed 5000",
            )));
        }
        let output_limit = match request.output_limit {
            Some(value) if value == 0 || value > max_output => {
                return result(Err(Fault::new(
                    "INVALID_REQUEST",
                    "output_limit is outside configured bounds",
                )));
            }
            Some(value) => value,
            None => (64 * 1024).min(max_output),
        };
        result(
            self.processes
                .submit(
                    self.config.clone(),
                    request.into_command(),
                    wait_ms,
                    output_limit,
                )
                .await,
        )
    }

    #[tool(
        description = "Run up to 8 independent registered commands concurrently inside one MCP request."
    )]
    async fn run_commands(
        &self,
        Parameters(request): Parameters<RunCommandsRequest>,
    ) -> CallToolResult {
        if request.commands.is_empty() || request.commands.len() > 8 {
            return result(Err(Fault::new(
                "INVALID_REQUEST",
                "commands must contain between 1 and 8 entries",
            )));
        }
        let max_output = self
            .config
            .limits
            .max_stdout_bytes
            .max(self.config.limits.max_stderr_bytes);
        let wait_ms = request.wait_ms.unwrap_or(250);
        if wait_ms > 5_000 {
            return result(Err(Fault::new(
                "INVALID_REQUEST",
                "wait_ms must not exceed 5000",
            )));
        }
        let output_limit = match request.output_limit {
            Some(value) if value == 0 || value > max_output => {
                return result(Err(Fault::new(
                    "INVALID_REQUEST",
                    "output_limit is outside configured bounds",
                )));
            }
            Some(value) => value,
            None => (64 * 1024).min(max_output),
        };
        let count = request.commands.len();
        let mut tasks = tokio::task::JoinSet::new();
        for (index, command) in request.commands.into_iter().enumerate() {
            let processes = self.processes.clone();
            let config = self.config.clone();
            tasks.spawn(async move {
                (
                    index,
                    processes
                        .submit(config, command, wait_ms, output_limit)
                        .await,
                )
            });
        }
        let mut rows = vec![Value::Null; count];
        while let Some(joined) = tasks.join_next().await {
            let (index, value) = match joined {
                Ok(value) => value,
                Err(_) => {
                    return result(Err(Fault::new("IO_ERROR", "command worker failed")));
                }
            };
            rows[index] = match value {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({
                    "ok": false,
                    "error": serde_json::to_value(error).expect("serializable error")
                }),
            };
        }
        result(Ok(json!({"results": rows})))
    }

    #[tool(description = "Start a registered long-running process")]
    fn start_process(&self, Parameters(request): Parameters<CommandRequest>) -> CallToolResult {
        result(self.processes.start(self.config.clone(), request))
    }

    #[tool(
        description = "List retained managed processes for recovery after an interrupted request"
    )]
    fn list_processes(&self) -> CallToolResult {
        result(Ok(self.processes.list()))
    }

    #[tool(
        description = "List completed managed processes by completion sequence without consuming them"
    )]
    fn list_ready_processes(
        &self,
        Parameters(request): Parameters<ReadyProcessesRequest>,
    ) -> CallToolResult {
        let max_output = self
            .config
            .limits
            .max_stdout_bytes
            .max(self.config.limits.max_stderr_bytes);
        result(
            self.processes.ready(
                request.after_sequence.unwrap_or(0),
                request
                    .limit
                    .unwrap_or(self.config.limits.max_running_processes),
                request.output_limit.unwrap_or(0),
                self.config.limits.max_running_processes,
                max_output,
            ),
        )
    }

    #[tool(
        description = "Read bounded process output, optionally waiting for new output or completion."
    )]
    async fn read_process_output(
        &self,
        Parameters(request): Parameters<ProcessOutputRequest>,
    ) -> CallToolResult {
        let max = self
            .config
            .limits
            .max_stdout_bytes
            .max(self.config.limits.max_stderr_bytes);
        result(
            self.processes
                .read_wait(
                    request.process_id,
                    request.stdout_offset.unwrap_or(0),
                    request.stderr_offset.unwrap_or(0),
                    request.limit.unwrap_or(max),
                    max,
                    request.wait_ms.unwrap_or(0),
                )
                .await,
        )
    }

    #[tool(description = "Send bounded UTF-8 text to process stdin")]
    async fn send_process_input(
        &self,
        Parameters(request): Parameters<ProcessInputRequest>,
    ) -> CallToolResult {
        result(
            self.processes
                .input(
                    request.process_id,
                    &request.text,
                    self.config.limits.max_file_read_bytes,
                    self.config.limits.default_timeout_ms,
                )
                .await,
        )
    }

    #[tool(description = "Stop a process and its process group")]
    async fn stop_process(
        &self,
        Parameters(request): Parameters<ProcessStopRequest>,
    ) -> CallToolResult {
        result(self.processes.stop(request.process_id).await)
    }

    #[tool(description = "List a bounded number of directory entries within allowed roots")]
    async fn list_directory(
        &self,
        Parameters(request): Parameters<filesystem::ListRequest>,
    ) -> CallToolResult {
        let config = self.config.clone();
        result(
            tokio::task::spawn_blocking(move || filesystem::list(&config, request))
                .await
                .unwrap_or_else(|_| Err(Fault::new("IO_ERROR", "directory worker failed"))),
        )
    }

    #[tool(description = "Read bounded UTF-8 text from an allowed absolute path")]
    async fn read_file(
        &self,
        Parameters(request): Parameters<filesystem::ReadRequest>,
    ) -> CallToolResult {
        let config = self.config.clone();
        result(
            tokio::task::spawn_blocking(move || filesystem::read(&config, request))
                .await
                .unwrap_or_else(|_| Err(Fault::new("IO_ERROR", "file worker failed"))),
        )
    }

    #[tool(description = "Create, overwrite, or append bounded UTF-8 text")]
    async fn write_file(
        &self,
        Parameters(request): Parameters<filesystem::WriteRequest>,
    ) -> CallToolResult {
        let config = self.config.clone();
        let lock = self.mutation.clone();
        result(
            tokio::task::spawn_blocking(move || {
                let _guard = lock.lock().unwrap();
                filesystem::write(&config, request)
            })
            .await
            .unwrap_or_else(|_| Err(Fault::new("IO_ERROR", "file worker failed"))),
        )
    }

    #[tool(description = "Replace exact text with an expected non-overlapping match count")]
    async fn replace_text(
        &self,
        Parameters(request): Parameters<filesystem::ReplaceRequest>,
    ) -> CallToolResult {
        let config = self.config.clone();
        let lock = self.mutation.clone();
        result(
            tokio::task::spawn_blocking(move || {
                let _guard = lock.lock().unwrap();
                filesystem::replace(&config, request)
            })
            .await
            .unwrap_or_else(|_| Err(Fault::new("IO_ERROR", "file worker failed"))),
        )
    }
}

#[tool_handler(router = ROUTER)]
impl rmcp::ServerHandler for Rivet {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("rivet", env!("CARGO_PKG_VERSION"))
                .with_title("Rivet")
                .with_description(
                    "Local MCP server for configured commands and allowed filesystem roots.",
                )
                .with_website_url("https://github.com/agredyaev/rivet"),
        )
    }
}

struct LineLimited<R> {
    inner: R,
    bytes: usize,
    cap: usize,
}
impl<R: AsyncRead + Unpin> AsyncRead for LineLimited<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut chunk = [0_u8; 8192];
        let size = chunk
            .len()
            .min(output.remaining())
            .min(self.cap.saturating_sub(self.bytes).saturating_add(1));
        let mut input = ReadBuf::new(&mut chunk[..size]);
        match Pin::new(&mut self.inner).poll_read(cx, &mut input) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {
                let read = input.filled();
                for byte in read {
                    self.bytes += 1;
                    if self.bytes > self.cap {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "MCP input line exceeds configured cap",
                        )));
                    }
                    if *byte == b'\n' {
                        self.bytes = 0;
                    }
                }
                output.put_slice(read);
                Poll::Ready(Ok(()))
            }
        }
    }
}

pub async fn serve(config: Arc<Config>) -> Result<(), String> {
    let runtime = Runtime::new(config.clone());
    let cap = config
        .limits
        .max_file_read_bytes
        .saturating_mul(6)
        .saturating_add(1_048_576)
        .min(64 * 1024 * 1024);
    let input = LineLimited {
        inner: tokio::io::stdin(),
        bytes: 0,
        cap,
    };
    let service = runtime
        .server
        .as_ref()
        .clone()
        .serve((input, tokio::io::stdout()))
        .await
        .map_err(|e| e.to_string())?;
    let ended = service.waiting().await.map_err(|e| e.to_string());
    runtime.shutdown().await;
    ended.map(|_| ())
}

#[cfg(test)]
mod direct_tests {
    use super::*;
    use crate::config::CommandConfig;
    use std::{
        collections::BTreeMap,
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn direct_config() -> (Arc<Config>, std::path::PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "rivet-direct-mcp-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let config_path = root.join("rivet.toml");
        fs::write(
            &config_path,
            r#"[limits]
max_stdout_bytes = 1024
max_stderr_bytes = 1024
max_file_read_bytes = 4096
max_directory_entries = 64
max_running_processes = 4
default_timeout_ms = 1000
max_timeout_ms = 10000
[environment]
pass = ["PATH"]
allow_override = []
"#,
        )
        .unwrap();
        let config = Config::load(
            &config_path,
            vec![root.clone()],
            BTreeMap::<String, CommandConfig>::new(),
        )
        .unwrap();
        (Arc::new(config), root)
    }

    #[tokio::test]
    async fn direct_stateless_tools_list_uses_same_router() {
        let (config, root) = direct_config();
        let runtime = Runtime::new(config);
        let request = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let response = runtime.dispatch_jsonrpc(request).await.unwrap();
        let Dispatch::Response(bytes) = response else {
            panic!("tools/list must return a terminal response");
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let tools = value["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "run_command"));
        assert!(tools.iter().any(|tool| tool["name"] == "run_commands"));
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "list_ready_processes")
        );
        runtime.shutdown().await;
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn direct_notification_is_acknowledged_without_retained_session() {
        let (config, root) = direct_config();
        let runtime = Runtime::new(config);
        let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        assert!(matches!(
            runtime.dispatch_jsonrpc(notification).await.unwrap(),
            Dispatch::NotificationAck
        ));
        runtime.shutdown().await;
        fs::remove_dir_all(root).unwrap();
    }
}
