//! One streaming POST: request, status check, SSE framing, cancellation.
//!
//! @nyan: shared by both OpenAI protocols. It knows HTTP and Server-Sent
//! Events and nothing about either wire body. One call is one HTTP request;
//! retry belongs to the caller. A non-2xx status is classified here, before any
//! frame is read, so a failed request never looks like an empty stream.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde_json::Value;

use super::errors;
use crate::services::provider::adapter::Cancel;
use crate::services::provider::ir::{ProviderError, ProviderErrorKind};

/// How long the server may take to answer with headers. The body is a stream
/// of unbounded length; its liveness is the query layer's idle watchdog.
const RESPONSE_HEADERS_TIMEOUT: Duration = Duration::from_secs(600);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A local server on loopback or the LAN either accepts at once or refuses; a
/// long wait means it is not there, so fail in seconds with the endpoint named.
pub const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One SSE message that carried data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub data: String,
}

pub type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame, ProviderError>> + Send>>;

pub struct Opened {
    /// Lower-cased response header names.
    pub headers: BTreeMap<String, String>,
    pub request_id: Option<String>,
    pub frames: FrameStream,
}

/// Header names that carry the server's request id, best first.
const REQUEST_ID_HEADERS: [&str; 3] = ["x-request-id", "request-id", "x-client-request-id"];

fn request_id_of(headers: &BTreeMap<String, String>) -> Option<String> {
    REQUEST_ID_HEADERS
        .iter()
        .find_map(|name| headers.get(*name).cloned())
}

fn aborted() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Aborted, "Request was aborted.")
}

fn transport_error(error: &reqwest::Error) -> ProviderError {
    // Not `error.to_string()`: reqwest can include the URL, and an endpoint
    // may carry a credential in its query string.
    let reason = if error.is_timeout() {
        "the request timed out"
    } else if error.is_connect() {
        "could not connect to the server"
    } else if error.is_body() || error.is_decode() {
        "the response body could not be read"
    } else {
        "the request failed"
    };
    ProviderError::new(ProviderErrorKind::Transport, reason)
}

fn lowercase_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            Some((
                name.as_str().to_ascii_lowercase(),
                value.to_str().ok()?.to_string(),
            ))
        })
        .collect()
}

/// POST `body` as JSON and return the SSE frames of the answer.
///
/// `headers` are added on top of the common layer; settings `httpHeaders`
/// override both by name.
pub async fn post_stream(
    url: &str,
    headers: Vec<(String, String)>,
    body: &Value,
    cancel: &Cancel,
    connect_timeout: Duration,
) -> Result<Opened, ProviderError> {
    post_stream_classified(
        url,
        headers,
        body,
        cancel,
        connect_timeout,
        errors::from_http,
    )
    .await
}

/// POST `body` as JSON and return the parsed JSON answer.
pub async fn post_json(
    url: &str,
    headers: Vec<(String, String)>,
    body: &Value,
    cancel: &Cancel,
    connect_timeout: Duration,
) -> Result<(BTreeMap<String, String>, Option<String>, Value), ProviderError> {
    post_json_classified(
        url,
        headers,
        body,
        cancel,
        connect_timeout,
        errors::from_http,
    )
    .await
}

/// Turns a non-2xx status, its headers, body text and request id into the
/// protocol's own [`ProviderError`].
pub type ClassifyHttp = fn(u16, BTreeMap<String, String>, &str, Option<String>) -> ProviderError;

/// [`post_json`] with the protocol's own error classification.
pub async fn post_json_classified(
    url: &str,
    headers: Vec<(String, String)>,
    body: &Value,
    cancel: &Cancel,
    connect_timeout: Duration,
    classify: ClassifyHttp,
) -> Result<(BTreeMap<String, String>, Option<String>, Value), ProviderError> {
    if cancel.is_aborted() {
        return Err(aborted());
    }
    let client = crate::utils::proxy::get_proxy_fetch_options()
        .and_then(|builder| {
            builder
                .connect_timeout(connect_timeout)
                .build()
                .map_err(anyhow::Error::from)
        })
        .map_err(|error| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("could not build the HTTP client: {error}"),
            )
        })?;

    let mut layered = crate::services::provider::anthropic::headers::common_headers();
    for (name, value) in headers {
        layered.insert(name, Some(value));
    }
    crate::services::provider::anthropic::headers::apply_configured_overrides(&mut layered);

    let mut request = client
        .post(url)
        .header("accept", "application/json")
        .header("content-type", "application/json");
    for (name, value) in layered {
        if let Some(value) = value {
            request = request.header(name, value);
        }
    }
    let request = request.body(
        serde_json::to_vec(body)
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e.to_string()))?,
    );

    let mut abort = cancel.clone();
    let response = tokio::select! {
        biased;
        _ = abort.aborted() => return Err(aborted()),
        sent = tokio::time::timeout(RESPONSE_HEADERS_TIMEOUT, request.send()) => match sent {
            Err(_) => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Transport,
                    "the server did not answer in time",
                ));
            }
            Ok(Err(error)) => return Err(transport_error(&error)),
            Ok(Ok(response)) => response,
        },
    };

    let status = response.status().as_u16();
    let response_headers = lowercase_headers(response.headers());
    let request_id = request_id_of(&response_headers);

    if !(200..300).contains(&status) {
        let text = tokio::select! {
            biased;
            _ = abort.aborted() => return Err(aborted()),
            text = response.text() => text.unwrap_or_default(),
        };
        return Err(classify(status, response_headers, &text, request_id));
    }

    let text = tokio::select! {
        biased;
        _ = abort.aborted() => return Err(aborted()),
        text = response.text() => match text {
            Ok(t) => t,
            Err(e) => return Err(transport_error(&e)),
        },
    };
    let value = serde_json::from_str(&text).map_err(|error| {
        ProviderError::new(
            ProviderErrorKind::InvalidRequest,
            format!("could not parse JSON response from server: {error}; raw body: {text}"),
        )
    })?;
    Ok((response_headers, request_id, value))
}

