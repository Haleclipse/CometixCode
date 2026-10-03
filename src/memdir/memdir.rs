//! Auto-memory prompt construction.
//!
//! Maps to CC `memdir/memdir.ts`.

use std::path::Path;

use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

pub const ENTRYPOINT_NAME: &str = "MEMORY.md";
pub const MAX_ENTRYPOINT_LINES: usize = 200;
pub const MAX_ENTRYPOINT_BYTES: usize = 25_000;
const AUTO_MEM_DISPLAY_NAME: &str = "auto memory";

/// Maps to CC `memdir/memdir.ts` `EntrypointTruncation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrypointTruncation {
    pub content: String,
    pub line_count: usize,
    pub byte_count: usize,
    pub was_line_truncated: bool,
    pub was_byte_truncated: bool,
}

/// Maps to CC `memdir/memdir.ts` `DIR_EXISTS_GUIDANCE`.
pub const DIR_EXISTS_GUIDANCE: &str = "This directory already exists — write to it directly with the Write tool (do not run mkdir or check for its existence).";

const MEMORY_FRONTMATTER_EXAMPLE: &[&str] = &[
    "```markdown",
    "---",
    "name: {{memory name}}",
    "description: {{one-line description — used to decide relevance in future conversations, so be specific}}",
    "type: {{user, feedback, project, reference}}",
    "---",
    "",
    "{{memory content — for feedback/project types, structure as: rule/fact, then **Why:** and **How to apply:** lines}}",
    "```",
];

