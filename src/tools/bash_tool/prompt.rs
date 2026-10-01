//! Maps to CC `tools/BashTool/prompt.ts`.

use super::tool_name::BASH_TOOL_NAME;

pub fn get_default_timeout_ms() -> u64 {
    crate::utils::timeouts::get_default_bash_timeout_ms()
}

pub fn get_max_timeout_ms() -> u64 {
    crate::utils::timeouts::get_max_bash_timeout_ms()
}

pub fn get_background_usage_note() -> Option<&'static str> {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
            .as_deref(),
    ) {
        return None;
    }
    Some(
        "You can use the `run_in_background` parameter to run the command in the background. Only use this if you don't need the result immediately and are OK being notified when the command completes later. You do not need to check the output right away - you'll be notified when it finishes. You do not need to use '&' at the end of the command when using this parameter.",
    )
}

/// Maps to CC `getCommitAndPRInstructions()` (`tools/BashTool/prompt.ts`:42).
fn get_commit_and_pr_instructions() -> String {
    // CC prepends undercover instructions for `USER_TYPE === 'ant' && isUndercover()`.
    // `utils/undercover.ts` is internal-only and unported; external builds always
    // have an empty undercover section.
    let undercover_section = String::new();

    if !crate::utils::git_settings::should_include_git_instructions() {
        return undercover_section;
    }

    // For ant users, CC uses the short version pointing to skills (:56-76).
    if crate::utils::build_profile::build_audience().is_internal() {
        let skills_section = if !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
        ) {
            "For git commits and pull requests, use the `/commit` and `/commit-push-pr` skills:\n- `/commit` - Create a git commit with staged changes\n- `/commit-push-pr` - Commit, push, and create a pull request\n\nThese skills handle git safety protocols, proper commit message formatting, and PR creation.\n\nBefore creating a pull request, run `/simplify` to review your changes, then test end-to-end (e.g. via `/tmux` for interactive features).\n\n"
        } else {
            ""
        };
        return format!(
            "{undercover_section}# Git operations\n\n{skills_section}IMPORTANT: NEVER skip hooks (--no-verify, --no-gpg-sign, etc) unless the user explicitly requests it.\n\nUse the gh command via the Bash tool for other GitHub-related tasks including working with issues, checks, and releases. If given a Github URL use the gh command to get the information needed.\n\n# Other common operations\n- View comments on a Github PR: gh api repos/foo/bar/pulls/123/comments"
        );
    }

    // For external users, include full inline instructions (:78-160).
    let texts = crate::utils::attribution::get_attribution_texts();
    let commit_attribution = texts.commit;
    let pr_attribution = texts.pr;

    let commit_message_suffix = if !commit_attribution.is_empty() {
        format!(" ending with:\n   {commit_attribution}")
    } else {
        ".".to_string()
    };
    let heredoc_attribution = if !commit_attribution.is_empty() {
        format!("\n\n   {commit_attribution}")
    } else {
        String::new()
    };
    let pr_body_attribution = if !pr_attribution.is_empty() {
        format!("\n\n{pr_attribution}")
    } else {
        String::new()
    };

    let todo_tool = crate::tools::todo_write_tool::constants::TODO_WRITE_TOOL_NAME;
    let agent_tool = crate::tools::agent_tool::constants::AGENT_TOOL_NAME;

    // Byte parity with CC `BashTool/prompt.ts:89`: the "...when given direct
    // instructions " line ends with a TRAILING SPACE in the source template.
    // The tool description is a prompt-cache prefix input, so editors must not
    // strip it.
    format!(
        r#"# Committing changes with git

Only create commits when requested by the user. If unclear, ask first. When the user asks you to create a new git commit, follow these steps carefully:

You can call multiple tools in a single response. When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel for optimal performance. The numbered steps below indicate which commands should be batched in parallel.

Git Safety Protocol:
- NEVER update the git config
- NEVER run destructive git commands (push --force, reset --hard, checkout ., restore ., clean -f, branch -D) unless the user explicitly requests these actions. Taking unauthorized destructive actions is unhelpful and can result in lost work, so it's best to ONLY run these commands when given direct instructions 
- NEVER skip hooks (--no-verify, --no-gpg-sign, etc) unless the user explicitly requests it
- NEVER run force push to main/master, warn the user if they request it
- CRITICAL: Always create NEW commits rather than amending, unless the user explicitly requests a git amend. When a pre-commit hook fails, the commit did NOT happen — so --amend would modify the PREVIOUS commit, which may result in destroying work or losing previous changes. Instead, after hook failure, fix the issue, re-stage, and create a NEW commit
- When staging files, prefer adding specific files by name rather than using "git add -A" or "git add .", which can accidentally include sensitive files (.env, credentials) or large binaries
- NEVER commit changes unless the user explicitly asks you to. It is VERY IMPORTANT to only commit when explicitly asked, otherwise the user will feel that you are being too proactive

1. Run the following bash commands in parallel, each using the {bash_tool} tool:
  - Run a git status command to see all untracked files. IMPORTANT: Never use the -uall flag as it can cause memory issues on large repos.
  - Run a git diff command to see both staged and unstaged changes that will be committed.
  - Run a git log command to see recent commit messages, so that you can follow this repository's commit message style.
2. Analyze all staged changes (both previously staged and newly added) and draft a commit message:
  - Summarize the nature of the changes (eg. new feature, enhancement to an existing feature, bug fix, refactoring, test, docs, etc.). Ensure the message accurately reflects the changes and their purpose (i.e. "add" means a wholly new feature, "update" means an enhancement to an existing feature, "fix" means a bug fix, etc.).
  - Do not commit files that likely contain secrets (.env, credentials.json, etc). Warn the user if they specifically request to commit those files
  - Draft a concise (1-2 sentences) commit message that focuses on the "why" rather than the "what"
  - Ensure it accurately reflects the changes and their purpose
3. Run the following commands in parallel:
   - Add relevant untracked files to the staging area.
   - Create the commit with a message{commit_message_suffix}
   - Run git status after the commit completes to verify success.
   Note: git status depends on the commit completing, so run it sequentially after the commit.
4. If the commit fails due to pre-commit hook: fix the issue and create a NEW commit

Important notes:
- NEVER run additional commands to read or explore code, besides git bash commands
- NEVER use the {todo_tool} or {agent_tool} tools
- DO NOT push to the remote repository unless the user explicitly asks you to do so
- IMPORTANT: Never use git commands with the -i flag (like git rebase -i or git add -i) since they require interactive input which is not supported.
- IMPORTANT: Do not use --no-edit with git rebase commands, as the --no-edit flag is not a valid option for git rebase.
- If there are no changes to commit (i.e., no untracked files and no modifications), do not create an empty commit
- In order to ensure good formatting, ALWAYS pass the commit message via a HEREDOC, a la this example:
<example>
git commit -m "$(cat <<'EOF'
   Commit message here.{heredoc_attribution}
   EOF
   )"
