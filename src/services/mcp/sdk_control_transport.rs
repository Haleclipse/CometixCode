//! SDK MCP control-channel transport boundary.
//! Maps to: CC `services/mcp/SdkControlTransport.ts`.
//!
//! Upstream purpose:
//! - `SdkControlClientTransport` lives in the CLI process and wraps MCP
//!   JSON-RPC messages in SDK control requests routed by `server_name`.
//! - `SdkControlServerTransport` lives in the SDK process and forwards MCP
//!   server responses back through the control request resolver.
//! - JSON-RPC request IDs are preserved by the SDK structured-I/O layer.

use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport as RmcpTransport;

pub const SDK_CONTROL_TRANSPORT_DEFERRED_REASON: &str =
    "SDK in-process MCP client setup is deferred";

/// Maps to: CC `services/mcp/SdkControlTransport.ts#SendMcpMessageCallback`.
pub type SendMcpMessageCallback = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>
        + Send
        + Sync,
>;

/// Maps to: CC `SdkControlServerTransport` constructor callback.
pub type ServerSendMcpMessageCallback = Arc<dyn Fn(Value) -> anyhow::Result<()> + Send + Sync>;
/// Maps to: CC assignable `Transport.onmessage` field.
pub type SdkControlMessageHandler = Arc<dyn Fn(Value) + Send + Sync + 'static>;
/// Maps to: CC assignable `Transport.onclose` field.
pub type SdkControlCloseHandler = Arc<dyn Fn() + Send + Sync + 'static>;
/// Maps to: CC assignable `Transport.onerror` field.
pub type SdkControlErrorHandler = Arc<dyn Fn(anyhow::Error) + Send + Sync + 'static>;

#[derive(Clone)]
struct SdkControlRuntimeChannels {
    incoming_tx: tokio::sync::mpsc::Sender<RxJsonRpcMessage<RoleClient>>,
    incoming_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<RxJsonRpcMessage<RoleClient>>>>,
}

impl SdkControlRuntimeChannels {
    fn new() -> Self {
        let (incoming_tx, incoming_rx) = tokio::sync::mpsc::channel(64);
        Self {
            incoming_tx,
            incoming_rx: Arc::new(tokio::sync::Mutex::new(incoming_rx)),
        }
    }
}

/// Error type for the rmcp transport implementation.
/// Maps to: CC promise rejection from `SdkControlClientTransport.send(...)`.
#[derive(Debug)]
pub struct SdkControlTransportError(anyhow::Error);

impl std::fmt::Display for SdkControlTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SdkControlTransportError {}

impl From<anyhow::Error> for SdkControlTransportError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

impl From<serde_json::Error> for SdkControlTransportError {
    fn from(value: serde_json::Error) -> Self {
        Self(anyhow::Error::new(value))
    }
}

/// CLI-side official boundary.
/// Maps to: CC `services/mcp/SdkControlTransport.ts#SdkControlClientTransport`.
#[derive(Clone)]
pub struct SdkControlClientTransport {
    server_name: String,
    send_mcp_message: SendMcpMessageCallback,
    is_closed: bool,
    on_message: Option<SdkControlMessageHandler>,
    on_close: Option<SdkControlCloseHandler>,
    #[allow(dead_code)]
    on_error: Option<SdkControlErrorHandler>,
    runtime_channels: SdkControlRuntimeChannels,
}

impl SdkControlClientTransport {
    pub fn new(server_name: impl Into<String>, send_mcp_message: SendMcpMessageCallback) -> Self {
        Self {
            server_name: server_name.into(),
            send_mcp_message,
            is_closed: false,
            on_message: None,
            on_close: None,
            on_error: None,
            runtime_channels: SdkControlRuntimeChannels::new(),
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Maps to: CC `SdkControlClientTransport.start()` (empty start method).
    pub async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Maps to: CC assignable `transport.onmessage` field.
    pub fn set_on_message(&mut self, handler: Option<SdkControlMessageHandler>) {
        self.on_message = handler;
    }

    /// Maps to: CC assignable `transport.onclose` field.
    pub fn set_on_close(&mut self, handler: Option<SdkControlCloseHandler>) {
        self.on_close = handler;
    }

    /// Maps to: CC assignable `transport.onerror` field.
    pub fn set_on_error(&mut self, handler: Option<SdkControlErrorHandler>) {
        self.on_error = handler;
    }

    /// Maps to: CC `SdkControlClientTransport.send(message)`: send the JSON-RPC
    /// message through the SDK control callback, then pass the returned response
    /// to `onmessage` for the MCP client.
    pub async fn send(&self, message: Value) -> anyhow::Result<()> {
        if self.is_closed {
            anyhow::bail!("Transport is closed");
        }
        let response = (self.send_mcp_message)(self.server_name.clone(), message).await?;
        if let Some(on_message) = &self.on_message {
            on_message(response);
        }
        Ok(())
    }

    /// Maps to: CC `SdkControlClientTransport.close()`.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        if self.is_closed {
            return Ok(());
        }
        self.is_closed = true;
        if let Some(on_close) = &self.on_close {
            on_close();
        }
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.is_closed
    }
}