const TYPES_SECTION_INDIVIDUAL: &[&str] = &[
    "## Types of memory",
    "",
    "There are several discrete types of memory that you can store in your memory system:",
    "",
    "<types>",
    "<type>",
    "    <name>user</name>",
    "    <description>Contain information about the user's role, goals, responsibilities, and knowledge. Great user memories help you tailor your future behavior to the user's preferences and perspective. Your goal in reading and writing these memories is to build up an understanding of who the user is and how you can be most helpful to them specifically. For example, you should collaborate with a senior software engineer differently than a student who is coding for the very first time. Keep in mind, that the aim here is to be helpful to the user. Avoid writing memories about the user that could be viewed as a negative judgement or that are not relevant to the work you're trying to accomplish together.</description>",
    "    <when_to_save>When you learn any details about the user's role, preferences, responsibilities, or knowledge</when_to_save>",
    "    <how_to_use>When your work should be informed by the user's profile or perspective. For example, if the user is asking you to explain a part of the code, you should answer that question in a way that is tailored to the specific details that they will find most valuable or that helps them build their mental model in relation to domain knowledge they already have.</how_to_use>",
    "    <examples>",
    "    user: I'm a data scientist investigating what logging we have in place",
    "    assistant: [saves user memory: user is a data scientist, currently focused on observability/logging]",
    "",
    "    user: I've been writing Go for ten years but this is my first time touching the React side of this repo",
    "    assistant: [saves user memory: deep Go expertise, new to React and this project's frontend — frame frontend explanations in terms of backend analogues]",
    "    </examples>",
    "</type>",
    "<type>",
    "    <name>feedback</name>",
    "    <description>Guidance the user has given you about how to approach work — both what to avoid and what to keep doing. These are a very important type of memory to read and write as they allow you to remain coherent and responsive to the way you should approach work in the project. Record from failure AND success: if you only save corrections, you will avoid past mistakes but drift away from approaches the user has already validated, and may grow overly cautious.</description>",
    "    <when_to_save>Any time the user corrects your approach (\"no not that\", \"don't\", \"stop doing X\") OR confirms a non-obvious approach worked (\"yes exactly\", \"perfect, keep doing that\", accepting an unusual choice without pushback). Corrections are easy to notice; confirmations are quieter — watch for them. In both cases, save what is applicable to future conversations, especially if surprising or not obvious from the code. Include *why* so you can judge edge cases later.</when_to_save>",
    "    <how_to_use>Let these memories guide your behavior so that the user does not need to offer the same guidance twice.</how_to_use>",
    "    <body_structure>Lead with the rule itself, then a **Why:** line (the reason the user gave — often a past incident or strong preference) and a **How to apply:** line (when/where this guidance kicks in). Knowing *why* lets you judge edge cases instead of blindly following the rule.</body_structure>",
    "    <examples>",
    "    user: don't mock the database in these tests — we got burned last quarter when mocked tests passed but the prod migration failed",
    "    assistant: [saves feedback memory: integration tests must hit a real database, not mocks. Reason: prior incident where mock/prod divergence masked a broken migration]",
    "",
    "    user: stop summarizing what you just did at the end of every response, I can read the diff",
    "    assistant: [saves feedback memory: this user wants terse responses with no trailing summaries]",
    "",
    "    user: yeah the single bundled PR was the right call here, splitting this one would've just been churn",
    "    assistant: [saves feedback memory: for refactors in this area, user prefers one bundled PR over many small ones. Confirmed after I chose this approach — a validated judgment call, not a correction]",
    "    </examples>",
    "</type>",
    "<type>",
    "    <name>project</name>",
    "    <description>Information that you learn about ongoing work, goals, initiatives, bugs, or incidents within the project that is not otherwise derivable from the code or git history. Project memories help you understand the broader context and motivation behind the work the user is doing within this working directory.</description>",
    "    <when_to_save>When you learn who is doing what, why, or by when. These states change relatively quickly so try to keep your understanding of this up to date. Always convert relative dates in user messages to absolute dates when saving (e.g., \"Thursday\" → \"2026-03-05\"), so the memory remains interpretable after time passes.</when_to_save>",
    "    <how_to_use>Use these memories to more fully understand the details and nuance behind the user's request and make better informed suggestions.</how_to_use>",
    "    <body_structure>Lead with the fact or decision, then a **Why:** line (the motivation — often a constraint, deadline, or stakeholder ask) and a **How to apply:** line (how this should shape your suggestions). Project memories decay fast, so the why helps future-you judge whether the memory is still load-bearing.</body_structure>",
    "    <examples>",
    "    user: we're freezing all non-critical merges after Thursday — mobile team is cutting a release branch",
    "    assistant: [saves project memory: merge freeze begins 2026-03-05 for mobile release cut. Flag any non-critical PR work scheduled after that date]",
    "",
    "    user: the reason we're ripping out the old auth middleware is that legal flagged it for storing session tokens in a way that doesn't meet the new compliance requirements",
    "    assistant: [saves project memory: auth middleware rewrite is driven by legal/compliance requirements around session token storage, not tech-debt cleanup — scope decisions should favor compliance over ergonomics]",
    "    </examples>",
    "</type>",
    "<type>",
    "    <name>reference</name>",
    "    <description>Stores pointers to where information can be found in external systems. These memories allow you to remember where to look to find up-to-date information outside of the project directory.</description>",
    "    <when_to_save>When you learn about resources in external systems and their purpose. For example, that bugs are tracked in a specific project in Linear or that feedback can be found in a specific Slack channel.</when_to_save>",
    "    <how_to_use>When the user references an external system or information that may be in an external system.</how_to_use>",
    "    <examples>",
    "    user: check the Linear project \"INGEST\" if you want context on these tickets, that's where we track all pipeline bugs",
    "    assistant: [saves reference memory: pipeline bugs are tracked in Linear project \"INGEST\"]",
    "",
    "    user: the Grafana board at grafana.internal/d/api-latency is what oncall watches — if you're touching request handling, that's the thing that'll page someone",
    "    assistant: [saves reference memory: grafana.internal/d/api-latency is the oncall latency dashboard — check it when editing request-path code]",
    "    </examples>",
    "</type>",
    "</types>",
    "",
];

const WHAT_NOT_TO_SAVE_SECTION: &[&str] = &[
    "## What NOT to save in memory",
    "",
    "- Code patterns, conventions, architecture, file paths, or project structure — these can be derived by reading the current project state.",
    "- Git history, recent changes, or who-changed-what — `git log` / `git blame` are authoritative.",
    "- Debugging solutions or fix recipes — the fix is in the code; the commit message has the context.",
    "- Anything already documented in CLAUDE.md files.",
    "- Ephemeral task details: in-progress work, temporary state, current conversation context.",
    "",
    "These exclusions apply even when the user explicitly asks you to save. If they ask you to save a PR list or activity summary, ask what was *surprising* or *non-obvious* about it — that is the part worth keeping.",
];

