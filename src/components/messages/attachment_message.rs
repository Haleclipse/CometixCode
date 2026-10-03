//! Maps to: CC `components/messages/AttachmentMessage.tsx`.
//!
//! Since batch D2 the component receives the typed model [`Attachment`] union
//! directly (CC's prop is `attachment: Attachment`) and derives every display
//! string at render time, like CC's switch (`AttachmentMessage.tsx:162`).
//! Read summaries derive their counts from the FileReadToolOutput payload
//! (`:169-192`) and hook attachments render per-hook-type lines with CC's
//! colors and Stop-hook null gates (`:350-434`) — the pre-D2 simplified copy
//! for both was aligned on 2026-08-07 (it was legacy shorthand, never a
//! user-ruled deviation).
//!
//! # Known seam: `<Text bold>` spans in the shared `content` path
//!
//! CC emphasizes a substring of many rows — `Listed directory <Text bold>{path}</Text>`
//! (`:166`), `Read <Text bold>{path}</Text> (…)` (`:174-192`),
//! `Referenced file <Text bold>{path}</Text>` (`:198`),
//! `Loaded <Text bold>{path}</Text>` (`:220`),
//! `Read MCP resource <Text bold>{name}</Text> from {server}` (`:342`),
//! `Task "<Text bold>{description}</Text>" {statusText}` (`:511`), and more.
//! The tail of this component funnels all of those through one `content: String`
//! and a single `MessageResponse`, so every one of them renders flat.
//!
//! The two hook rows CC bolds (`:359-363` async completion, `:427-434`
//! permission decision) were un-flattened under #191 via
//! [`hook_event_emphasis_line`]; the `content`-path rows above are NOT fixed and
//! need the same `MixedText` treatment plus a `content`-as-spans refactor. Do
//! not read the two fixed arms as evidence that the rest are aligned.

use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::diagnostics_display::DiagnosticsDisplay;
use crate::components::message_response::MessageResponse;
use crate::components::messages::null_rendering_attachments::is_null_rendering_attachment_type;
use crate::types::message::Attachment;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// CC keeps `displayPath` beside the absolute path for stable display; old
/// sessions may lack it (backfilled at the recovery seam per
/// conversationRecovery.ts:112-131), so fall back to the raw path.
fn display_or<'a>(display_path: &'a str, fallback: &'a str) -> &'a str {
    if display_path.is_empty() {
        fallback
    } else {
        display_path
    }
}

/// Maps to: CC `AttachmentMessage.tsx:169-192` — the parenthesized read
/// summary derived from the typed `FileReadToolOutput` payload
/// (`content.file.cells.length` cells / `unchanged` / `numLines` with the
/// truncation `+` / `formatFileSize(originalSize)`).
fn read_content_summary(
    content: &crate::tools::file_read_tool::Output,
    truncated: Option<bool>,
) -> String {
    use crate::tools::file_read_tool::Output as FileReadToolOutput;
    match content {
        FileReadToolOutput::Notebook { cells, .. } => format!("{} cells", cells.len()),
        FileReadToolOutput::FileUnchanged { .. } => "unchanged".to_string(),
        FileReadToolOutput::Text { num_lines, .. } => {
            let plus = if truncated == Some(true) { "+" } else { "" };
            format!("{num_lines}{plus} lines")
        }
        FileReadToolOutput::Image { original_size, .. }
        | FileReadToolOutput::Pdf { original_size, .. }
        | FileReadToolOutput::Parts { original_size, .. } => {
            crate::utils::format::format_file_size(original_size.as_u64().unwrap_or(0))
        }
    }
}

