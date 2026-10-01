//! WebFetch network helpers.
//!
//! Maps to: CC `tools/WebFetchTool/utils.ts`.
//!
//! This module owns URL validation, preflight domain checks, redirect handling,
//! content fetching/caching, native HTML-to-markdown conversion, and the
//! secondary-model prompt application used by `WebFetchTool.call`. UI and
//! permission decisions stay in their official Rust counterparts.

use crate::tool::AbortController;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Maps to CC `MAX_URL_LENGTH`.
const MAX_URL_LENGTH: usize = 2_000;
/// Maps to CC `MAX_HTTP_CONTENT_LENGTH`.
pub const MAX_HTTP_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
/// Maps to CC `FETCH_TIMEOUT_MS`.
const FETCH_TIMEOUT_MS: u64 = 60_000;
/// Maps to CC `DOMAIN_CHECK_TIMEOUT_MS`.
#[allow(dead_code)]
const DOMAIN_CHECK_TIMEOUT_MS: u64 = 10_000;
/// Maps to CC `MAX_REDIRECTS`.
const MAX_REDIRECTS: usize = 10;
/// Maps to CC `CACHE_TTL_MS`.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// Maps to CC `MAX_CACHE_SIZE_BYTES`.
const MAX_CACHE_SIZE_BYTES: usize = 50 * 1024 * 1024;
/// Maps to CC `MAX_MARKDOWN_LENGTH`.
pub const MAX_MARKDOWN_LENGTH: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RedirectInfo {
    pub(crate) original_url: String,
    pub(crate) redirect_url: String,
    pub(crate) status_code: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FetchedContent {
    pub(crate) content: String,
    pub(crate) bytes: usize,
    pub(crate) code: u16,
    pub(crate) code_text: String,
    pub(crate) content_type: String,
    /// Maps to CC `persistedPath` from `utils/mcpOutputStorage.ts`.
    pub(crate) persisted_path: Option<String>,
    /// Maps to CC `persistedSize` from `utils/mcpOutputStorage.ts`.
    pub(crate) persisted_size: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UrlMarkdownContent {
    Fetched(FetchedContent),
    Redirect(RedirectInfo),
}

#[derive(Clone, Debug)]
struct CacheEntry {
    inserted_at: Instant,
    last_access_order: u64,
    bytes: usize,
    code: u16,
    code_text: String,
    content: String,
    content_type: String,
    persisted_path: Option<String>,
    persisted_size: Option<usize>,
    cache_size: usize,
}

#[derive(Default)]
// Maps to CC `URL_CACHE = new LRUCache(...)`. Rust mirrors the official TTL,
// aggregate size cap, and least-recently-used eviction semantics without adding
// a tool-specific cache dependency: `insert`/successful `get` advance a
// monotonic access order, while `inserted_at` remains the TTL anchor.
struct WebFetchCache {
    entries: HashMap<String, CacheEntry>,
    next_access_order: u64,
}

impl WebFetchCache {
    fn mark_access(&mut self) -> u64 {
        self.next_access_order = self.next_access_order.wrapping_add(1);
        self.next_access_order
    }

    fn get(&mut self, url: &str) -> Option<FetchedContent> {
        let now = Instant::now();
        self.entries
            .retain(|_, entry| now.duration_since(entry.inserted_at) <= CACHE_TTL);
        let access_order = if self.entries.contains_key(url) {
            self.mark_access()
        } else {
            return None;
        };
        self.entries.get_mut(url).map(|entry| {
            entry.last_access_order = access_order;
            FetchedContent {
                content: entry.content.clone(),
                bytes: entry.bytes,
                code: entry.code,
                code_text: entry.code_text.clone(),
                content_type: entry.content_type.clone(),
                persisted_path: entry.persisted_path.clone(),
                persisted_size: entry.persisted_size,
            }
        })
    }

    fn insert(&mut self, url: String, mut entry: CacheEntry) {
        entry.last_access_order = self.mark_access();
        self.entries.insert(url, entry);
        self.prune_to_size();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.next_access_order = 0;
    }

    fn prune_to_size(&mut self) {
        let mut total = self.total_size();
        if total <= MAX_CACHE_SIZE_BYTES {
            return;
        }
        let mut oldest: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(url, entry)| (url.clone(), entry.last_access_order))
            .collect();
        oldest.sort_by_key(|(_, access_order)| *access_order);
        for (url, _) in oldest {
            if total <= MAX_CACHE_SIZE_BYTES {
                break;
            }
            if let Some(entry) = self.entries.remove(&url) {
                total = total.saturating_sub(entry.cache_size.max(1));
            }
        }
    }

    fn total_size(&self) -> usize {
        self.entries
            .values()
            .map(|entry| entry.cache_size.max(1))
            .sum()
    }
}

fn url_cache() -> &'static Mutex<WebFetchCache> {
    static URL_CACHE: OnceLock<Mutex<WebFetchCache>> = OnceLock::new();
    URL_CACHE.get_or_init(|| Mutex::new(WebFetchCache::default()))
}

