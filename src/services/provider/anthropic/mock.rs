//! Test-only local Anthropic Messages server.
//!
//! @nyan: binds `127.0.0.1:<random>`, records every request (request line,
//! lower-cased headers, JSON body) and answers with a canned SSE stream or a
//! canned error. It exists so adapter tests can assert the exact wire request
//! and drive the real SDK stream parser end to end.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub request_line: String,
    /// Header names are lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// What the server answers to one request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// `200` with these SSE events, each `(event name, data)`.
    Sse(Vec<(String, Value)>),
    /// A non-200 JSON error body.
    Error { status: u16, body: Value },
}

pub struct MockAnthropic {
    pub base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockAnthropic {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockAnthropic {
    /// Serve `replies` in order, one per request.
    pub async fn start(replies: Vec<Reply>) -> Self {
        crate::utils::tls_provider::install_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut raw = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buffer[..count]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        Some((key.trim().to_ascii_lowercase(), value.trim().to_string()))
                    })
                    .collect();
                let length: usize = headers
                    .iter()
                    .find(|(key, _)| key == "content-length")
                    .and_then(|(_, value)| value.parse().ok())
                    .unwrap_or(0);
                while raw.len() - header_end < length {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buffer[..count]);
                }
                let body = serde_json::from_slice(&raw[header_end..]).unwrap_or(Value::Null);
                recorded.lock().unwrap().push(RecordedRequest {
                    request_line,
                    headers,
                    body,
                });
                let response = match reply {
                    Reply::Sse(events) => {
                        let mut body = String::new();
                        for (name, data) in events {
                            body.push_str(&format!("event: {name}\ndata: {data}\n\n"));
                        }
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                             Cache-Control: no-cache\r\nrequest-id: req_mock\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    Reply::Error { status, body } => {
                        let body = body.to_string();
                        format!(
                            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\n\
                             request-id: req_mock\r\nContent-Length: {}\r\n\
                             Connection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

/// A complete, valid streamed assistant message: optional thinking, then text,
/// then an optional tool call, with usage and a stop reason.
pub fn message_stream(spec: StreamSpec) -> Vec<(String, Value)> {
    use serde_json::json;
    let mut events = vec![(
        "message_start".to_string(),
        json!({"type":"message_start","message":{
            "id":"msg_mock","type":"message","role":"assistant","model":"claude-sonnet-4-6",
            "content":[],"stop_reason":null,"stop_sequence":null,
            "usage":{"input_tokens":spec.input_tokens,"output_tokens":1,
                     "cache_read_input_tokens":spec.cache_read_tokens,
                     "cache_creation_input_tokens":spec.cache_write_tokens}}}),
    )];
    let mut index = 0;
    if let Some(thinking) = spec.thinking {
        events.push((
            "content_block_start".into(),
            json!({"type":"content_block_start","index":index,
                   "content_block":{"type":"thinking","thinking":"","signature":""}}),
        ));
        events.push((
            "content_block_delta".into(),
            json!({"type":"content_block_delta","index":index,
                   "delta":{"type":"thinking_delta","thinking":thinking}}),
        ));
        events.push((
            "content_block_delta".into(),
            json!({"type":"content_block_delta","index":index,
                   "delta":{"type":"signature_delta","signature":"sig-mock"}}),
        ));
        events.push((
            "content_block_stop".into(),
            json!({"type":"content_block_stop","index":index}),
        ));
        index += 1;
    }
    if let Some(text) = spec.text {
        events.push((
            "content_block_start".into(),
            json!({"type":"content_block_start","index":index,
                   "content_block":{"type":"text","text":""}}),
        ));
        events.push((
            "content_block_delta".into(),
            json!({"type":"content_block_delta","index":index,
                   "delta":{"type":"text_delta","text":text}}),
        ));
        events.push((
            "content_block_stop".into(),
            json!({"type":"content_block_stop","index":index}),
        ));
        index += 1;
    }
    if let Some((name, input)) = spec.tool_call {
        let input = input.to_string();
        let (first, second) = input.split_at(input.len() / 2);
        events.push((
            "content_block_start".into(),
            json!({"type":"content_block_start","index":index,
                   "content_block":{"type":"tool_use","id":"toolu_mock","name":name,"input":{}}}),
        ));
        for fragment in [first, second] {
            events.push((
                "content_block_delta".into(),
                json!({"type":"content_block_delta","index":index,
                       "delta":{"type":"input_json_delta","partial_json":fragment}}),
            ));
        }
        events.push((
            "content_block_stop".into(),
            json!({"type":"content_block_stop","index":index}),
        ));
    }
    events.push((
        "message_delta".into(),
        json!({"type":"message_delta","delta":{"stop_reason":spec.stop_reason},
               "usage":{"output_tokens":spec.output_tokens}}),
    ));
    events.push(("message_stop".into(), json!({"type":"message_stop"})));
    events
}

pub struct StreamSpec {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub thinking: Option<&'static str>,
    pub text: Option<&'static str>,
    pub tool_call: Option<(&'static str, Value)>,
    pub stop_reason: &'static str,
}

impl Default for StreamSpec {
    fn default() -> Self {
        Self {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            thinking: None,
            text: Some("ok"),
            tool_call: None,
            stop_reason: "end_turn",
        }
    }
}