impl RmcpTransport<RoleClient> for SdkControlClientTransport {
    type Error = SdkControlTransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let is_closed = self.is_closed;
        let send_mcp_message = self.send_mcp_message.clone();
        let server_name = self.server_name.clone();
        let incoming_tx = self.runtime_channels.incoming_tx.clone();
        async move {
            if is_closed {
                return Err(anyhow::anyhow!("Transport is closed").into());
            }
            let expects_response = matches!(item, TxJsonRpcMessage::<RoleClient>::Request(_));
            let message = serde_json::to_value(item)?;
            let response = send_mcp_message(server_name, message).await?;
            if expects_response {
                let response: RxJsonRpcMessage<RoleClient> = serde_json::from_value(response)?;
                incoming_tx
                    .send(response)
                    .await
                    .map_err(|_| anyhow::anyhow!("SDK control response channel is closed"))?;
            }
            Ok(())
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        let incoming_rx = self.runtime_channels.incoming_rx.clone();
        async move { incoming_rx.lock().await.recv().await }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let already_closed = self.is_closed;
        self.is_closed = true;
        let on_close = self.on_close.clone();
        async move {
            if !already_closed {
                if let Some(on_close) = on_close {
                    on_close();
                }
            }
            Ok(())
        }
    }
}

/// SDK-side official boundary.
/// Maps to: CC `services/mcp/SdkControlTransport.ts#SdkControlServerTransport`.
#[derive(Clone)]
pub struct SdkControlServerTransport {
    send_mcp_message: ServerSendMcpMessageCallback,
    is_closed: bool,
    on_message: Option<SdkControlMessageHandler>,
    on_close: Option<SdkControlCloseHandler>,
    #[allow(dead_code)]
    on_error: Option<SdkControlErrorHandler>,
}

impl SdkControlServerTransport {
    pub fn new(send_mcp_message: ServerSendMcpMessageCallback) -> Self {
        Self {
            send_mcp_message,
            is_closed: false,
            on_message: None,
            on_close: None,
            on_error: None,
        }
    }

    /// Maps to: CC `SdkControlServerTransport.start()` (empty start method).
    pub async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Maps to: CC assignable `transport.onmessage` field.
    pub fn set_on_message(&mut self, handler: Option<SdkControlMessageHandler>) {
        self.on_message = handler;
    }

    /// Maps to: CC assignable `transport.onclose` field.
    pub fn set_on_close(&mut self, handler: Option<SdkControlCloseHandler>) {
        self.on_close = handler;
    }

    /// Maps to: CC assignable `transport.onerror` field.
    pub fn set_on_error(&mut self, handler: Option<SdkControlErrorHandler>) {
        self.on_error = handler;
    }

    /// Maps to: CC `SdkControlServerTransport.send(message)`: forward the
    /// JSON-RPC response back through the SDK-side control resolver.
    pub async fn send(&self, message: Value) -> anyhow::Result<()> {
        if self.is_closed {
            anyhow::bail!("Transport is closed");
        }
        (self.send_mcp_message)(message)
    }

    /// Maps to: CC `SdkControlServerTransport.close()`.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        if self.is_closed {
            return Ok(());
        }
        self.is_closed = true;
        if let Some(on_close) = &self.on_close {
            on_close();
        }
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.is_closed
    }

    /// Test/integration helper matching the SDK process receiving a control
    /// request and invoking `transport.onmessage`.
    /// Maps to: CC StructuredIO dispatch into `SdkControlServerTransport.onmessage`.
    pub fn receive_control_message(&self, message: Value) {
        if let Some(on_message) = &self.on_message {
            on_message(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sdk_control_client_transport_round_trips_response_to_onmessage() {
        let callback: SendMcpMessageCallback = Arc::new(|server_name, message| {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message.get("id").cloned().unwrap_or(Value::Null),
                    "result": { "server": server_name }
                }))
            })
        });
        let mut transport = SdkControlClientTransport::new("sdk-server", callback);
        assert_eq!(transport.server_name(), "sdk-server");
        let (tx, rx) = async_channel::bounded(1);
        transport.set_on_message(Some(Arc::new(move |message| {
            tx.try_send(message).unwrap();
        })));

        transport
            .send(serde_json::json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }))
            .await
            .unwrap();
        let response = rx.recv().await.unwrap();
        assert_eq!(response["id"].as_i64(), Some(7));
        assert_eq!(response["result"]["server"].as_str(), Some("sdk-server"));

        transport.close().await.unwrap();
        assert!(transport.is_closed());
        let error = transport
            .send(serde_json::json!({ "jsonrpc": "2.0", "id": 8 }))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Transport is closed");
    }

    #[tokio::test]
    async fn sdk_control_server_transport_forwards_responses_and_receives_requests() {
        let (response_tx, response_rx) = async_channel::bounded(1);
        let callback: ServerSendMcpMessageCallback = Arc::new(move |message| {
            response_tx.try_send(message).unwrap();
            Ok(())
        });
        let mut transport = SdkControlServerTransport::new(callback);
        let (request_tx, request_rx) = async_channel::bounded(1);
        transport.set_on_message(Some(Arc::new(move |message| {
            request_tx.try_send(message).unwrap();
        })));

        transport.receive_control_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        }));
        assert_eq!(
            request_rx.recv().await.unwrap()["method"].as_str(),
            Some("initialize")
        );

        transport
            .send(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} }))
            .await
            .unwrap();
        assert_eq!(response_rx.recv().await.unwrap()["id"].as_i64(), Some(1));
    }
}
