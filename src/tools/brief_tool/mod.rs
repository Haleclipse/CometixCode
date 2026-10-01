//! Port of official `BriefTool` (`SendUserMessage`) metadata, attachment
//! validation/resolution, model result, and transcript delivery UI.

pub mod prompt;
pub mod ui;
pub mod upload;

/// Maps to: CC `tools/BriefTool/BriefTool.ts:88-100` `isBriefEntitled()`.
/// Decides whether an opt-in should be *honored*, never whether one happened.
pub fn is_brief_entitled() -> bool {
    if !brief_build_feature_enabled() {
        return false;
    }
    crate::bootstrap::state::get_kairos_active()
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_BRIEF").as_deref(),
        )
        || crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::KairosBriefEntitlement,
        )
}

/// Maps to: CC `tools/BriefTool/BriefTool.ts:126-134` `isBriefEnabled()`.
/// Activation needs an explicit opt-in; assistant mode bypasses it because its
/// system prompt hard-codes the tool. No opt-in means false regardless of the
/// entitlement gate.
pub fn is_brief_tool_enabled() -> bool {
    if !brief_build_feature_enabled() {
        return false;
    }
    (crate::bootstrap::state::get_kairos_active() || crate::bootstrap::state::get_user_msg_opt_in())
        && is_brief_entitled()
}

/// Maps to: CC's `feature('KAIROS') || feature('KAIROS_BRIEF')` guard, which
/// both gates repeat verbatim so Bun can constant-fold them away.
fn brief_build_feature_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(crate::utils::feature_flags::FeatureFlag::Kairos)
        || crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::KairosBrief,
        )
}

/// Maps to: CC `BriefTool.ts:20-37` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "message",
                zod::string()
                    .describe("The message for the user. Supports markdown formatting."),
            ),
            (
                "attachments",
                zod::array(zod::string()).optional().describe(
                    "Optional file paths (absolute or relative to cwd) to attach. Use for photos, screenshots, diffs, logs, or any file the user should see alongside your message.",
                ),
            ),
            (
                "status",
                zod::enumeration(vec!["normal", "proactive"]).describe(
                    "Use 'proactive' when you're surfacing something the user hasn't asked for and needs to see now — task completion while they're away, a blocker you hit, an unsolicited status update. Use 'normal' when replying to something the user just said.",
                ),
            ),
        ])
    })
}

/// Maps to: CC `tools/BriefTool/BriefTool.ts` `BriefTool` metadata.
pub fn brief_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::BRIEF_TOOL_NAME.to_string(),
        aliases: vec![prompt::LEGACY_BRIEF_TOOL_NAME.to_string()],
        // CC sends `await tool.prompt()` as the API tool description
        // (`utils/api.ts:171`); `DESCRIPTION` only feeds the tool picker UI.
        description: prompt::BRIEF_TOOL_PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// Maps to: CC `tools/BriefTool/BriefTool.ts:42-61` `outputSchema` attachment.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BriefAttachmentOutput {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) is_image: bool,
    #[serde(rename = "file_uuid", default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_uuid: Option<String>,
}

/// Maps to: CC `tools/BriefTool/BriefTool.ts:42-61` `outputSchema` / `Output`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BriefOutput {
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attachments: Option<Vec<BriefAttachmentOutput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sent_at: Option<String>,
}

/// Maps to: CC `tools/BriefTool/BriefTool.ts:142-144` `BriefTool.userFacingName`.
pub fn user_facing_name() -> &'static str {
    ""
}

fn resolve_attachment_paths(
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Result<Vec<BriefAttachmentOutput>, String> {
    let mut attachments = Vec::new();
    for raw_path in input
        .get("attachments")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        let path = crate::utils::permissions::path_validation::expand_tilde(raw_path);
        let path = std::path::PathBuf::from(path);
        let full_path = if path.is_absolute() {
            path
        } else {
            context.effective_cwd().join(path)
        };
        let metadata = std::fs::metadata(&full_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!(
                "Attachment \"{raw_path}\" does not exist. Current working directory: {}.",
                context.effective_cwd().display()
            ),
            std::io::ErrorKind::PermissionDenied => {
                format!("Attachment \"{raw_path}\" is not accessible (permission denied).")
            }
            _ => format!("Unable to access attachment \"{raw_path}\": {error}"),
        })?;
        if !metadata.is_file() {
            return Err(format!("Attachment \"{raw_path}\" is not a regular file."));
        }
        let path = full_path.display().to_string();
        attachments.push(BriefAttachmentOutput {
            is_image: is_image_path(&path),
            path,
            size: metadata.len(),
            file_uuid: None,
        });
    }
    Ok(attachments)
}

/// Maps to: CC `utils/imagePaste.ts:270` `IMAGE_EXTENSION_REGEX`. Kept in sync
/// with `upload::MIME_BY_EXT`: an extension here but not there (e.g. `bmp`)
/// uploads as octet-stream with no `/preview` variant, so remote viewers render
/// a broken thumbnail.
fn is_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    // The official regex anchors on a literal `.`, so a dotless name whose
    // whole spelling is an extension is not an image.
    matches!(
        lower.rsplit_once('.').map(|(_, extension)| extension),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    )
}