/// The two hook rows CC renders as `prefix <Text bold>{hookEvent}</Text> suffix`
/// inside a `Line` — `async_hook_response` (`AttachmentMessage.tsx:359-363`) and
/// `hook_permission_decision` (`:427-434`).
///
/// One `MixedText` run rather than sibling `Text`s: CC nests the bold span
/// inside the `Line`'s single `<Text … wrap="wrap">` (`:562`), so the whole row
/// wraps as one text run. `Line`'s `dimColor` default (`:550`) maps to
/// `theme.inactive`, which every other hook arm in this file already uses, and
/// the bold span inherits it in Ink — CC only opts out of dim+bold explicitly
/// where it needs to (`:539-547`), which it does not do here.
fn hook_event_emphasis_line(
    theme: &Theme,
    prefix: String,
    hook_event: &str,
    suffix: &str,
) -> Vec<MixedTextContent> {
    vec![
        MixedTextContent::new(prefix).color(theme.inactive),
        MixedTextContent::new(hook_event)
            .color(theme.inactive)
            .weight(Weight::Bold),
        MixedTextContent::new(suffix).color(theme.inactive),
    ]
}

/// CC `queued_command` render reads `attachment.prompt` as string or content
/// blocks (`AttachmentMessage.tsx:322-326` via `getContentText`).
fn prompt_text(prompt: &serde_json::Value) -> String {
    match prompt.as_str() {
        Some(text) => text.to_string(),
        None => crate::components::messages::user_tool_result_message::utils::value_to_text(prompt),
    }
}

#[derive(Default, Props)]
pub struct AttachmentMessageProps {
    pub attachment: Option<Attachment>,
    pub add_margin: bool,
    pub verbose: bool,
    pub is_transcript_mode: bool,
}