const WHEN_TO_ACCESS_SECTION: &[&str] = &[
    "## When to access memories",
    "- When memories seem relevant, or the user references prior-conversation work.",
    "- You MUST access memory when the user explicitly asks you to check, recall, or remember.",
    "- If the user says to *ignore* or *not use* memory: proceed as if MEMORY.md were empty. Do not apply remembered facts, cite, compare against, or mention memory content.",
    "- Memory records can become stale over time. Use memory as context for what was true at a given point in time. Before answering the user or building assumptions based solely on information in memory records, verify that the memory is still correct and up-to-date by reading the current state of the files or resources. If a recalled memory conflicts with current information, trust what you observe now — and update or remove the stale memory rather than acting on it.",
];

const TRUSTING_RECALL_SECTION: &[&str] = &[
    "## Before recommending from memory",
    "",
    "A memory that names a specific function, file, or flag is a claim that it existed *when the memory was written*. It may have been renamed, removed, or never merged. Before recommending it:",
    "",
    "- If the memory names a file path: check the file exists.",
    "- If the memory names a function or flag: grep for it.",
    "- If the user is about to act on your recommendation (not just asking about history), verify first.",
    "",
    "\"The memory says X exists\" is not the same as \"X exists now.\"",
    "",
    "A memory that summarizes repo state (activity logs, architecture snapshots) is frozen in time. If the user asks about *recent* or *current* state, prefer `git log` or reading the code over recalling the snapshot.",
];

/// Maps to CC `memdir/memdir.ts` `buildMemoryLines(...)` individual-only branch.
pub fn build_memory_lines(
    display_name: &str,
    memory_dir: &Path,
    extra_guidelines: Option<&[String]>,
    skip_index: bool,
) -> Vec<String> {
    let memory_dir_display = display_memory_dir(memory_dir);
    let mut lines = vec![
        format!("# {display_name}"),
        String::new(),
        format!(
            "You have a persistent, file-based memory system at `{memory_dir_display}`. {DIR_EXISTS_GUIDANCE}"
        ),
        String::new(),
        "You should build up this memory system over time so that future conversations can have a complete picture of who the user is, how they'd like to collaborate with you, what behaviors to avoid or repeat, and the context behind the work the user gives you.".to_string(),
        String::new(),
        "If the user explicitly asks you to remember something, save it immediately as whichever type fits best. If they ask you to forget something, find and remove the relevant entry.".to_string(),
        String::new(),
    ];
    lines.extend(
        TYPES_SECTION_INDIVIDUAL
            .iter()
            .map(|line| (*line).to_string()),
    );
    lines.extend(
        WHAT_NOT_TO_SAVE_SECTION
            .iter()
            .map(|line| (*line).to_string()),
    );
    lines.push(String::new());
    lines.extend(how_to_save_lines(skip_index));
    lines.push(String::new());
    lines.extend(
        WHEN_TO_ACCESS_SECTION
            .iter()
            .map(|line| (*line).to_string()),
    );
    lines.push(String::new());
    lines.extend(
        TRUSTING_RECALL_SECTION
            .iter()
            .map(|line| (*line).to_string()),
    );
    lines.push(String::new());
    lines.extend([
        "## Memory and other forms of persistence".to_string(),
        "Memory is one of several persistence mechanisms available to you as you assist the user in a given conversation. The distinction is often that memory can be recalled in future conversations and should not be used for persisting information that is only useful within the scope of the current conversation.".to_string(),
        "- When to use or update a plan instead of memory: If you are about to start a non-trivial implementation task and would like to reach alignment with the user on your approach you should use a Plan rather than saving this information to memory. Similarly, if you already have a plan within the conversation and you have changed your approach persist that change by updating the plan rather than saving a memory.".to_string(),
        "- When to use or update tasks instead of memory: When you need to break your work in current conversation into discrete steps or keep track of your progress use tasks instead of saving to memory. Tasks are great for persisting information about the work that needs to be done in the current conversation, but memory should be reserved for information that will be useful in future conversations.".to_string(),
        String::new(),
    ]);
    if let Some(extra_guidelines) = extra_guidelines {
        lines.extend(extra_guidelines.iter().cloned());
        lines.push(String::new());
    }
    lines.extend(build_searching_past_context_section(memory_dir));
    lines
}