#[allow(dead_code)]
fn domain_check_cache() -> &'static Mutex<HashMap<String, Instant>> {
    static DOMAIN_CHECK_CACHE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    DOMAIN_CHECK_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Maps to CC `clearWebFetchCache()` (called from `commands/clear/caches`).
pub fn clear_web_fetch_cache() {
    if let Ok(mut cache) = url_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = domain_check_cache().lock() {
        cache.clear();
    }
}

/// Maps to CC `isPreapprovedUrl(...)`.
pub(crate) fn is_preapproved_url(url: &str) -> bool {
    crate::tools::web_fetch_tool::preapproved::is_preapproved_web_fetch_url(url)
}

/// Maps to CC `validateURL(...)`.
pub(crate) fn validate_url(url: &str) -> bool {
    if url.len() > MAX_URL_LENGTH {
        return false;
    }
    validate_url_parts(url).is_some_and(|parts| {
        !parts.has_credentials
            && parts.hostname.split('.').count() >= 2
            && !parts.hostname.is_empty()
    })
}

/// Maps to CC `WebFetchTool.validateInput`'s bare `new URL(url)` parse probe
/// (`WebFetchTool.ts:193-195`), which is looser than `validateURL`.
pub(crate) fn is_parseable_url(url: &str) -> bool {
    #[cfg(feature = "mcp_runtime")]
    {
        reqwest::Url::parse(url).is_ok()
    }

    #[cfg(not(feature = "mcp_runtime"))]
    {
        validate_url_parts(url).is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UrlParts {
    protocol: String,
    hostname: String,
    port: Option<String>,
    has_credentials: bool,
}

fn validate_url_parts(url: &str) -> Option<UrlParts> {
    #[cfg(feature = "mcp_runtime")]
    {
        let parsed = reqwest::Url::parse(url).ok()?;
        return Some(UrlParts {
            protocol: parsed.scheme().to_string(),
            hostname: parsed
                .host_str()?
                .trim_end_matches('.')
                .to_ascii_lowercase(),
            port: parsed.port().map(|port| port.to_string()),
            has_credentials: !parsed.username().is_empty() || parsed.password().is_some(),
        });
    }

    #[cfg(not(feature = "mcp_runtime"))]
    {
        let after_scheme = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let authority_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if authority.is_empty() {
            return None;
        }
        let (has_credentials, host_port) = match authority.rsplit_once('@') {
            Some((_, host)) => (true, host),
            None => (false, authority),
        };
        let hostname = host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
            .trim_end_matches('.')
            .to_ascii_lowercase();
        Some(UrlParts {
            protocol: if url.starts_with("https://") {
                "https"
            } else {
                "http"
            }
            .to_string(),
            hostname,
            port: host_port.split_once(':').map(|(_, port)| port.to_string()),
            has_credentials,
        })
    }
}

/// Maps to CC `isPermittedRedirect(...)`.
pub(crate) fn is_permitted_redirect(original_url: &str, redirect_url: &str) -> bool {
    let Some(original) = validate_url_parts(original_url) else {
        return false;
    };
    let Some(redirect) = validate_url_parts(redirect_url) else {
        return false;
    };
    if original.protocol != redirect.protocol || original.port != redirect.port {
        return false;
    }
    if redirect.has_credentials {
        return false;
    }
    fn strip_www(host: &str) -> &str {
        host.strip_prefix("www.").unwrap_or(host)
    }
    strip_www(&original.hostname) == strip_www(&redirect.hostname)
}

fn ensure_not_aborted(abort_controller: &AbortController) -> anyhow::Result<()> {
    if abort_controller.is_aborted() {
        anyhow::bail!("Operation aborted");
    }
    Ok(())
}

async fn wait_or_abort<F, T>(future: F, abort_controller: &AbortController) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    ensure_not_aborted(abort_controller)?;
    let mut signal = abort_controller.signal();
    tokio::select! {
        output = future => Ok(output),
        _ = signal.aborted() => anyhow::bail!("Operation aborted"),
    }
}

fn redirect_status_text(status_code: u16) -> &'static str {
    // Maps to CC `WebFetchTool.call` redirect status copy.
    match status_code {
        301 => "Moved Permanently",
        308 => "Permanent Redirect",
        307 => "Temporary Redirect",
        _ => "Found",
    }
}

