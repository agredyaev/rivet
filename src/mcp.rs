//! MCP stdio adapter and tool request routing.
//!
//! Handlers delegate command, filesystem, and process policy to their modules
//! and convert the results into MCP responses.
use crate::{commands::CommandRequest, config::Config, filesystem, process::ProcessTable};
use rmcp::{
    ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
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

// Build the route table once instead of reconstructing it for each request.
static ROUTER: LazyLock<ToolRouter<Rivet>> = LazyLock::new(Rivet::tool_router);

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
        description = "Submit a registered command and return a managed process handle immediately"
    )]
    fn run_command(&self, Parameters(request): Parameters<CommandRequest>) -> CallToolResult {
        result(self.processes.run(self.config.clone(), request))
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

    #[tool(description = "Read bounded process stdout and stderr from byte offsets")]
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
    let processes = Arc::new(ProcessTable::new());
    let server = Rivet {
        config: config.clone(),
        processes: processes.clone(),
        mutation: Arc::new(Mutex::new(())),
    };
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
    let service = server
        .serve((input, tokio::io::stdout()))
        .await
        .map_err(|e| e.to_string())?;
    let ended = service.waiting().await.map_err(|e| e.to_string());
    processes.shutdown().await;
    ended.map(|_| ())
}