/// Maps to: CC `tools/BriefTool/attachments.ts:63-110` `resolveAttachments`.
///
/// Stat serially (local, fast) to keep ordering deterministic, then upload in
/// parallel (network, slow). Upload failures resolve `None` — the attachment
/// still carries `{path, size, isImage}` for local renderers.
async fn resolve_attachments(
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Result<Vec<BriefAttachmentOutput>, String> {
    let stated = resolve_attachment_paths(input, context)?;
    // CC gates the dynamic `./upload.js` import on `feature('BRIDGE_MODE')`
    // (attachments.ts:88) so the upload module stays out of non-bridge builds.
    if !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BridgeMode,
    ) {
        return Ok(stated);
    }

    // Headless/SDK callers never set `replBridgeEnabled` (only the TTY REPL
    // does, at init). `CLAUDE_CODE_BRIEF_UPLOAD` lets a host that runs the CLI
    // as a subprocess opt in — e.g. the cowork desktop bridge, which already
    // passes `CLAUDE_CODE_OAUTH_TOKEN` for auth (attachments.ts:93-95).
    let should_upload = context
        .get_app_state()
        .is_some_and(|state| state.repl_bridge_enabled)
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_BRIEF_UPLOAD").as_deref(),
        );
    let uploads = stated.iter().map(|attachment| {
        let ctx = upload::BriefUploadContext {
            repl_bridge_enabled: should_upload,
            abort: Some(context.abort_controller.clone()),
        };
        async move { upload::upload_brief_attachment(&attachment.path, attachment.size, &ctx).await }
    });
    let uuids = futures::future::join_all(uploads).await;

    Ok(stated
        .into_iter()
        .zip(uuids)
        .map(|(attachment, file_uuid)| BriefAttachmentOutput {
            file_uuid,
            ..attachment
        })
        .collect())
}

/// Behavioral half of CC `BriefTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct BriefTool;

impl crate::tool::ToolCall for BriefTool {
    fn name(&self) -> &'static str {
        "SendUserMessage"
    }

    /// Maps to: CC `BriefTool.ts:172-174` `async prompt() { return
    /// BRIEF_TOOL_PROMPT }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::BRIEF_TOOL_PROMPT.to_string()
    }

    fn is_enabled(&self) -> bool {
        is_brief_tool_enabled()
    }

    /// Maps to: CC `BriefTool.isConcurrencySafe()` (BriefTool.ts:154) — true.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("send a message to the user — your primary visible output channel")
    }

    /// Maps to: CC `BriefTool.ts:142-144` `userFacingName()` — the empty
    /// string (mirrors the module-level [`user_facing_name`] the render layer
    /// consumes).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
    }

    /// Maps to: CC `BriefTool.ts:169-171` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        match resolve_attachment_paths(args, context) {
            Ok(_) => crate::tool::ValidationResult::ok(),
            Err(error) => crate::tool::ValidationResult::error(error, 1),
        }
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["Brief"]
    }
    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            // Maps to: CC `tools/BriefTool/BriefTool.ts:186-207` `BriefTool.call`.
            let data = match resolve_attachments(args, context).await {
                Ok(attachments) => crate::tool::ToolOutput::Brief(BriefOutput {
                    message: args
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    attachments: (!attachments.is_empty()).then_some(attachments),
                    sent_at: Some(
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                }),
                Err(error) => crate::tool::ToolOutput::Composed {
                    content: format!("<tool_use_error>{error}</tool_use_error>"),
                    status: crate::types::message::ToolResultStatus::Error,
                },
            };
            crate::tool::ToolResult {
                data,
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/BriefTool/BriefTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:175-183).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::Brief(output) => {
                let count = output.attachments.as_deref().map_or(0, <[_]>::len);
                let suffix = match count {
                    0 => String::new(),
                    1 => " (1 attachment included)".to_string(),
                    n => format!(" ({n} attachments included)"),
                };
                (
                    format!("Message delivered to user.{suffix}"),
                    crate::types::message::ToolResultStatus::Success,
                )
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>SendUserMessage returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording BriefTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Brief(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tool::ToolCall;

    #[test]
    fn brief_attachment_validation_stats_real_files_and_rejects_missing_paths() {
        let root =
            std::env::temp_dir().join(format!("cometix-brief-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("photo.png"), b"1234").unwrap();
        let context = crate::tool::ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let valid = serde_json::json!({
            "message": "done",
            "status": "normal",
            "attachments": ["photo.png"]
        });
        assert!(super::BriefTool.validate_input(&valid, &context).is_ok());
        let attachments = super::resolve_attachment_paths(&valid, &context).unwrap();
        assert_eq!(attachments[0].size, 4);
        assert!(attachments[0].is_image);

        let missing = serde_json::json!({
            "message": "done",
            "status": "normal",
            "attachments": ["missing.txt"]
        });
        assert!(matches!(
            super::BriefTool.validate_input(&missing, &context),
            crate::tool::ValidationResult::Error { error_code: 1, .. }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn brief_tool_schema_matches_official_name_alias_and_required_fields() {
        let schema = super::brief_tool_schema();

        assert_eq!(schema.name, "SendUserMessage");
        assert_eq!(schema.aliases, vec!["Brief"]);
        // CC ships `await tool.prompt()` as the API-visible description
        // (utils/api.ts:171), i.e. `BRIEF_TOOL_PROMPT` — not the one-line
        // `description()`.
        assert_eq!(schema.description, super::prompt::BRIEF_TOOL_PROMPT);
        assert!(
            schema
                .description
                .contains("Text outside this tool is visible in the detail view")
        );
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["message", "status"])
        );
        assert_eq!(
            schema.input_schema["properties"]["status"]["enum"],
            serde_json::json!(["normal", "proactive"])
        );
    }
}
