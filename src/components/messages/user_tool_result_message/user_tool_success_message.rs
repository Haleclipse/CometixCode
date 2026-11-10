//! Maps to: CC `components/messages/UserToolResultMessage/UserToolSuccessMessage.tsx`.

use super::utils::{ToolRenderBackground, ToolRenderOptions, ToolRenderTone};
use super::{
    contains_ansi_escape, official_file_result_element, render_tool_result_lines_for_result,
    success_tool_result_is_nonvisual,
};
use crate::components::markdown::Markdown;
use crate::components::shell::use_expand_shell_output;
use crate::components::structured_diff::color_diff::SyntaxHighlightTheme;
use crate::state::app_state::use_app_state;
use crate::tools::brief_tool::ui::BriefToolResultMessage;
use crate::types::message::ToolResultStatus;
use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct UserToolSuccessMessageProps {
    pub tool_name: String,
    pub content: String,
    /// Maps to: CC `message.toolUseResult` — the raw per-tool output the
    /// success renderer parses with the tool's own schema
    /// (`UserToolSuccessMessage.tsx:80`); the by-tool-name dispatch reads it.
    pub tool_use_result: Option<serde_json::Value>,
    /// Maps to: CC `options.input` — the paired tool_use input resolved from
    /// the lookups (`UserToolSuccessMessage.tsx:97`).
    pub tool_input: Option<serde_json::Value>,
    /// Maps to: CC `progressMessagesForMessage` (`UserToolSuccessMessage.tsx:89`).
    pub progress_messages: Vec<crate::types::message::ToolUseProgressMessage>,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// Maps to: CC `style?: 'condensed'` — forwarded to the per-tool
    /// `renderToolResultMessage` (AgentTool/UI.tsx:705 sets it).
    pub style: Option<String>,
}

