//! McpHandler — receives raw JSON messages from any transport and dispatches
//! to the correct MCP method handler or tool executor.

use super::error::{extract_error_kind, ToolErrorKind};
use super::protocol::*;
use super::server::McpServerState;
use crate::observability::{
    default_calls_log_path, new_call_id, unix_ms, CallObserver, CallRecord, CallStatus,
};
use crate::router::{meta_tools, ToolRouter};
use axum::response::sse::Event;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Clone-able handle to the MCP request handler.
/// Multiple transports (STDIO + HTTP) share the same handler.
#[derive(Clone)]
pub struct McpHandler {
    ctx: Arc<crate::tools::ToolContext>,
    sse_senders: Arc<RwLock<Vec<mpsc::Sender<Event>>>>,
    /// Raw-JSON-line notification sinks for non-SSE transports (stdio). A
    /// server-initiated notification (e.g. tools/list_changed) must reach the
    /// active transport; SSE senders only cover HTTP, so stdio registers here
    /// to receive the same notifications. Without this, notifications are
    /// silently dropped on stdio — the cause of issue #19.
    notif_sinks: Arc<RwLock<Vec<mpsc::Sender<String>>>>,
    observer: CallObserver,
}

impl McpHandler {
    pub async fn new(config: crate::tools::ServerConfig) -> anyhow::Result<Self> {
        let router = Arc::new(ToolRouter::new());

        // Load only the starter kit at startup so baseline `tools/list` stays small
        // (~2K tokens, not ~23K). The LLM expands on demand via `load_toolset`.
        router.load_starter_kit().await;

        let observer = CallObserver::new(Some(default_calls_log_path()));
        let ctx = Arc::new(crate::tools::ToolContext::new_with_observer(
            config,
            router,
            observer.clone(),
        ));

        Ok(McpHandler {
            ctx,
            sse_senders: Arc::new(RwLock::new(Vec::new())),
            notif_sinks: Arc::new(RwLock::new(Vec::new())),
            observer,
        })
    }

    /// Accessor for the `CallObserver` — used by meta-tools `get_recent_calls`
    /// and `server_stats` that live on `ToolContext`.
    pub fn observer(&self) -> &CallObserver {
        &self.observer
    }

    pub async fn register_sse_sender(&self, tx: mpsc::Sender<Event>) {
        self.sse_senders.write().await.push(tx);
    }

    /// Register a raw-JSON-line notification sink (used by the stdio transport).
    /// Each server-initiated notification is delivered here as a serialized
    /// JSON-RPC string, which the transport writes to its output stream.
    pub async fn register_notification_sink(&self, tx: mpsc::Sender<String>) {
        self.notif_sinks.write().await.push(tx);
    }

    /// Process one JSON-RPC message and return an optional response.
    /// Returns `None` for notifications (no response required).
    pub async fn handle_message(&self, msg: Value) -> Option<JsonRpcResponse> {
        // Distinguish request (has "method") from response (has "result"/"error")
        msg.get("method")?;

        let req: JsonRpcRequest = match serde_json::from_value(msg) {
            Ok(r) => r,
            Err(e) => {
                return Some(JsonRpcResponse::error(
                    Value::Null,
                    JsonRpcError {
                        code: INVALID_REQUEST,
                        message: format!("Invalid request: {}", e),
                        data: None,
                    },
                ));
            }
        };

        let id = req.id.clone().unwrap_or(Value::Null);
        debug!("Handling method: {}", req.method);

        let result = self.dispatch(&req).await;

        match result {
            Ok(None) => None, // notification — no response
            Ok(Some(val)) => Some(JsonRpcResponse::success(id, val)),
            Err(e) => Some(JsonRpcResponse::error(
                id,
                JsonRpcError {
                    code: INTERNAL_ERROR,
                    message: e.to_string(),
                    data: None,
                },
            )),
        }
    }

    async fn dispatch(&self, req: &JsonRpcRequest) -> anyhow::Result<Option<Value>> {
        match req.method.as_str() {
            // ── Lifecycle ──────────────────────────────────────────────────
            "initialize" => {
                let result = McpServerState::build_initialize_result();
                Ok(Some(serde_json::to_value(result)?))
            }
            "notifications/initialized" => Ok(None),
            "ping" => Ok(Some(json!({}))),

            // ── Tool listing ───────────────────────────────────────────────
            "tools/list" => {
                // Meta-tools (always visible) + all domain tools (pre-loaded at startup)
                let mut tools = meta_tools::meta_tool_descriptions();
                for def in self.ctx.router.active_tools().await {
                    tools.push(def.to_mcp_description());
                }
                let result = ListToolsResult {
                    tools,
                    next_cursor: None,
                };
                Ok(Some(serde_json::to_value(result)?))
            }

            // ── Tool execution ─────────────────────────────────────────────
            "tools/call" => {
                let params: CallToolParams =
                    serde_json::from_value(req.params.clone().unwrap_or(Value::Null))?;

                let call_result = self.execute_tool(&params).await;
                Ok(Some(serde_json::to_value(call_result)?))
            }

            // ── Unimplemented MCP methods ──────────────────────────────────
            "resources/list" | "resources/read" => Ok(Some(json!({ "resources": [] }))),
            "prompts/list" => Ok(Some(json!({ "prompts": [] }))),

            method => {
                warn!("Unknown method: {}", method);
                Err(anyhow::anyhow!("Method not found: {}", method))
            }
        }
    }

