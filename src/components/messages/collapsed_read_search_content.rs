//! Maps to: CC `components/messages/CollapsedReadSearchContent.tsx`.

use super::assistant_tool_use_message::AssistantToolUseMessage;
use super::user_tool_result_message::UserToolResultMessage;
use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::tool_use_loader::ToolUseLoader;
use crate::types::message::{CollapsedReadSearchEntry, CollapsedReadSearchGroup};
use crate::utils::format::format_seconds_short;
use iocraft::prelude::*;

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[derive(Default, Props)]
pub struct CollapsedReadSearchContentProps {
    pub message: Option<CollapsedReadSearchGroup>,
    pub add_margin: bool,
    pub can_animate: bool,
    /// CC prop (CollapsedReadSearchContent.tsx:39). NOT part of the expansion
    /// decision here (single-source ruling, see `expand_by_default`); still
    /// threaded to the per-entry tool renderers below.
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// Cometix extension (no CC counterpart) — user-authorized L2 (v3 ruling,
    /// 2026-08-01). SINGLE-SOURCE expansion control REPLACING CC's verbose gate
    /// (CollapsedReadSearchContent.tsx:36-43 gates per-tool rows on `verbose`
    /// only): when true, render per-tool rows instead of the one-line summary;
    /// `verbose` no longer participates in this decision. Default true (factory
    /// `expandCollapsedReadSearch`, `AppState.expand_collapsed_read_search`).
    pub expand_by_default: bool,
}

fn push_count_part(
    parts: &mut Vec<String>,
    first_verb: &str,
    next_verb: &str,
    count: usize,
    singular: &str,
    plural: &str,
) {
    if count == 0 {
        return;
    }
    let verb = if parts.is_empty() {
        first_verb
    } else {
        next_verb
    };
    let noun = if count == 1 { singular } else { plural };
    parts.push(format!("{verb} {count} {noun}"));
}

fn collapsed_has_visible_counts(message: &CollapsedReadSearchGroup) -> bool {
    !message.commits.is_empty()
        || !message.pushes.is_empty()
        || !message.branches.is_empty()
        || !message.prs.is_empty()
        || message.search_count > 0
        || message.read_count > 0
        || message.list_count > 0
        || message.mcp_call_count > 0
        || message.bash_count > 0
        || message.memory_read_count > 0
        || message.memory_search_count > 0
        || message.memory_write_count > 0
        || super::team_mem_collapsed::check_has_team_mem_ops(message)
}

