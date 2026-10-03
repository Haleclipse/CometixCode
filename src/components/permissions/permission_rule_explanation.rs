//! Maps to: CC `components/permissions/PermissionRuleExplanation.tsx`.
//!
//! The component is driven by CC `toolUseConfirm.permissionResult.decisionReason`
//! (`PermissionRequest.tsx:139`), carried here on
//! `PermissionRequest::decision_reason`. `stringsForDecisionReason` returning
//! `null` renders NOTHING (`:95-97`), which is the common case: a decision that
//! became `ask` through the step-3 `passthrough → ask` conversion has no reason
//! at all.
//!
//! Producers that populate `decision_reason`: the step-1a deny rule, the step-1b
//! ask rule, whatever a tool's own `check_permissions` returned, and — since the
//! #127/#129 batch — the auto-mode classifier
//! (`permissions.rs#resolve_auto_mode_classifier_decision` block deny +
//! denial-limit ask, `#resolve_classifier_unavailable` fail-closed deny), which
//! used to flatten its text into `PermissionRequest::description` and so left
//! the `Classifier` arm below with no live producer.
//!
//! All seven CC mounts are ported
//! (`ast-grep --lang tsx -p '<PermissionRuleExplanation $$$/>' src/` → Bash:548,
//! PowerShell:276, WebFetch:135, Skill:240, ExitPlanMode:874, Fallback:183,
//! SubmitQuestionsView:81).
//!
//! REMAINING SEAM (producer side, not this component). CC's `allow` decisions
//! also carry a reason (`permissions.ts:1272-1297`: `{type:'mode'}` for bypass,
//! `{type:'rule'}` for a whole-tool allow; `:641-648`/`:678-685`
//! `{type:'mode', mode:'auto'}` for the two auto fast-paths).
//! `HasPermissionsToUseToolResult::Allow` is a unit variant with no request, so
//! those are dropped. Deliberately not carried: no CC consumer is user-visible.
//! Enumerated CC readers of an allow-side reason —
//! `useCanUseTool.tsx:116-125` (only `{type:'classifier', classifier:'auto-mode'}`,
//! and this port already serves it via `set_yolo_classifier_approval`),
//! `toolExecution.ts:952-977` `decisionReasonToOTelSource` (telemetry; `'mode'`
//! maps to the same `'config'` an ABSENT reason yields, `:211-213` vs `:236-244`),
//! and `toolExecution.ts:980-993` (only `{type:'hook', hookName:'PermissionRequest'}`).
//! `PermissionDecisionDebugInfo` only ever sees an `ask`.

use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_result::PermissionDecisionReason;
use crate::utils::theme::{Theme, ThemeColorKey};
use iocraft::prelude::*;

/// SGR pairs `chalk.bold` / `chalk.dim` emit. CC builds these reason strings
/// with `chalk` and hands the result to `<Ansi>` (`:111-113`); this port emits
/// the same escapes and renders them through iocraft's `Ansi`, the 1:1 port of
/// CC `ink/Ansi.tsx`. Same convention as `components/markdown.rs:416`.
const BOLD: &str = "\x1b[1m";
const BOLD_OFF: &str = "\x1b[22m";
const DIM: &str = "\x1b[2m";
const DIM_OFF: &str = "\x1b[22m";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionRuleToolType {
    #[default]
    Tool,
    Command,
    Edit,
    Read,
}

impl PermissionRuleToolType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Edit => "edit",
            Self::Read => "read",
        }
    }
}

/// Maps to: CC `PermissionRuleExplanation.tsx:19-24#DecisionReasonStrings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRuleExplanationStrings {
    pub reason_string: String,
    pub config_string: Option<String>,
    /// When set, `reason_string` is rendered in this theme color instead of the
    /// `<Ansi>` pass-through.
    pub theme_color: Option<ThemeColorKey>,
}

