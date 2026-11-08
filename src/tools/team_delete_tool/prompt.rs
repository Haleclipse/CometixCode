//! Maps to CC `tools/TeamDeleteTool/constants.ts` and `prompt.ts`.

pub const TEAM_DELETE_TOOL_NAME: &str = "TeamDelete";

/// Maps to: CC `TeamDeleteTool.ts:50-52` `description()`.
pub const DESCRIPTION: &str = "Clean up team and task directories when the swarm is complete";

/// Maps to: CC `TeamDeleteTool/prompt.ts` `getPrompt()`.
pub fn get_prompt() -> String {
    r#"
# TeamDelete

Remove team and task directories when the swarm work is complete.

This operation:
- Removes the team directory (`~/.nyan/teams/{team-name}/`)
- Removes the task directory (`~/.nyan/tasks/{team-name}/`)
- Clears team context from the current session

**IMPORTANT**: TeamDelete will fail if the team still has active members. Gracefully terminate teammates first, then call TeamDelete after all teammates have shut down.

Use this when all teammates have finished their work and you want to clean up the team resources. The team name is automatically determined from the current session's team context.
"#
    .trim()
    .to_string()
}