pub(crate) fn redirect_message(response: &RedirectInfo, prompt: &str) -> String {
    // Maps to CC `WebFetchTool.call` redirect response body.
    format!(
        "REDIRECT DETECTED: The URL redirects to a different host.\n\nOriginal URL: {}\nRedirect URL: {}\nStatus: {} {}\n\nTo complete your request, I need to fetch content from the redirected URL. Please use WebFetch again with these parameters:\n- url: \"{}\"\n- prompt: \"{}\"",
        response.original_url,
        response.redirect_url,
        response.status_code,
        redirect_status_text(response.status_code),
        response.redirect_url,
        prompt
    )
}

pub(crate) fn redirect_code_text(status_code: u16) -> String {
    redirect_status_text(status_code).to_string()
}

/// Maps to CC `getURLMarkdownContent(...)`.
#[cfg(feature = "mcp_runtime")]
pub(crate) async fn get_url_markdown_content(
    url: &str,
    abort_controller: &AbortController,
) -> anyhow::Result<UrlMarkdownContent> {
    ensure_not_aborted(abort_controller)?;
    if !validate_url(url) {
        anyhow::bail!("Invalid URL");
    }

    if let Ok(mut cache) = url_cache().lock() {
        if let Some(entry) = cache.get(url) {
            return Ok(UrlMarkdownContent::Fetched(entry));
        }
    }

    let mut parsed = reqwest::Url::parse(url)?;
    if parsed.scheme() == "http" {
        parsed
            .set_scheme("https")
            .map_err(|_| anyhow::anyhow!("Unable to upgrade URL to HTTPS"))?;
    }
    let upgraded_url = parsed.to_string();
    let hostname = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL"))?
        .to_string();
    let settings = crate::utils::settings::get_initial_settings();
    if settings.skip_web_fetch_preflight != Some(true) {
        match check_domain_blocklist(&hostname, abort_controller).await? {
            DomainCheckResult::Allowed => {}
            DomainCheckResult::Blocked => {
                anyhow::bail!("Claude Code is unable to fetch from {hostname}");
            }
            DomainCheckResult::CheckFailed(_) => {
                anyhow::bail!(
                    "Unable to verify if domain {hostname} is safe to fetch. This may be due to network restrictions or enterprise security policies blocking claude.ai."
                );
            }
        }
    }

    // CC's `axios.get` (`utils.ts:272-282`) goes through the global axios
    // interceptor; `create_axios_instance` resolves the same transport.
    let client = crate::utils::proxy::create_axios_instance()?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
        .build()?;
    let response =
        get_with_permitted_redirects(&client, &upgraded_url, abort_controller, 0).await?;
    let fetched = match response {
        HttpFetchResult::Redirect(info) => return Ok(UrlMarkdownContent::Redirect(info)),
        HttpFetchResult::Response(response) => response,
    };

    let content_type = fetched.content_type;
    let status_text = fetched.status_text;
    let bytes = fetched.body.len();

    // Maps to CC `getURLMarkdownContent(...)` binary persistence branch.
    let (persisted_path, persisted_size) =
        if crate::utils::mcp_output_storage::is_binary_content_type(&content_type) {
            let persist_id = format!(
                "webfetch-{}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .chars()
                    .take(6)
                    .collect::<String>()
            );
            match crate::utils::mcp_output_storage::persist_binary_content(
                &fetched.body,
                Some(&content_type),
                &persist_id,
            ) {
                crate::utils::mcp_output_storage::PersistBinaryResult::Saved(saved) => (
                    Some(saved.filepath.to_string_lossy().to_string()),
                    Some(saved.size),
                ),
                crate::utils::mcp_output_storage::PersistBinaryResult::Error { .. } => (None, None),
            }
        } else {
            (None, None)
        };

    let decoded = String::from_utf8_lossy(&fetched.body).to_string();
    let markdown_content = if content_type.to_ascii_lowercase().contains("text/html") {
        html_to_markdown(&decoded)
    } else {
        decoded
    };
    let cache_size = markdown_content.len().max(1);
    let entry = CacheEntry {
        inserted_at: Instant::now(),
        last_access_order: 0,
        bytes,
        code: fetched.status_code,
        code_text: status_text.clone(),
        content: markdown_content.clone(),
        content_type: content_type.clone(),
        persisted_path: persisted_path.clone(),
        persisted_size,
        cache_size,
    };
    if let Ok(mut cache) = url_cache().lock() {
        cache.insert(url.to_string(), entry);
    }

    Ok(UrlMarkdownContent::Fetched(FetchedContent {
        content: markdown_content,
        bytes,
        code: fetched.status_code,
        code_text: status_text,
        content_type,
        persisted_path,
        persisted_size,
    }))
}

