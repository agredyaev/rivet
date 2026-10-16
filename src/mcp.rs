//! MCP tool request routing for the native tunnel runtime.
use crate::{commands::CommandRequest, config::Config, filesystem, process::ProcessTable, profile};
use rivet::mcp_wire::decode_client_message;
use rmcp::{
    RoleServer,
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
use serde_json::{Value, json, value::RawValue};
use std::{
    sync::{Arc, LazyLock, Mutex},
    time::Instant,
};

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
pub(crate) struct Rivet {
    config: Arc<Config>,
    processes: Arc<ProcessTable>,
    mutation: Arc<Mutex<()>>,
}

pub(crate) enum Dispatch {
    Response(Box<RawValue>),
    NotificationAck,
}

impl Rivet {
    pub(crate) fn new(config: Arc<Config>) -> Self {
        let processes = Arc::new(ProcessTable::new(&config));
        Self {
            config,
            processes,
            mutation: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_jsonrpc(&self, input: &[u8]) -> Result<Dispatch, String> {
        self.dispatch_jsonrpc_profiled(input, None).await
    }

    pub(crate) async fn dispatch_jsonrpc_profiled(
        &self,
        input: &[u8],
        profile_seq: Option<u64>,
    ) -> Result<Dispatch, String> {
        let profiling = profile::enabled();
        let total_started = profiling.then(Instant::now);
        let rss_before_bytes = profile::self_current_rss_bytes();
        let decode_started = profiling.then(Instant::now);
        let message: ClientJsonRpcMessage = decode_client_message(input)
            .map_err(|error| format!("invalid MCP JSON-RPC: {error}"))?;
        let decode_us = decode_started
            .as_ref()
            .map(|started| started.elapsed().as_micros());

        match message {
            ClientJsonRpcMessage::Request(request) => {
                let setup_started = profiling.then(Instant::now);
                let (transport, mut receiver) =
                    OneshotTransport::<RoleServer>::new(ClientJsonRpcMessage::Request(request));
                let service =
                    serve_directly::<RoleServer, _, _, _, _>(self.clone(), transport, None);
                let service_setup_us = setup_started
                    .as_ref()
                    .map(|started| started.elapsed().as_micros());

                let response_wait_started = profiling.then(Instant::now);
                let response = receiver
                    .recv()
                    .await
                    .ok_or("MCP service closed without a response")?;
                let response_wait_us = response_wait_started
                    .as_ref()
                    .map(|started| started.elapsed().as_micros());

                let serialize_started = profiling.then(Instant::now);
                let raw = serde_json::value::to_raw_value(&response)
                    .map_err(|error| error.to_string())?;
                let serialize_us = serialize_started
                    .as_ref()
                    .map(|started| started.elapsed().as_micros());

                let service_wait_started = profiling.then(Instant::now);
                let _ = service.waiting().await;
                let service_wait_us = service_wait_started
                    .as_ref()
                    .map(|started| started.elapsed().as_micros());

                profile::event(|| {
                    json!({
                        "event": "mcp_dispatch",
                        "seq": profile_seq,
                        "request_bytes": input.len(),
                        "response_bytes": raw.get().len(),
                        "decode_us": decode_us,
                        "service_setup_us": service_setup_us,
                        "response_wait_us": response_wait_us,
                        "serialize_us": serialize_us,
                        "service_wait_us": service_wait_us,
                        "total_us": total_started
                            .as_ref()
                            .map(|started| started.elapsed().as_micros()),
                        "self_rss_before_bytes": rss_before_bytes,
                        "self_rss_after_bytes": profile::self_current_rss_bytes(),
                        "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                        "children_peak_rss_bytes": profile::children_peak_rss_bytes()
                    })
                });
                Ok(Dispatch::Response(raw))
            }
            ClientJsonRpcMessage::Notification(_) => {
                profile::event(|| {
                    json!({
                        "event": "mcp_dispatch",
                        "seq": profile_seq,
                        "request_bytes": input.len(),
                        "response_bytes": 0,
                        "decode_us": decode_us,
                        "total_us": total_started
                            .as_ref()
                            .map(|started| started.elapsed().as_micros()),
                        "kind": "notification",
                        "self_peak_rss_bytes": profile::self_peak_rss_bytes()
                    })
                });
                Ok(Dispatch::NotificationAck)
            }
            _ => Err("unsupported inbound MCP JSON-RPC message".into()),
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.processes.shutdown().await;
    }
}

// Build the route table once instead of reconstructing it for each request.
static ROUTER: LazyLock<ToolRouter<Rivet>> = LazyLock::new(Rivet::tool_router);

const SERVER_INSTRUCTIONS: &str = "Batch independent operations. Submit long-running commands asynchronously and continue independent work instead of polling repeatedly. Use list_processes for process state and read_process_output only when captured output is required. Keep substantial work in bounded phases and return a concise checkpoint between phases. Do not build readiness queues, completion cursors, or custom polling protocols on top of Rivet.";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunCommandsRequest {
    commands: Vec<CommandRequest>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFilesRequest {
    files: Vec<filesystem::ReadRequest>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProcessOutputRequest {
    process_id: u64,
    stdout_offset: Option<u64>,
    stderr_offset: Option<u64>,
    limit: Option<usize>,
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
        result(Ok(
            json!({"roots": [self.config.root.path.display().to_string()]}),
        ))
    }

    #[tool(description = "List registered command names and resolved executables")]
    fn list_commands(&self) -> CallToolResult {
        let commands: Vec<_> = self
            .config
            .commands
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "executable": entry.path.display().to_string()
                })
            })
            .collect();
        result(Ok(json!({"commands": commands})))
    }

    #[tool(description = "Submit a registered command and return its managed process ID")]
    async fn run_command(&self, Parameters(request): Parameters<CommandRequest>) -> CallToolResult {
        result(self.processes.submit(&self.config, request).await)
    }

    #[tool(description = "Submit up to 8 independent registered commands in one MCP request.")]
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

        let batch_started = profile::enabled().then(Instant::now);
        let rss_before_bytes = profile::self_current_rss_bytes();
        let mut rows = Vec::with_capacity(request.commands.len());
        for command in request.commands {
            rows.push(match self.processes.submit(&self.config, command).await {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({
                    "ok": false,
                    "error": serde_json::to_value(error).expect("serializable error")
                }),
            });
        }

        profile::event(|| {
            json!({
                "event": "run_commands",
                "count": rows.len(),
                "total_us": batch_started.map(|started| started.elapsed().as_micros()),
                "self_rss_before_bytes": rss_before_bytes,
                "self_rss_after_bytes": profile::self_current_rss_bytes(),
                "self_peak_rss_bytes": profile::self_peak_rss_bytes(),
                "children_peak_rss_bytes": profile::children_peak_rss_bytes()
            })
        });
        result(Ok(json!({"results": rows})))
    }

    #[tool(description = "Start a registered interactive process and return its process ID")]
    async fn start_process(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> CallToolResult {
        result(self.processes.start(&self.config, request).await)
    }

    #[tool(
        description = "Return one snapshot of all retained running and completed managed processes"
    )]
    fn list_processes(&self) -> CallToolResult {
        result(Ok(self.processes.list(&self.config)))
    }

    #[tool(description = "Read bounded captured stdout and stderr for one process")]
    fn read_process_output(
        &self,
        Parameters(request): Parameters<ProcessOutputRequest>,
    ) -> CallToolResult {
        let max = self
            .config
            .limits
            .max_stdout_bytes
            .max(self.config.limits.max_stderr_bytes);
        result(self.processes.read(
            request.process_id,
            request.stdout_offset.unwrap_or(0),
            request.stderr_offset.unwrap_or(0),
            request.limit.unwrap_or(max),
            max,
        ))
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

    #[tool(description = "Read up to 8 bounded UTF-8 file slices in one MCP request")]
    async fn read_files(
        &self,
        Parameters(request): Parameters<ReadFilesRequest>,
    ) -> CallToolResult {
        if request.files.is_empty() || request.files.len() > 8 {
            return result(Err(Fault::new(
                "INVALID_REQUEST",
                "files must contain between 1 and 8 entries",
            )));
        }

        let max_total = self.config.limits.max_file_read_bytes;
        let total_limit = request.files.iter().try_fold(0_usize, |total, request| {
            total.checked_add(request.limit.unwrap_or(max_total))
        });
        if total_limit.is_none_or(|total| total > max_total) {
            return result(Err(Fault::new(
                "INVALID_REQUEST",
                "total read limit exceeds max_file_read_bytes",
            )));
        }

        let config = self.config.clone();
        result(
            tokio::task::spawn_blocking(move || {
                let rows = request
                    .files
                    .into_iter()
                    .map(|request| match filesystem::read(&config, request) {
                        Ok(value) => json!({"ok": true, "result": value}),
                        Err(error) => json!({
                            "ok": false,
                            "error": serde_json::to_value(error).expect("serializable error")
                        }),
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"results": rows}))
            })
            .await
            .unwrap_or_else(|_| Err(Fault::new("IO_ERROR", "file batch worker failed"))),
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
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("rivet", env!("CARGO_PKG_VERSION"))
                    .with_title("Rivet")
                    .with_description(
                        "Local MCP server for registered commands and the current workspace.",
                    )
                    .with_website_url("https://github.com/agredyaev/rivet"),
            )
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

#[cfg(test)]
mod direct_tests {
    use super::*;
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
        let config = Config::new(root.clone(), BTreeMap::new(), Vec::new(), false).unwrap();
        (Arc::new(config), root)
    }

    #[tokio::test]
    async fn direct_stateless_tools_list_uses_same_router() {
        let (config, root) = direct_config();
        let runtime = Rivet::new(config);
        let request = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let response = runtime.dispatch_jsonrpc(request).await.unwrap();
        let Dispatch::Response(raw) = response else {
            panic!("tools/list must return a terminal response");
        };
        let value: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        let tools = value["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "run_command"));
        assert!(tools.iter().any(|tool| tool["name"] == "read_files"));
        assert!(tools.iter().any(|tool| tool["name"] == "list_processes"));
        assert!(
            !tools
                .iter()
                .any(|tool| tool["name"] == "list_ready_processes")
        );
        runtime.shutdown().await;
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn direct_read_files_preserves_results_and_total_bound() {
        let (config, root) = direct_config();
        fs::write(root.join("a.txt"), "alpha").unwrap();
        fs::write(root.join("b.txt"), "bravo").unwrap();
        let runtime = Rivet::new(config);

        let request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "read_files",
                "arguments": {
                    "files": [
                        {"path": root.join("a.txt"), "offset": 0, "limit": 5},
                        {"path": root.join("b.txt"), "offset": 1, "limit": 3}
                    ]
                }
            }
        }))
        .unwrap();
        let response = runtime.dispatch_jsonrpc(&request).await.unwrap();
        let Dispatch::Response(raw) = response else {
            panic!("read_files must return a terminal response");
        };
        let value: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        let rows = value["result"]["structuredContent"]["results"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[0]["result"]["text"], "alpha");
        assert_eq!(rows[1]["ok"], true);
        assert_eq!(rows[1]["result"]["text"], "rav");

        let oversized = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "read_files",
                "arguments": {
                    "files": [
                        {"path": root.join("a.txt"), "limit": 3 * 1024 * 1024},
                        {"path": root.join("b.txt"), "limit": 3 * 1024 * 1024}
                    ]
                }
            }
        }))
        .unwrap();
        let response = runtime.dispatch_jsonrpc(&oversized).await.unwrap();
        let Dispatch::Response(raw) = response else {
            panic!("read_files bound failure must return a terminal response");
        };
        let value: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        assert_eq!(
            value["result"]["structuredContent"]["code"],
            "INVALID_REQUEST"
        );

        runtime.shutdown().await;
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn direct_notification_is_acknowledged_without_retained_session() {
        let (config, root) = direct_config();
        let runtime = Rivet::new(config);
        let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        assert!(matches!(
            runtime.dispatch_jsonrpc(notification).await.unwrap(),
            Dispatch::NotificationAck
        ));
        runtime.shutdown().await;
        fs::remove_dir_all(root).unwrap();
    }
}
