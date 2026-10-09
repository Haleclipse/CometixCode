//! MCP WebSocket transport.
//! Maps to: CC `utils/mcpWebSocketTransport.ts`.
//!
//! CC wraps an already-created WebSocket in `WebSocketTransport`. Rust opens
//! the WebSocket here too, the JavaScript `new WebSocket(url, { protocols:
//! ['mcp'], headers, proxy, tls })` step: the handshake on CC's transport,
//! then `tokio-tungstenite` on the upgraded connection.

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use futures::stream::BoxStream;
    use futures::{SinkExt, StreamExt};
    use http::{HeaderName, HeaderValue};
    use rmcp::RoleClient;
    use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
    use rmcp::transport::Transport as RmcpTransport;
    use std::collections::BTreeMap;
    use std::fmt;
    use std::future::Future;
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Error as WsError;
    use tokio_tungstenite::tungstenite::error::{ProtocolError, SubProtocolError};
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::{Message as WsMessage, Role};

    use reqwest::header;

    type WsMcpStream = WebSocketStream<reqwest::Upgraded>;

    /// Maps to: CC `utils/mcpWebSocketTransport.ts#WebSocketTransport`.
    pub(crate) struct WebSocketTransport {
        sink: Arc<Mutex<futures::stream::SplitSink<WsMcpStream, WsMessage>>>,
        incoming: BoxStream<'static, RxJsonRpcMessage<RoleClient>>,
    }

    #[derive(Debug)]
    pub(crate) struct WebSocketTransportError(anyhow::Error);

    impl fmt::Display for WebSocketTransportError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for WebSocketTransportError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.0.source()
        }
    }

    impl RmcpTransport<RoleClient> for WebSocketTransport {
        type Error = WebSocketTransportError;

        fn send(
            &mut self,
            item: TxJsonRpcMessage<RoleClient>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            let sink = self.sink.clone();
            async move {
                // Maps to: CC `utils/mcpWebSocketTransport.ts#send`.
                let payload = serde_json::to_string(&item)
                    .map_err(|error| WebSocketTransportError(anyhow::Error::new(error)))?;
                sink.lock()
                    .await
                    .send(WsMessage::Text(payload.into()))
                    .await
                    .map_err(|error| WebSocketTransportError(anyhow::Error::new(error)))
            }
        }

        fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
            self.incoming.next()
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            // Maps to: CC `utils/mcpWebSocketTransport.ts#close`.
            let _ = self.sink.lock().await.send(WsMessage::Close(None)).await;
            Ok(())
        }
    }

    fn parse_websocket_message(
        server_name: &str,
        payload: &str,
    ) -> Option<RxJsonRpcMessage<RoleClient>> {
        // Maps to: CC `utils/mcpWebSocketTransport.ts#onNodeMessage` /
        // `onBunMessage` JSONRPCMessageSchema parse.
        match serde_json::from_str::<RxJsonRpcMessage<RoleClient>>(payload) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::warn!(server = server_name, error = %error, "failed to parse MCP WebSocket message");
                None
            }
        }
    }

    /// The opening handshake (RFC 6455 §4.1) of CC's
    /// `new WebSocket(serverRef.url, { protocols: ['mcp'], headers })`, as an
    /// HTTP/1.1 request on `client`. The caller's headers come last, as they
    /// did on tungstenite's request.
    fn build_websocket_request(
        client: &reqwest::Client,
        url: &str,
        key: &str,
        headers: BTreeMap<String, String>,
    ) -> anyhow::Result<reqwest::Request> {
        let unsupported = || anyhow::anyhow!("URL error: URL scheme not supported");
        let mut url = reqwest::Url::parse(url)?;
        let scheme = match url.scheme() {
            "ws" => "http",
            "wss" => "https",
            _ => return Err(unsupported()),
        };
        url.set_scheme(scheme).map_err(|()| unsupported())?;
        let mut request = client
            .get(url)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, key)
            .header(header::SEC_WEBSOCKET_PROTOCOL, "mcp")
            .build()?;
        for (key, value) in headers {
            let name = HeaderName::from_str(&key)?;
            let value = HeaderValue::from_str(&value)?;
            request.headers_mut().insert(name, value);
        }
        Ok(request)
    }

    /// tungstenite's client `verify_response` (RFC 6455 §4.1), with its
    /// errors, so a failed handshake reads as it did when tungstenite made
    /// the connection.
    async fn verify_response(
        response: reqwest::Response,
        key: &str,
    ) -> Result<reqwest::Response, WsError> {
        if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            let status = response.status();
            let body = response.bytes().await.ok().map(|body| body.to_vec());
            let mut failed = http::Response::new(body);
            *failed.status_mut() = status;
            return Err(WsError::Http(Box::new(failed)));
        }
        let headers = response.headers();
        let value =
            |name: header::HeaderName| headers.get(name).and_then(|value| value.to_str().ok());
        if !value(header::UPGRADE).is_some_and(|value| value.eq_ignore_ascii_case("websocket")) {
            return Err(WsError::Protocol(
                ProtocolError::MissingUpgradeWebSocketHeader,
            ));
        }
        if !value(header::CONNECTION).is_some_and(|value| value.eq_ignore_ascii_case("Upgrade")) {
            return Err(WsError::Protocol(
                ProtocolError::MissingConnectionUpgradeHeader,
            ));
        }
        if headers
            .get(header::SEC_WEBSOCKET_ACCEPT)
            .map(|value| value.as_bytes())
            != Some(derive_accept_key(key.as_bytes()).as_bytes())
        {
            return Err(WsError::Protocol(
                ProtocolError::SecWebSocketAcceptKeyMismatch,
            ));
        }
        // `mcp` is the one subprotocol asked for.
        match headers.get(header::SEC_WEBSOCKET_PROTOCOL) {
            None => {
                return Err(WsError::Protocol(
                    ProtocolError::SecWebSocketSubProtocolError(SubProtocolError::NoSubProtocol),
                ));
            }
            Some(protocol) if protocol.to_str()? != "mcp" => {
                return Err(WsError::Protocol(
                    ProtocolError::SecWebSocketSubProtocolError(
                        SubProtocolError::InvalidSubProtocol,
                    ),
                ));
            }
            Some(_) => {}
        }
        Ok(response)
    }

    /// Maps to: CC `utils/mcpWebSocketTransport.ts#WebSocketTransport.constructor`
    /// plus the `client.ts:708-787` WebSocket construction (`ws` and
    /// `ws-ide`), on CC's Bun branch, which a native binary follows:
    /// `new globalThis.WebSocket(url, { protocols: ['mcp'], headers, proxy:
    /// getWebSocketProxyUrl(url), tls: getWebSocketTLSOptions() })`.
    ///
    /// Those options are `getProxyUrl()` unless `shouldBypassProxy(url)`, and
    /// the mTLS and CA options: the three `createAxiosInstance` combines. So
    /// the handshake goes out on [`create_axios_instance`]'s transport, HTTP/1.1
    /// only (an upgrade has no HTTP/2 form), and the upgraded connection
    /// carries the WebSocket. Deviation: through a proxy, reqwest forwards a
    /// `ws://` handshake to the proxy, where Bun's WebSocket opens a CONNECT
    /// tunnel; a `wss://` one tunnels in both.
    ///
    /// [`create_axios_instance`]: crate::utils::proxy::create_axios_instance
    pub(crate) async fn connect_mcp_websocket_transport(
        server_name: &str,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> anyhow::Result<WebSocketTransport> {
        let client = crate::utils::proxy::create_axios_instance()?
            .http1_only()
            .build()?;
        let key = generate_key();
        let request = build_websocket_request(&client, url, &key, headers)?;
        let response = verify_response(client.execute(request).await?, &key).await?;
        let upgraded = response.upgrade().await?;
        let stream = WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await;
        let (sink, source) = stream.split();
        let name = server_name.to_string();
        let incoming = source.filter_map(move |message| {
            let name = name.clone();
            async move {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::warn!(server = %name, error = %error, "WebSocket MCP stream error");
                        return None;
                    }
                };
                match message {
                    WsMessage::Text(payload) => parse_websocket_message(&name, &payload),
                    WsMessage::Binary(payload) => std::str::from_utf8(&payload)
                        .ok()
                        .and_then(|payload| parse_websocket_message(&name, payload)),
                    WsMessage::Close(_) => None,
                    WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => None,
                }
            }
        });

        Ok(WebSocketTransport {
            sink: Arc::new(Mutex::new(sink)),
            incoming: incoming.boxed(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn websocket_request_sets_mcp_subprotocol_and_headers_like_official_transport() {
            crate::utils::tls_provider::install_crypto_provider();
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let request = build_websocket_request(
                &client,
                "wss://127.0.0.1:9999/mcp",
                "dGhlIHNhbXBsZSBub25jZQ==",
                BTreeMap::from([("Authorization".to_string(), "Bearer token".to_string())]),
            )
            .unwrap();
            assert_eq!(request.url().as_str(), "https://127.0.0.1:9999/mcp");
            for (name, value) in [
                ("Connection", "Upgrade"),
                ("Upgrade", "websocket"),
                ("Sec-WebSocket-Version", "13"),
                ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
                ("Sec-WebSocket-Protocol", "mcp"),
                ("Authorization", "Bearer token"),
            ] {
                assert_eq!(
                    request.headers().get(name),
                    Some(&HeaderValue::from_static(value)),
                    "{name}"
                );
            }
            let error =
                build_websocket_request(&client, "http://127.0.0.1/mcp", "k", BTreeMap::new())
                    .unwrap_err();
            assert_eq!(error.to_string(), "URL error: URL scheme not supported");
        }

        use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

        /// A WebSocket server that accepts one handshake, answering with
        /// `protocol` as its subprotocol, records the request target, and
        /// sends one server notification.
        async fn spawn_server(
            protocol: Option<&'static str>,
        ) -> (
            std::net::SocketAddr,
            tokio::task::JoinHandle<Result<String, WsError>>,
        ) {
            use tokio_tungstenite::tungstenite::handshake::server::{
                Request as ServerRequest, Response as ServerResponse,
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut target = String::new();
                let mut websocket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |request: &ServerRequest, mut response: ServerResponse| {
                        target = request.uri().to_string();
                        if let Some(protocol) = protocol {
                            response.headers_mut().insert(
                                "Sec-WebSocket-Protocol",
                                HeaderValue::from_static(protocol),
                            );
                        }
                        Ok(response)
                    },
                )
                .await?;
                websocket
                    .send(WsMessage::Text(
                        r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#.into(),
                    ))
                    .await?;
                // Keep the connection open until the client has read it.
                let _ = websocket.next().await;
                Ok(target)
            });
            (address, server)
        }

        fn proxy_guards(https_proxy: &str, no_proxy: Option<&str>) -> Vec<EnvVarGuard> {
            let mut guards = vec![
                EnvVarGuard::set("HTTPS_PROXY", https_proxy),
                EnvVarGuard::unset("https_proxy"),
                EnvVarGuard::unset("HTTP_PROXY"),
                EnvVarGuard::unset("http_proxy"),
                EnvVarGuard::unset("no_proxy"),
            ];
            guards.push(match no_proxy {
                Some(no_proxy) => EnvVarGuard::set("NO_PROXY", no_proxy),
                None => EnvVarGuard::unset("NO_PROXY"),
            });
            guards
        }

        /// CC's `proxy: getWebSocketProxyUrl(url)`: the carrier's proxy takes
        /// the handshake (a `ws://` one in absolute form; the host resolves
        /// nowhere else), and frames flow over the upgraded connection.
        #[tokio::test]
        async fn the_handshake_goes_through_the_carriers_proxy() {
            let _lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (proxy, server) = spawn_server(Some("mcp")).await;
            let _guards = proxy_guards(&format!("http://{proxy}"), None);
            let mut transport =
                connect_mcp_websocket_transport("docs", "ws://mcp.invalid/mcp", BTreeMap::new())
                    .await
                    .unwrap();
            assert!(transport.receive().await.is_some());
            let _ = transport.close().await;
            assert_eq!(server.await.unwrap().unwrap(), "http://mcp.invalid/mcp");
        }

        /// `shouldBypassProxy(url)`: a NO_PROXY host is reached directly,
        /// past a proxy nothing listens on.
        #[tokio::test]
        async fn a_no_proxy_host_is_reached_directly() {
            let _lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (address, server) = spawn_server(Some("mcp")).await;
            let _guards = proxy_guards("http://127.0.0.1:9", Some("127.0.0.1"));
            let mut transport = connect_mcp_websocket_transport(
                "docs",
                &format!("ws://{address}/mcp"),
                BTreeMap::new(),
            )
            .await
            .unwrap();
            assert!(transport.receive().await.is_some());
            let _ = transport.close().await;
            assert_eq!(server.await.unwrap().unwrap(), "/mcp");
        }

        /// tungstenite's checks and texts: a server that picks no subprotocol
        /// fails the handshake, and so does one that does not upgrade.
        #[tokio::test]
        async fn failed_handshakes_report_what_tungstenite_did() {
            let _lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _guards = proxy_guards("http://127.0.0.1:9", Some("127.0.0.1"));
            let (address, _server) = spawn_server(None).await;
            let error = connect_mcp_websocket_transport(
                "docs",
                &format!("ws://{address}/mcp"),
                BTreeMap::new(),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(
                error.to_string(),
                WsError::Protocol(ProtocolError::SecWebSocketSubProtocolError(
                    SubProtocolError::NoSubProtocol
                ))
                .to_string()
            );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            });
            let error = connect_mcp_websocket_transport(
                "docs",
                &format!("ws://{address}/mcp"),
                BTreeMap::new(),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "HTTP error: 404 Not Found");
        }
    }
}

#[cfg(feature = "mcp_runtime")]
pub(crate) use runtime::{WebSocketTransport, connect_mcp_websocket_transport};