/// Safe fallback when the network/runtime feature is disabled.
/// Maps to CC `getURLMarkdownContent(...)`; current safety behavior: no HTTP
/// request is sent. Build with default features (`mcp_runtime`) to enable the
/// real network path.
#[cfg(not(feature = "mcp_runtime"))]
pub(crate) async fn get_url_markdown_content(
    _url: &str,
    _abort_controller: &AbortController,
) -> anyhow::Result<UrlMarkdownContent> {
    anyhow::bail!("WebFetch network execution requires the mcp_runtime feature")
}

#[cfg(feature = "mcp_runtime")]
#[derive(Clone, Debug, PartialEq, Eq)]
enum DomainCheckResult {
    Allowed,
    Blocked,
    CheckFailed(String),
}

/// Maps to CC `checkDomainBlocklist(...)`.
#[cfg(feature = "mcp_runtime")]
async fn check_domain_blocklist(
    domain: &str,
    abort_controller: &AbortController,
) -> anyhow::Result<DomainCheckResult> {
    let now = Instant::now();
    if let Ok(mut cache) = domain_check_cache().lock() {
        cache.retain(|_, inserted| now.duration_since(*inserted) <= Duration::from_secs(5 * 60));
        if cache.contains_key(domain) {
            return Ok(DomainCheckResult::Allowed);
        }
    }

    // CC `axios.get` (`utils.ts:183-186`), through the global interceptor.
    let client = crate::utils::proxy::create_axios_instance()?
        .timeout(Duration::from_millis(DOMAIN_CHECK_TIMEOUT_MS))
        .build()?;
    let mut url = reqwest::Url::parse("https://api.anthropic.com/api/web/domain_info")?;
    url.query_pairs_mut().append_pair("domain", domain);
    let response = match wait_or_abort(client.get(url).send(), abort_controller).await? {
        Ok(response) => response,
        Err(error) => return Ok(DomainCheckResult::CheckFailed(error.to_string())),
    };
    if response.status() != reqwest::StatusCode::OK {
        return Ok(DomainCheckResult::CheckFailed(format!(
            "Domain check returned status {}",
            response.status().as_u16()
        )));
    }
    let body = match wait_or_abort(response.json::<serde_json::Value>(), abort_controller).await? {
        Ok(body) => body,
        Err(error) => return Ok(DomainCheckResult::CheckFailed(error.to_string())),
    };
    if body.get("can_fetch").and_then(|value| value.as_bool()) == Some(true) {
        if let Ok(mut cache) = domain_check_cache().lock() {
            cache.insert(domain.to_string(), Instant::now());
        }
        return Ok(DomainCheckResult::Allowed);
    }
    Ok(DomainCheckResult::Blocked)
}