/// Maps to CC `memdir/memdir.ts` `buildSearchingPastContextSection(...)`.
pub fn build_searching_past_context_section(auto_mem_dir: &Path) -> Vec<String> {
    if !feature_enabled(FeatureFlag::MemorySearchPastContext) {
        return Vec::new();
    }

    let original_cwd = crate::bootstrap::state::get_original_cwd();
    let project_dir =
        crate::utils::session_storage::get_project_dir(&original_cwd.display().to_string());
    build_searching_past_context_section_for(auto_mem_dir, &project_dir, false)
}

fn build_searching_past_context_section_for(
    auto_mem_dir: &Path,
    project_dir: &Path,
    embedded_search_tools: bool,
) -> Vec<String> {
    let auto_mem_dir = display_memory_dir(auto_mem_dir);
    let project_dir = display_memory_dir(project_dir);
    let mem_search = if embedded_search_tools {
        format!("grep -rn \"<search term>\" {auto_mem_dir} --include=\"*.md\"")
    } else {
        format!(
            "{} with pattern=\"<search term>\" path=\"{auto_mem_dir}\" glob=\"*.md\"",
            crate::tools::grep_tool::prompt::GREP_TOOL_NAME
        )
    };
    let transcript_search = if embedded_search_tools {
        format!("grep -rn \"<search term>\" {project_dir} --include=\"*.jsonl\"")
    } else {
        format!(
            "{} with pattern=\"<search term>\" path=\"{project_dir}\" glob=\"*.jsonl\"",
            crate::tools::grep_tool::prompt::GREP_TOOL_NAME
        )
    };

    vec![
        "## Searching past context".to_string(),
        String::new(),
        "When looking for past context:".to_string(),
        "1. Search topic files in your memory directory:".to_string(),
        "```".to_string(),
        mem_search,
        "```".to_string(),
        "2. Session transcript logs (last resort — large files, slow):".to_string(),
        "```".to_string(),
        transcript_search,
        "```".to_string(),
        "Use narrow search terms (error messages, file paths, function names) rather than broad keywords.".to_string(),
        String::new(),
    ]
}

/// Maps to CC `memdir/memdir.ts` `truncateEntrypointContent(...)`.
pub fn truncate_entrypoint_content(raw: &str) -> EntrypointTruncation {
    let trimmed = raw.trim();
    let content_lines: Vec<&str> = trimmed.split('\n').collect();
    let line_count = content_lines.len();
    let byte_count = trimmed.len();

    let was_line_truncated = line_count > MAX_ENTRYPOINT_LINES;
    let was_byte_truncated = byte_count > MAX_ENTRYPOINT_BYTES;

    if !was_line_truncated && !was_byte_truncated {
        return EntrypointTruncation {
            content: trimmed.to_string(),
            line_count,
            byte_count,
            was_line_truncated,
            was_byte_truncated,
        };
    }

    let mut truncated = if was_line_truncated {
        content_lines[..MAX_ENTRYPOINT_LINES].join("\n")
    } else {
        trimmed.to_string()
    };

    if truncated.len() > MAX_ENTRYPOINT_BYTES {
        let cut_at = last_newline_before_or_at(&truncated, MAX_ENTRYPOINT_BYTES)
            .filter(|index| *index > 0)
            .unwrap_or_else(|| floor_char_boundary(&truncated, MAX_ENTRYPOINT_BYTES));
        truncated.truncate(cut_at);
    }

    let reason = if was_byte_truncated && !was_line_truncated {
        format!(
            "{} (limit: {}) — index entries are too long",
            crate::utils::format::format_file_size(byte_count as u64),
            crate::utils::format::format_file_size(MAX_ENTRYPOINT_BYTES as u64)
        )
    } else if was_line_truncated && !was_byte_truncated {
        format!("{line_count} lines (limit: {MAX_ENTRYPOINT_LINES})")
    } else {
        format!(
            "{line_count} lines and {}",
            crate::utils::format::format_file_size(byte_count as u64)
        )
    };

    EntrypointTruncation {
        content: format!(
            "{truncated}\n\n> WARNING: {ENTRYPOINT_NAME} is {reason}. Only part of it was loaded. Keep index entries to one line under ~200 chars; move detail into topic files."
        ),
        line_count,
        byte_count,
        was_line_truncated,
        was_byte_truncated,
    }
}

