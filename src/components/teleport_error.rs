//! Maps to: CC `components/TeleportError.tsx`.
//!
//! Preconditions (`checkNeedsClaudeAiLogin`, `checkIsGitClean`) and shutdown
//! side effects remain in the teleport/runtime slice. This file ports the
//! official error prioritization and render branches from caller-provided state.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::components::teleport_stash::{TeleportStash, TeleportStashState};
use iocraft::prelude::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TeleportLocalErrorType {
    NeedsLogin,
    NeedsGitStash,
}

impl TeleportLocalErrorType {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::NeedsLogin => "needsLogin",
            Self::NeedsGitStash => "needsGitStash",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeleportPreconditions {
    pub needs_claude_ai_login: bool,
    pub is_git_clean: bool,
}

#[derive(Default, Props)]
pub struct TeleportErrorProps {
    pub current_error: Option<TeleportLocalErrorType>,
    pub is_logging_in: bool,
    pub errors_to_ignore: Vec<TeleportLocalErrorType>,
    pub stash_state: TeleportStashState,
}

/// Maps to: CC `TeleportError.tsx#getTeleportErrors`.
pub fn get_teleport_errors_from_preconditions(
    preconditions: TeleportPreconditions,
) -> BTreeSet<TeleportLocalErrorType> {
    let mut errors = BTreeSet::new();
    if preconditions.needs_claude_ai_login {
        errors.insert(TeleportLocalErrorType::NeedsLogin);
    }
    if !preconditions.is_git_clean {
        errors.insert(TeleportLocalErrorType::NeedsGitStash);
    }
    errors
}

/// Maps to: CC `TeleportError.tsx#checkErrors` filtered-error priority.
pub fn select_current_teleport_error(
    errors: &BTreeSet<TeleportLocalErrorType>,
    errors_to_ignore: &[TeleportLocalErrorType],
) -> Option<TeleportLocalErrorType> {
    let ignored = errors_to_ignore.iter().copied().collect::<BTreeSet<_>>();
    if errors.contains(&TeleportLocalErrorType::NeedsLogin)
        && !ignored.contains(&TeleportLocalErrorType::NeedsLogin)
    {
        return Some(TeleportLocalErrorType::NeedsLogin);
    }
    if errors.contains(&TeleportLocalErrorType::NeedsGitStash)
        && !ignored.contains(&TeleportLocalErrorType::NeedsGitStash)
    {
        return Some(TeleportLocalErrorType::NeedsGitStash);
    }
    None
}

/// Maps to: CC `TeleportError.tsx` login dialog select options. There is no
/// claude.ai login to offer, so the only choice is to exit.
pub fn teleport_login_options() -> Vec<SelectOptionData> {
    vec![SelectOptionData {
        label: "Exit".to_string(),
        value: "exit".to_string(),
        ..SelectOptionData::default()
    }]
}

/// Maps to: CC `components/TeleportError.tsx#TeleportError`.
#[component]
pub fn TeleportError(props: &TeleportErrorProps) -> impl Into<AnyElement<'static>> {
    let Some(current_error) = props.current_error else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };

    match current_error {
        TeleportLocalErrorType::NeedsGitStash => element! {
            TeleportStash(state: props.stash_state.clone())
        }
        .into_any(),
        TeleportLocalErrorType::NeedsLogin => {
            let options = teleport_login_options();
            element! {
                Dialog(title: "Teleport is unavailable".to_string()) {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "Teleport requires a Claude.ai account, which nyan does not support.".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    }
                    Select(
                        options: options,
                        focused_index: 0usize,
                        visible_option_count: 1usize,
                        visible_from_index: 0usize,
                        layout: SelectLayout::Expanded,
                    )
                }
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render(props: TeleportErrorProps) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TeleportError(
                    current_error: props.current_error,
                    is_logging_in: props.is_logging_in,
                    errors_to_ignore: props.errors_to_ignore,
                    stash_state: props.stash_state,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn teleport_error_collection_and_priority_match_official_check_errors() {
        let errors = get_teleport_errors_from_preconditions(TeleportPreconditions {
            needs_claude_ai_login: true,
            is_git_clean: false,
        });
        assert!(errors.contains(&TeleportLocalErrorType::NeedsLogin));
        assert!(errors.contains(&TeleportLocalErrorType::NeedsGitStash));
        assert_eq!(
            select_current_teleport_error(&errors, &[]),
            Some(TeleportLocalErrorType::NeedsLogin)
        );
        assert_eq!(
            select_current_teleport_error(&errors, &[TeleportLocalErrorType::NeedsLogin]),
            Some(TeleportLocalErrorType::NeedsGitStash)
        );
        assert_eq!(
            select_current_teleport_error(
                &errors,
                &[
                    TeleportLocalErrorType::NeedsLogin,
                    TeleportLocalErrorType::NeedsGitStash
                ]
            ),
            None
        );
    }

    #[test]
    fn teleport_login_options_offer_only_exit() {
        let options = teleport_login_options();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].label, "Exit");
        assert_eq!(options[0].value, "exit");
    }

    #[test]
    fn teleport_error_renders_login_and_stash_branches() {
        let none = render(TeleportErrorProps::default());
        assert_eq!(none, "");

        let login = render(TeleportErrorProps {
            current_error: Some(TeleportLocalErrorType::NeedsLogin),
            ..TeleportErrorProps::default()
        });
        assert!(
            login.contains("Teleport is unavailable"),
            "canvas=\n{login}"
        );
        assert!(
            login.contains("Teleport requires a Claude.ai account"),
            "canvas=\n{login}"
        );
        assert!(
            !login.contains("Login with Claude account"),
            "canvas=\n{login}"
        );

        let stash = render(TeleportErrorProps {
            current_error: Some(TeleportLocalErrorType::NeedsGitStash),
            stash_state: TeleportStashState::Loading,
            ..TeleportErrorProps::default()
        });
        assert!(stash.contains("Checking git status…"), "canvas=\n{stash}");
    }
}