#[cfg(feature = "mcp_runtime")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct HttpResponseBody {
    status_code: u16,
    status_text: String,
    content_type: String,
    body: Vec<u8>,
}

#[cfg(feature = "mcp_runtime")]
enum HttpFetchResult {
    Response(HttpResponseBody),
    Redirect(RedirectInfo),
}

/// Maps to CC `getWithPermittedRedirects(...)`.
#[cfg(feature = "mcp_runtime")]
async fn get_with_permitted_redirects(
    client: &reqwest::Client,
    url: &str,
    abort_controller: &AbortController,
    depth: usize,
) -> anyhow::Result<HttpFetchResult> {
    ensure_not_aborted(abort_controller)?;
    if depth > MAX_REDIRECTS {
        anyhow::bail!("Too many redirects (exceeded {MAX_REDIRECTS})");
    }

    let response = wait_or_abort(
        client
            .get(url)
            .header(reqwest::header::ACCEPT, "text/markdown, text/html, */*")
            .header(
                reqwest::header::USER_AGENT,
                crate::utils::http::get_web_fetch_user_agent(),
            )
            .send(),
        abort_controller,
    )
    .await??;
    ensure_not_aborted(abort_controller)?;

    let status = response.status();
    if matches!(status.as_u16(), 301 | 302 | 307 | 308) {
        let redirect_location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("Redirect missing Location header"))?;
        let redirect_url = reqwest::Url::parse(url)?
            .join(redirect_location)?
            .to_string();
        if is_permitted_redirect(url, &redirect_url) {
            return Box::pin(get_with_permitted_redirects(
                client,
                &redirect_url,
                abort_controller,
                depth + 1,
            ))
            .await;
        }
        return Ok(HttpFetchResult::Redirect(RedirectInfo {
            original_url: url.to_string(),
            redirect_url,
            status_code: status.as_u16(),
        }));
    }

    if status == reqwest::StatusCode::FORBIDDEN
        && response
            .headers()
            .get("x-proxy-error")
            .and_then(|value| value.to_str().ok())
            == Some("blocked-by-allowlist")
    {
        let hostname = reqwest::Url::parse(url)?
            .host_str()
            .unwrap_or_default()
            .to_string();
        anyhow::bail!(
            "{}",
            serde_json::json!({
                "error_type": "EGRESS_BLOCKED",
                "domain": hostname,
                "message": format!("Access to {hostname} is blocked by the network egress proxy."),
            })
        );
    }

    if !status.is_success() {
        anyhow::bail!("Request failed with status {}", status.as_u16());
    }

    if response
        .content_length()
        .is_some_and(|length| length as usize > MAX_HTTP_CONTENT_LENGTH)
    {
        anyhow::bail!(
            "Response exceeded maximum content length of {MAX_HTTP_CONTENT_LENGTH} bytes"
        );
    }

    let status_code = status.as_u16();
    let status_text = status.canonical_reason().unwrap_or_default().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = wait_or_abort(response.bytes(), abort_controller)
        .await??
        .to_vec();
    ensure_not_aborted(abort_controller)?;
    if body.len() > MAX_HTTP_CONTENT_LENGTH {
        anyhow::bail!(
            "Response exceeded maximum content length of {MAX_HTTP_CONTENT_LENGTH} bytes"
        );
    }

    Ok(HttpFetchResult::Response(HttpResponseBody {
        status_code,
        status_text,
        content_type,
        body,
    }))
}