/// Maps to: CC `components/messages/CollapsedReadSearchContent.tsx:120-530`
/// `CollapsedReadSearchContent`.
#[component]
pub fn CollapsedReadSearchContent(
    props: &CollapsedReadSearchContentProps,
    _hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let message = props.message.clone().unwrap_or(CollapsedReadSearchGroup {
        read_count: 0,
        search_count: 0,
        list_count: 0,
        bash_count: 0,
        git_op_bash_count: 0,
        commits: Vec::new(),
        pushes: Vec::new(),
        branches: Vec::new(),
        prs: Vec::new(),
        mcp_call_count: 0,
        mcp_server_names: Vec::new(),
        memory_search_count: 0,
        memory_read_count: 0,
        memory_write_count: 0,
        team_memory_search_count: 0,
        team_memory_read_count: 0,
        team_memory_write_count: 0,
        hook_total_ms: None,
        hook_count: 0,
        hook_infos: Vec::new(),
        relevant_memories: Vec::new(),
        verbose_entries: Vec::new(),
        hint: String::new(),
        active: false,
        errored: false,
    });
    // Single-source rule (user-authorized L2, v3 2026-08-01): the expand
    // switch alone decides expansion; `verbose` is deliberately excluded
    // (CC gates on verbose, CollapsedReadSearchContent.tsx:36-43).
    let show_expanded = props.expand_by_default;
    let show_full = show_expanded || props.is_transcript_mode;

    use crate::tools::shared::git_operation_tracking::{BranchAction, CommitKind, PrAction};

    fn push_git_part(parts: &mut Vec<String>, verb: &str, body: String) {
        let verb = if parts.is_empty() {
            let mut chars = verb.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        } else {
            verb.to_string()
        };
        parts.push(format!("{verb} {body}"));
    }

    let mut parts = Vec::new();
    for (kind, verb) in [
        (CommitKind::Committed, "committed"),
        (CommitKind::Amended, "amended commit"),
        (CommitKind::CherryPicked, "cherry-picked"),
    ] {
        let shas = message
            .commits
            .iter()
            .filter(|commit| commit.kind == kind)
            .map(|commit| commit.sha.clone())
            .collect::<Vec<_>>();
        if !shas.is_empty() {
            push_git_part(&mut parts, verb, shas.join(", "));
        }
    }
    if !message.pushes.is_empty() {
        let mut branches = message
            .pushes
            .iter()
            .map(|push| push.branch.clone())
            .collect::<Vec<_>>();
        branches.sort();
        branches.dedup();
        push_git_part(&mut parts, "pushed to", branches.join(", "));
    }
    for branch in &message.branches {
        let verb = match branch.action {
            BranchAction::Merged => "merged",
            BranchAction::Rebased => "rebased onto",
        };
        push_git_part(&mut parts, verb, branch.reference.clone());
    }
    for pr in &message.prs {
        let verb = match pr.action {
            PrAction::Created => "created",
            PrAction::Edited => "edited",
            PrAction::Merged => "merged",
            PrAction::Commented => "commented on",
            PrAction::Closed => "closed",
            PrAction::Ready => "marked ready",
        };
        push_git_part(&mut parts, verb, format!("PR #{}", pr.number));
    }
    if message.search_count > 0 {
        let (first, next) = if message.active {
            ("Searching for", "searching for")
        } else {
            ("Searched for", "searched for")
        };
        push_count_part(
            &mut parts,
            first,
            next,
            message.search_count,
            "pattern",
            "patterns",
        );
    }
    if message.read_count > 0 {
        let (first, next) = if message.active {
            ("Reading", "reading")
        } else {
            ("Read", "read")
        };
        push_count_part(&mut parts, first, next, message.read_count, "file", "files");
    }
    if message.list_count > 0 {
        let (first, next) = if message.active {
            ("Listing", "listing")
        } else {
            ("Listed", "listed")
        };
        push_count_part(
            &mut parts,
            first,
            next,
            message.list_count,
            "directory",
            "directories",
        );
    }
    if message.mcp_call_count > 0 {
        let server_label = if message.mcp_server_names.is_empty() {
            "MCP".to_string()
        } else {
            message
                .mcp_server_names
                .iter()
                .map(|name| name.strip_prefix("claude.ai ").unwrap_or(name).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let verb = if message.active {
            if parts.is_empty() {
                "Querying"
            } else {
                "querying"
            }
        } else if parts.is_empty() {
            "Queried"
        } else {
            "queried"
        };
        let suffix = if message.mcp_call_count > 1 {
            format!(" {} times", message.mcp_call_count)
        } else {
            String::new()
        };
        parts.push(format!("{verb} {server_label}{suffix}"));
    }
    let remaining_bash_count = message.bash_count.saturating_sub(message.git_op_bash_count);
    if remaining_bash_count > 0 {
        let (first, next) = if message.active {
            ("Running", "running")
        } else {
            ("Ran", "ran")
        };
        push_count_part(
            &mut parts,
            first,
            next,
            remaining_bash_count,
            "bash command",
            "bash commands",
        );
    }
    if message.memory_read_count > 0 {
        let (first, next) = if message.active {
            ("Recalling", "recalling")
        } else {
            ("Recalled", "recalled")
        };
        push_count_part(
            &mut parts,
            first,
            next,
            message.memory_read_count,
            "memory",
            "memories",
        );
    }
    if message.memory_search_count > 0 {
        let text = if message.active {
            if parts.is_empty() {
                "Searching memories"
            } else {
                "searching memories"
            }
        } else if parts.is_empty() {
            "Searched memories"
        } else {
            "searched memories"
        };
        parts.push(text.to_string());
    }
    if message.memory_write_count > 0 {
        let (first, next) = if message.active {
            ("Writing", "writing")
        } else {
            ("Wrote", "wrote")
        };
        push_count_part(
            &mut parts,
            first,
            next,
            message.memory_write_count,
            "memory",
            "memories",
        );
    }
    super::team_mem_collapsed::push_team_mem_count_parts(&message, &mut parts);

    let mut summary = parts.join(", ");
    if message.active && !summary.is_empty() {
        summary.push('…');
    }
    if !show_full && !summary.is_empty() {
        summary.push(' ');
        summary.push_str(&ctrl_o_to_expand_hint());
    }

    if !show_expanded && !collapsed_has_visible_counts(&message) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    element! {
        View(
            flex_direction: FlexDirection::Column,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
        ) {
            #(if !show_expanded {
                Some(element! {
                    View(flex_direction: FlexDirection::Row) {
                        #(if message.active {
                            // CC `CollapsedReadSearchContent.tsx:575`: the
                            // summary loader is `shouldAnimate` literally —
                            // the prop only reaches the expanded entries
                            // (:97, :251).
                            Some(element! {
                                ToolUseLoader(should_animate: true, is_unresolved: true, is_error: message.errored)
                            }.into_any())
                        } else {
                            Some(element! { View(min_width: 2u32, flex_shrink: 0.0f32) {} }.into_any())
                        })
                        Text(content: summary, dim: !message.active, wrap: TextWrap::NoWrap)
                    }
                })
            } else {
                None
            })
            #(if !show_expanded && (show_full || message.active) && !message.hint.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Row) {
                        View(width: 5u32, flex_shrink: 0.0f32) {
                            Text(content: "  ⎿  ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        }
                        View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                            #(message.hint.split('\n').map(|line| {
                                element! { Text(content: line.to_string(), dim: true) }
                            }))
                        }
                    }
                })
            } else {
                None
            })
            #(if !show_expanded && message.hook_total_ms.unwrap_or(0) > 0 {
                let hook_count = message.hook_count;
                let noun = if hook_count == 1 { "hook" } else { "hooks" };
                Some(element! {
                    Text(
                        content: format!("  ⎿  Ran {hook_count} PreToolUse {noun} ({})", format_seconds_short(message.hook_total_ms.unwrap_or(0))),
                        dim: true,
                    )
                })
            } else {
                None
            })
            #(if show_expanded && !message.verbose_entries.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        #(message.verbose_entries.iter().map(|entry| {
                            match entry {
                                CollapsedReadSearchEntry::ToolUse { tool_name, input, tool_use_id, description, status } => element! {
                                    AssistantToolUseMessage(
                                        tool_name: tool_name.clone(),
                                        input: input.clone(),
                                        tool_use_id: tool_use_id.clone(),
                                        description: description.clone(),
                                        status: Some(*status),
                                        add_margin: true,
                                        can_animate: props.can_animate,
                                        verbose: props.verbose,
                                        is_transcript_mode: props.is_transcript_mode,
                                    )
                                }.into_any(),
                                CollapsedReadSearchEntry::ToolResult { tool_name, status: _, content, tool_use_result } => element! {
                                    UserToolResultMessage(
                                        tool_name: tool_name.clone(),
                                        // collapse only records Success results
                                        // (collapse_read_search.rs pushes the
                                        // entry solely on Success), so the
                                        // error leaf can never apply here.
                                        is_error: false,
                                        content: content.clone(),
                                        tool_use_result: tool_use_result.clone(),
                                        // Keep ToolMessage body visible with normal fold
                                        // truncation — do not force verbose full dumps.
                                        verbose: props.verbose,
                                        is_transcript_mode: props.is_transcript_mode,
                                    )
                                }.into_any(),
                            }
                        }))
                    }
                })
            } else {
                None
            })
            #(if show_expanded && !message.hook_infos.is_empty() {
                let hook_count = message.hook_count;
                let noun = if hook_count == 1 { "hook" } else { "hooks" };
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(
                            content: format!("  ⎿  Ran {hook_count} PreToolUse {noun} ({})", format_seconds_short(message.hook_total_ms.unwrap_or(0))),
                            dim: true,
                        )
                        #(message.hook_infos.iter().map(|info| {
                            element! {
                                Text(
                                    content: format!(
                                        "     ⎿ {} ({})",
                                        info.command.as_deref().unwrap_or_default(),
                                        format_seconds_short(info.duration_ms.unwrap_or(0)),
                                    ),
                                    dim: true,
                                )
                            }
                        }))
                    }
                })
            } else {
                None
            })
            #(if show_full && !message.relevant_memories.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        #(message.relevant_memories.iter().map(|memory| {
                            element! {
                                View(flex_direction: FlexDirection::Column, margin_top: if show_expanded { 1u32 } else { 0u32 }) {
                                    Text(content: format!("  ⎿  Recalled {}", basename(&memory.path)), dim: true)
                                    #(if show_full && !memory.content.is_empty() {
                                        Some(element! {
                                            View(padding_left: 5u32) {
                                                Ansi(content: memory.content.clone())
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
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(
        read_count: usize,
        search_count: usize,
        list_count: usize,
        bash_count: usize,
        active: bool,
    ) -> CollapsedReadSearchGroup {
        CollapsedReadSearchGroup {
            read_count,
            search_count,
            list_count,
            bash_count,
            git_op_bash_count: 0,
            commits: Vec::new(),
            pushes: Vec::new(),
            branches: Vec::new(),
            prs: Vec::new(),
            mcp_call_count: 0,
            mcp_server_names: Vec::new(),
            memory_search_count: 0,
            memory_read_count: 0,
            memory_write_count: 0,
            team_memory_search_count: 0,
            team_memory_read_count: 0,
            team_memory_write_count: 0,
            hook_total_ms: None,
            hook_count: 0,
            hook_infos: Vec::new(),
            relevant_memories: Vec::new(),
            verbose_entries: Vec::new(),
            hint: String::new(),
            active,
            errored: false,
        }
    }

    fn rendered_summary(message: CollapsedReadSearchGroup) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                CollapsedReadSearchContent(
                    message: Some(message),
                    add_margin: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string()
    }

    #[test]
    fn collapsed_summary_omits_zero_counts_and_uses_official_past_verbs() {
        assert!(
            rendered_summary(group(0, 1, 2, 0, false))
                .contains("Searched for 1 pattern, listed 2 directories (ctrl+o to expand)")
        );
    }

    #[test]
    fn collapsed_summary_uses_present_tense_and_ellipsis_when_active() {
        assert!(
            rendered_summary(group(2, 0, 0, 1, true))
                .contains("Reading 2 files, running 1 bash command… (ctrl+o to expand)")
        );
    }

    #[test]
    fn collapsed_content_non_verbose_hides_absorbed_silent_groups_with_zero_counts() {
        let mut message = group(0, 0, 0, 0, false);
        message.verbose_entries = vec![CollapsedReadSearchEntry::ToolUse {
            tool_name: "ToolSearch".to_string(),
            input: None,
            tool_use_id: None,
            description: "load deferred tool schemas".to_string(),
            status: crate::types::message::ToolUseStatus::Succeeded,
        }];

        let canvas = element! {
            CollapsedReadSearchContent(
                message: Some(message),
                add_margin: false,
                verbose: false,
                is_transcript_mode: false,
            )
        }
        .render(None);

        assert_eq!(canvas.to_string().trim(), "");
    }

    #[test]
    fn collapsed_summary_leads_with_git_outcomes_and_subtracts_bash_count() {
        let mut message = group(0, 0, 0, 2, false);
        message.git_op_bash_count = 1;
        message.commits.push(
            crate::tools::shared::git_operation_tracking::GitCommitSummary {
                sha: "abc123".to_string(),
                kind: crate::tools::shared::git_operation_tracking::CommitKind::Committed,
            },
        );
        message
            .prs
            .push(crate::tools::shared::git_operation_tracking::GitPrSummary {
                number: 42,
                url: Some("https://github.com/o/r/pull/42".to_string()),
                action: crate::tools::shared::git_operation_tracking::PrAction::Created,
            });

        assert!(
            rendered_summary(message).contains(
                "Committed abc123, created PR #42, ran 1 bash command (ctrl+o to expand)"
            )
        );
    }

    #[test]
    fn collapsed_summary_renders_mcp_server_counts_like_official() {
        let mut message = group(0, 0, 0, 0, false);
        message.mcp_call_count = 2;
        message.mcp_server_names = vec!["claude.ai slack".to_string()];

        assert!(rendered_summary(message).contains("Queried slack 2 times (ctrl+o to expand)"));
    }

    #[test]
    fn collapsed_summary_renders_managed_memory_counts_after_non_memory_parts() {
        let mut message = group(1, 0, 0, 0, false);
        message.memory_read_count = 1;
        message.memory_search_count = 1;
        message.memory_write_count = 2;

        assert!(rendered_summary(message).contains(
            "Read 1 file, recalled 1 memory, searched memories, wrote 2 memories (ctrl+o to expand)"
        ));
    }

    #[test]
    fn collapsed_content_renders_pre_tool_use_hook_timing_below_summary() {
        let mut message = group(1, 0, 0, 0, false);
        message.hook_count = 2;
        message.hook_total_ms = Some(250);

        let canvas = element! {
            CollapsedReadSearchContent(
                message: Some(message),
                add_margin: false,
                verbose: false,
                is_transcript_mode: false,
            )
        }
        .render(None);
        let rendered = canvas.to_string();

        assert!(rendered.contains("Read 1 file"));
        assert!(rendered.contains("Ran 2 PreToolUse hooks (0.3s)"));
    }

    #[test]
    fn collapsed_content_renders_verbose_tool_use_and_result_entries() {
        let mut message = group(1, 0, 0, 0, false);
        message.verbose_entries = vec![
            CollapsedReadSearchEntry::ToolUse {
                tool_name: "Read".to_string(),
                input: Some(serde_json::json!({"file_path":"src/main.rs","offset":2,"limit":3})),
                tool_use_id: Some("toolu_read_collapsed".to_string()),
                description: "src/main.rs".to_string(),
                status: crate::types::message::ToolUseStatus::Succeeded,
            },
            CollapsedReadSearchEntry::ToolResult {
                tool_name: "Read".to_string(),
                status: crate::types::message::ToolResultStatus::Success,
                content: "2\tfn main() {}".to_string(),
                // No Read display shape; the raw rides the entry and the
                // by-tool-name renderer draws "Read 1 line" from it.
                tool_use_result: Some(serde_json::json!({
                    "type": "text",
                    "file": {
                        "filePath": "src/main.rs",
                        "content": "fn main() {}",
                        "numLines": 1,
                        "startLine": 2,
                        "totalLines": 4
                    }
                })),
            },
        ];

        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                CollapsedReadSearchContent(
                    message: Some(message),
                    add_margin: false,
                    verbose: true,
                    is_transcript_mode: false,
                    // Single-source rule: the expand switch (not verbose)
                    // opts into per-tool rows.
                    expand_by_default: true,
                )
            }
        }
        .render(None);
        let rendered = canvas.to_string();

        assert!(rendered.contains("Read("));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("lines 2-4"));
        assert!(rendered.contains("Read 1 line"));
        assert!(!rendered.contains("2→fn main"));
    }

    #[test]
    fn collapsed_content_verbose_renders_hook_details_and_ansi_relevant_memory() {
        let mut message = group(1, 0, 0, 0, false);
        message.hook_count = 1;
        message.hook_total_ms = Some(250);
        message.hook_infos = vec![crate::types::message::StopHookInfo {
            command: Some("echo hook".to_string()),
            prompt_text: None,
            duration_ms: Some(150),
            output: None,
            error: None,
            prevented_continuation: false,
        }];
        message.relevant_memories = vec![crate::types::message::RelevantMemory {
            header: None,
            limit: None,
            path: "/repo/CLAUDE.md".to_string(),
            content: "\x1b[31mhot\x1b[0m plain".to_string(),
            mtime_ms: None,
        }];

        let canvas = element! {
            CollapsedReadSearchContent(
                message: Some(message),
                add_margin: false,
                verbose: true,
                is_transcript_mode: false,
                // Single-source rule: the expand switch (not verbose)
                // opts into per-tool rows.
                expand_by_default: true,
            )
        }
        .render(None);
        let rendered = canvas.to_string();

        assert!(!rendered.contains("Read 1 file"));
        assert!(rendered.contains("Ran 1 PreToolUse hook (0.3s)"));
        assert!(rendered.contains("echo hook (0.1s)"));
        assert!(rendered.contains("Recalled CLAUDE.md"));
        assert!(rendered.contains("hot plain"));
        assert!((0..20).any(|x| {
            (0..10).any(|y| {
                canvas
                    .resolved_text_style(x, y)
                    .is_some_and(|style| style.color == Some(Color::DarkRed))
            })
        }));
    }

    #[test]
    fn verbose_true_without_expand_switch_renders_summary_row_not_per_tool_rows() {
        // Pins the single-source rule (user-authorized L2, v3 2026-08-01):
        // `verbose` alone must NOT expand the group into per-tool rows.
        let mut message = group(1, 0, 0, 0, false);
        message.verbose_entries = vec![CollapsedReadSearchEntry::ToolUse {
            tool_name: "Read".to_string(),
            input: Some(serde_json::json!({"file_path":"src/main.rs"})),
            tool_use_id: Some("toolu_read_pin".to_string()),
            description: "src/main.rs".to_string(),
            status: crate::types::message::ToolUseStatus::Succeeded,
        }];

        let rendered = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                CollapsedReadSearchContent(
                    message: Some(message),
                    add_margin: false,
                    verbose: true,
                    is_transcript_mode: false,
                    expand_by_default: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(
            rendered.contains("Read 1 file (ctrl+o to expand)"),
            "canvas=\n{rendered}"
        );
        assert!(!rendered.contains("Read("), "canvas=\n{rendered}");
    }

    #[test]
    fn collapsed_summary_renders_team_memory_counts_after_memory_parts() {
        let mut message = group(0, 0, 0, 0, false);
        message.memory_read_count = 1;
        message.team_memory_read_count = 2;
        message.team_memory_search_count = 1;
        message.team_memory_write_count = 1;

        assert!(rendered_summary(message).contains(
            "Recalled 1 memory, recalled 2 team memories, searched team memories, wrote 1 team memory (ctrl+o to expand)"
        ));
    }
}