/// [`post_stream`] with the protocol's own error classification: the transport
/// is shared, the shape of an error body is not.
pub async fn post_stream_classified(
    url: &str,
    headers: Vec<(String, String)>,
    body: &Value,
    cancel: &Cancel,
    connect_timeout: Duration,
    classify: ClassifyHttp,
) -> Result<Opened, ProviderError> {
    if cancel.is_aborted() {
        return Err(aborted());
    }
    let client = crate::utils::proxy::get_proxy_fetch_options()
        .and_then(|builder| {
            builder
                .connect_timeout(connect_timeout)
                .build()
                .map_err(anyhow::Error::from)
        })
        .map_err(|error| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("could not build the HTTP client: {error}"),
            )
        })?;

    let mut layered = crate::services::provider::anthropic::headers::common_headers();
    for (name, value) in headers {
        layered.insert(name, Some(value));
    }
    crate::services::provider::anthropic::headers::apply_configured_overrides(&mut layered);

    let mut request = client
        .post(url)
        .header("accept", "text/event-stream")
        .header("content-type", "application/json");
    for (name, value) in layered {
        if let Some(value) = value {
            request = request.header(name, value);
        }
    }
    let request = request.body(
        serde_json::to_vec(body)
            .map_err(|e| ProviderError::new(ProviderErrorKind::InvalidRequest, e.to_string()))?,
    );

    let mut abort = cancel.clone();
    let response = tokio::select! {
        biased;
        _ = abort.aborted() => return Err(aborted()),
        sent = tokio::time::timeout(RESPONSE_HEADERS_TIMEOUT, request.send()) => match sent {
            Err(_) => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Transport,
                    "the server did not answer in time",
                ));
            }
            Ok(Err(error)) => return Err(transport_error(&error)),
            Ok(Ok(response)) => response,
        },
    };

    let status = response.status().as_u16();
    let response_headers = lowercase_headers(response.headers());
    let request_id = request_id_of(&response_headers);

    if !(200..300).contains(&status) {
        let text = tokio::select! {
            biased;
            _ = abort.aborted() => return Err(aborted()),
            text = response.text() => text.unwrap_or_default(),
        };
        return Err(classify(status, response_headers, &text, request_id));
    }

    let frames = sse_frames(response, cancel.clone());
    Ok(Opened {
        headers: response_headers,
        request_id,
        frames,
    })
}

fn sse_frames(response: reqwest::Response, cancel: Cancel) -> FrameStream {
    struct State {
        sse: Pin<Box<dyn Stream<Item = Result<sse_stream::Sse, sse_stream::Error>> + Send>>,
        cancel: Cancel,
        done: bool,
    }
    let state = State {
        sse: Box::pin(sse_stream::SseStream::from_bytes_stream(
            response.bytes_stream(),
        )),
        cancel,
        done: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.done {
                return None;
            }
            let next = tokio::select! {
                biased;
                _ = state.cancel.aborted() => {
                    state.done = true;
                    return Some((Err(aborted()), state));
                }
                next = state.sse.next() => next,
            };
            match next {
                None => return None,
                Some(Err(error)) => {
                    state.done = true;
                    let reason = match error {
                        sse_stream::Error::Body(_) => "the connection dropped mid-stream",
                        _ => "the server sent a malformed event stream",
                    };
                    return Some((
                        Err(ProviderError::new(ProviderErrorKind::Transport, reason)),
                        state,
                    ));
                }
                Some(Ok(sse)) => {
                    // Comment lines and keep-alives arrive without data.
                    let Some(data) = sse.data.filter(|data| !data.is_empty()) else {
                        continue;
                    };
                    return Some((
                        Ok(Frame {
                            event: sse.event,
                            data,
                        }),
                        state,
                    ));
                }
            }
        }
    }))
}