/// Maps to: CC
/// `components/messages/UserToolResultMessage/UserToolSuccessMessage.tsx:38-149`
/// `UserToolSuccessMessage`; the success renderer options correspond to
/// `Tool.ts:566-579` `renderToolResultMessage`.
///
/// Partial seam: CC's compile-time `KAIROS_BRIEF` capability has no faithful
/// local build-feature projection. The canonical `FeatureFlag::Kairos` branch
/// is preserved; the runtime `NYAN_BRIEF` opt-in is deliberately not
/// substituted.
#[component]
pub fn UserToolSuccessMessage(
    props: &UserToolSuccessMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (terminal_width, _) = hooks.use_terminal_size();
    // Mirrors the upstream compile-time ternary: external builds do not read
    // AppState for every scrollback row. If KAIROS becomes available, this is
    // intentionally the canonical `useAppState`, not the provider-optional hook.
    //
    // Not a conditional-hook violation, despite the shape: `feature_enabled`
    // resolves against the `FEATURE_SWITCHES` constant table, so the branch is
    // fixed for the life of the process. iocraft only requires the hook
    // SEQUENCE to be identical across renders of a component — a branch that is
    // always taken, or never, satisfies that. The source relies on the same
    // property, where `feature('KAIROS')` is eliminated at bundle time.
    let is_brief_only = if feature_enabled(FeatureFlag::Kairos) {
        use_app_state(&mut hooks, |state| state.is_brief_only)
    } else {
        false
    };
    // Maps to CC OutputLine `shouldShowFull = verbose || expandShellOutput`.
    let expand_shell_output = use_expand_shell_output(&mut hooks);

    // CC ToolSearch/TodoWrite have no renderToolResultMessage → success is null.
    if success_tool_result_is_nonvisual(&props.tool_name, props.tool_use_result.as_ref()) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    let text = props.content.trim().to_string();
    // CC `UserToolSuccessMessage.tsx:86-99` `tool.renderToolResultMessage?.()`;
    // the Agent body lives with its owner (`tools/AgentTool/UI.tsx:325-470`).
    if let Some(agent_transcript) = crate::tools::agent_tool::ui::render_tool_result_message(
        &props.tool_name,
        props.tool_use_result.as_ref(),
        &props.progress_messages,
        ToolResultStatus::Success,
        props.verbose,
        props.is_transcript_mode,
    ) {
        return agent_transcript;
    }
    // Brief renders from the raw `toolUseResult` on the row, resolved by
    // tool name exactly as CC's per-tool renderToolResultMessage is
    // (SendUserMessage is the tool name; Brief is its legacy alias).
    if props.tool_name.eq_ignore_ascii_case("SendUserMessage")
        || props.tool_name.eq_ignore_ascii_case("Brief")
    {
        let Some(output) = props
            .tool_use_result
            .as_ref()
            .and_then(crate::tools::brief_tool::ui::parse_output)
        else {
            // No raw, or output the schema rejects — CC's success leaf
            // returns null (`UserToolSuccessMessage.tsx:72,81`).
            return element! { View(width: 0u32, height: 0u32) }.into_any();
        };
        let Some(rendered) = crate::tools::brief_tool::ui::render_tool_result_message(
            &output,
            crate::tools::brief_tool::ui::BriefRenderOptions {
                is_transcript_mode: props.is_transcript_mode,
                is_brief_only,
            },
        ) else {
            return element! { View(width: 0u32, height: 0u32) }.into_any();
        };
        return element! { BriefToolResultMessage(rendered) }.into_any();
    }
    // The file tools render their success element from the raw
    // `toolUseResult`, resolved by tool name — CC's per-tool render runs
    // unconditionally: UserToolSuccessMessage.tsx:86-99 always calls
    // `tool.renderToolResultMessage`, and FileEditTool's implementation
    // (UI.tsx:92) destructures `{ style, verbose }` only; `isTranscriptMode`
    // is passed but ignored.
    if let Some(element) = official_file_result_element(
        &props.tool_name,
        ToolResultStatus::Success,
        props.tool_use_result.as_ref(),
        props.tool_input.as_ref(),
        props.verbose,
        props.style.as_deref(),
    ) {
        return element;
    }
    // Official `expandShellOutput` only affects Bash/PowerShell OutputLine.
    // Those tools carry no display variant, so the gate keys on the tool
    // name, as CC's per-tool render does.
    let shell_verbose = props.verbose
        || (expand_shell_output
            && (props.tool_name.eq_ignore_ascii_case("Bash")
                || props.tool_name.eq_ignore_ascii_case("PowerShell")));
    // Grep and Glob no longer carry display variants, so the
    // SearchResultSummary wrap behavior keys on the tool name, as CC's
    // per-tool render does (Glob reuses Grep's renderer, GlobTool/UI.tsx:56).
    let search_result_gutter = props.tool_name.eq_ignore_ascii_case("Grep")
        || props.tool_name.eq_ignore_ascii_case("Glob")
        || props.tool_name.eq_ignore_ascii_case("CodeSearch");
    let lines = render_tool_result_lines_for_result(
        &props.tool_name,
        ToolResultStatus::Success,
        &text,
        props.tool_use_result.as_ref(),
        props.tool_input.as_ref(),
        &props.progress_messages,
        ToolRenderOptions {
            verbose: shell_verbose,
            is_transcript_mode: props.is_transcript_mode,
            terminal_width: terminal_width as usize,
            syntax_highlighting: true,
            syntax_theme: SyntaxHighlightTheme::from_theme(*theme),
        },
    );

    element! {
        View(
            flex_direction: FlexDirection::Column,
        ) {
            #(lines.into_iter().enumerate().map(|(idx, line)| {
                let line_color = match line.tone {
                    ToolRenderTone::Normal => None,
                    ToolRenderTone::Success => Some(theme.success),
                    ToolRenderTone::Warning => Some(theme.warning),
                    ToolRenderTone::Error => Some(theme.error),
                    ToolRenderTone::Inactive => Some(theme.inactive),
                };
                let background_color = line.background.map(|background| match background {
                    ToolRenderBackground::DiffAdded => theme.diff_added,
                    ToolRenderBackground::DiffRemoved => theme.diff_removed,
                    ToolRenderBackground::DiffAddedWord => theme.diff_added_word,
                    ToolRenderBackground::DiffRemovedWord => theme.diff_removed_word,
                });
                let has_ansi = contains_ansi_escape(&line.text);
                let line_wrap = if search_result_gutter && props.verbose && idx > 0 {
                    TextWrap::Wrap
                } else {
                    TextWrap::NoWrap
                };
                let dim_text = line.dim;
                let dim_ansi = matches!(line.tone, ToolRenderTone::Inactive) || line.dim;
                element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(
                            content: if search_result_gutter {
                                if idx == 0 { "  ⎿  ".to_string() } else { "     ".to_string() }
                            } else if idx == 0 {
                                "  ⎿ ".to_string()
                            } else {
                                "    ".to_string()
                            },
                            color: theme.inactive,
                            wrap: TextWrap::NoWrap,
                        )
                        #(if has_ansi {
                            Some(element! {
                                Ansi(content: line.text, color: line_color, dim_color: dim_ansi, wrap: line_wrap)
                            }.into_any())
                        } else if !line.segments.is_empty() {
                            Some(element! {
                                View(flex_direction: FlexDirection::Row) {
                                    #(line.segments.into_iter().map(|segment| {
                                        let segment_background = segment.background.map(|background| match background {
                                            ToolRenderBackground::DiffAdded => theme.diff_added,
                                            ToolRenderBackground::DiffRemoved => theme.diff_removed,
                                            ToolRenderBackground::DiffAddedWord => theme.diff_added_word,
                                            ToolRenderBackground::DiffRemovedWord => theme.diff_removed_word,
                                        }).or(background_color);
                                        let segment_color = segment.foreground.or(line_color);
                                        let segment_dim = dim_text || segment.dim;
                                        element! {
                                            Text(content: segment.text, color: segment_color, background_color: segment_background, dim: segment_dim, bold: segment.bold, wrap: TextWrap::NoWrap)
                                        }
                                    }))
                                }
                            }.into_any())
                        } else {
                            Some(element! {
                                Text(content: line.text, color: line_color, background_color: background_color, dim: dim_text, wrap: line_wrap)
                            }.into_any())
                        })
                    }
                }
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;
    use crate::state::store::AppStore;

    fn render_with_provider(is_brief_only: bool, is_transcript_mode: bool) -> String {
        let mut state = AppState::default();
        state.is_brief_only = is_brief_only;
        let store = AppStore::new(state, None);
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        UserToolSuccessMessage(
                            tool_name: "SendUserMessage".to_string(),
                            content: "Message delivered to user.".to_string(),
                            // The raw `toolUseResult` drives the Brief
                            // component (CC call() data, BriefTool.ts:193).
                            tool_use_result: Some(serde_json::json!({
                                "message": "Hello **there**"
                            })),
                            verbose: false,
                            is_transcript_mode: is_transcript_mode,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(80))
        .to_string()
    }

    #[test]
    fn user_tool_success_brief_dispatch_matches_official_default_and_transcript_ui() {
        let default = render_with_provider(false, false);
        assert!(default.contains("Hello there"), "canvas=\n{default}");
        assert!(!default.contains(crate::constants::figures::BLACK_CIRCLE));
        assert!(!default.contains("nyan"));

        let transcript = render_with_provider(true, true);
        assert!(transcript.contains("Hello there"), "canvas=\n{transcript}");
        assert!(transcript.contains(crate::constants::figures::BLACK_CIRCLE));
        assert!(!transcript.contains("nyan"));
    }

    #[test]
    fn user_tool_success_kairos_brief_seam_matches_official_available_build_projection() {
        assert!(
            !feature_enabled(FeatureFlag::Kairos),
            "this seam oracle assumes the current external KAIROS build projection"
        );
        let canvas = render_with_provider(true, false);
        assert!(canvas.contains("Hello there"), "canvas=\n{canvas}");
        assert!(!canvas.contains("nyan"), "canvas=\n{canvas}");
    }
}
