//! Port of official `tools/BriefTool/upload.ts` — upload BriefTool attachments
//! to private_api so web viewers can preview them.
//!
//! When the repl bridge is active, attachment paths are meaningless to a web
//! viewer (they're on Claude's machine). We upload to `/api/oauth/file_upload` —
//! the same store MessageComposer/SpaceMessage render from — and stash the
//! returned `file_uuid` alongside the path. Web resolves `file_uuid` → preview;
//! desktop/local try path first.
//!
//! Best-effort: any failure (no token, bridge off, network error, 4xx) logs
//! debug and returns `None`. The attachment still carries `{path, size,
//! isImage}`, so local-terminal and same-machine-desktop render unaffected.
//!
//! `bridge/bridgeConfig.ts` has no Rust module yet, so the two override getters
//! this file consumes are inlined below with their own source attribution
//! rather than reaching across an unported boundary.

use crate::utils::process_env::JsTruthy;

/// Maps to: CC `tools/BriefTool/upload.ts:32` — matches the private_api backend
/// limit.
const MAX_UPLOAD_BYTES: u64 = 30 * 1024 * 1024;

/// Maps to: CC `tools/BriefTool/upload.ts:34` `UPLOAD_TIMEOUT_MS`.
const UPLOAD_TIMEOUT_MS: u64 = 30_000;

/// Maps to: CC `tools/BriefTool/upload.ts:43-49` `MIME_BY_EXT`.
///
/// Backend dispatches on mime: `image/*` → `upload_image_wrapped` (writes
/// PREVIEW/THUMBNAIL, no ORIGINAL), everything else → `upload_generic_file`
/// (ORIGINAL only, no preview). Only whitelist raster formats the transcoder
/// reliably handles — svg/bmp/ico risk a 400, and pdf routes to
/// `upload_pdf_file_wrapped` which also skips ORIGINAL. Dispatch viewers use
/// `/preview` for images and `/contents` for everything else, so images go
/// `image/*` and the rest go octet-stream.
const MIME_BY_EXT: &[(&str, &str)] = &[
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
];

/// Maps to: CC `tools/BriefTool/upload.ts:51-54` `guessMimeType`.
pub(crate) fn guess_mime_type(filename: &str) -> &'static str {
    let Some(extension) = std::path::Path::new(filename)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    else {
        return "application/octet-stream";
    };
    let extension = format!(".{}", extension.to_ascii_lowercase());
    MIME_BY_EXT
        .iter()
        .find_map(|(candidate, mime)| (*candidate == extension).then_some(*mime))
        .unwrap_or("application/octet-stream")
}

/// Maps to: CC `tools/BriefTool/upload.ts:56-58` `debug`.
fn debug(message: &str) {
    crate::utils::debug::log_for_debugging(&format!("[brief:upload] {message}"));
}

/// Maps to: CC `bridge/bridgeConfig.ts:18-24` `getBridgeTokenOverride`.
fn get_bridge_token_override() -> Option<String> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::ManagedConfiguration,
    ) {
        return None;
    }
    crate::utils::process_env::var("CLAUDE_BRIDGE_OAUTH_TOKEN").truthy()
}

/// Maps to: CC `bridge/bridgeConfig.ts:27-32` `getBridgeBaseUrlOverride`.
fn get_bridge_base_url_override() -> Option<String> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::ManagedConfiguration,
    ) {
        return None;
    }
    crate::utils::process_env::var("CLAUDE_BRIDGE_BASE_URL").truthy()
}

/// Maps to: CC `bridge/bridgeConfig.ts:38-40` `getBridgeAccessToken`. `None`
/// means "not logged in".
fn get_bridge_access_token() -> Option<String> {
    get_bridge_token_override().or_else(|| {
        crate::utils::auth::get_claude_ai_oauth_tokens().map(|tokens| tokens.access_token)
    })
}

/// Maps to: CC `tools/BriefTool/upload.ts:69-75` `getBridgeBaseUrl` — distinct
/// from `bridgeConfig.ts`'s same-named helper because it prefers
/// `ANTHROPIC_BASE_URL` over the OAuth config.
///
/// Subprocess hosts (cowork) pass `ANTHROPIC_BASE_URL` alongside
/// `CLAUDE_CODE_OAUTH_TOKEN` — prefer that since `getOauthConfig()` only returns
/// staging when `USE_STAGING_OAUTH` is set, which such hosts don't set. Without
/// this a staging token hits api.anthropic.com → 401 → silent skip → web viewer
/// sees inert cards with no `file_uuid`.
fn get_bridge_base_url() -> Option<String> {
    if let Some(override_url) = get_bridge_base_url_override() {
        return Some(override_url);
    }
    if let Some(base_url) = crate::utils::process_env::var("ANTHROPIC_BASE_URL") {
        return Some(base_url);
    }
    crate::constants::oauth::get_oauth_config()
        .ok()
        .map(|config| config.base_api_url)
}