</example>

# Creating pull requests
Use the gh command via the Bash tool for ALL GitHub-related tasks including working with issues, pull requests, checks, and releases. If given a Github URL use the gh command to get the information needed.

IMPORTANT: When the user asks you to create a pull request, follow these steps carefully:

1. Run the following bash commands in parallel using the {bash_tool} tool, in order to understand the current state of the branch since it diverged from the main branch:
   - Run a git status command to see all untracked files (never use -uall flag)
   - Run a git diff command to see both staged and unstaged changes that will be committed
   - Check if the current branch tracks a remote branch and is up to date with the remote, so you know if you need to push to the remote
   - Run a git log command and `git diff [base-branch]...HEAD` to understand the full commit history for the current branch (from the time it diverged from the base branch)
2. Analyze all changes that will be included in the pull request, making sure to look at all relevant commits (NOT just the latest commit, but ALL commits that will be included in the pull request!!!), and draft a pull request title and summary:
   - Keep the PR title short (under 70 characters)
   - Use the description/body for details, not the title
3. Run the following commands in parallel:
   - Create new branch if needed
   - Push to remote with -u flag if needed
   - Create PR using gh pr create with the format below. Use a HEREDOC to pass the body to ensure correct formatting.
<example>
gh pr create --title "the pr title" --body "$(cat <<'EOF'
## Summary
<1-3 bullet points>

## Test plan
[Bulleted markdown checklist of TODOs for testing the pull request...]{pr_body_attribution}
EOF
)"
</example>

Important:
- DO NOT use the {todo_tool} or {agent_tool} tools
- Return the PR URL when you're done, so the user can see it

# Other common operations
- View comments on a Github PR: gh api repos/foo/bar/pulls/123/comments"#,
        bash_tool = BASH_TOOL_NAME,
    )
}

/// CC `[...new Set(arr)]` — order-preserving dedup used by the sandbox section.
fn dedup_preserve_order(items: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .filter(|item| seen.insert((*item).clone()))
        .cloned()
        .collect()
}