/// Maps to: CC `PermissionRuleExplanation.tsx:26-84#stringsForDecisionReason`.
///
/// `None` is CC's `return null`, which the component turns into "render
/// nothing" — the `default:` arm plus the `if (!reason)` guard.
pub fn strings_for_decision_reason(
    reason: Option<&PermissionDecisionReason>,
    tool_type: PermissionRuleToolType,
) -> Option<PermissionRuleExplanationStrings> {
    let reason = reason?;
    let tool_type = tool_type.as_str();

    // CC `:33-48` gates the classifier arm on
    // `feature('BASH_CLASSIFIER') || feature('TRANSCRIPT_CLASSIFIER')`. Only the
    // transcript half has a FeatureFlag row in this port; the bash classifier is
    // an external stub whose own gate is a hardcoded `false`
    // (`utils/permissions/bash_classifier.rs:47-49`), so the disjunction reduces
    // to the transcript gate.
    if crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
        if let PermissionDecisionReason::Classifier { classifier, reason } = reason {
            if classifier == "auto-mode" {
                return Some(PermissionRuleExplanationStrings {
                    reason_string: format!(
                        "Auto mode classifier requires confirmation for this {tool_type}.\n{reason}"
                    ),
                    config_string: None,
                    theme_color: Some(ThemeColorKey::Error),
                });
            }
            return Some(PermissionRuleExplanationStrings {
                // CC `:45` `chalk.bold(reason.classifier)`.
                reason_string: format!(
                    "Classifier {BOLD}{classifier}{BOLD_OFF} requires confirmation for this {tool_type}.\n{reason}"
                ),
                config_string: None,
                theme_color: None,
            });
        }
    }

    match reason {
        // CC `:50-59`. The quoted rule is the rule that MATCHED, and
        // `policySettings` rules cannot be edited so they get no config hint.
        PermissionDecisionReason::Rule { rule } => Some(PermissionRuleExplanationStrings {
            // CC `:52-54` `chalk.bold(permissionRuleValueToString(...))`.
            reason_string: format!(
                "Permission rule {BOLD}{}{BOLD_OFF} requires confirmation for this {tool_type}.",
                crate::utils::permissions::permission_rule_parser::permission_rule_value_to_string(
                    &rule.rule_value
                )
            ),
            config_string: (rule.source
                != crate::types::permissions::PermissionRuleSource::PolicySettings)
                .then(|| "/permissions to update rules".to_string()),
            theme_color: None,
        }),
        // CC `:60-69`.
        PermissionDecisionReason::Hook {
            hook_name,
            hook_source,
            reason,
        } => {
            // CC `reason.reason ? \`:\n${reason.reason}\` : '.'` — JS truthiness,
            // so an empty string takes the `'.'` branch.
            let hook_reason_string = match reason.as_deref().filter(|value| !value.is_empty()) {
                Some(reason) => format!(":\n{reason}"),
                None => ".".to_string(),
            };
            // CC `:62-64` `\` ${chalk.dim(\`[${reason.hookSource}]\`)}\``.
            let source_label = match hook_source.as_deref().filter(|value| !value.is_empty()) {
                Some(source) => format!(" {DIM}[{source}]{DIM_OFF}"),
                None => String::new(),
            };
            Some(PermissionRuleExplanationStrings {
                // CC `:66` `chalk.bold(reason.hookName)`.
                reason_string: format!(
                    "Hook {BOLD}{hook_name}{BOLD_OFF} requires confirmation for this {tool_type}{hook_reason_string}{source_label}"
                ),
                config_string: Some("/hooks to update".to_string()),
                theme_color: None,
            })
        }
        // CC `:70-75`.
        PermissionDecisionReason::SafetyCheck { reason, .. }
        | PermissionDecisionReason::Other { reason } => Some(PermissionRuleExplanationStrings {
            reason_string: reason.clone(),
            config_string: None,
            theme_color: None,
        }),
        // CC `:76-80`.
        PermissionDecisionReason::WorkingDir { reason } => Some(PermissionRuleExplanationStrings {
            reason_string: reason.clone(),
            config_string: Some("/permissions to update rules".to_string()),
            theme_color: None,
        }),
        // CC `:81-83` `default: return null` — `mode`, `subcommandResults`,
        // `permissionPromptTool`, `asyncAgent`, `sandboxOverride`, and
        // `classifier` with the feature off all render nothing.
        _ => None,
    }
}

#[derive(Default, Props)]
pub struct PermissionRuleExplanationProps {
    /// Maps to: CC `permissionResult={toolUseConfirm.permissionResult}` — every
    /// call site passes the whole decision and the component reads
    /// `.decisionReason`.
    pub decision_reason: Option<PermissionDecisionReason>,
    pub tool_type: PermissionRuleToolType,
    /// Maps to: CC `useAppState(s => s.toolPermissionContext.mode)` (`:90`).
    /// Callers pass `request.mode`, the mode the decision was taken under.
    pub permission_mode: PermissionMode,
}