/// Maps to: CC `tools/BriefTool/upload.ts:83-86` `BriefUploadContext`.
pub(crate) struct BriefUploadContext {
    pub(crate) repl_bridge_enabled: bool,
    pub(crate) abort: Option<crate::tool::AbortController>,
}

/// Maps to: CC `tools/BriefTool/upload.ts:129-137` — manual multipart, same
/// pattern as `filesApi.ts`. The oauth endpoint takes a single `file` part (no
/// `purpose` field like the public Files API).
pub(crate) fn build_multipart_body(
    boundary: &str,
    filename: &str,
    mime_type: &str,
    content: &[u8],
) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {mime_type}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Maps to: CC `tools/BriefTool/upload.ts:153` —
/// `jsonStringify(response.data).slice(0, 200)`. axios parses a JSON body into
/// a value before `jsonStringify` re-serializes it; anything else stays a
/// string and serializes quoted.
fn response_body_preview(body: &str) -> String {
    let serialized = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => value.to_string(),
        Err(_) => serde_json::Value::String(body.to_string()).to_string(),
    };
    serialized.chars().take(200).collect()
}

/// Maps to: CC `tools/BriefTool/upload.ts:151-167` — strict 201, then
/// `uploadResponseSchema` (`:79-81`, `z.object({ file_uuid: z.string() })`).
pub(crate) fn parse_upload_response(
    full_path: &str,
    size: u64,
    status: u16,
    body: &str,
) -> Option<String> {
    if status != 201 {
        debug(&format!(
            "upload failed for {full_path}: status={status} body={}",
            response_body_preview(body)
        ));
        return None;
    }

    // CC surfaces zod's own `parsed.error.message` here; that text is a zod
    // implementation detail, so this reports the same failure cause instead.
    let file_uuid = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|value| value.get("file_uuid"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let Some(file_uuid) = file_uuid else {
        debug(&format!(
            "unexpected response shape for {full_path}: expected an object with a string file_uuid"
        ));
        return None;
    };

    debug(&format!(
        "uploaded {full_path} \u{2192} {file_uuid} ({size} bytes)"
    ));
    Some(file_uuid)
}

/// Maps to: CC `tools/BriefTool/upload.ts:92-174` `uploadBriefAttachment`.
/// Returns the `file_uuid` on success, `None` otherwise — every early return is
/// intentional graceful degradation.
pub(crate) async fn upload_brief_attachment(
    full_path: &str,
    size: u64,
    ctx: &BriefUploadContext,
) -> Option<String> {
    // CC wraps the body in `feature('BRIDGE_MODE')` so Bun eliminates it from
    // non-bridge builds (`upload.ts:97-99`).
    if !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BridgeMode,
    ) {
        return None;
    }
    if !ctx.repl_bridge_enabled {
        return None;
    }

    if size > MAX_UPLOAD_BYTES {
        debug(&format!(
            "skip {full_path}: {size} bytes exceeds {MAX_UPLOAD_BYTES} limit"
        ));
        return None;
    }

    let token = get_bridge_access_token();
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        debug("skip: no oauth token");
        return None;
    };

    let content = match std::fs::read(full_path) {
        Ok(content) => content,
        Err(error) => {
            debug(&format!("read failed for {full_path}: {error}"));
            return None;
        }
    };

    let Some(base_url) = get_bridge_base_url() else {
        debug(&format!(
            "upload threw for {full_path}: no base URL is configured"
        ));
        return None;
    };
    let url = format!("{base_url}/api/oauth/file_upload");
    let filename = std::path::Path::new(full_path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(full_path)
        .to_string();
    let mime_type = guess_mime_type(&filename);
    let boundary = format!("----FormBoundary{}", uuid::Uuid::new_v4());
    let body = build_multipart_body(&boundary, &filename, mime_type, &content);

    // CC posts through the global axios instance (`configureGlobalAgents`);
    // `create_axios_instance` resolves the same proxy, NO_PROXY, mTLS and CA.
    let client = match crate::utils::proxy::create_axios_instance().and_then(|builder| {
        Ok(builder
            .timeout(std::time::Duration::from_millis(UPLOAD_TIMEOUT_MS))
            .build()?)
    }) {
        Ok(client) => client,
        Err(error) => {
            debug(&format!("upload threw for {full_path}: {error}"));
            return None;
        }
    };
    let request = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(reqwest::header::CONTENT_LENGTH, body.len().to_string())
        .body(body);

    // TODO(parity): CC `upload.ts:140-149` sends the prepared request with
    // `validateStatus: () => true`. This port stops at the credential-bearing
    // HTTP outlet, which fails closed exactly like every other OAuth egress in
    // this build; everything above and `parse_upload_response` below is the
    // official logic and is exercised directly by tests.
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        debug(&format!(
            "upload threw for {full_path}: {}",
            crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
        ));
        return None;
    }

    // CC hands axios `ctx.signal`; `AbortController` here is polled rather than
    // future-based, so the send races a poll of the same flag.
    let send = request.send();
    tokio::pin!(send);
    let response = loop {
        tokio::select! {
            result = &mut send => break result,
            () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if ctx.abort.as_ref().is_some_and(crate::tool::AbortController::is_aborted) {
                    debug(&format!("upload threw for {full_path}: aborted"));
                    return None;
                }
            }
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            debug(&format!("upload threw for {full_path}: {error}"));
            return None;
        }
    };
    let status = response.status().as_u16();
    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            debug(&format!("upload threw for {full_path}: {error}"));
            return None;
        }
    };
    parse_upload_response(full_path, size, status, &body)
}