fn json_string_array(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// Maps to CC `getSimpleSandboxSection()` (`tools/BashTool/prompt.ts`:172).
///
/// JSON key order follows CC's object literal insertion order, so the two JSON
/// snippets are built by hand instead of via a map type. CC distinguishes
/// `undefined` (key omitted by JSON.stringify) from `[]`; the Rust runtime
/// config only carries `Vec`s, so empty optional lists omit their key — the
/// non-empty rendering is identical.
fn get_simple_sandbox_section() -> String {
    use crate::utils::sandbox::sandbox_adapter;

    if !sandbox_adapter::is_sandboxing_enabled() {
        return String::new();
    }

    let settings = crate::utils::settings::load_settings_from_disk().settings;
    let config = sandbox_adapter::convert_to_sandbox_runtime_config(&settings);
    let allow_unsandboxed_commands = sandbox_adapter::are_unsandboxed_commands_allowed(&settings);

    // Replace the per-UID temp dir literal (e.g. /private/tmp/claude-1001/) with
    // "$TMPDIR" so the prompt is identical across users — avoids busting the
    // cross-user global prompt cache. The sandbox already sets $TMPDIR at runtime.
    let claude_temp_dir = crate::utils::permissions::filesystem::get_claude_temp_dir()
        .display()
        .to_string();
    let normalized_allow_only: Vec<String> = dedup_preserve_order(&config.filesystem.allow_write)
        .into_iter()
        .map(|path| {
            if path == claude_temp_dir {
                "$TMPDIR".to_string()
            } else {
                path
            }
        })
        .collect();

    let mut read_json = format!(
        "{{\"denyOnly\":{}",
        json_string_array(&dedup_preserve_order(&config.filesystem.deny_read))
    );
    if !config.filesystem.allow_read.is_empty() {
        read_json.push_str(&format!(
            ",\"allowWithinDeny\":{}",
            json_string_array(&dedup_preserve_order(&config.filesystem.allow_read))
        ));
    }
    read_json.push('}');
    let mut write_json = format!(
        "{{\"allowOnly\":{}",
        json_string_array(&normalized_allow_only)
    );
    if !config.filesystem.deny_write.is_empty() {
        write_json.push_str(&format!(
            ",\"denyWithinAllow\":{}",
            json_string_array(&dedup_preserve_order(&config.filesystem.deny_write))
        ));
    }
    write_json.push('}');
    let filesystem_json = format!("{{\"read\":{read_json},\"write\":{write_json}}}");

    let mut network_parts: Vec<String> = Vec::new();
    if !config.network.allowed_domains.is_empty() {
        network_parts.push(format!(
            "\"allowedHosts\":{}",
            json_string_array(&dedup_preserve_order(&config.network.allowed_domains))
        ));
    }
    if !config.network.denied_domains.is_empty() {
        network_parts.push(format!(
            "\"deniedHosts\":{}",
            json_string_array(&dedup_preserve_order(&config.network.denied_domains))
        ));
    }
    if let Some(allow_unix_sockets) = &config.network.allow_unix_sockets {
        network_parts.push(format!(
            "\"allowUnixSockets\":{}",
            json_string_array(&dedup_preserve_order(allow_unix_sockets))
        ));
    }

    let mut restrictions_lines: Vec<String> = vec![format!("Filesystem: {filesystem_json}")];
    if !network_parts.is_empty() {
        restrictions_lines.push(format!("Network: {{{}}}", network_parts.join(",")));
    }
    if let Some(ignore_violations) = &config.ignore_violations {
        restrictions_lines.push(format!(
            "Ignored violations: {}",
            serde_json::to_string(ignore_violations).unwrap_or_else(|_| "null".to_string())
        ));
    }

    use crate::constants::prompts::{PromptItem, prepend_bullets};
    let mut items: Vec<PromptItem> = Vec::new();
    if allow_unsandboxed_commands {
        items.push(PromptItem::Text("You should always default to running commands within the sandbox. Do NOT attempt to set `dangerouslyDisableSandbox: true` unless:".to_string()));
        items.push(PromptItem::Subitems(vec![
            "The user *explicitly* asks you to bypass sandbox".to_string(),
            "A specific command just failed and you see evidence of sandbox restrictions causing the failure. Note that commands can fail for many reasons unrelated to the sandbox (missing files, wrong arguments, network issues, etc.).".to_string(),
        ]));
        items.push(PromptItem::Text(
            "Evidence of sandbox-caused failures includes:".to_string(),
        ));
        items.push(PromptItem::Subitems(vec![
            "\"Operation not permitted\" errors for file/network operations".to_string(),
            "Access denied to specific paths outside allowed directories".to_string(),
            "Network connection failures to non-whitelisted hosts".to_string(),
            "Unix socket connection errors".to_string(),
        ]));
        items.push(PromptItem::Text(
            "When you see evidence of sandbox-caused failure:".to_string(),
        ));
        items.push(PromptItem::Subitems(vec![
            "Immediately retry with `dangerouslyDisableSandbox: true` (don't ask, just do it)".to_string(),
            "Briefly explain what sandbox restriction likely caused the failure. Be sure to mention that the user can use the `/sandbox` command to manage restrictions.".to_string(),
            "This will prompt the user for permission".to_string(),
        ]));
        items.push(PromptItem::Text("Treat each command you execute with `dangerouslyDisableSandbox: true` individually. Even if you have recently run a command with this setting, you should default to running future commands within the sandbox.".to_string()));
        items.push(PromptItem::Text("Do not suggest adding sensitive paths like ~/.bashrc, ~/.zshrc, ~/.ssh/*, or credential files to the sandbox allowlist.".to_string()));
    } else {
        items.push(PromptItem::Text("All commands MUST run in sandbox mode - the `dangerouslyDisableSandbox` parameter is disabled by policy.".to_string()));
        items.push(PromptItem::Text(
            "Commands cannot run outside the sandbox under any circumstances.".to_string(),
        ));
        items.push(PromptItem::Text("If a command fails due to sandbox restrictions, work with the user to adjust sandbox settings instead.".to_string()));
    }
    items.push(PromptItem::Text("For temporary files, always use the `$TMPDIR` environment variable. TMPDIR is automatically set to the correct sandbox-writable directory in sandbox mode. Do NOT use `/tmp` directly - use `$TMPDIR` instead.".to_string()));

    [
        String::new(),
        "## Command sandbox".to_string(),
        "By default, your command will be run in a sandbox. This sandbox controls which directories and network hosts commands may access or modify without an explicit override.".to_string(),
        String::new(),
        "The sandbox has the following restrictions:".to_string(),
        restrictions_lines.join("\n"),
        String::new(),
        prepend_bullets(&items),
    ]
    .join("\n")
}

/// Maps to CC `getSimplePrompt()` (`tools/BashTool/prompt.ts`:275).
pub fn get_simple_prompt() -> String {
    use crate::constants::prompts::{PromptItem, prepend_bullets};
    use crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME;
    use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
    use crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME;
    use crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
    use crate::tools::grep_tool::prompt::GREP_TOOL_NAME;

    // Ant-native builds alias find/grep to embedded bfs/ugrep in Claude's shell,
    // so we don't steer away from them (and Glob/Grep tools are removed).
    let embedded = crate::utils::embedded_tools::has_embedded_search_tools();

    let mut tool_preference_items: Vec<PromptItem> = Vec::new();
    if !embedded {
        tool_preference_items.push(PromptItem::Text(format!(
            "File search: Use {GLOB_TOOL_NAME} (NOT find or ls)"
        )));
        tool_preference_items.push(PromptItem::Text(format!(
            "Content search: Use {GREP_TOOL_NAME} (NOT grep or rg)"
        )));
    }
    tool_preference_items.push(PromptItem::Text(format!(
        "Read files: Use {FILE_READ_TOOL_NAME} (NOT cat/head/tail)"
    )));
    tool_preference_items.push(PromptItem::Text(format!(
        "Edit files: Use {FILE_EDIT_TOOL_NAME} (NOT sed/awk)"
    )));
    tool_preference_items.push(PromptItem::Text(format!(
        "Write files: Use {FILE_WRITE_TOOL_NAME} (NOT echo >/cat <<EOF)"
    )));
    tool_preference_items.push(PromptItem::Text(
        "Communication: Output text directly (NOT echo/printf)".to_string(),
    ));

    let avoid_commands = if embedded {
        "`cat`, `head`, `tail`, `sed`, `awk`, or `echo`"
    } else {
        "`find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo`"
    };

    let multiple_commands_subitems = vec![
        format!("If the commands are independent and can run in parallel, make multiple {BASH_TOOL_NAME} tool calls in a single message. Example: if you need to run \"git status\" and \"git diff\", send a single message with two {BASH_TOOL_NAME} tool calls in parallel."),
        format!("If the commands depend on each other and must run sequentially, use a single {BASH_TOOL_NAME} call with '&&' to chain them together."),
        "Use ';' only when you need to run commands sequentially but don't care if earlier commands fail.".to_string(),
        "DO NOT use newlines to separate commands (newlines are ok in quoted strings).".to_string(),
    ];

    let git_subitems = vec![
        "Prefer to create a new commit rather than amending an existing commit.".to_string(),
        "Before running destructive operations (e.g., git reset --hard, git push --force, git checkout --), consider whether there is a safer alternative that achieves the same goal. Only use destructive operations when they are truly the best approach.".to_string(),
        "Never skip hooks (--no-verify) or bypass signing (--no-gpg-sign, -c commit.gpgsign=false) unless the user has explicitly asked for it. If a hook fails, investigate and fix the underlying issue.".to_string(),
    ];

    // Non-MONITOR_TOOL branch (MONITOR_TOOL is an unported ant feature gate).
    let sleep_subitems = vec![
        "Do not sleep between commands that can run immediately — just run them.".to_string(),
        "If your command is long running and you would like to be notified when it finishes — use `run_in_background`. No sleep needed.".to_string(),
        "Do not retry failing commands in a sleep loop — diagnose the root cause.".to_string(),
        "If waiting for a background task you started with `run_in_background`, you will be notified when it completes — do not poll.".to_string(),
        "If you must poll an external process, use a check command (e.g. `gh run view`) rather than sleeping first.".to_string(),
        "If you must sleep, keep the duration short (1-5 seconds) to avoid blocking the user.".to_string(),
    ];

    let max_ms = get_max_timeout_ms();
    let default_ms = get_default_timeout_ms();

    let mut instruction_items: Vec<PromptItem> = vec![
        PromptItem::Text("If your command will create new directories or files, first use this tool to run `ls` to verify the parent directory exists and is the correct location.".to_string()),
        PromptItem::Text("Always quote file paths that contain spaces with double quotes in your command (e.g., cd \"path with spaces/file.txt\")".to_string()),
        PromptItem::Text("Try to maintain your current working directory throughout the session by using absolute paths and avoiding usage of `cd`. You may use `cd` if the User explicitly requests it.".to_string()),
        PromptItem::Text(format!(
            "You may specify an optional timeout in milliseconds (up to {max_ms}ms / {} minutes). By default, your command will timeout after {default_ms}ms ({} minutes).",
            max_ms / 60000,
            default_ms / 60000
        )),
    ];
    if let Some(note) = get_background_usage_note() {
        instruction_items.push(PromptItem::Text(note.to_string()));
    }
    instruction_items.push(PromptItem::Text(
        "When issuing multiple commands:".to_string(),
    ));
    instruction_items.push(PromptItem::Subitems(multiple_commands_subitems));
    instruction_items.push(PromptItem::Text("For git commands:".to_string()));
    instruction_items.push(PromptItem::Subitems(git_subitems));
    instruction_items.push(PromptItem::Text(
        "Avoid unnecessary `sleep` commands:".to_string(),
    ));
    instruction_items.push(PromptItem::Subitems(sleep_subitems));
    if embedded {
        instruction_items.push(PromptItem::Text(
            "When using `find -regex` with alternation, put the longest alternative first. Example: use `'.*\\.\\(tsx\\|ts\\)'` not `'.*\\.\\(ts\\|tsx\\)'` — the second form silently skips `.tsx` files.".to_string(),
        ));
    }

    let mut lines = vec![
        "Executes a given bash command and returns its output.".to_string(),
        String::new(),
        "The working directory persists between commands, but shell state does not. The shell environment is initialized from the user's profile (bash or zsh).".to_string(),
        String::new(),
        format!("IMPORTANT: Avoid using this tool to run {avoid_commands} commands, unless explicitly instructed or after you have verified that a dedicated tool cannot accomplish your task. Instead, use the appropriate dedicated tool as this will provide a much better experience for the user:"),
        String::new(),
        prepend_bullets(&tool_preference_items),
        format!("While the {BASH_TOOL_NAME} tool can do similar things, it’s better to use the built-in tools as they provide a better user experience and make it easier to review tool calls and give permission."),
        String::new(),
        "# Instructions".to_string(),
        prepend_bullets(&instruction_items),
    ];
    // CC appends the sandbox section unconditionally (empty string when
    // sandboxing is off) and the commit/PR section only when non-empty.
    lines.push(get_simple_sandbox_section());
    let commit_and_pr = get_commit_and_pr_instructions();
    if !commit_and_pr.is_empty() {
        lines.push(String::new());
        lines.push(commit_and_pr);
    }
    lines.join("\n")
}
