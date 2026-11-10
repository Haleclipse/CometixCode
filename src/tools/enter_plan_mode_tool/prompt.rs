//! Maps to CC `tools/EnterPlanModeTool/prompt.ts`.

use crate::tools::ask_user_question_tool::prompt::ASK_USER_QUESTION_TOOL_NAME;

/// Maps to: CC `prompt.ts:4-14` `WHAT_HAPPENS_SECTION`. Omitted when the
/// interview phase is enabled, because the detailed workflow then arrives via
/// the `plan_mode` attachment (`messages.ts`).
fn what_happens_section() -> String {
    format!(
        "## What Happens in Plan Mode\n\nIn plan mode, you'll:\n1. Thoroughly explore the codebase using Glob, Grep, and Read tools\n2. Understand existing patterns and architecture\n3. Design an implementation approach\n4. Present your plan to the user for approval\n5. Use {ASK_USER_QUESTION_TOOL_NAME} if you need to clarify approaches\n6. Exit plan mode with ExitPlanMode when ready to implement\n\n"
    )
}

fn what_happens() -> String {
    if crate::utils::plan_mode_v2::is_plan_mode_interview_phase_enabled() {
        String::new()
    } else {
        what_happens_section()
    }
}

/// Maps to: CC `prompt.ts:16-99` `getEnterPlanModeToolPromptExternal`, the
/// branch `getEnterPlanModeToolPrompt` (`prompt.ts:166-170`) selects.
pub fn get_enter_plan_mode_tool_prompt() -> String {
    let what_happens = what_happens();
    format!(
        r#"Use this tool proactively when you're about to start a non-trivial implementation task. Getting user sign-off on your approach before writing code prevents wasted effort and ensures alignment. This tool transitions you into plan mode where you can explore the codebase and design an implementation approach for user approval.

## When to Use This Tool

**Prefer using EnterPlanMode** for implementation tasks unless they're simple. Use it when ANY of these conditions apply:

1. **New Feature Implementation**: Adding meaningful new functionality
   - Example: "Add a logout button" - where should it go? What should happen on click?
   - Example: "Add form validation" - what rules? What error messages?

2. **Multiple Valid Approaches**: The task can be solved in several different ways
   - Example: "Add caching to the API" - could use Redis, in-memory, file-based, etc.
   - Example: "Improve performance" - many optimization strategies possible

3. **Code Modifications**: Changes that affect existing behavior or structure
   - Example: "Update the login flow" - what exactly should change?
   - Example: "Refactor this component" - what's the target architecture?

4. **Architectural Decisions**: The task requires choosing between patterns or technologies
   - Example: "Add real-time updates" - WebSockets vs SSE vs polling
   - Example: "Implement state management" - Redux vs Context vs custom solution

5. **Multi-File Changes**: The task will likely touch more than 2-3 files
   - Example: "Refactor the authentication system"
   - Example: "Add a new API endpoint with tests"

6. **Unclear Requirements**: You need to explore before understanding the full scope
   - Example: "Make the app faster" - need to profile and identify bottlenecks
   - Example: "Fix the bug in checkout" - need to investigate root cause

7. **User Preferences Matter**: The implementation could reasonably go multiple ways
   - If you would use {ASK_USER_QUESTION_TOOL_NAME} to clarify the approach, use EnterPlanMode instead
   - Plan mode lets you explore first, then present options with context

## When NOT to Use This Tool

Only skip EnterPlanMode for simple tasks:
- Single-line or few-line fixes (typos, obvious bugs, small tweaks)
- Adding a single function with clear requirements
- Tasks where the user has given very specific, detailed instructions
- Pure research/exploration tasks (use the Agent tool with explore agent instead)

{what_happens}## Examples

### GOOD - Use EnterPlanMode:
User: "Add user authentication to the app"
- Requires architectural decisions (session vs JWT, where to store tokens, middleware structure)

User: "Optimize the database queries"
- Multiple approaches possible, need to profile first, significant impact

User: "Implement dark mode"
- Architectural decision on theme system, affects many components

User: "Add a delete button to the user profile"
- Seems simple but involves: where to place it, confirmation dialog, API call, error handling, state updates

User: "Update the error handling in the API"
- Affects multiple files, user should approve the approach

### BAD - Don't use EnterPlanMode:
User: "Fix the typo in the README"
- Straightforward, no planning needed

User: "Add a console.log to debug this function"
- Simple, obvious implementation

User: "What files handle routing?"
- Research task, not implementation planning

## Important Notes

- This tool REQUIRES user approval - they must consent to entering plan mode
- If unsure whether to use it, err on the side of planning - it's better to get alignment upfront than to redo work
- Users appreciate being consulted before significant changes are made to their codebase
"#
    )
}