#[cfg(test)]
mod tests {
    /// Maps to: CC `upload.ts:43-54`. The whitelist deliberately excludes
    /// svg/bmp/ico/pdf — see `MIME_BY_EXT`'s note on backend dispatch.
    #[test]
    fn mime_guess_matches_official_whitelist_and_octet_stream_fallback() {
        assert_eq!(super::guess_mime_type("shot.png"), "image/png");
        assert_eq!(super::guess_mime_type("shot.JPG"), "image/jpeg");
        assert_eq!(super::guess_mime_type("shot.jpeg"), "image/jpeg");
        assert_eq!(super::guess_mime_type("loop.gif"), "image/gif");
        assert_eq!(super::guess_mime_type("shot.WebP"), "image/webp");
        for filename in [
            "diagram.bmp",
            "icon.ico",
            "vector.svg",
            "paper.pdf",
            "notes.txt",
            "Makefile",
            ".png",
        ] {
            assert_eq!(
                super::guess_mime_type(filename),
                "application/octet-stream",
                "{filename}"
            );
        }
    }

    /// Maps to: CC `upload.ts:129-137` — one `file` part, CRLF framing, raw
    /// bytes preserved, closing `--boundary--`.
    #[test]
    fn multipart_body_matches_official_framing_and_preserves_raw_bytes() {
        let content = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let body = super::build_multipart_body("----FormBoundaryX", "a.png", "image/png", &content);

        let head: &[u8] = b"------FormBoundaryX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n";
        assert_eq!(&body[..head.len()], head);
        assert_eq!(&body[head.len()..head.len() + content.len()], &content);
        assert_eq!(
            &body[head.len() + content.len()..],
            b"\r\n------FormBoundaryX--\r\n"
        );
    }

    /// Maps to: CC `upload.ts:151-167` — strict 201 and the `file_uuid` schema.
    #[test]
    fn upload_response_requires_201_and_a_string_file_uuid() {
        assert_eq!(
            super::parse_upload_response("/tmp/a.png", 4, 201, r#"{"file_uuid":"abc","extra":1}"#),
            Some("abc".to_string())
        );
        for (status, body) in [
            (200u16, r#"{"file_uuid":"abc"}"#),
            (202, r#"{"file_uuid":"abc"}"#),
            (401, "unauthorized"),
        ] {
            assert_eq!(
                super::parse_upload_response("/tmp/a.png", 4, status, body),
                None
            );
        }
        for body in [r#"{"file_uuid":7}"#, "{}", "[]", "not json"] {
            assert_eq!(
                super::parse_upload_response("/tmp/a.png", 4, 201, body),
                None
            );
        }
    }

    /// Maps to: CC `upload.ts:153` — the failure preview is capped at 200.
    #[test]
    fn failed_upload_body_preview_is_truncated_like_the_official_slice() {
        let body = serde_json::json!({ "error": "x".repeat(400) }).to_string();
        assert_eq!(super::response_body_preview(&body).chars().count(), 200);
        assert_eq!(super::response_body_preview("plain"), "\"plain\"");
    }

    /// Maps to: CC `upload.ts:32,34`.
    #[test]
    fn upload_limits_match_the_official_backend_constants() {
        assert_eq!(super::MAX_UPLOAD_BYTES, 31_457_280);
        assert_eq!(super::UPLOAD_TIMEOUT_MS, 30_000);
    }

    /// Maps to: CC `upload.ts:99-105` — the bridge gate and the size cap both
    /// short-circuit before any token read.
    #[tokio::test]
    async fn upload_skips_when_the_bridge_is_off_or_the_file_is_too_large() {
        let ctx = super::BriefUploadContext {
            repl_bridge_enabled: false,
            abort: None,
        };
        assert_eq!(
            super::upload_brief_attachment("/tmp/does-not-exist.png", 4, &ctx).await,
            None
        );

        let ctx = super::BriefUploadContext {
            repl_bridge_enabled: true,
            abort: None,
        };
        assert_eq!(
            super::upload_brief_attachment(
                "/tmp/does-not-exist.png",
                super::MAX_UPLOAD_BYTES + 1,
                &ctx
            )
            .await,
            None
        );
    }
}