/// Maps to CC `memdir/memdir.ts` `buildMemoryPrompt(...)`.
///
/// This builder is used by agent-memory style prompts where `MEMORY.md` content
/// is included directly in the prompt. The default system prompt's
/// `loadMemoryPrompt()` intentionally keeps matching upstream and injects only
/// behavioral guidance; auto-memory index content is loaded through CLAUDE.md /
/// user-context plumbing in the official app.
pub fn build_memory_prompt(
    display_name: &str,
    memory_dir: &Path,
    extra_guidelines: Option<&[String]>,
) -> String {
    let entrypoint = memory_dir.join(ENTRYPOINT_NAME);
    let entrypoint_content = std::fs::read_to_string(entrypoint).unwrap_or_default();
    let mut lines = build_memory_lines(display_name, memory_dir, extra_guidelines, false);

    lines.push(format!("## {ENTRYPOINT_NAME}"));
    lines.push(String::new());

    if entrypoint_content.trim().is_empty() {
        lines.push(format!(
            "Your {ENTRYPOINT_NAME} is currently empty. When you save new memories, they will appear here."
        ));
    } else {
        let truncation = truncate_entrypoint_content(&entrypoint_content);
        let memory_type = if display_name == AUTO_MEM_DISPLAY_NAME {
            "auto"
        } else {
            "agent"
        };
        log_memory_dir_counts(memory_dir, &truncation, memory_type);
        lines.push(truncation.content);
    }

    lines.join("\n")
}

/// Maps to CC `memdir/memdir.ts` `logMemoryDirCounts(...)`.
///
/// Upstream logs analytics asynchronously. Cometix does not have the analytics
/// service ported, so this records equivalent metadata through `tracing::debug!`
/// and never affects prompt construction.
fn log_memory_dir_counts(memory_dir: &Path, truncation: &EntrypointTruncation, memory_type: &str) {
    let mut file_count = 0usize;
    let mut subdir_count = 0usize;
    let mut read_ok = false;

    if let Ok(dirents) = std::fs::read_dir(memory_dir) {
        read_ok = true;
        for dirent in dirents.flatten() {
            if let Ok(file_type) = dirent.file_type() {
                if file_type.is_file() {
                    file_count += 1;
                } else if file_type.is_dir() {
                    subdir_count += 1;
                }
            }
        }
    }

    tracing::debug!(
        target: "cometix::memdir",
        memory_dir = %memory_dir.display(),
        memory_type,
        content_length = truncation.byte_count,
        line_count = truncation.line_count,
        was_truncated = truncation.was_line_truncated,
        was_byte_truncated = truncation.was_byte_truncated,
        total_file_count = file_count,
        total_subdir_count = subdir_count,
        read_ok,
        "tengu_memdir_loaded"
    );
}

fn last_newline_before_or_at(value: &str, max_byte_index: usize) -> Option<usize> {
    let mut last = None;
    for (index, ch) in value.char_indices() {
        if index > max_byte_index {
            break;
        }
        if ch == '\n' {
            last = Some(index);
        }
    }
    last
}

fn floor_char_boundary(value: &str, max_byte_index: usize) -> usize {
    if max_byte_index >= value.len() {
        return value.len();
    }
    let mut index = max_byte_index;
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Maps to CC `memdir/memdir.ts` `loadMemoryPrompt()` for the auto-memory
/// single-directory branch. Team memory and KAIROS daily logs remain absent
/// unless their owning features are ported. GrowthBook-only skip-index and
/// search-past-context prompt deltas are wired through source-controlled feature
/// switches.
pub fn load_memory_prompt() -> Option<String> {
    if !crate::memdir::paths::is_auto_memory_enabled() {
        return None;
    }
    let memory_dir = crate::memdir::paths::get_auto_mem_path();
    // Preserve production's CC-aligned ensureMemoryDirExists side effect, while
    // keeping parallel tests from creating whichever config-home path another
    // test temporarily installs. Tests that explicitly override the memory path
    // still exercise directory creation.
    if !cfg!(test) || crate::memdir::paths::has_auto_mem_path_override() {
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            eprintln!(
                "ensureMemoryDirExists failed for {}: {error}",
                memory_dir.display()
            );
        }
    }
    // CC memdir.ts:441-446 gates on the trimmed text but threads the raw value.
    let extra_guidelines = crate::utils::process_env::var("CLAUDE_COWORK_MEMORY_EXTRA_GUIDELINES")
        .filter(|value| !value.trim().is_empty())
        .map(|value| vec![value]);
    Some(
        build_memory_lines(
            AUTO_MEM_DISPLAY_NAME,
            &memory_dir,
            extra_guidelines.as_deref(),
            feature_enabled(FeatureFlag::MemorySkipIndex),
        )
        .join("\n"),
    )
}

