//! Test-only local Gemini-style mock server for contract fixtures.
//!
//! @nyan: binds `127.0.0.1:<random>`, records incoming requests and answers
//! with scripted replies (SSE stream or HTTP error status).

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum Reply {
    Sse(Vec<(Option<String>, String)>),
    SseThenCut(Vec<(Option<String>, String)>),
    Error {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
}

impl Reply {
    pub fn sse_data(items: impl IntoIterator<Item = Value>) -> Self {
        Self::Sse(items.into_iter().map(|v| (None, v.to_string())).collect())
    }

    pub fn error(status: u16, body: Value) -> Self {
        Self::Error {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }
}

pub struct MockGemini {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockGemini {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn sse_body(messages: &[(Option<String>, String)]) -> String {
    let mut body = String::new();
    for (event, data) in messages {
        if let Some(event) = event {
            body.push_str(&format!("event: {event}\n"));
        }
        body.push_str(&format!("data: {data}\n\n"));
    }
    body
}

impl MockGemini {
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
                recorded.lock().unwrap().push(Recorded {
                    request_line,
                    headers,
                    body,
                });
                match reply {
                    Reply::Sse(messages) => {
                        let body = sse_body(&messages);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                             Cache-Control: no-cache\r\nx-request-id: req_mock_gemini\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(body.as_bytes()).await;
                    }
                    Reply::SseThenCut(messages) => {
                        let body = sse_body(&messages);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                             x-request-id: req_mock_gemini\r\nContent-Length: {}\r\n\
                             Connection: close\r\n\r\n",
                            body.len() + 4096
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(body.as_bytes()).await;
                    }
                    Reply::Error {
                        status,
                        headers,
                        body,
                    } => {
                        let mut head = format!(
                            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\n\
                             x-request-id: req_mock_gemini\r\nContent-Length: {}\r\nConnection: close\r\n",
                            body.len()
                        );
                        for (name, value) in headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(body.as_bytes()).await;
                    }
                }
                let _ = stream.shutdown().await;
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}