    async fn execute_tool(&self, params: &CallToolParams) -> CallToolResult {
        let args = params.arguments.clone().unwrap_or(json!({}));
        let call_id = new_call_id();
        let started = Instant::now();
        let ts = unix_ms();

        // Pre-compute the owning toolset (if any) once for the call record.
        let toolset = self
            .ctx
            .router
            .find_toolset_for_tool(&params.name)
            .map(str::to_string);

        let args_bytes = serde_json::to_string(&args).map(|s| s.len()).unwrap_or(0);

        info!(
            call_id = %call_id,
            tool = %params.name,
            toolset = toolset.as_deref().unwrap_or("-"),
            "tool_call_start"
        );

        let (result, status, error_kind) = self.dispatch_tool(&params.name, &args).await;

        let dur_ms = started.elapsed().as_millis() as u64;
        let result_bytes = result_content_bytes(&result);

        info!(
            call_id = %call_id,
            tool = %params.name,
            status = %status.as_str(),
            dur_ms = dur_ms,
            "tool_call_end"
        );

        self.observer
            .record(CallRecord {
                call_id,
                ts,
                tool: params.name.clone(),
                toolset,
                dur_ms,
                status,
                error_kind,
                args_bytes,
                result_bytes,
            })
            .await;

        result
    }

    /// Core dispatch: meta-tool → loaded domain tool → actionable error.
    /// Returns the outcome triple so `execute_tool` can record it.
    async fn dispatch_tool(
        &self,
        name: &str,
        args: &Value,
    ) -> (CallToolResult, CallStatus, Option<String>) {
        // Meta-tools always win.
        if let Some(result) = meta_tools::handle_meta_tool(name, args, &self.ctx).await {
            if name == "load_toolset" || name == "unload_toolset" {
                self.notify_tools_list_changed().await;
            }
            let status = if result.is_error {
                CallStatus::Error
            } else {
                CallStatus::Ok
            };
            return (result, status, None);
        }

        // Loaded domain tool?
        if let Some(tool_def) = self.ctx.router.get_tool(name).await {
            // Contain a panicking handler. Without this the unwind escapes
            // `handle_message`, leaves `run_stdio`, and takes the process with
            // it — the client sees the transport die with no diagnostic, and
            // every other loaded tool dies with it. One bad call should cost
            // one failed call.
            //
            // Runs on a spawned task rather than inline: tokio turns a panic
            // there into a `JoinError` instead of letting it unwind. That
            // needs no new dependency, and file writes are atomic
            // (tmp -> fsync -> rename), so an aborted call cannot leave a
            // half-written file behind.
            //
            // This cannot catch a stack overflow — that aborts the process and
            // no guard intercepts it. See the iterative union-find in
            // `tools::sch_analysis::NetGraph::find`.
            let call = (tool_def.handler)(args, self.ctx.clone());

            let outcome = match tokio::spawn(call).await {
                Ok(r) => r,
                Err(join_err) => {
                    let detail = join_err
                        .try_into_panic()
                        .ok()
                        .and_then(|p| {
                            p.downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .or_else(|| p.downcast_ref::<String>().cloned())
                        })
                        .unwrap_or_else(|| "unknown panic payload".to_string());

                    tracing::error!(tool = %name, panic = %detail, "tool handler panicked");

                    let kind = ToolErrorKind::HandlerError {
                        reason: format!("handler panicked: {detail}"),
                    };
                    return (
                        CallToolResult::error_kind(
                            kind,
                            format!(
                                "Tool '{name}' panicked: {detail}. This is a bug in the tool; \
                                 the server is still running and other tools are unaffected."
                            ),
                        ),
                        CallStatus::Error,
                        Some("handler_error".to_string()),
                    );
                }
            };

            return match outcome {
                Ok(result) => {
                    let status = if result.is_error {
                        CallStatus::Error
                    } else {
                        CallStatus::Ok
                    };
                    // Structured errors carry their own kind in the body; plain-text
                    // errors fall back to "handler_error" via extract_error_kind.
                    let error_kind = extract_error_kind(&result);
                    (result, status, error_kind)
                }
                Err(e) => {
                    warn!(tool = %name, error = %e, "tool handler returned anyhow::Error");
                    let kind = ToolErrorKind::HandlerError {
                        reason: e.to_string(),
                    };
                    (
                        CallToolResult::error_kind(kind, format!("Tool error: {}", e)),
                        CallStatus::Error,
                        Some("handler_error".to_string()),
                    )
                }
            };
        }

        // Not loaded — try to give an actionable hint.
        match self.ctx.router.find_toolset_for_tool(name) {
            Some(toolset) => {
                let kind = ToolErrorKind::ToolsetNotLoaded {
                    toolset: toolset.to_string(),
                    tool: name.to_string(),
                };
                let msg = format!(
                    "Tool '{}' is in toolset '{}' which is not currently loaded. \
                     Call load_toolset('{}') first, then retry.",
                    name, toolset, toolset
                );
                (
                    CallToolResult::error_kind(kind, msg),
                    CallStatus::NotFound,
                    Some("toolset_not_loaded".to_string()),
                )
            }
            None => {
                let kind = ToolErrorKind::UnknownTool {
                    tool: name.to_string(),
                };
                let msg = format!(
                    "Tool '{}' not found. Use list_toolboxes() to see available toolsets.",
                    name
                );
                (
                    CallToolResult::error_kind(kind, msg),
                    CallStatus::NotFound,
                    Some("unknown_tool".to_string()),
                )
            }
        }
    }

