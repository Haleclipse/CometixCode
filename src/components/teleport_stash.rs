//! Maps to: CC `components/TeleportStash.tsx`.
//!
//! Git probing (`getFileStatus`) and stashing (`stashToCleanState`) remain in the
//! future teleport service slice. This component ports the official rendering
//! states and option copy from a caller-provided status snapshot.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::components::spinner::Spinner;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeleportGitFileStatus {
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
}

impl TeleportGitFileStatus {
    pub fn changed_files(&self) -> Vec<String> {
        self.tracked
            .iter()
            .chain(self.untracked.iter())
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeleportStashState {
    Loading,
    Error(String),
    Ready {
        git_file_status: TeleportGitFileStatus,
        stashing: bool,
        focused_index: usize,
    },
}

impl Default for TeleportStashState {
    fn default() -> Self {
        Self::Loading
    }
}

#[derive(Default, Props)]
pub struct TeleportStashProps {
    pub state: TeleportStashState,
}

/// Maps to: CC `TeleportStash.tsx` options passed to `<Select>`.
pub fn teleport_stash_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Stash changes and continue".to_string(),
            value: "stash".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Exit".to_string(),
            value: "exit".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `components/TeleportStash.tsx#TeleportStash`.
#[component]
pub fn TeleportStash(props: &TeleportStashProps) -> impl Into<AnyElement<'static>> {
    match &props.state {
        TeleportStashState::Loading => element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                View(flex_direction: FlexDirection::Row, margin_bottom: 1u32) {
                    Spinner
                    Text(content: " Checking git status…".to_string(), wrap: TextWrap::NoWrap)
                }
            }
        }
        .into_any(),
        TeleportStashState::Error(error) => element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                Text(content: format!("Error: {error}"), weight: Weight::Bold, color: Color::Red, wrap: TextWrap::Wrap)
                View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                    Text(content: "Press ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: "Escape".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: " to cancel".to_string(), dim: true, wrap: TextWrap::NoWrap)
                }
            }
        }
        .into_any(),
        TeleportStashState::Ready {
            git_file_status,
            stashing,
            focused_index,
        } => {
            let changed_files = git_file_status.changed_files();
            let show_file_count = changed_files.len() > 8;
            let options = teleport_stash_options();
            let focused_index = (*focused_index).min(options.len().saturating_sub(1));
            element! {
                Dialog(title: "Working Directory Has Changes".to_string()) {
                    Text(content: "Teleport will switch git branches. The following changes were found:".to_string(), wrap: TextWrap::Wrap)
                    View(flex_direction: FlexDirection::Column, padding_left: 2u32) {
                        #(if changed_files.is_empty() {
                            vec![element! { Text(content: "No changes detected".to_string(), dim: true, wrap: TextWrap::NoWrap) }]
                        } else if show_file_count {
                            vec![element! { Text(content: format!("{} files changed", changed_files.len()), wrap: TextWrap::NoWrap) }]
                        } else {
                            changed_files.into_iter().map(|file| element! {
                                Text(content: file, wrap: TextWrap::NoWrap)
                            }).collect::<Vec<_>>()
                        })
                    }
                    Text(content: "Would you like to stash these changes and continue with teleport?".to_string(), wrap: TextWrap::Wrap)
                    #(if *stashing {
                        Some(element! {
                            View(flex_direction: FlexDirection::Row) {
                                Spinner
                                Text(content: " Stashing changes...".to_string(), wrap: TextWrap::NoWrap)
                            }
                        }.into_any())
                    } else {
                        Some(element! {
                            Select(
                                options: options,
                                focused_index: focused_index,
                                visible_option_count: 2usize,
                                visible_from_index: 0usize,
                                layout: SelectLayout::Expanded,
                            )
                        }.into_any())
                    })
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

    fn render(state: TeleportStashState) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TeleportStash(state: state)
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn teleport_stash_options_match_official_copy_and_values() {
        let options = teleport_stash_options();
        assert_eq!(options[0].label, "Stash changes and continue");
        assert_eq!(options[0].value, "stash");
        assert_eq!(options[1].label, "Exit");
        assert_eq!(options[1].value, "exit");
    }

    #[test]
    fn teleport_stash_loading_and_error_states_match_official_copy() {
        let loading = render(TeleportStashState::Loading);
        assert!(
            loading.contains("Checking git status…"),
            "canvas=\n{loading}"
        );

        let error = render(TeleportStashState::Error(
            "Failed to stash changes".to_string(),
        ));
        assert!(
            error.contains("Error: Failed to stash changes"),
            "canvas=\n{error}"
        );
        assert!(error.contains("Press Escape to cancel"), "canvas=\n{error}");
    }

    #[test]
    fn teleport_stash_ready_renders_files_or_count_and_select() {
        let few = render(TeleportStashState::Ready {
            git_file_status: TeleportGitFileStatus {
                tracked: vec!["src/main.rs".to_string()],
                untracked: vec!["notes.md".to_string()],
            },
            stashing: false,
            focused_index: 0,
        });
        assert!(
            few.contains("Working Directory Has Changes"),
            "canvas=\n{few}"
        );
        assert!(few.contains("src/main.rs"), "canvas=\n{few}");
        assert!(few.contains("notes.md"), "canvas=\n{few}");
        assert!(few.contains("Stash changes and continue"), "canvas=\n{few}");

        let many = render(TeleportStashState::Ready {
            git_file_status: TeleportGitFileStatus {
                tracked: (0..9).map(|i| format!("file-{i}.rs")).collect(),
                untracked: vec![],
            },
            stashing: false,
            focused_index: 0,
        });
        assert!(many.contains("9 files changed"), "canvas=\n{many}");

        let stashing = render(TeleportStashState::Ready {
            git_file_status: TeleportGitFileStatus::default(),
            stashing: true,
            focused_index: 0,
        });
        assert!(
            stashing.contains("Stashing changes..."),
            "canvas=\n{stashing}"
        );
    }
}