fn html_to_markdown(html: &str) -> String {
    // Maps to CC `tools/WebFetchTool/utils.ts#getURLMarkdownContent` Turndown
    // conversion branch. Intentional Rust implementation choice: use the
    // native `supermarkdown` crate rather than embedding JS `turndown@7.2.2`.
    // Compatibility knobs below mirror Turndown defaults where supermarkdown
    // exposes an equivalent (Setext h1/h2, `*` bullets, inline links). Known
    // output divergences: code blocks remain fenced, tables remain GFM tables,
    // strikethrough is preserved, and whitespace is supermarkdown-normalized.
    let options = supermarkdown::Options::new()
        .heading_style(supermarkdown::HeadingStyle::Setext)
        .bullet_marker('*')
        .link_style(supermarkdown::LinkStyle::Inline);
    supermarkdown::convert_with_options(html, &options)
}

/// Maps to CC `applyPromptToMarkdown(...)`.
pub(crate) async fn apply_prompt_to_markdown(
    prompt: &str,
    markdown_content: &str,
    abort_controller: &AbortController,
    is_non_interactive_session: bool,
    is_preapproved_domain: bool,
) -> anyhow::Result<String> {
    ensure_not_aborted(abort_controller)?;
    let truncated_content = if markdown_content.len() > MAX_MARKDOWN_LENGTH {
        let mut end = MAX_MARKDOWN_LENGTH;
        while end > 0 && !markdown_content.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n\n[Content truncated due to length...]",
            &markdown_content[..end]
        )
    } else {
        markdown_content.to_string()
    };
    let model_prompt = crate::tools::web_fetch_tool::prompt::make_secondary_model_prompt(
        &truncated_content,
        prompt,
        is_preapproved_domain,
    );
    let mut options = crate::services::api::claude::Options::new(
        crate::utils::model::model::get_small_fast_model(),
        "web_fetch_apply".to_string(),
    );
    options.is_non_interactive_session = is_non_interactive_session;
    options.has_append_system_prompt = false;
    options.mcp_tools = Vec::new();
    options.abort_signal = Some(abort_controller.signal());
    let assistant =
        crate::services::api::claude::query_haiku(&Vec::new(), &model_prompt, None, &options)
            .await?;
    ensure_not_aborted(abort_controller)?;
    for block in assistant.content {
        if let crate::types::message::AssistantContent::Text(text) = block {
            return Ok(text);
        }
    }
    Ok("No response from model".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_matches_official_security_filters() {
        assert!(validate_url("https://example.com/docs"));
        assert!(validate_url("http://example.com/docs"));
        assert!(!validate_url("https://example"));
        assert!(!validate_url("not a url"));
        assert!(!validate_url("https://user:pass@example.com/docs"));
        assert!(!validate_url(&format!(
            "https://example.com/{}",
            "a".repeat(2_001)
        )));
    }

    #[test]
    fn redirect_policy_allows_only_same_host_or_www_variants() {
        assert!(is_permitted_redirect(
            "https://example.com/docs",
            "https://www.example.com/guide"
        ));
        assert!(is_permitted_redirect(
            "https://www.example.com/docs",
            "https://example.com/guide"
        ));
        assert!(is_permitted_redirect(
            "https://example.com/docs",
            "https://example.com/other?q=1"
        ));
        assert!(!is_permitted_redirect(
            "https://example.com/docs",
            "http://example.com/guide"
        ));
        assert!(!is_permitted_redirect(
            "https://example.com/docs",
            "https://evil.example/guide"
        ));
        assert!(!is_permitted_redirect(
            "https://example.com/docs",
            "https://user:pass@example.com/guide"
        ));
    }

    #[test]
    fn redirect_message_matches_official_retry_instruction_shape() {
        let info = RedirectInfo {
            original_url: "https://example.com/a".to_string(),
            redirect_url: "https://other.example/b".to_string(),
            status_code: 302,
        };
        let message = redirect_message(&info, "summarize");
        assert!(message.contains("REDIRECT DETECTED"));
        assert!(message.contains("Original URL: https://example.com/a"));
        assert!(message.contains("Redirect URL: https://other.example/b"));
        assert!(message.contains("Status: 302 Found"));
        assert!(message.contains("- url: \"https://other.example/b\""));
        assert!(message.contains("- prompt: \"summarize\""));
    }

    fn cache_entry(label: &str, cache_size: usize) -> CacheEntry {
        CacheEntry {
            inserted_at: Instant::now(),
            last_access_order: 0,
            bytes: label.len(),
            code: 200,
            code_text: "OK".to_string(),
            content: label.to_string(),
            content_type: "text/markdown".to_string(),
            persisted_path: None,
            persisted_size: None,
            cache_size,
        }
    }

    #[test]
    fn web_fetch_cache_evicts_least_recently_used_like_official_lru_cache() {
        let mut cache = WebFetchCache::default();
        let chunk = MAX_CACHE_SIZE_BYTES / 2;
        cache.insert("https://example.com/a".to_string(), cache_entry("a", chunk));
        cache.insert("https://example.com/b".to_string(), cache_entry("b", chunk));

        assert_eq!(
            cache
                .get("https://example.com/a")
                .expect("a should be cached")
                .content,
            "a"
        );
        cache.insert("https://example.com/c".to_string(), cache_entry("c", chunk));

        assert!(cache.get("https://example.com/b").is_none());
        assert_eq!(
            cache
                .get("https://example.com/a")
                .expect("recently accessed a should survive")
                .content,
            "a"
        );
        assert_eq!(
            cache
                .get("https://example.com/c")
                .expect("new c should be cached")
                .content,
            "c"
        );
    }

    #[test]
    fn web_fetch_cache_expires_entries_by_insert_age_without_extending_ttl_on_get() {
        let mut cache = WebFetchCache::default();
        let mut expired = cache_entry("expired", 1);
        expired.inserted_at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
        cache.insert("https://example.com/expired".to_string(), expired);
        assert!(cache.get("https://example.com/expired").is_none());
    }

    #[test]
    fn html_conversion_uses_supermarkdown_with_turndown_compat_knobs() {
        let markdown = html_to_markdown(
            "<html><body><h1>Title &amp; Docs</h1><h2>Subhead</h2><ul><li>One</li></ul><p>Hello<br>world</p></body></html>",
        );
        assert!(markdown.contains("Title & Docs\n====="));
        assert!(markdown.contains("Subhead\n---"));
        assert!(markdown.contains("* One"));
        assert!(markdown.contains("Hello"));
        assert!(markdown.contains("world"));
        assert!(!markdown.contains("<h1>"));
    }

    #[test]
    fn html_conversion_documents_intentional_supermarkdown_divergences() {
        let markdown = html_to_markdown(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table><pre><code class=\"language-rust\">fn main() {}</code></pre><del>old</del>",
        );
        assert!(markdown.contains("| A"));
        assert!(markdown.contains("```rust"));
        assert!(markdown.contains("~~old~~"));
    }
}