    async fn notify_tools_list_changed(&self) {
        let notification = JsonRpcNotification::new(TOOLS_LIST_CHANGED, None);
        let Ok(json) = serde_json::to_string(&notification) else {
            return;
        };

        // HTTP/SSE clients: wrap the JSON in an SSE event. (Unchanged path.)
        {
            let event = Event::default().data(json.clone());
            let mut senders = self.sse_senders.write().await;
            senders.retain(|tx| tx.try_send(event.clone()).is_ok());
        }

        // stdio (and any other raw-line transport): deliver the JSON directly.
        // try_send is non-blocking, so emitting a notification from inside
        // request handling can never block on a full channel and deadlock the
        // request that triggered it; a dropped sink is pruned like the SSE case.
        {
            let mut sinks = self.notif_sinks.write().await;
            sinks.retain(|tx| tx.try_send(json.clone()).is_ok());
        }
    }
}

/// Sum of content bytes in a `CallToolResult` — used for observability size
/// accounting. Images are counted by their (already-base64-encoded) data len,
/// which matches what the client sees over the wire.
fn result_content_bytes(result: &CallToolResult) -> usize {
    result
        .content
        .iter()
        .map(|c| match c {
            ToolContent::Text { text } => text.len(),
            ToolContent::Image { data, .. } => data.len(),
        })
        .sum()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod panic_boundary_tests {
    use super::*;
    use crate::tools::{ServerConfig, ToolDef};

    fn test_config() -> ServerConfig {
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
        }
    }

    /// A tool whose handler panics part-way through, as a buggy handler would.
    fn panicking_tool() -> ToolDef {
        let handler: crate::tools::ToolHandlerFn = Arc::new(|_args, _ctx| {
            Box::pin(async move { panic!("deliberate panic from a tool handler") })
        });
        ToolDef {
            name: "panicking_test_tool",
            description: "test-only tool that panics",
            input_schema: json!({ "type": "object" }),
            handler,
        }
    }

    /// A panicking handler used to unwind out of `handle_message`, out of the
    /// stdio loop and out of `main`, killing the server and every other loaded
    /// tool with it. It must now be contained as one failed call.
    #[tokio::test]
    async fn panicking_handler_is_contained_as_a_tool_error() {
        let handler = McpHandler::new(test_config()).await.unwrap();
        handler
            .ctx
            .router
            .insert_tool_for_test(panicking_tool())
            .await;

        let result = handler
            .execute_tool(&CallToolParams {
                name: "panicking_test_tool".to_string(),
                arguments: Some(json!({})),
            })
            .await;

        assert!(result.is_error, "a panic must surface as an error result");

        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected a text content block");
        };
        let body: Value = serde_json::from_str(text).expect("structured error body");
        assert_eq!(body["error"]["kind"], "handler_error");
        assert!(
            body["message"].as_str().unwrap().contains("panicked"),
            "message should name the panic, got: {}",
            body["message"]
        );
    }

    /// The server keeps serving after a panic — the point of the boundary.
    #[tokio::test]
    async fn server_still_dispatches_after_a_handler_panics() {
        let handler = McpHandler::new(test_config()).await.unwrap();
        handler
            .ctx
            .router
            .insert_tool_for_test(panicking_tool())
            .await;

        let _ = handler
            .execute_tool(&CallToolParams {
                name: "panicking_test_tool".to_string(),
                arguments: Some(json!({})),
            })
            .await;

        // A meta-tool must still answer normally afterwards.
        let after = handler
            .execute_tool(&CallToolParams {
                name: "get_active_toolsets".to_string(),
                arguments: Some(json!({})),
            })
            .await;
        assert!(!after.is_error, "server must keep serving after a panic");
    }
}