/// Maps to: CC `PermissionRuleExplanation` (`:86-118`).
#[component]
pub fn PermissionRuleExplanation(
    props: &PermissionRuleExplanationProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let Some(strings) =
        strings_for_decision_reason(props.decision_reason.as_ref(), props.tool_type)
    else {
        return element! { View() {} }.into_any();
    };

    // CC `:99-104`: a hook ask in auto mode is warning-colored.
    let theme_color = strings.theme_color.or_else(|| {
        (matches!(
            props.decision_reason,
            Some(PermissionDecisionReason::Hook { .. })
        ) && props.permission_mode == PermissionMode::Auto)
            .then_some(ThemeColorKey::Warning)
    });

    element! {
        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
            // CC `:106-114` has two arms: `<ThemedText color>` when a theme
            // color was chosen, `<Text><Ansi>` otherwise. Both end up writing the
            // same bytes to the terminal — Ink's Text emits a child string's
            // embedded SGR verbatim inside its own color wrapper, and strips ANSI
            // when measuring width. iocraft's plain Text does neither, so `Ansi`
            // with an optional default color is the transport adapter for both
            // arms (same reasoning as `fallback_tool_use_error_message.rs:161-164`).
            // Only the hook arm can carry escapes into the themed branch, via
            // `chalk.bold(hookName)` / `chalk.dim([source])`.
            Ansi(
                content: strings.reason_string,
                color: theme_color.map(|key| theme.color(key)),
                wrap: TextWrap::Wrap,
            )
            #(strings.config_string.map(|config_string| element! {
                Text(content: config_string, color: theme.inactive, wrap: TextWrap::NoWrap)
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionRule, PermissionRuleSource, PermissionRuleValue,
    };
    use crate::utils::test_env::TEST_ENV_LOCK;
    use crate::utils::theme;

    fn ask_rule(source: PermissionRuleSource, content: &str) -> PermissionDecisionReason {
        PermissionDecisionReason::Rule {
            rule: PermissionRule {
                source,
                rule_behavior: PermissionBehavior::Ask,
                rule_value: PermissionRuleValue::new("Bash", Some(content.to_string())),
            },
        }
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:51-58` — the rule value is
    /// wrapped in `chalk.bold`, i.e. `\x1b[1m…\x1b[22m`, and handed to `<Ansi>`
    /// at `:111-113`. Re-derived from CC when the `<Ansi>` port landed; the
    /// previous plain-text expectation was transcribing the port.
    #[test]
    fn permission_rule_explanation_formats_rule_reason_and_hint() {
        let strings = strings_for_decision_reason(
            Some(&ask_rule(PermissionRuleSource::LocalSettings, "cargo test")),
            PermissionRuleToolType::Command,
        )
        .expect("rule reason renders");
        assert_eq!(
            strings.reason_string,
            "Permission rule \x1b[1mBash(cargo test)\x1b[22m requires confirmation for this command."
        );
        assert_eq!(
            strings.config_string.as_deref(),
            Some("/permissions to update rules")
        );
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:56-58` — a policy rule cannot
    /// be edited, so it gets no `/permissions` hint.
    #[test]
    fn permission_rule_explanation_matches_official_policy_settings_has_no_config_hint() {
        let strings = strings_for_decision_reason(
            Some(&ask_rule(PermissionRuleSource::PolicySettings, "rm:*")),
            PermissionRuleToolType::Command,
        )
        .expect("rule reason renders");
        assert_eq!(strings.config_string, None);
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:30-32,81-83` — no reason and
    /// the `default:` arm both return null, and `:95-97` renders nothing.
    #[test]
    fn permission_rule_explanation_matches_official_null_for_missing_and_unhandled_reasons() {
        assert!(strings_for_decision_reason(None, PermissionRuleToolType::Tool).is_none());
        assert!(
            strings_for_decision_reason(
                Some(&PermissionDecisionReason::Mode {
                    mode: PermissionMode::Plan
                }),
                PermissionRuleToolType::Tool,
            )
            .is_none()
        );
        assert!(
            strings_for_decision_reason(
                Some(&PermissionDecisionReason::AsyncAgent {
                    reason: "async agent".to_string()
                }),
                PermissionRuleToolType::Tool,
            )
            .is_none()
        );
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:60-69` — `chalk.bold` around
    /// the hook name (`:66`) and `chalk.dim` around the `[source]` suffix
    /// (`:62-64`).
    #[test]
    fn permission_rule_explanation_matches_official_hook_strings() {
        let with_reason = strings_for_decision_reason(
            Some(&PermissionDecisionReason::Hook {
                hook_name: "PreToolUse".to_string(),
                hook_source: Some("settings".to_string()),
                reason: Some("needs review".to_string()),
            }),
            PermissionRuleToolType::Tool,
        )
        .expect("hook reason renders");
        assert_eq!(
            with_reason.reason_string,
            "Hook \x1b[1mPreToolUse\x1b[22m requires confirmation for this tool:\nneeds review \x1b[2m[settings]\x1b[22m"
        );
        assert_eq!(
            with_reason.config_string.as_deref(),
            Some("/hooks to update")
        );

        let without_reason = strings_for_decision_reason(
            Some(&PermissionDecisionReason::Hook {
                hook_name: "PreToolUse".to_string(),
                hook_source: None,
                reason: None,
            }),
            PermissionRuleToolType::Tool,
        )
        .expect("hook reason renders");
        assert_eq!(
            without_reason.reason_string,
            "Hook \x1b[1mPreToolUse\x1b[22m requires confirmation for this tool."
        );
    }

    #[test]
    fn permission_rule_explanation_renders_matched_rule() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRuleExplanation(
                    decision_reason: Some(PermissionDecisionReason::Rule {
                        rule: PermissionRule {
                            source: PermissionRuleSource::LocalSettings,
                            rule_behavior: PermissionBehavior::Ask,
                            rule_value: PermissionRuleValue::new("Read", Some("src/main.rs".to_string())),
                        },
                    }),
                    tool_type: PermissionRuleToolType::Read,
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            text.contains("Permission rule Read(src/main.rs)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("/permissions to update rules"),
            "canvas=\n{text}"
        );
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:33-42` + `:99-109` — the
    /// auto-mode classifier arm builds
    /// `Auto mode classifier requires confirmation for this ${toolType}.\n${reason}`
    /// with `themeColor: 'error'`, and `:108-109` renders that arm through
    /// `<ThemedText color="error">`.
    ///
    /// The producer is `permissions.rs#resolve_auto_mode_classifier_decision`'s
    /// denial-limit fallback, which maps to CC `handleDenialLimitExceeded`
    /// (`permissions.ts:1050-1057`).
    #[test]
    fn permission_rule_explanation_matches_official_classifier_arm_in_error_color() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");

        let reason = PermissionDecisionReason::Classifier {
            classifier: "auto-mode".to_string(),
            reason: "3 consecutive actions were blocked.".to_string(),
        };
        let strings = strings_for_decision_reason(Some(&reason), PermissionRuleToolType::Command)
            .expect("classifier reason renders");
        assert_eq!(
            strings.reason_string,
            "Auto mode classifier requires confirmation for this command.\n3 consecutive actions were blocked."
        );
        assert_eq!(strings.config_string, None, "CC :40 configString undefined");
        assert_eq!(strings.theme_color, Some(ThemeColorKey::Error));

        let theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                PermissionRuleExplanation(
                    decision_reason: Some(reason),
                    tool_type: PermissionRuleToolType::Command,
                )
            }
        }
        .render(Some(80));

        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");

        let text = canvas.to_string();
        assert!(
            text.contains("Auto mode classifier requires confirmation for this command."),
            "canvas=\n{text}"
        );
        assert_eq!(
            canvas.resolved_text_style(0, 0).expect("cell style").color,
            Some(theme.error),
            "CC :108-109 renders the classifier arm in the `error` theme color; canvas=\n{text}"
        );
    }

    /// Maps to: CC `PermissionRuleExplanation.tsx:95-97` — no strings, no Box.
    #[test]
    fn permission_rule_explanation_matches_official_empty_render_without_reason() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRuleExplanation(
                    decision_reason: None,
                    tool_type: PermissionRuleToolType::Tool,
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(!text.contains("Permission rule"), "canvas=\n{text}");
        assert!(
            !text.contains("/permissions to update rules"),
            "canvas=\n{text}"
        );
    }
}