#[component]
pub fn AttachmentMessage(
    props: &AttachmentMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();

    if props
        .attachment
        .as_ref()
        .is_some_and(is_null_rendering_attachment_type)
    {
        // Maps to CC `components/messages/nullRenderingAttachments.ts` + the
        // switch `default: return null` branch.
        return element! { Fragment }.into_any();
    }

    if let Some(Attachment::SkillListing {
        skill_count,
        is_initial,
        ..
    }) = props.attachment.clone()
    {
        // CC `AttachmentMessage.tsx:281-283`: the initial listing renders null.
        if is_initial {
            return element! { Fragment }.into_any();
        }
        let noun = if skill_count == 1 { "skill" } else { "skills" };
        return element! {
            View(
                flex_direction: FlexDirection::Row,
            ) {
                Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                Text(content: skill_count.to_string(), color: theme.inactive, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: format!(" {noun} available"), color: theme.inactive)
            }
        }
        .into_any();
    }

    if let Some(Attachment::AgentListingDelta {
        added_types,
        is_initial,
        ..
    }) = props.attachment.clone()
    {
        // CC `AttachmentMessage.tsx:291-300`: the initial listing and deltas
        // that added nothing render null; otherwise the added count shows.
        if is_initial || added_types.is_empty() {
            return element! { Fragment }.into_any();
        }
        let count = added_types.len();
        let noun = if count == 1 { "type" } else { "types" };
        return element! {
            View(
                flex_direction: FlexDirection::Row,
            ) {
                Text(content: "  ⎿ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                Text(content: count.to_string(), color: theme.inactive, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: format!(" agent {noun} available"), color: theme.inactive)
            }
        }
        .into_any();
    }

    if let Some(Attachment::TeammateShutdownBatch { count }) = props.attachment.clone() {
        let noun = if count == 1 { "teammate" } else { "teammates" };
        return element! {
            View(
                flex_direction: FlexDirection::Row,
                margin_top: if props.add_margin { 1u32 } else { 0u32 },
            ) {
                Text(content: "● ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                Text(content: format!("{count} {noun} shut down gracefully"), color: theme.inactive)
            }
        }
        .into_any();
    }

    if let Some(Attachment::RelevantMemories { memories }) = props.attachment.clone() {
        let count = memories.len();
        let noun = if count == 1 { "memory" } else { "memories" };
        let expand_hint = if props.is_transcript_mode {
            String::new()
        } else {
            format!(" {}", ctrl_o_to_expand_hint())
        };
        return element! {
            View(
                flex_direction: FlexDirection::Column,
                margin_top: if props.add_margin { 1u32 } else { 0u32 },
            ) {
                View(flex_direction: FlexDirection::Row) {
                    View(min_width: 2u32, flex_shrink: 0.0f32) {}
                    Text(content: format!("Recalled {count} {noun}{expand_hint}"), color: theme.inactive)
                }
                #(if props.verbose || props.is_transcript_mode {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            #(memories.iter().map(|memory| {
                                element! {
                                    View(flex_direction: FlexDirection::Column) {
                                        MessageResponse(content: basename(&memory.path).to_string(), color: Some(theme.inactive))
                                        #(if props.is_transcript_mode && !memory.content.is_empty() {
                                            Some(element! {
                                                View(padding_left: 5u32) {
                                                    Text(content: memory.content.clone())
                                                }
                                            })
                                        } else {
                                            None
                                        })
                                    }
                                }
                            }))
                        }
                    })
                } else {
                    None
                })
            }
        }
        .into_any();
    }

    if let Some(Attachment::Diagnostics { files, .. }) = props.attachment.clone() {
        if props.is_transcript_mode && !files.is_empty() {
            return element! {
                DiagnosticsDisplay(files: files, verbose: true)
            }
            .into_any();
        }
        return element! {
            DiagnosticsDisplay(files: files, verbose: props.verbose)
        }
        .into_any();
    }

    // Maps to: CC `AttachmentMessage.tsx:365-434` — per-hook-type lines.
    // Stop/SubagentStop members render null here because their summary comes
    // from SystemStopHookSummaryMessage instead.
    if let Some(attachment) = props.attachment.clone() {
        let stop_hook = matches!(
            &attachment,
            Attachment::HookBlockingError { hook_event, .. }
            | Attachment::HookNonBlockingError { hook_event, .. }
            | Attachment::HookErrorDuringExecution { hook_event, .. }
            | Attachment::HookStoppedContinuation { hook_event, .. }
                if hook_event == "Stop" || hook_event == "SubagentStop"
        );
        if stop_hook {
            return element! { Fragment }.into_any();
        }
        match attachment {
            Attachment::HookBlockingError {
                hook_name,
                blocking_error,
                ..
            } => {
                // CC :374-382: show stderr so the user can see why the hook
                // blocked (`attachment.blockingError.blockingError.trim()`).
                let stderr = blocking_error.blocking_error.trim().to_string();
                return element! {
                    View(flex_direction: FlexDirection::Column) {
                        MessageResponse(
                            content: format!("{hook_name} hook returned blocking error"),
                            color: Some(theme.error),
                        )
                        #((!stderr.is_empty()).then(|| element! {
                            MessageResponse(content: stderr.clone(), color: Some(theme.error))
                        }))
                    }
                }
                .into_any();
            }
            Attachment::HookNonBlockingError { hook_name, .. } => {
                return element! {
                    MessageResponse(
                        content: format!("{hook_name} hook error"),
                        color: Some(theme.error),
                    )
                }
                .into_any();
            }
            Attachment::HookErrorDuringExecution { hook_name, .. } => {
                return element! {
                    MessageResponse(
                        content: format!("{hook_name} hook warning"),
                        color: Some(theme.inactive),
                    )
                }
                .into_any();
            }
            Attachment::HookStoppedContinuation {
                hook_name, message, ..
            } => {
                return element! {
                    MessageResponse(
                        content: format!("{hook_name} hook stopped continuation: {message}"),
                        color: Some(theme.warning),
                    )
                }
                .into_any();
            }
            Attachment::HookSystemMessage {
                hook_name, content, ..
            } => {
                return element! {
                    MessageResponse(
                        content: format!("{hook_name} says: {content}"),
                        color: Some(theme.inactive),
                    )
                }
                .into_any();
            }
            Attachment::HookPermissionDecision {
                decision,
                hook_event,
                ..
            } => {
                // Maps to: CC `AttachmentMessage.tsx:427-434` —
                //   `{action} by <Text bold>{attachment.hookEvent}</Text> hook`
                // The hook event name is its own BOLD span; the rest of the row
                // is the `Line` default (`:549-568`, `dimColor` → theme.inactive
                // here). #191: this used to be one flat `format!`, which the
                // `hook_permission_decision` producer added by #186 made
                // user-visible on the PermissionRequest path.
                let action = if decision == "allow" {
                    "Allowed"
                } else {
                    "Denied"
                };
                return element! {
                    MessageResponse {
                        MixedText(
                            // CC `Line`'s `<Text … wrap="wrap">` (`:562`).
                            wrap: TextWrap::Wrap,
                            contents: hook_event_emphasis_line(
                                &theme,
                                format!("{action} by "),
                                &hook_event,
                                " hook",
                            ),
                        )
                    }
                }
                .into_any();
            }
            Attachment::AsyncHookResponse { hook_event, .. } => {
                // CC :350-363: SessionStart completions only in verbose; all
                // async completions hidden outside verbose/transcript.
                if (hook_event == "SessionStart" && !props.verbose)
                    || (!props.verbose && !props.is_transcript_mode)
                {
                    return element! { Fragment }.into_any();
                }
                // Maps to: CC `AttachmentMessage.tsx:359-363` —
                //   `Async hook <Text bold>{attachment.hookEvent}</Text> completed`
                // Same flattened-bold defect as the `hook_permission_decision`
                // arm above; found by reading CC's block rather than the one
                // reported line (#191).
                return element! {
                    MessageResponse {
                        MixedText(
                            wrap: TextWrap::Wrap,
                            contents: hook_event_emphasis_line(
                                &theme,
                                "Async hook ".to_string(),
                                &hook_event,
                                " completed",
                            ),
                        )
                    }
                }
                .into_any();
            }
            _ => {}
        }
    }

    let content = match props.attachment.clone() {
        Some(Attachment::Directory {
            path, display_path, ..
        }) => format!("Listed directory {}/", display_or(&display_path, &path)),
        // CC AttachmentMessage.tsx:169-192: `file` and `already_read_file`
        // share one render deriving the count from the FileReadToolOutput
        // payload (notebook cells / unchanged / line count with truncation
        // `+` / formatted byte size).
        Some(
            Attachment::File {
                filename,
                content,
                truncated,
                display_path,
            }
            | Attachment::AlreadyReadFile {
                filename,
                content,
                truncated,
                display_path,
            },
        ) => format!(
            "Read {} ({})",
            display_or(&display_path, &filename),
            read_content_summary(&content, truncated)
        ),
        Some(Attachment::PdfReference {
            filename,
            page_count,
            display_path,
            ..
        }) => format!(
            "Read {} (PDF reference ({page_count} pages))",
            display_or(&display_path, &filename)
        ),
        Some(Attachment::CompactFileReference {
            filename,
            display_path,
        }) => format!("Referenced file {}", display_or(&display_path, &filename)),
        Some(Attachment::SkillDiscovery { skills, .. }) => {
            let names = skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Relevant skills: {names}")
        }
        Some(Attachment::DynamicSkill {
            skill_names,
            display_path,
            ..
        }) => {
            let count = skill_names.len();
            format!(
                "Loaded {count} {} from {display_path}",
                if count == 1 { "skill" } else { "skills" }
            )
        }
        Some(Attachment::NestedMemory {
            path, display_path, ..
        }) => format!("Loaded {}", display_or(&display_path, &path)),
        Some(
            Attachment::HookBlockingError { .. }
            | Attachment::HookNonBlockingError { .. }
            | Attachment::HookErrorDuringExecution { .. }
            | Attachment::HookStoppedContinuation { .. }
            | Attachment::HookSystemMessage { .. }
            | Attachment::AsyncHookResponse { .. }
            | Attachment::HookPermissionDecision { .. },
        ) => {
            unreachable!("hook attachments render in the typed branch above")
        }
        Some(Attachment::QueuedCommand { prompt, .. }) => {
            format!("Queued command: {}", prompt_text(&prompt))
        }
        Some(Attachment::TeammateMailbox { messages }) => {
            let count = messages.len();
            format!(
                "Teammate mailbox: {count} {}",
                if count == 1 { "message" } else { "messages" }
            )
        }
        Some(Attachment::McpResource { name, server, .. }) => {
            format!("Read MCP resource {name} from {server}")
        }
        Some(Attachment::TaskStatus {
            task_type,
            status,
            description,
            ..
        }) => {
            if task_type == "in_process_teammate" && status == "completed" {
                "Teammate shut down gracefully".to_string()
            } else {
                let status_text = match status.as_str() {
                    "completed" => "completed in background".to_string(),
                    "killed" => "stopped".to_string(),
                    "running" => "still running in background".to_string(),
                    other => other.to_string(),
                };
                format!("Task \"{}\" {}", description, status_text)
            }
        }
        // Maps to: CC `AttachmentMessage.tsx:207-217` `case 'selected_lines_in_ide'`.
        Some(Attachment::SelectedLinesInIde {
            ide_name,
            line_start,
            line_end,
            filename,
            display_path,
            ..
        }) => format!(
            "⧉ Selected {} lines from {} in {ide_name}",
            line_end.saturating_sub(line_start).saturating_add(1),
            display_or(&display_path, &filename)
        ),
        // Maps to: CC `AttachmentMessage.tsx:339-344` `case 'plan_file_reference'`.
        Some(Attachment::PlanFileReference { plan_file_path, .. }) => {
            format!("Plan file referenced ({plan_file_path})")
        }
        // Maps to: CC `AttachmentMessage.tsx:345-351` `case 'invoked_skills'`.
        Some(Attachment::InvokedSkills { skills }) => {
            if skills.is_empty() {
                return element! { Fragment }.into_any();
            }
            let names = skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Skills restored ({names})")
        }
        // CC's switch has no case for these; `default:` returns null
        // (AttachmentMessage.tsx:452-467).
        Some(
            Attachment::EditedTextFile { .. }
            | Attachment::EditedImageFile { .. }
            | Attachment::OpenedFileInIde { .. }
            | Attachment::TodoReminder { .. }
            | Attachment::TaskReminder { .. }
            | Attachment::OutputStyle { .. }
            | Attachment::PlanMode { .. }
            | Attachment::PlanModeReentry { .. }
            | Attachment::PlanModeExit { .. }
            | Attachment::AutoMode { .. }
            | Attachment::AutoModeExit
            | Attachment::CriticalSystemReminder { .. }
            | Attachment::CommandPermissions { .. }
            | Attachment::AgentMention { .. }
            | Attachment::TokenUsage { .. }
            | Attachment::BudgetUsd { .. }
            | Attachment::OutputTokenUsage { .. }
            | Attachment::StructuredOutput { .. }
            | Attachment::TeamContext { .. }
            | Attachment::HookCancelled { .. }
            | Attachment::HookSuccess { .. }
            | Attachment::HookAdditionalContext { .. }
            | Attachment::VerifyPlanReminder
            | Attachment::MaxTurnsReached { .. }
            | Attachment::CurrentSessionMemory { .. }
            | Attachment::CompactionReminder
            | Attachment::ContextEfficiency
            | Attachment::DateChange { .. }
            | Attachment::UltrathinkEffort { .. }
            | Attachment::DeferredToolsDelta { .. }
            | Attachment::McpInstructionsDelta { .. }
            | Attachment::CompanionIntro { .. }
            | Attachment::BagelConsole { .. }
            // Both arms of `agent_listing_delta` returned above; this keeps
            // the match exhaustive without reintroducing it as null-rendering.
            | Attachment::AgentListingDelta { .. }
            | Attachment::Unknown(_),
        ) => {
            return element! { Fragment }.into_any();
        }
        Some(
            Attachment::SkillListing { .. }
            | Attachment::TeammateShutdownBatch { .. }
            | Attachment::RelevantMemories { .. }
            | Attachment::Diagnostics { .. },
        ) => {
            unreachable!("handled above")
        }
        None => "Attachment".to_string(),
    };

    element! {
        View {
            MessageResponse(content: content, color: Some(theme.inactive))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::super::user_prompt_message::UserPromptMessage;
    use super::*;
    use crate::utils::test_env::EnvVarGuard;

    #[test]
    fn skill_listing_does_not_add_margin_after_slash_command() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                View(flex_direction: FlexDirection::Column) {
                    UserPromptMessage(content: "/fetch-cc".to_string(), add_margin: false)
                    AttachmentMessage(
                        attachment: Some(Attachment::SkillListing {
                            content: String::new(),
                            skill_count: 1,
                            is_initial: false,
                        }),
                        add_margin: true,
                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
        }
        .render(None);

        let rendered = canvas
            .to_string()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rendered, "❯ /fetch-cc\n  ⎿ 1 skill available");
    }

    /// Maps to: CC `AttachmentMessage.tsx:291-300` — a non-initial delta that
    /// added agent types shows the count; the initial listing and empty deltas
    /// render null. This attachment is deliberately absent from CC's
    /// null-rendering allowlist, so its row reaches the component at all.
    #[test]
    fn agent_listing_delta_renders_added_count_like_official() {
        let render = |added: Vec<&str>, is_initial: bool| {
            element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    AttachmentMessage(
                        attachment: Some(Attachment::AgentListingDelta {
                            added_types: added.into_iter().map(str::to_string).collect(),
                            added_lines: Vec::new(),
                            removed_types: Vec::new(),
                            is_initial,
                            show_concurrency_note: false,
                        }),
                        add_margin: false,
                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
            .render(None)
            .to_string()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
        };

        assert_eq!(
            render(vec!["explorer"], false),
            "  ⎿ 1 agent type available"
        );
        assert_eq!(
            render(vec!["explorer", "planner"], false),
            "  ⎿ 2 agent types available"
        );
        assert_eq!(render(vec!["explorer"], true).trim(), "");
        assert_eq!(render(Vec::new(), false).trim(), "");
    }

    /// Renders through the real component and keeps the SGR codes, because the
    /// thing under test is styling: a string-equality assertion cannot see bold
    /// and is exactly what let #191 sit unnoticed. Returns `(ansi, plain)`.
    fn render_ansi(attachment: Attachment, verbose: bool) -> (String, String) {
        // This fixture asserts enabled ANSI spans, so provide the source
        // terminal capability explicitly instead of inheriting a test pipe.
        let _force = EnvVarGuard::set("FORCE_COLOR", "3");
        let _term = EnvVarGuard::set("TERM", "dumb");
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AttachmentMessage(
                    attachment: Some(attachment),
                    add_margin: false,
                    verbose: verbose,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(80));
        let plain = canvas.to_string().trim_end().to_string();
        let mut bytes = Vec::new();
        canvas.write_ansi(&mut bytes).expect("canvas ansi");
        (String::from_utf8(bytes).expect("ansi is utf-8"), plain)
    }

    /// The SGR pair that opens and closes a bold span. Observed emission for
    /// the fixed `hook_permission_decision` row (80 columns, default theme):
    ///
    /// ```text
    /// ESC[0m ESC[38;2;153;153;153m "  ⎿ Allowed by " ESC[1m "PermissionRequest"
    ///        ESC[22m " hook" ESC[0m ESC[K ESC[0m CRLF
    /// ```
    ///
    /// The gray run spans the whole line and the bold span nests inside it —
    /// Ink's `<Text dimColor>` wrapping `<Text bold>` (`AttachmentMessage.tsx:430-432`,
    /// `:549-568`). Before the fix the same render emitted
    /// `ESC[0m ESC[38;2;153;153;153m "  ⎿ Allowed by PermissionRequest hook" …`
    /// with no `ESC[1m` anywhere.
    const BOLD_ON: &str = "\u{1b}[1m";
    const BOLD_OFF: &str = "\u{1b}[22m";

    /// Maps to: CC `AttachmentMessage.tsx:427-434` —
    /// `{action} by <Text bold>{attachment.hookEvent}</Text> hook`.
    ///
    /// #191: the port rendered one flat `format!("{action} by {hook_event} hook")`,
    /// so the emitted stream contained no `ESC[1m` at all — this test fails on
    /// that shape at the first assertion. `d9ae9a8` (#186) gave
    /// `hook_permission_decision` its first producer, which is what made the
    /// missing emphasis reach users on the PermissionRequest path.
    #[test]
    fn hook_permission_decision_bolds_the_hook_event_like_official() {
        let (ansi, plain) = render_ansi(
            Attachment::HookPermissionDecision {
                decision: "allow".to_string(),
                tool_use_id: "toolu_bold".to_string(),
                hook_event: "PermissionRequest".to_string(),
            },
            false,
        );

        assert!(
            ansi.contains(BOLD_ON),
            "the hook event must be emphasized, not flattened: {ansi:?}"
        );
        // The bold run opens immediately before the event name and the name is
        // the only thing inside it — `Allowed by ` and ` hook` stay unbolded,
        // matching CC's single `<Text bold>` child.
        let bold_at = ansi.find(BOLD_ON).expect("bold span");
        let after_bold = &ansi[bold_at + BOLD_ON.len()..];
        assert!(
            after_bold.starts_with(&format!("PermissionRequest{BOLD_OFF} hook")),
            "the bold span must cover the hook event and nothing else: {after_bold:?}"
        );
        assert_eq!(plain, "  ⎿ Allowed by PermissionRequest hook");

        let (denied_ansi, denied_plain) = render_ansi(
            Attachment::HookPermissionDecision {
                decision: "deny".to_string(),
                tool_use_id: "toolu_bold".to_string(),
                hook_event: "PermissionRequest".to_string(),
            },
            false,
        );
        assert_eq!(denied_plain, "  ⎿ Denied by PermissionRequest hook");
        assert!(denied_ansi.contains(BOLD_ON));
    }

    /// Maps to: CC `AttachmentMessage.tsx:359-363` —
    /// `Async hook <Text bold>{attachment.hookEvent}</Text> completed`.
    ///
    /// Found by reading CC's block instead of the single reported line (#191);
    /// it had the same flat `format!`, so it fails the same way on the old
    /// shape. `verbose: true` is required to get past CC's `:352-358` gates.
    #[test]
    fn async_hook_response_bolds_the_hook_event_like_official() {
        let (ansi, plain) = render_ansi(
            Attachment::AsyncHookResponse {
                process_id: "pid-1".to_string(),
                hook_name: "notify".to_string(),
                hook_event: "PostToolUse".to_string(),
                tool_name: None,
                response: serde_json::json!({}),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            true,
        );

        assert!(
            ansi.contains(BOLD_ON),
            "the hook event must be emphasized, not flattened: {ansi:?}"
        );
        let bold_at = ansi.find(BOLD_ON).expect("bold span");
        assert!(
            ansi[bold_at + BOLD_ON.len()..]
                .starts_with(&format!("PostToolUse{BOLD_OFF} completed")),
            "the bold span must cover the hook event and nothing else"
        );
        assert_eq!(plain, "  ⎿ Async hook PostToolUse completed");
    }
}
