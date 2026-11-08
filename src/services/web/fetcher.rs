//! SSRF-checked WebFetch transport.
//!
//! @nyan: CC's WebFetch transport is retained only when `nyan.web` is absent.
//! When this module is selected, every redirect is normalized and checked again,
//! proxy mode is explicit, and a response is either fully accepted or discarded.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;

use crate::services::web::host_guard::{self, HostGuardConfig, HostGuardError, ValidatedUrl};
use crate::tool::AbortController;
use crate::utils::settings::nyan::{FetchVia, WebSettings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectHop {
    pub from: String,
    pub to: String,
    pub status: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedPage {
    pub original_url: String,
    pub final_url: String,
    pub redirects: Vec<RedirectHop>,
    pub status: u16,
    pub content_type: String,
    pub fetched_at: DateTime<Utc>,
    pub raw_body: Vec<u8>,
    pub text: String,
}

impl FetchedPage {
    pub fn bytes(&self) -> usize {
        self.raw_body.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    InvalidUrl,
    DeniedScheme,
    DeniedPort(u16),
    DeniedPrivateNetwork,
    DeniedHost,
    ProxyUnconfigured,
    TooManyRedirects,
    TooLarge,
    Timeout,
    Cancelled,
    HttpStatus(u16),
    ContentTypeDenied(String),
    PdfConversionUnavailable,
    PdfConversionFailed,
    ResolveFailed,
    ResolvePrivateNetwork,
    Network,
    InvalidResponse,
}

impl FetchError {
    pub fn code(&self) -> String {
        match self {
            Self::InvalidUrl => "denied_host".to_string(),
            Self::DeniedScheme => "denied_scheme".to_string(),
            Self::DeniedPort(_) => "denied_port".to_string(),
            Self::DeniedPrivateNetwork => "denied_private_network".to_string(),
            Self::DeniedHost => "denied_host".to_string(),
            Self::ProxyUnconfigured => "proxy_unconfigured".to_string(),
            Self::TooManyRedirects => "too_many_redirects".to_string(),
            Self::TooLarge => "too_large".to_string(),
            Self::Timeout => "timeout".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::HttpStatus(status) => format!("http_{status}"),
            Self::ContentTypeDenied(_)
            | Self::PdfConversionUnavailable
            | Self::PdfConversionFailed => "content_type_denied".to_string(),
            Self::ResolveFailed | Self::ResolvePrivateNetwork | Self::Network => {
                "network_error".to_string()
            }
            Self::InvalidResponse => "invalid_response".to_string(),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str("denied_host: invalid URL"),
            Self::DeniedScheme => f.write_str("denied_scheme"),
            Self::DeniedPort(port) => write!(f, "denied_port: {port}"),
            Self::DeniedPrivateNetwork => f.write_str("denied_private_network"),
            Self::DeniedHost => f.write_str("denied_host"),
            Self::ProxyUnconfigured => f.write_str("proxy_unconfigured"),
            Self::TooManyRedirects => f.write_str("too_many_redirects"),
            Self::TooLarge => f.write_str("too_large"),
            Self::Timeout => f.write_str("timeout"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::HttpStatus(status) => write!(f, "http_{status}"),
            Self::ContentTypeDenied(reason) => write!(f, "content_type_denied: {reason}"),
            Self::PdfConversionUnavailable => {
                f.write_str("content_type_denied: pdf conversion unavailable")
            }
            Self::PdfConversionFailed => f.write_str("content_type_denied: pdf conversion failed"),
            Self::ResolveFailed => f.write_str("network_error: DNS resolution failed"),
            Self::ResolvePrivateNetwork => f.write_str("denied_private_network"),
            Self::Network => f.write_str("network_error"),
            Self::InvalidResponse => f.write_str("invalid_response"),
        }
    }
}

impl std::error::Error for FetchError {}

#[derive(Clone, Debug)]
pub struct FetchConfig {
    pub via: FetchVia,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_redirects: usize,
    pub max_bytes: usize,
    pub host_guard: HostGuardConfig,
    #[cfg(test)]
    pub allow_private_for_tests: bool,
}

impl FetchConfig {
    pub fn from_settings(web: &WebSettings) -> Self {
        let fetch = web.fetch();
        Self {
            via: web.fetch_via(),
            timeout: Duration::from_millis(fetch.timeout_ms()),
            connect_timeout: Duration::from_millis(fetch.connect_timeout_ms()),
            max_redirects: fetch.max_redirects() as usize,
            max_bytes: web.max_bytes().min(usize::MAX as u64) as usize,
            host_guard: HostGuardConfig {
                allow_hosts: web.allow_hosts().to_vec(),
                deny_hosts: web.deny_hosts().to_vec(),
                allow_ports: web.allow_ports().to_vec(),
                #[cfg(test)]
                allow_private_for_tests: false,
            },
            #[cfg(test)]
            allow_private_for_tests: false,
        }
    }

    #[cfg(test)]
    pub fn for_local_mock() -> Self {
        let mut config = Self::from_settings(&WebSettings::default());
        config.allow_private_for_tests = true;
        config.host_guard.allow_private_for_tests = true;
        config.via = FetchVia::Direct;
        config
    }
}

pub fn validate_target(raw_url: &str, config: &FetchConfig) -> Result<ValidatedUrl, FetchError> {
    validate(raw_url, config)
}

pub async fn fetch_url(
    raw_url: &str,
    abort_controller: &AbortController,
    config: &FetchConfig,
) -> Result<FetchedPage, FetchError> {
    ensure_not_aborted(abort_controller)?;
    let mut current = validate(raw_url, config)?;
    let original_url = current.url.to_string();
    let mut redirects = Vec::new();

    loop {
        ensure_not_aborted(abort_controller)?;
        let client = build_client(&current, config).await?;
        let response =
            send_request(&client, &current.url, abort_controller, config.timeout).await?;
        let status = response.status();

        if status.is_redirection() {
            if redirects.len() >= config.max_redirects {
                return Err(FetchError::TooManyRedirects);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::InvalidResponse)?;
            let next_url = current
                .url
                .join(location)
                .map_err(|_| FetchError::InvalidResponse)?;
            let next = validate(next_url.as_str(), config)?;
            redirects.push(RedirectHop {
                from: current.url.to_string(),
                to: next.url.to_string(),
                status: status.as_u16(),
            });
            current = next;
            continue;
        }

        let status_code = status.as_u16();
        if !status.is_success() {
            return Err(FetchError::HttpStatus(status_code));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        if !is_allowed_content_type(&content_type) {
            return Err(FetchError::ContentTypeDenied(content_type));
        }
        if response
            .content_length()
            .is_some_and(|length| length > config.max_bytes as u64)
        {
            return Err(FetchError::TooLarge);
        }
        let raw_body = read_body(response, config.max_bytes, abort_controller).await?;
        let text = convert_body(&content_type, &raw_body).await?;
        return Ok(FetchedPage {
            original_url,
            final_url: current.url.to_string(),
            redirects,
            status: status_code,
            content_type,
            fetched_at: Utc::now(),
            raw_body,
            text,
        });
    }
}

fn validate(raw_url: &str, config: &FetchConfig) -> Result<ValidatedUrl, FetchError> {
    #[cfg(test)]
    {
        let mut guard = config.host_guard.clone();
        guard.allow_private_for_tests |= config.allow_private_for_tests;
        host_guard::validate_url(raw_url, &guard).map_err(map_guard_error)
    }
    #[cfg(not(test))]
    host_guard::validate_url(raw_url, &config.host_guard).map_err(map_guard_error)
}

fn map_guard_error(error: HostGuardError) -> FetchError {
    match error {
        HostGuardError::InvalidUrl | HostGuardError::Credentials => FetchError::InvalidUrl,
        HostGuardError::DeniedScheme => FetchError::DeniedScheme,
        HostGuardError::DeniedPort(port) => FetchError::DeniedPort(port),
        HostGuardError::DeniedPrivateNetwork => FetchError::DeniedPrivateNetwork,
        HostGuardError::DeniedHost => FetchError::DeniedHost,
    }
}

async fn build_client(
    validated: &ValidatedUrl,
    config: &FetchConfig,
) -> Result<reqwest::Client, FetchError> {
    crate::utils::tls_provider::install_crypto_provider();
    let mut builder = crate::utils::proxy::client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(config.connect_timeout)
        .timeout(config.timeout)
        .pool_idle_timeout(Duration::from_secs(90));

    match config.via {
        FetchVia::Proxy => {
            let env = crate::utils::process_env::snapshot();
            let Some(proxy_url) = crate::utils::proxy::get_proxy_url(&env) else {
                return Err(FetchError::ProxyUnconfigured);
            };
            let proxy = reqwest::Proxy::all(proxy_url).map_err(|_| FetchError::InvalidResponse)?;
            // @nyan: do not call create_axios_instance here. Its NO_PROXY
            // matcher would silently turn an explicit proxy fetch into direct I/O.
            builder = builder.proxy(proxy);
        }
        FetchVia::Direct => {
            builder = builder.no_proxy();
            // @nyan: the mock is on loopback, which the resolver check rejects
            // by design. Test builds skip pinning for it; production never does.
            #[cfg(test)]
            let pin = !config.allow_private_for_tests;
            #[cfg(not(test))]
            let pin = true;
            if pin {
                let addresses = resolve_public_addresses(validated).await?;
                builder = builder.resolve_to_addrs(&validated.host, &addresses);
            }
        }
    }

    builder.build().map_err(|_| FetchError::InvalidResponse)
}

async fn resolve_public_addresses(validated: &ValidatedUrl) -> Result<Vec<SocketAddr>, FetchError> {
    let addresses = tokio::net::lookup_host((validated.host.as_str(), validated.port))
        .await
        .map_err(|_| FetchError::ResolveFailed)?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(FetchError::ResolveFailed);
    }
    if addresses
        .iter()
        .any(|address| host_guard::is_private_ip(&address.ip()))
    {
        return Err(FetchError::ResolvePrivateNetwork);
    }
    Ok(addresses)
}

async fn send_request(
    client: &reqwest::Client,
    url: &reqwest::Url,
    abort_controller: &AbortController,
    timeout: Duration,
) -> Result<reqwest::Response, FetchError> {
    let request = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "text/html, text/plain, text/markdown, application/json, application/xml, application/pdf")
        .header(
            reqwest::header::USER_AGENT,
            crate::utils::http::get_web_fetch_user_agent(),
        )
        .send();
    let mut signal = abort_controller.signal();
    tokio::time::timeout(timeout, async {
        tokio::select! {
            result = request => result.map_err(|error| if error.is_timeout() { FetchError::Timeout } else { FetchError::Network }),
            _ = signal.aborted() => Err(FetchError::Cancelled),
        }
    })
    .await
    .map_err(|_| FetchError::Timeout)?
}

async fn read_body(
    response: reqwest::Response,
    max_bytes: usize,
    abort_controller: &AbortController,
) -> Result<Vec<u8>, FetchError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut signal = abort_controller.signal();
    while let Some(chunk) = tokio::select! {
        value = stream.next() => value,
        _ = signal.aborted() => return Err(FetchError::Cancelled),
    } {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_allowed_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "text/html"
            | "text/plain"
            | "text/markdown"
            | "application/json"
            | "application/xml"
            | "text/xml"
            | "application/pdf"
    )
}

async fn convert_body(content_type: &str, body: &[u8]) -> Result<String, FetchError> {
    if content_type == "application/pdf" {
        return convert_pdf(body).await;
    }
    let decoded = String::from_utf8_lossy(body).into_owned();
    if content_type == "text/html" {
        Ok(html_to_markdown(&decoded))
    } else {
        Ok(decoded)
    }
}

fn html_to_markdown(html: &str) -> String {
    // @nyan: the converter keeps script/style text by default; a snapshot must
    // hold page content only (external-search 4.3).
    let options = supermarkdown::Options::new()
        .exclude_selectors(
            ["script", "style", "noscript", "template"]
                .map(str::to_string)
                .to_vec(),
        )
        .heading_style(supermarkdown::HeadingStyle::Setext)
        .bullet_marker('*')
        .link_style(supermarkdown::LinkStyle::Inline);
    supermarkdown::convert_with_options(html, &options)
}

async fn convert_pdf(body: &[u8]) -> Result<String, FetchError> {
    let snapshots = crate::utils::env_utils::get_config_home_dir().join("web-snapshots");
    let directory = snapshots.join("tmp");
    std::fs::create_dir_all(&directory).map_err(|_| FetchError::PdfConversionFailed)?;
    set_private_directory(&snapshots);
    set_private_directory(&directory);
    let path = directory.join(format!("{}.pdf", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, body).map_err(|_| FetchError::PdfConversionFailed)?;
    set_private_permissions(&path);
    let output = tokio::process::Command::new("pdftotext")
        .arg("-enc")
        .arg("UTF-8")
        .arg(&path)
        .arg("-")
        .output()
        .await;
    let _ = std::fs::remove_file(&path);
    let output = output.map_err(|_| FetchError::PdfConversionUnavailable)?;
    if !output.status.success() {
        return Err(FetchError::PdfConversionFailed);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn set_private_directory(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn set_private_permissions(path: &PathBuf) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn ensure_not_aborted(abort_controller: &AbortController) -> Result<(), FetchError> {
    if abort_controller.is_aborted() {
        Err(FetchError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_allowlist_is_closed() {
        assert!(is_allowed_content_type("text/html"));
        assert!(is_allowed_content_type("application/pdf"));
        assert!(!is_allowed_content_type("image/png"));
        assert!(!is_allowed_content_type("application/octet-stream"));
    }

    #[tokio::test]
    async fn proxy_mode_without_proxy_is_named_and_does_not_connect() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let _http = crate::utils::test_env::EnvVarGuard::unset("http_proxy");
        let _https = crate::utils::test_env::EnvVarGuard::unset("https_proxy");
        let _http_upper = crate::utils::test_env::EnvVarGuard::unset("HTTP_PROXY");
        let _https_upper = crate::utils::test_env::EnvVarGuard::unset("HTTPS_PROXY");
        let config = FetchConfig::from_settings(&WebSettings::default());
        let error = fetch_url("https://example.com", &AbortController::default(), &config)
            .await
            .unwrap_err();
        assert_eq!(error, FetchError::ProxyUnconfigured);
    }

    #[test]
    fn pdf_conversion_uses_private_temp_path_helper() {
        let path = PathBuf::from("/tmp/nyan-web-fetch-test.pdf");
        set_private_permissions(&path);
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::services::web::test_support::{Route, serve};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn config() -> FetchConfig {
        let mut config = FetchConfig::for_local_mock();
        config.timeout = Duration::from_secs(5);
        config
    }

    async fn fetch(url: &str, config: &FetchConfig) -> Result<FetchedPage, FetchError> {
        fetch_url(url, &AbortController::default(), config).await
    }

    #[tokio::test]
    async fn html_is_converted_and_the_raw_bytes_are_kept() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p><script>evil()</script></body></html>";
        let server = serve(vec![("/page", Route::html(html))]).await;
        let page = fetch(&server.url("/page"), &config()).await.unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.content_type, "text/html");
        assert_eq!(page.raw_body, html.as_bytes());
        assert!(page.text.contains("Title"), "{}", page.text);
        assert!(page.text.contains("**world**") || page.text.contains("world"));
        assert!(!page.text.contains("evil()"), "{}", page.text);
        assert!(page.redirects.is_empty());
        let seen = &server.seen()[0];
        assert!(seen.header("accept").unwrap().contains("text/markdown"));
        assert_eq!(
            seen.header("user-agent").unwrap(),
            crate::utils::http::get_web_fetch_user_agent()
        );
    }

    #[tokio::test]
    async fn redirects_are_followed_across_paths_and_recorded() {
        let server = serve(vec![
            ("/a", Route::redirect(301, "/b")),
            ("/b", Route::redirect(302, "/c")),
            ("/c", Route::text("final body")),
        ])
        .await;
        let page = fetch(&server.url("/a"), &config()).await.unwrap();
        assert_eq!(page.text, "final body");
        assert_eq!(page.final_url, server.url("/c"));
        assert_eq!(page.original_url, server.url("/a"));
        assert_eq!(
            page.redirects
                .iter()
                .map(|hop| (hop.status, hop.to.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (301, server.url("/b").as_str()),
                (302, server.url("/c").as_str())
            ]
        );
    }

    #[tokio::test]
    async fn the_sixth_redirect_is_too_many() {
        let routes = vec![
            ("/0", Route::redirect(302, "/1")),
            ("/1", Route::redirect(302, "/2")),
            ("/2", Route::redirect(302, "/3")),
            ("/3", Route::redirect(302, "/4")),
            ("/4", Route::redirect(302, "/5")),
            ("/5", Route::redirect(302, "/6")),
            ("/6", Route::text("unreachable")),
        ];
        let server = serve(routes).await;
        let mut limited = config();
        limited.max_redirects = 5;
        assert_eq!(
            fetch(&server.url("/0"), &limited).await.unwrap_err(),
            FetchError::TooManyRedirects
        );
        assert_eq!(server.hits("/6"), 0);
        // Exactly five hops still succeed.
        assert_eq!(
            fetch(&server.url("/1"), &limited).await.unwrap().text,
            "unreachable"
        );
    }

    #[tokio::test]
    async fn a_redirect_into_a_private_network_is_denied_on_that_hop() {
        // The production guard (no test seam) rejects each private spelling.
        let production = FetchConfig::from_settings(&WebSettings::default());
        for target in [
            "https://127.0.0.1/admin",
            "https://[::ffff:127.0.0.1]/",
            "https://169.254.169.254/latest/meta-data",
            "https://100.80.19.12:443/",
            "https://printer.local/",
        ] {
            assert_eq!(
                validate_target(target, &production).unwrap_err(),
                FetchError::DeniedPrivateNetwork,
                "{target}"
            );
        }
        // The loop applies the same validation to `Location`: a hop to a denied
        // host is rejected even though the start URL was fine.
        let server = serve(vec![(
            "/start",
            Route::redirect(302, "https://denied.example/secret"),
        )])
        .await;
        let mut guarded = config();
        guarded.host_guard.deny_hosts = vec!["denied.example".to_string()];
        assert_eq!(
            fetch(&server.url("/start"), &guarded).await.unwrap_err(),
            FetchError::DeniedHost
        );
    }

    #[tokio::test]
    async fn a_redirect_without_a_location_is_invalid() {
        let mut route = Route::new(302, "text/plain", "");
        route.headers.retain(|(name, _)| name != "location");
        let server = serve(vec![("/x", route)]).await;
        assert_eq!(
            fetch(&server.url("/x"), &config()).await.unwrap_err(),
            FetchError::InvalidResponse
        );
    }

    #[tokio::test]
    async fn http_errors_and_content_types_are_named() {
        let server = serve(vec![
            ("/forbidden", Route::new(403, "text/html", "no")),
            ("/limited", Route::new(429, "text/html", "slow down")),
            ("/gone", Route::new(404, "text/html", "gone")),
            (
                "/image",
                Route::new(200, "image/png", vec![0x89, b'P', b'N', b'G']),
            ),
            ("/blob", Route::new(200, "application/octet-stream", "x")),
            ("/none", Route::new(200, "", "x")),
            (
                "/json",
                Route::new(200, "application/json; charset=utf-8", r#"{"a":1}"#),
            ),
        ])
        .await;
        let cfg = config();
        assert_eq!(
            fetch(&server.url("/forbidden"), &cfg).await.unwrap_err(),
            FetchError::HttpStatus(403)
        );
        assert_eq!(
            fetch(&server.url("/limited"), &cfg).await.unwrap_err(),
            FetchError::HttpStatus(429)
        );
        assert_eq!(
            fetch(&server.url("/gone"), &cfg).await.unwrap_err(),
            FetchError::HttpStatus(404)
        );
        assert!(matches!(
            fetch(&server.url("/image"), &cfg).await.unwrap_err(),
            FetchError::ContentTypeDenied(kind) if kind == "image/png"
        ));
        assert!(matches!(
            fetch(&server.url("/blob"), &cfg).await.unwrap_err(),
            FetchError::ContentTypeDenied(_)
        ));
        assert!(matches!(
            fetch(&server.url("/none"), &cfg).await.unwrap_err(),
            FetchError::ContentTypeDenied(_)
        ));
        assert_eq!(
            fetch(&server.url("/json"), &cfg).await.unwrap().text,
            r#"{"a":1}"#
        );
    }

    #[tokio::test]
    async fn oversized_bodies_are_rejected_not_truncated() {
        let server = serve(vec![("/big", Route::text(&"x".repeat(2048)))]).await;
        let mut small = config();
        small.max_bytes = 1024;
        assert_eq!(
            fetch(&server.url("/big"), &small).await.unwrap_err(),
            FetchError::TooLarge
        );
        small.max_bytes = 4096;
        assert_eq!(
            fetch(&server.url("/big"), &small)
                .await
                .unwrap()
                .raw_body
                .len(),
            2048
        );
    }

    #[tokio::test]
    async fn a_slow_response_times_out_by_name() {
        let server = serve(vec![(
            "/slow",
            Route::text("late").delayed(Duration::from_millis(700)),
        )])
        .await;
        let mut quick = config();
        quick.timeout = Duration::from_millis(150);
        assert_eq!(
            fetch(&server.url("/slow"), &quick).await.unwrap_err(),
            FetchError::Timeout
        );
    }

    #[tokio::test]
    async fn an_aborted_fetch_is_cancelled_before_any_request() {
        let server = serve(vec![("/x", Route::text("body"))]).await;
        let abort = AbortController::default();
        abort.abort();
        assert_eq!(
            fetch_url(&server.url("/x"), &abort, &config())
                .await
                .unwrap_err(),
            FetchError::Cancelled
        );
        assert!(server.seen().is_empty());
    }

    #[tokio::test]
    async fn pdf_is_converted_by_pdftotext_and_leaves_no_temp_file() {
        let pdf = tiny_pdf("Hello from the PDF fixture");
        let home = std::env::temp_dir().join(format!("nyan-pdf-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _home = EnvVarGuard::set("NYAN_CONFIG_DIR", &home);
        let server = serve(vec![(
            "/doc.pdf",
            Route::new(200, "application/pdf", pdf.clone()),
        )])
        .await;
        let result = fetch(&server.url("/doc.pdf"), &config()).await;
        let leftovers = std::fs::read_dir(home.join("web-snapshots/tmp"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = std::fs::remove_dir_all(&home);
        match result {
            Ok(page) => {
                assert_eq!(page.content_type, "application/pdf");
                assert_eq!(page.raw_body, pdf);
                assert!(
                    page.text.contains("Hello from the PDF fixture"),
                    "{}",
                    page.text
                );
            }
            Err(FetchError::PdfConversionUnavailable) => {
                eprintln!("skipped: pdftotext is not installed");
            }
            Err(error) => panic!("unexpected pdf failure: {error}"),
        }
        assert_eq!(leftovers, 0, "temporary PDF was not removed");
    }

    /// A one-page PDF with a single text string, offsets computed in place.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 18 Tf 20 100 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[tokio::test]
    async fn proxy_mode_ignores_no_proxy_and_sends_the_request_to_the_proxy() {
        let proxy = serve(vec![("/via-proxy", Route::text("served by the proxy"))]).await;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _http = EnvVarGuard::set("http_proxy", &proxy.origin);
        let _https = EnvVarGuard::set("https_proxy", &proxy.origin);
        let _http_upper = EnvVarGuard::unset("HTTP_PROXY");
        let _https_upper = EnvVarGuard::unset("HTTPS_PROXY");
        // NO_PROXY names the target; a direct client would skip the proxy.
        let _no_proxy = EnvVarGuard::set("no_proxy", "example.test,127.0.0.1");
        let _no_proxy_upper = EnvVarGuard::set("NO_PROXY", "example.test,127.0.0.1");
        let mut proxied = config();
        proxied.via = FetchVia::Proxy;
        let page = fetch("http://example.test/via-proxy", &proxied)
            .await
            .unwrap();
        assert_eq!(page.text, "served by the proxy");
        let seen = proxy.seen();
        assert_eq!(seen.len(), 1, "the proxy must have received the request");
        assert!(
            seen[0].target.contains("example.test"),
            "{}",
            seen[0].target
        );
    }

    #[tokio::test]
    async fn direct_mode_does_not_use_a_configured_proxy() {
        let proxy = serve(vec![("/x", Route::text("proxy"))]).await;
        let origin = serve(vec![("/x", Route::text("origin"))]).await;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _http = EnvVarGuard::set("http_proxy", &proxy.origin);
        let _https = EnvVarGuard::set("https_proxy", &proxy.origin);
        let direct = config();
        assert_eq!(direct.via, FetchVia::Direct);
        let page = fetch(&origin.url("/x"), &direct).await.unwrap();
        assert_eq!(page.text, "origin");
        assert!(proxy.seen().is_empty());
    }
}