fn how_to_save_lines(skip_index: bool) -> Vec<String> {
    let mut lines = vec!["## How to save memories".to_string(), String::new()];
    if skip_index {
        lines.extend([
            "Write each memory to its own file (e.g., `user_role.md`, `feedback_testing.md`) using this frontmatter format:".to_string(),
            String::new(),
        ]);
        lines.extend(
            MEMORY_FRONTMATTER_EXAMPLE
                .iter()
                .map(|line| (*line).to_string()),
        );
        lines.extend([
            String::new(),
            "- Keep the name, description, and type fields in memory files up-to-date with the content".to_string(),
            "- Organize memory semantically by topic, not chronologically".to_string(),
            "- Update or remove memories that turn out to be wrong or outdated".to_string(),
            "- Do not write duplicate memories. First check if there is an existing memory you can update before writing a new one.".to_string(),
        ]);
    } else {
        lines.extend([
            "Saving a memory is a two-step process:".to_string(),
            String::new(),
            "**Step 1** — write the memory to its own file (e.g., `user_role.md`, `feedback_testing.md`) using this frontmatter format:".to_string(),
            String::new(),
        ]);
        lines.extend(
            MEMORY_FRONTMATTER_EXAMPLE
                .iter()
                .map(|line| (*line).to_string()),
        );
        lines.extend([
            String::new(),
            format!("**Step 2** — add a pointer to that file in `{ENTRYPOINT_NAME}`. `{ENTRYPOINT_NAME}` is an index, not a memory — each entry should be one line, under ~150 characters: `- [Title](file.md) — one-line hook`. It has no frontmatter. Never write memory content directly into `{ENTRYPOINT_NAME}`."),
            String::new(),
            format!("- `{ENTRYPOINT_NAME}` is always loaded into your conversation context — lines after {MAX_ENTRYPOINT_LINES} will be truncated, so keep the index concise"),
            "- Keep the name, description, and type fields in memory files up-to-date with the content".to_string(),
            "- Organize memory semantically by topic, not chronologically".to_string(),
            "- Update or remove memories that turn out to be wrong or outdated".to_string(),
            "- Do not write duplicate memories. First check if there is an existing memory you can update before writing a new one.".to_string(),
        ]);
    }
    lines
}

