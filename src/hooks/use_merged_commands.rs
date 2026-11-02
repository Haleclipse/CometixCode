//! Maps to: CC `hooks/useMergedCommands.ts`.

use crate::commands::Command;
use iocraft::hooks::UseMemo;
use iocraft::prelude::Hooks;
use std::collections::HashSet;
use std::sync::Arc;

fn merge_commands(
    initial_commands: Arc<Vec<Command>>,
    mcp_commands: &[Command],
) -> Arc<Vec<Command>> {
    if mcp_commands.is_empty() {
        return initial_commands;
    }

    let mut merged = Vec::with_capacity(initial_commands.len() + mcp_commands.len());
    let mut names = HashSet::with_capacity(merged.capacity());
    for command in initial_commands.iter().chain(mcp_commands.iter()) {
        if names.insert(command.name.to_string()) {
            merged.push(command.clone());
        }
    }
    Arc::new(merged)
}

/// Maps to CC `useMergedCommands(initialCommands, mcpCommands)`.
///
/// The initial/local registry wins duplicate names, preserving the stable
/// local prefix while MCP commands are appended as a live suffix.
/// L1: retained Arc arrays carry the source useMemo array identities; command
/// body/options changes must not be reduced to a subset of metadata fields.
pub fn use_merged_commands(
    hooks: &mut Hooks,
    initial_commands: Arc<Vec<Command>>,
    mcp_commands: Arc<Vec<Command>>,
) -> Arc<Vec<Command>> {
    let initial_identity = Arc::as_ptr(&initial_commands) as usize;
    let mcp_identity = Arc::as_ptr(&mcp_commands) as usize;
    let (_, _, merged) = hooks.use_memo(
        move || {
            let merged = merge_commands(initial_commands.clone(), &mcp_commands);
            // iocraft hashes dependencies without retaining them. Keep both
            // source arrays in the memo VALUE so their addresses cannot be
            // reused while this cached merge is still current.
            (initial_commands, mcp_commands, merged)
        },
        (initial_identity, mcp_identity),
    );
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use iocraft::prelude::*;

    #[component]
    fn RefreshedCommandBody(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let initial = hooks.use_const(|| Arc::new(Vec::new()));
        let versions = hooks.use_const(|| {
            let mut old = crate::commands::declared_commands_for_tests()
                .into_iter()
                .find(|command| command.name == "init")
                .unwrap();
            old.name = "plugin:body".into();
            old.description = "Same description".into();
            old.source = crate::commands::CommandSource::Plugin;
            old.get_prompt_for_command = Some(|_, _, _| {
                Ok(vec![crate::types::message::UserContent::Text(
                    "old body".into(),
                )])
            });
            let mut new = old.clone();
            new.get_prompt_for_command = Some(|_, _, _| {
                Ok(vec![crate::types::message::UserContent::Text(
                    "new body".into(),
                )])
            });
            // Even value equality considers these callback-bearing commands
            // equal. Source array identity must still replace the executable.
            assert_eq!(old, new);
            [Arc::new(vec![old]), Arc::new(vec![new])]
        });
        let mut version = hooks.use_state(|| 0usize);
        let merged = use_merged_commands(&mut hooks, initial, versions[version.get()].clone());
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(event) = event
                && event.code == KeyCode::F(2)
            {
                version.set(1);
            }
        });
        let body = merged[0].get_prompt_for_command.unwrap()(
            &merged[0],
            "",
            &crate::tool::ToolUseContext::default(),
        )
        .unwrap();
        element!(Text(content: format!("{body:?}")))
    }

    #[test]
    fn merged_commands_matches_official_new_array_identity_when_body_changes() {
        let canvases = futures::executor::block_on(async {
            let events = stream::once(async {
                futures_timer::Delay::new(std::time::Duration::from_millis(25)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::F(2)))
            });
            let mut app = element!(RefreshedCommandBody);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(50, 4),
            ));
            let mut canvases = Vec::new();
            while let Some(canvas) = crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_secs(2)).await;
                None
            })
            .await
            {
                canvases.push(canvas.to_string());
                if canvases.last().unwrap().contains("new body") {
                    break;
                }
            }
            canvases
        });
        assert!(
            canvases.iter().any(|text| text.contains("old body")),
            "{canvases:?}"
        );
        assert!(
            canvases.iter().any(|text| text.contains("new body")),
            "{canvases:?}"
        );
    }

    #[test]
    fn merge_order_and_local_name_precedence_match_official_uniq_by() {
        let local = crate::commands::declared_commands_for_tests();
        let duplicate = local[0].clone();
        let dynamic = crate::services::mcp::client::mcp_command_for_test(
            "docs",
            "summarize",
            Some("Summarize docs"),
            &[],
        );

        let merged = merge_commands(Arc::new(local.clone()), &[duplicate, dynamic]);
        let expected_local_names = local
            .iter()
            .map(|command| command.name.as_ref())
            .collect::<HashSet<_>>();

        assert_eq!(merged.len(), expected_local_names.len() + 1);
        for name in expected_local_names {
            let original = local
                .iter()
                .find(|command| command.name.as_ref() == name)
                .unwrap();
            let actual = merged
                .iter()
                .find(|command| command.name.as_ref() == name)
                .unwrap();
            assert_eq!(actual, original, "local command must win duplicate name");
        }
        assert_eq!(merged.last().unwrap().name, "mcp__docs__summarize");
    }
}
