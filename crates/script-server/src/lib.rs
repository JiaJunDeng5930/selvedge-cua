//! MCP transport and the lifetime of one JavaScript invocation.
#![forbid(unsafe_code)]

use std::{error::Error, future::Future, io, sync::Arc};

use rmcp::{
    RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars,
    service::{QuitReason, RequestContext},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

mod script;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    ObserveScreen,
    ReadUi,
    ListWindows,
    Pointer,
    Keyboard,
    Scroll,
    InsertText,
    PerformUiAction,
    ListApps,
    LaunchApp,
    ReadMenuBar,
}

pub struct Response {
    pub value: Value,
    /// Images are PNGs. Observation indices address this vector.
    pub images: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub struct OperationError {
    pub message: String,
    pub code: Option<String>,
}

impl OperationError {
    pub fn coded(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
        }
    }
}

impl From<String> for OperationError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}

impl From<&str> for OperationError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

/// Operations return after dispatch or observation. Only screen observations
/// receive a separate channel for announcing the first captured frame.
/// Once cancelled, the implementation stops scheduling new effects and settles
/// any dispatch already in progress before returning.
pub trait Desktop: Send + Sync + 'static {
    /// Return sample times, including the endpoint, before evaluating a trajectory.
    fn pointer_offsets(&self, duration_ms: f64, interval_ms: f64) -> Result<Vec<f64>, String>;

    fn call(
        &self,
        name: Operation,
        options: Value,
        started: Option<oneshot::Sender<Result<Value, String>>>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<Response, OperationError>> + Send;

    /// Called once after every invocation whose native work has been drained.
    fn finish_run(&self, successful: bool) -> impl Future<Output = Result<(), String>> + Send;
}

struct Server<D> {
    desktop: Arc<D>,
    invocation: Arc<Mutex<()>>,
    tools: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Run {
    /// Body of an async JavaScript function. Use await and return.
    code: String,
}

#[tool_router(router = tools)]
impl<D: Desktop> Server<D> {
    #[tool(
        description = "Run async JavaScript with observeScreen, listWindows, readUI, pointer, keyboard, scroll, insertText and performUIAction. Return JSON data. Screen observations attach PNG images."
    )]
    async fn run(
        &self,
        Parameters(Run { code }): Parameters<Run>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let desktop = Arc::clone(&self.desktop);
        let invocation = Arc::clone(&self.invocation);
        let cancel = context.ct;

        // The MCP request may disappear while native input is in flight. The
        // invocation keeps ownership of serialization and cleanup in that case.
        let execution = tokio::spawn(async move {
            let _invocation = tokio::select! {
                biased;
                _ = cancel.cancelled() => return script::Output::failed("Script was cancelled"),
                lock = invocation.lock() => lock,
            };
            script::execute(desktop, code, cancel).await
        });
        match execution.await {
            Ok(output) => output.into_tool_result(),
            // DEFENSE: A platform adapter panic is reported as a tool failure.
            Err(error) => script::Output::failed(error.to_string()).into_tool_result(),
        }
    }
}

#[tool_handler(router = self.tools)]
impl<D: Desktop> ServerHandler for Server<D> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "selvedge-cua-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Use run({code}) with an async JavaScript function body. \
                 Find background windows with listWindows({bundleId}), then readUI({windowId}). \
                 Start pointer input with a ready windowId from readUI and keyboard input with the target pid. \
                 Pass the operation reference to continue held input. \
                 observeScreen(options) has started and finished promises and can also be awaited directly. \
                 The result includes observations with image offsets into the PNG attachments.",
            )
    }
}

pub async fn serve(desktop: impl Desktop) -> Result<(), Box<dyn Error + Send + Sync>> {
    let desktop = Arc::new(desktop);
    let invocation = Arc::new(Mutex::new(()));
    let server = Server {
        desktop: Arc::clone(&desktop),
        invocation: Arc::clone(&invocation),
        tools: Server::tools(),
    };
    let reason = server
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await;
    let _invocation = invocation.lock().await;
    let cleanup = desktop.finish_run(false).await;
    if let QuitReason::JoinError(error) = reason? {
        return Err(error.into());
    }
    cleanup.map_err(|error| io::Error::other(error).into())
}