fn display_memory_dir(path: &Path) -> String {
    let mut display = path.display().to_string();
    let sep = std::path::MAIN_SEPARATOR;
    if !display.ends_with(sep) {
        display.push(sep);
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::path::PathBuf;

    #[test]
    fn build_memory_lines_matches_official_auto_memory_shape() {
        let lines = build_memory_lines(
            AUTO_MEM_DISPLAY_NAME,
            &PathBuf::from("/tmp/cometix-memory"),
            None,
            false,
        );
        let prompt = lines.join("\n");

        assert!(prompt.starts_with("# auto memory\n\nYou have a persistent"));
        assert!(prompt.contains("This directory already exists"));
        assert!(prompt.contains("## Types of memory"));
        assert!(prompt.contains("## How to save memories"));
        assert!(prompt.contains("**Step 2** — add a pointer"));
        assert!(prompt.contains("## When to access memories"));
        assert!(prompt.contains("## Before recommending from memory"));
        assert!(prompt.contains("## Memory and other forms of persistence"));
        assert!(!prompt.contains("## Searching past context"));
    }

    #[test]
    fn build_memory_lines_skip_index_matches_official_gate_shape() {
        let lines = build_memory_lines(
            AUTO_MEM_DISPLAY_NAME,
            &PathBuf::from("/tmp/cometix-memory"),
            None,
            true,
        );
        let prompt = lines.join("\n");

        assert!(prompt.contains("Write each memory to its own file"));
        assert!(!prompt.contains("**Step 2** — add a pointer"));
    }

    #[test]
    fn build_searching_past_context_section_matches_official_grep_shape() {
        let lines = build_searching_past_context_section_for(
            &PathBuf::from("/tmp/cometix-memory"),
            &PathBuf::from("/tmp/cometix-project-logs"),
            false,
        );
        let prompt = lines.join("\n");

        assert!(prompt.starts_with("## Searching past context"));
        assert!(prompt.contains("Grep with pattern=\"<search term>\""));
        assert!(prompt.contains("path=\"/tmp/cometix-memory/\" glob=\"*.md\""));
        assert!(prompt.contains("path=\"/tmp/cometix-project-logs/\" glob=\"*.jsonl\""));
    }

    #[test]
    fn build_searching_past_context_section_supports_embedded_search_shape() {
        let lines = build_searching_past_context_section_for(
            &PathBuf::from("/tmp/cometix-memory"),
            &PathBuf::from("/tmp/cometix-project-logs"),
            true,
        );
        let prompt = lines.join("\n");

        assert!(
            prompt.contains("grep -rn \"<search term>\" /tmp/cometix-memory/ --include=\"*.md\"")
        );
        assert!(prompt.contains(
            "grep -rn \"<search term>\" /tmp/cometix-project-logs/ --include=\"*.jsonl\""
        ));
    }

    #[test]
    fn truncate_entrypoint_content_preserves_small_entrypoint() {
        let truncation = truncate_entrypoint_content("\n- [User prefs](user.md) — terse replies\n");

        assert_eq!(
            truncation.content,
            "- [User prefs](user.md) — terse replies"
        );
        assert_eq!(truncation.line_count, 1);
        assert!(!truncation.was_line_truncated);
        assert!(!truncation.was_byte_truncated);
    }

    #[test]
    fn truncate_entrypoint_content_warns_on_line_cap() {
        let raw = (0..=MAX_ENTRYPOINT_LINES)
            .map(|index| format!("- memory {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let truncation = truncate_entrypoint_content(&raw);

        assert!(truncation.was_line_truncated);
        assert!(!truncation.was_byte_truncated);
        assert_eq!(truncation.line_count, MAX_ENTRYPOINT_LINES + 1);
        assert!(
            truncation
                .content
                .contains("WARNING: MEMORY.md is 201 lines")
        );
        assert!(truncation.content.contains("- memory 199"));
        assert!(!truncation.content.contains("- memory 200\n"));
    }

    #[test]
    fn truncate_entrypoint_content_warns_on_byte_cap_at_line_boundary() {
        let raw = format!("{}\nkeep-out", "x".repeat(MAX_ENTRYPOINT_BYTES + 32));
        let truncation = truncate_entrypoint_content(&raw);

        assert!(truncation.was_byte_truncated);
        assert!(truncation.content.contains("index entries are too long"));
        assert!(!truncation.content.contains("keep-out"));
        assert!(truncation.content.is_char_boundary(MAX_ENTRYPOINT_BYTES));
    }

    #[test]
    fn build_memory_prompt_loads_memory_entrypoint_content() {
        let dir = temp_memory_dir("loads-entrypoint");
        std::fs::write(
            dir.join(ENTRYPOINT_NAME),
            "- [Preferences](user_preferences.md) — prefers concise answers\n",
        )
        .unwrap();

        let prompt = build_memory_prompt(AUTO_MEM_DISPLAY_NAME, &dir, None);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(prompt.contains("## MEMORY.md\n\n- [Preferences](user_preferences.md)"));
        assert!(!prompt.contains("currently empty"));
    }

    #[test]
    fn build_memory_prompt_reports_empty_entrypoint() {
        let dir = temp_memory_dir("empty-entrypoint");
        let prompt = build_memory_prompt(AUTO_MEM_DISPLAY_NAME, &dir, None);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(prompt.contains(
            "Your MEMORY.md is currently empty. When you save new memories, they will appear here."
        ));
    }

    #[test]
    fn load_memory_prompt_respects_auto_memory_disable_gate() {
        use crate::utils::settings::settings_cache;
        use crate::utils::settings::types::SettingsJson;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _env = [
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
            "CLAUDE_CODE_SIMPLE",
            "CLAUDE_CODE_REMOTE",
            "CLAUDE_CODE_REMOTE_MEMORY_DIR",
        ]
        .map(EnvVarGuard::unset);
        // The lock guard resets the settings caches again on release.
        settings_cache::set_session_settings_cache(
            crate::utils::settings::validation::SettingsWithErrors {
                settings: SettingsJson {
                    auto_memory_enabled: Some(false),
                    ..SettingsJson::default()
                },
                ..Default::default()
            },
        );
        assert!(load_memory_prompt().is_none());
    }

    fn temp_memory_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cometix-memdir-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
