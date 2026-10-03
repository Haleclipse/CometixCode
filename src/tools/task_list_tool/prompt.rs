//! Maps to CC `tools/TaskListTool/constants.ts` and `prompt.ts`.

pub const TASK_LIST_TOOL_NAME: &str = "TaskList";
pub const DESCRIPTION: &str = "List all tasks in the task list";

/// Maps to: CC `TaskListTool/prompt.ts:5-49` `getPrompt()` — the teammate
/// use-case line and the "## Teammate Workflow" section only appear when
/// agent swarms are enabled. (CC's `idDescription` branch has identical arms
/// on both sides, :11-13 — carried as the single line here.)
pub fn get_prompt() -> String {
    let swarms_enabled = crate::utils::agent_swarms_enabled::is_agent_swarms_enabled();
    let teammate_use_case = if swarms_enabled {
        "- Before assigning tasks to teammates, to see what's available\n"
    } else {
        ""
    };
    let teammate_workflow = if swarms_enabled {
        "\n## Teammate Workflow\n\nWhen working as a teammate:\n1. After completing your current task, call TaskList to find available work\n2. Look for tasks with status 'pending', no owner, and empty blockedBy\n3. **Prefer tasks in ID order** (lowest ID first) when multiple tasks are available, as earlier tasks often set up context for later ones\n4. Claim an available task using TaskUpdate (set `owner` to your name), or wait for leader assignment\n5. If blocked, focus on unblocking tasks or notify the team lead\n"
    } else {
        ""
    };

    format!(
        r#"Use this tool to list all tasks in the task list.

## When to Use This Tool

- To see what tasks are available to work on (status: 'pending', no owner, not blocked)
- To check overall progress on the project
- To find tasks that are blocked and need dependencies resolved
{teammate_use_case}- After completing a task, to check for newly unblocked work or claim the next available task
- **Prefer working on tasks in ID order** (lowest ID first) when multiple tasks are available, as earlier tasks often set up context for later ones

## Output

Returns a summary of each task:
- **id**: Task identifier (use with TaskGet, TaskUpdate)
- **subject**: Brief description of the task
- **status**: 'pending', 'in_progress', or 'completed'
- **owner**: Agent ID if assigned, empty if available
- **blockedBy**: List of open task IDs that must be resolved first (tasks with blockedBy cannot be claimed until dependencies resolve)

Use TaskGet with a specific task ID to view full details including description and comments.
{teammate_workflow}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Maps to: CC `TaskListTool/prompt.ts:5-49` — the teammate line and the
    /// "## Teammate Workflow" section only exist under agent swarms.
    ///
    /// The swarms gate is ant-always-on (`agentSwarmsEnabled.ts:26`, a
    /// build-time `--define`), so the "off" half only exists on the external
    /// build; the internal build's prompt carries the teammate sections
    /// regardless of the env opt-in. This test used to assert the off shape
    /// unconditionally, which failed under `--features anthropic_internal`.
    #[test]
    fn task_list_prompt_matches_official_swarms_branches() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();

        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
        let no_opt_in = get_prompt();
        if crate::utils::build_profile::build_audience().is_internal() {
            assert!(no_opt_in.contains("Before assigning tasks to teammates"));
            assert!(no_opt_in.contains("## Teammate Workflow"));
        } else {
            assert!(!no_opt_in.contains("Before assigning tasks to teammates"));
            assert!(!no_opt_in.contains("## Teammate Workflow"));
            assert!(no_opt_in.ends_with("description and comments.\n"));
        }
        assert!(no_opt_in.contains(
            "must be resolved first (tasks with blockedBy cannot be claimed until dependencies resolve)"
        ));

        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        let on = get_prompt();
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
        assert!(on.contains("- Before assigning tasks to teammates, to see what's available\n- After completing a task,"));
        assert!(on.contains("## Teammate Workflow"));
        assert!(on.ends_with("5. If blocked, focus on unblocking tasks or notify the team lead\n"));
    }
}
