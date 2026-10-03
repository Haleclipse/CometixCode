//! Maps to: CC `commands/init.ts:1-256`.
//!
//! The prompt text, dynamic description, onboarding-completion side effect,
//! and `getPromptForCommand` boundary remain owned by this file. Aggregate
//! registration stays in `commands/mod.rs`, matching CC `commands.ts`.

use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

pub const NAME: &str = "init";
pub const PROGRESS_MESSAGE: &str = "analyzing your codebase";
pub const CONTENT_LENGTH: usize = 0;

const OLD_DESCRIPTION: &str = "Initialize a new CLAUDE.md file with codebase documentation";
const NEW_DESCRIPTION: &str =
    "Initialize new CLAUDE.md file(s) and optional skills/hooks with codebase documentation";

const OLD_INIT_PROMPT: &str = r##"Please analyze this codebase and create a CLAUDE.md file, which will be given to future instances of Claude Code to operate in this repository.

What to add:
1. Commands that will be commonly used, such as how to build, lint, and run tests. Include the necessary commands to develop in this codebase, such as how to run a single test.
2. High-level code architecture and structure so that future instances can be productive more quickly. Focus on the "big picture" architecture that requires reading multiple files to understand.

Usage notes:
- If there's already a CLAUDE.md, suggest improvements to it.
- When you make the initial CLAUDE.md, do not repeat yourself and do not include obvious instructions like "Provide helpful error messages to users", "Write unit tests for all new utilities", "Never include sensitive information (API keys, tokens) in code or commits".
- Avoid listing every component or file structure that can be easily discovered.
- Don't include generic development practices.
- If there are Cursor rules (in .cursor/rules/ or .cursorrules) or Copilot rules (in .github/copilot-instructions.md), make sure to include the important parts.
- If there is a README.md, make sure to include the important parts.
- Do not make up information such as "Common Development Tasks", "Tips for Development", "Support and Documentation" unless this is expressly included in other files that you read.
- Be sure to prefix the file with the following text:

```
# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.
```"##;

const NEW_INIT_PROMPT: &str = r##"Set up a minimal CLAUDE.md (and optionally skills and hooks) for this repo. CLAUDE.md is loaded into every Claude Code session, so it must be concise — only include what Claude would get wrong without it.

## Phase 1: Ask what to set up

Use AskUserQuestion to find out what the user wants:

- "Which CLAUDE.md files should /init set up?"
  Options: "Project CLAUDE.md" | "Personal CLAUDE.local.md" | "Both project + personal"
  Description for project: "Team-shared instructions checked into source control — architecture, coding standards, common workflows."
  Description for personal: "Your private preferences for this project (gitignored, not shared) — your role, sandbox URLs, preferred test data, workflow quirks."

- "Also set up skills and hooks?"
  Options: "Skills + hooks" | "Skills only" | "Hooks only" | "Neither, just CLAUDE.md"
  Description for skills: "On-demand capabilities you or Claude invoke with `/skill-name` — good for repeatable workflows and reference knowledge."
  Description for hooks: "Deterministic shell commands that run on tool events (e.g., format after every edit). Claude can't skip them."

## Phase 2: Explore the codebase

Launch a subagent to survey the codebase, and ask it to read key files to understand the project: manifest files (package.json, Cargo.toml, pyproject.toml, go.mod, pom.xml, etc.), README, Makefile/build configs, CI config, existing CLAUDE.md, .claude/rules/, AGENTS.md, .cursor/rules or .cursorrules, .github/copilot-instructions.md, .windsurfrules, .clinerules, .mcp.json.

Detect:
- Build, test, and lint commands (especially non-standard ones)
- Languages, frameworks, and package manager
- Project structure (monorepo with workspaces, multi-module, or single project)
- Code style rules that differ from language defaults
- Non-obvious gotchas, required env vars, or workflow quirks
- Existing .claude/skills/ and .claude/rules/ directories
- Formatter configuration (prettier, biome, ruff, black, gofmt, rustfmt, or a unified format script like `npm run format` / `make fmt`)
- Git worktree usage: run `git worktree list` to check if this repo has multiple worktrees (only relevant if the user wants a personal CLAUDE.local.md)

Note what you could NOT figure out from code alone — these become interview questions.

## Phase 3: Fill in the gaps

Use AskUserQuestion to gather what you still need to write good CLAUDE.md files and skills. Ask only things the code can't answer.

If the user chose project CLAUDE.md or both: ask about codebase practices — non-obvious commands, gotchas, branch/PR conventions, required env setup, testing quirks. Skip things already in README or obvious from manifest files. Do not mark any options as "recommended" — this is about how their team works, not best practices.

If the user chose personal CLAUDE.local.md or both: ask about them, not the codebase. Do not mark any options as "recommended" — this is about their personal preferences, not best practices. Examples of questions:
  - What's their role on the team? (e.g., "backend engineer", "data scientist", "new hire onboarding")
  - How familiar are they with this codebase and its languages/frameworks? (so Claude can calibrate explanation depth)
  - Do they have personal sandbox URLs, test accounts, API key paths, or local setup details Claude should know?
  - Only if Phase 2 found multiple git worktrees: ask whether their worktrees are nested inside the main repo (e.g., `.claude/worktrees/<name>/`) or siblings/external (e.g., `../myrepo-feature/`). If nested, the upward file walk finds the main repo's CLAUDE.local.md automatically — no special handling needed. If sibling/external, the personal content should live in a home-directory file (e.g., `~/.claude/<project-name>-instructions.md`) and each worktree gets a one-line CLAUDE.local.md stub that imports it: `@~/.claude/<project-name>-instructions.md`. Never put this import in the project CLAUDE.md — that would check a personal reference into the team-shared file.
  - Any communication preferences? (e.g., "be terse", "always explain tradeoffs", "don't summarize at the end")

**Synthesize a proposal from Phase 2 findings** — e.g., format-on-edit if a formatter exists, a `/verify` skill if tests exist, a CLAUDE.md note for anything from the gap-fill answers that's a guideline rather than a workflow. For each, pick the artifact type that fits, **constrained by the Phase 1 skills+hooks choice**:

  - **Hook** (stricter) — deterministic shell command on a tool event; Claude can't skip it. Fits mechanical, fast, per-edit steps: formatting, linting, running a quick test on the changed file.
  - **Skill** (on-demand) — you or Claude invoke `/skill-name` when you want it. Fits workflows that don't belong on every edit: deep verification, session reports, deploys.
  - **CLAUDE.md note** (looser) — influences Claude's behavior but not enforced. Fits communication/thinking preferences: "plan before coding", "be terse", "explain tradeoffs".

  **Respect Phase 1's skills+hooks choice as a hard filter**: if the user picked "Skills only", downgrade any hook you'd suggest to a skill or a CLAUDE.md note. If "Hooks only", downgrade skills to hooks (where mechanically possible) or notes. If "Neither", everything becomes a CLAUDE.md note. Never propose an artifact type the user didn't opt into.

**Show the proposal via AskUserQuestion's `preview` field, not as a separate text message** — the dialog overlays your output, so preceding text is hidden. The `preview` field renders markdown in a side-panel (like plan mode); the `question` field is plain-text-only. Structure it as:

  - `question`: short and plain, e.g. "Does this proposal look right?"
  - Each option gets a `preview` with the full proposal as markdown. The "Looks good — proceed" option's preview shows everything; per-item-drop options' previews show what remains after that drop.
  - **Keep previews compact — the preview box truncates with no scrolling.** One line per item, no blank lines between items, no header. Example preview content:

    • **Format-on-edit hook** (automatic) — `ruff format <file>` via PostToolUse
    • **/verify skill** (on-demand) — `make lint && make typecheck && make test`
    • **CLAUDE.md note** (guideline) — "run lint/typecheck/test before marking done"

  - Option labels stay short ("Looks good", "Drop the hook", "Drop the skill") — the tool auto-adds an "Other" free-text option, so don't add your own catch-all.

**Build the preference queue** from the accepted proposal. Each entry: {type: hook|skill|note, description, target file, any Phase-2-sourced details like the actual test/format command}. Phases 4-7 consume this queue.

## Phase 4: Write CLAUDE.md (if user chose project or both)

Write a minimal CLAUDE.md at the project root. Every line must pass this test: "Would removing this cause Claude to make mistakes?" If no, cut it.

**Consume `note` entries from the Phase 3 preference queue whose target is CLAUDE.md** (team-level notes) — add each as a concise line in the most relevant section. These are the behaviors the user wants Claude to follow but didn't need guaranteed (e.g., "propose a plan before implementing", "explain the tradeoffs when refactoring"). Leave personal-targeted notes for Phase 5.

Include:
- Build/test/lint commands Claude can't guess (non-standard scripts, flags, or sequences)
- Code style rules that DIFFER from language defaults (e.g., "prefer type over interface")
- Testing instructions and quirks (e.g., "run single test with: pytest -k 'test_name'")
- Repo etiquette (branch naming, PR conventions, commit style)
- Required env vars or setup steps
- Non-obvious gotchas or architectural decisions
- Important parts from existing AI coding tool configs if they exist (AGENTS.md, .cursor/rules, .cursorrules, .github/copilot-instructions.md, .windsurfrules, .clinerules)

Exclude:
- File-by-file structure or component lists (Claude can discover these by reading the codebase)
- Standard language conventions Claude already knows
- Generic advice ("write clean code", "handle errors")
- Detailed API docs or long references — use `@path/to/import` syntax instead (e.g., `@docs/api-reference.md`) to inline content on demand without bloating CLAUDE.md
- Information that changes frequently — reference the source with `@path/to/import` so Claude always reads the current version
- Long tutorials or walkthroughs (move to a separate file and reference with `@path/to/import`, or put in a skill)
- Commands obvious from manifest files (e.g., standard "npm test", "cargo test", "pytest")

Be specific: "Use 2-space indentation in TypeScript" is better than "Format code properly."

Do not repeat yourself and do not make up sections like "Common Development Tasks" or "Tips for Development" — only include information expressly found in files you read.

Prefix the file with:

```
# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.
```

If CLAUDE.md already exists: read it, propose specific changes as diffs, and explain why each change improves it. Do not silently overwrite.

For projects with multiple concerns, suggest organizing instructions into `.claude/rules/` as separate focused files (e.g., `code-style.md`, `testing.md`, `security.md`). These are loaded automatically alongside CLAUDE.md and can be scoped to specific file paths using `paths` frontmatter.

For projects with distinct subdirectories (monorepos, multi-module projects, etc.): mention that subdirectory CLAUDE.md files can be added for module-specific instructions (they're loaded automatically when Claude works in those directories). Offer to create them if the user wants.

## Phase 5: Write CLAUDE.local.md (if user chose personal or both)

Write a minimal CLAUDE.local.md at the project root. This file is automatically loaded alongside CLAUDE.md. After creating it, add `CLAUDE.local.md` to the project's .gitignore so it stays private.

**Consume `note` entries from the Phase 3 preference queue whose target is CLAUDE.local.md** (personal-level notes) — add each as a concise line. If the user chose personal-only in Phase 1, this is the sole consumer of note entries.

Include:
- The user's role and familiarity with the codebase (so Claude can calibrate explanations)
- Personal sandbox URLs, test accounts, or local setup details
- Personal workflow or communication preferences

Keep it short — only include what would make Claude's responses noticeably better for this user.

If Phase 2 found multiple git worktrees and the user confirmed they use sibling/external worktrees (not nested inside the main repo): the upward file walk won't find a single CLAUDE.local.md from all worktrees. Write the actual personal content to `~/.claude/<project-name>-instructions.md` and make CLAUDE.local.md a one-line stub that imports it: `@~/.claude/<project-name>-instructions.md`. The user can copy this one-line stub to each sibling worktree. Never put this import in the project CLAUDE.md. If worktrees are nested inside the main repo (e.g., `.claude/worktrees/`), no special handling is needed — the main repo's CLAUDE.local.md is found automatically.

If CLAUDE.local.md already exists: read it, propose specific additions, and do not silently overwrite.

## Phase 6: Suggest and create skills (if user chose "Skills + hooks" or "Skills only")

Skills add capabilities Claude can use on demand without bloating every session.

**First, consume `skill` entries from the Phase 3 preference queue.** Each queued skill preference becomes a SKILL.md tailored to what the user described. For each:
- Name it from the preference (e.g., "verify-deep", "session-report", "deploy-sandbox")
- Write the body using the user's own words from the interview plus whatever Phase 2 found (test commands, report format, deploy target). If the preference maps to an existing bundled skill (e.g., `/verify`), write a project skill that adds the user's specific constraints on top — tell the user the bundled one still exists and theirs is additive.
- Ask a quick follow-up if the preference is underspecified (e.g., "which test command should verify-deep run?")

**Then suggest additional skills** beyond the queue when you find:
- Reference knowledge for specific tasks (conventions, patterns, style guides for a subsystem)
- Repeatable workflows the user would want to trigger directly (deploy, fix an issue, release process, verify changes)

For each suggested skill, provide: name, one-line purpose, and why it fits this repo.

If `.claude/skills/` already exists with skills, review them first. Do not overwrite existing skills — only propose new ones that complement what is already there.

Create each skill at `.claude/skills/<skill-name>/SKILL.md`:

```yaml
---
name: <skill-name>
description: <what the skill does and when to use it>
---

<Instructions for Claude>
```

Both the user (`/<skill-name>`) and Claude can invoke skills by default. For workflows with side effects (e.g., `/deploy`, `/fix-issue 123`), add `disable-model-invocation: true` so only the user can trigger it, and use `$ARGUMENTS` to accept input.

## Phase 7: Suggest additional optimizations

Tell the user you're going to suggest a few additional optimizations now that CLAUDE.md and skills (if chosen) are in place.

Check the environment and ask about each gap you find (use AskUserQuestion):

- **GitHub CLI**: Run `which gh` (or `where gh` on Windows). If it's missing AND the project uses GitHub (check `git remote -v` for github.com), ask the user if they want to install it. Explain that the GitHub CLI lets Claude help with commits, pull requests, issues, and code review directly.

- **Linting**: If Phase 2 found no lint config (no .eslintrc, ruff.toml, .golangci.yml, etc. for the project's language), ask the user if they want Claude to set up linting for this codebase. Explain that linting catches issues early and gives Claude fast feedback on its own edits.

- **Proposal-sourced hooks** (if user chose "Skills + hooks" or "Hooks only"): Consume `hook` entries from the Phase 3 preference queue. If Phase 2 found a formatter and the queue has no formatting hook, offer format-on-edit as a fallback. If the user chose "Neither" or "Skills only" in Phase 1, skip this bullet entirely.

  For each hook preference (from the queue or the formatter fallback):

  1. Target file: default based on the Phase 1 CLAUDE.md choice — project → `.claude/settings.json` (team-shared, committed); personal → `.claude/settings.local.json`. Only ask if the user chose "both" in Phase 1 or the preference is ambiguous. Ask once for all hooks, not per-hook.

  2. Pick the event and matcher from the preference:
     - "after every edit" → `PostToolUse` with matcher `Write|Edit`
     - "when Claude finishes" / "before I review" → `Stop` event (fires at the end of every turn — including read-only ones)
     - "before running bash" → `PreToolUse` with matcher `Bash`
     - "before committing" (literal git-commit gate) → **not a hooks.json hook.** Matchers can't filter Bash by command content, so there's no way to target only `git commit`. Route this to a git pre-commit hook (`.git/hooks/pre-commit`, husky, pre-commit framework) instead — offer to write one. If the user actually means "before I review and commit Claude's output", that's `Stop` — probe to disambiguate.
     Probe if the preference is ambiguous.

  3. **Load the hook reference** (once per `/init` run, before the first hook): invoke the Skill tool with `skill: 'update-config'` and args starting with `[hooks-only]` followed by a one-line summary of what you're building — e.g., `[hooks-only] Constructing a PostToolUse/Write|Edit format hook for .claude/settings.json using ruff`. This loads the hooks schema and verification flow into context. Subsequent hooks reuse it — don't re-invoke.

  4. Follow the skill's **"Constructing a Hook"** flow: dedup check → construct for THIS project → pipe-test raw → wrap → write JSON → `jq -e` validate → live-proof (for `Pre|PostToolUse` on triggerable matchers) → cleanup → handoff. Target file and event/matcher come from steps 1–2 above.

Act on each "yes" before moving on.

## Phase 8: Summary and next steps

Recap what was set up — which files were written and the key points included in each. Remind the user these files are a starting point: they should review and tweak them, and can run `/init` again anytime to re-scan.

Then tell the user that you'll be introducing a few more suggestions for optimizing their codebase and Claude Code setup based on what you found. Present these as a single, well-formatted to-do list where every item is relevant to this repo. Put the most impactful items first.

When building the list, work through these checks and include only what applies:
- If frontend code was detected (React, Vue, Svelte, etc.): `/plugin install frontend-design@claude-plugins-official` gives Claude design principles and component patterns so it produces polished UI; `/plugin install playwright@claude-plugins-official` lets Claude launch a real browser, screenshot what it built, and fix visual bugs itself.
- If you found gaps in Phase 7 (missing GitHub CLI, missing linting) and the user said no: list them here with a one-line reason why each helps.
- If tests are missing or sparse: suggest setting up a test framework so Claude can verify its own changes.
- To help you create skills and optimize existing skills using evals, Claude Code has an official skill-creator plugin you can install. Install it with `/plugin install skill-creator@claude-plugins-official`, then run `/skill-creator <skill-name>` to create new skills or refine any existing skill. (Always include this one.)
- Browse official plugins with `/plugin` — these bundle skills, agents, hooks, and MCP servers that you may find helpful. You can also create your own custom plugins to share them with others. (Always include this one.)"##;

/// Maps to the repeated `feature('NEW_INIT') && (...)` gate in CC
/// `commands/init.ts:229-250`. The build feature is source-controlled; the
/// runtime cohort is the internal build or `CLAUDE_CODE_NEW_INIT` truthy.
pub fn is_new_init_enabled_for_audience(
    audience: crate::utils::build_profile::BuildAudience,
    env_enabled: bool,
) -> bool {
    feature_enabled(FeatureFlag::NewInit)
        && (crate::utils::build_profile::audience_has_internal_capability(
            audience,
            crate::utils::build_profile::InternalCapability::Commands,
        ) || env_enabled)
}

pub fn is_new_init_enabled() -> bool {
    is_new_init_enabled_for_audience(
        crate::utils::build_profile::build_audience(),
        crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_NEW_INIT").as_deref(),
        ),
    )
}

/// Maps to: CC `commands/init.ts:229-235` dynamic `description` getter.
pub fn description() -> &'static str {
    if is_new_init_enabled() {
        NEW_DESCRIPTION
    } else {
        OLD_DESCRIPTION
    }
}

/// Maps to: CC `commands/init.ts:239-253::getPromptForCommand`.
pub fn get_prompt_for_command() -> &'static str {
    if let Err(error) = crate::project_onboarding_state::maybe_mark_project_onboarding_complete() {
        // CC's side effect is void and never prevents the prompt from running.
        crate::utils::debug::log_for_debugging(&format!(
            "Failed to mark project onboarding complete from /init: {error}"
        ));
    }
    if is_new_init_enabled() {
        NEW_INIT_PROMPT
    } else {
        OLD_INIT_PROMPT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use sha2::{Digest, Sha256};

    struct EnvGuard {
        _env: [EnvVarGuard; 2],
    }

    impl EnvGuard {
        fn clear() -> Self {
            Self {
                _env: [
                    EnvVarGuard::unset("CLAUDE_CODE_NEW_INIT"),
                    EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0"),
                ],
            }
        }
    }

    fn sha256(value: &str) -> String {
        Sha256::digest(value.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn init_prompts_match_official_source_bytes() {
        assert_eq!(OLD_INIT_PROMPT.len(), 1592);
        assert_eq!(
            sha256(OLD_INIT_PROMPT),
            "cfdedaa2c59770dce2afbc73047805cda7078d961cc650463e39125197b55a39"
        );
        assert_eq!(NEW_INIT_PROMPT.len(), 18067);
        assert_eq!(
            sha256(NEW_INIT_PROMPT),
            "66cd3611d601c2d03a0293875bec9fbb3914dadcd1a45d7d517b5662a7f40869"
        );
    }

    #[test]
    fn init_description_and_prompt_gate_match_official_runtime_cohort() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::clear();
        let registry = crate::commands::declared_commands_for_tests();
        let command = crate::commands::find_command("init", &registry).unwrap();

        assert_eq!(
            is_new_init_enabled(),
            crate::utils::build_profile::build_audience().is_internal()
        );
        assert_eq!(
            description(),
            if crate::utils::build_profile::build_audience().is_internal() {
                NEW_DESCRIPTION
            } else {
                OLD_DESCRIPTION
            }
        );
        crate::utils::process_env::set("CLAUDE_CODE_NEW_INIT", "1");
        assert!(is_new_init_enabled());
        assert_eq!(description(), NEW_DESCRIPTION);
        // CC init.ts:229-235 is a getter even on an already loaded catalog.
        assert_eq!(
            crate::commands::command_description(command),
            NEW_DESCRIPTION
        );
        crate::utils::process_env::remove("CLAUDE_CODE_NEW_INIT");
        assert!(is_new_init_enabled_for_audience(
            crate::utils::build_profile::BuildAudience::AnthropicInternal,
            false,
        ));
    }

    #[test]
    fn init_prompt_callback_matches_official_prompt_command_transport() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::clear();
        let registry = crate::commands::declared_commands_for_tests();
        let result = crate::utils::process_user_input::process_slash_command::process_slash_command(
            "/init ignored args",
            Some("init-command".to_string()),
            &registry,
        );

        assert!(result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 3);
        assert!(matches!(
            &result.messages[0].kind,
            crate::types::message::RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::Text(text))
                    // CC processSlashCommand.tsx:1049-1060,1205: prompt
                    // loading metadata differs from local-command input tags.
                    if text == "<command-message>init</command-message>\n<command-name>/init</command-name>\n<command-args>ignored args</command-args>"
            )
        ));
        let expected_prompt = if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Commands,
        ) {
            NEW_INIT_PROMPT
        } else {
            OLD_INIT_PROMPT
        };
        assert!(matches!(
            &result.messages[1].kind,
            crate::types::message::RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::MetaText(text)) if text == &expected_prompt
            )
        ));
    }

    #[test]
    fn init_images_and_compact_preservation_matches_official_prompt_pipeline() {
        use crate::types::message::{Message, RenderableMessageKind, UserContent};
        use crate::utils::process_user_input::{
            ProcessInputMode, ProcessUserInputParams, process_user_input_base,
        };
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::clear();
        let agent = "init-command-image-agent";
        crate::bootstrap::state::clear_invoked_skills_for_agent(agent);
        let mut command =
            crate::commands::find_command("init", &crate::commands::declared_commands_for_tests())
                .unwrap()
                .clone();
        command.user_facing_name = Some("setup-project".into());
        let mut context = crate::tool::ToolUseContext::default();
        context.agent_id = Some(agent.to_string());
        let image = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6nhsAAAAASUVORK5CYII=";
        let result = process_user_input_base(ProcessUserInputParams {
            input: "/init".to_string(),
            uuid: Some("init-image-submit".to_string()),
            mode: ProcessInputMode::Prompt,
            pre_expansion_input: None,
            skip_slash_commands: false,
            is_meta: false,
            commands: std::sync::Arc::new(vec![command]),
            tool_use_context: context,
            image_content_blocks: vec![UserContent::Image {
                media_type: "image/png".to_string(),
                data: image.to_string(),
            }],
            image_paste_ids: Vec::new(),
        });
        assert!(result.should_query);
        assert_eq!(result.messages.len(), 3);
        // CC processSlashCommand.tsx:1049-1060,1074-1075: canonical name,
        // no empty command-args tag, independent of userFacingName.
        let RenderableMessageKind::User { message: metadata } = &result.messages[0].kind else {
            panic!("expected command metadata");
        };
        assert_eq!(
            metadata.content,
            vec![UserContent::Text(
                "<command-message>init</command-message>\n<command-name>/init</command-name>"
                    .into(),
            )]
        );
        let RenderableMessageKind::User { message: prompt } = &result.messages[1].kind else {
            panic!("expected meta prompt");
        };
        // CC :1212-1215,1242-1245: images precede the prompt inside isMeta.
        assert!(matches!(prompt.content.as_slice(),
            [UserContent::MetaImage { data, .. }, UserContent::MetaText(text)]
                if data == image && text.contains("CLAUDE.md")
        ));
        assert!(!crate::utils::messages::should_show_user_message(
            &result.messages[1],
            false
        ));
        let api =
            crate::services::api::claude::user_message_to_message_param(prompt, false, false, None);
        let api = serde_json::to_value(api).unwrap();
        assert_eq!(api["content"][0]["type"], "image");
        assert_eq!(api["content"][0]["source"]["data"], image);
        assert_eq!(api["content"][1]["type"], "text");
        let wire =
            crate::utils::session_storage::typed_messages_as_transcript_values(&[Message::User(
                prompt.clone(),
            )]);
        assert_eq!(wire[0]["isMeta"], true);
        assert_eq!(wire[0]["message"]["content"][0]["type"], "image");
        // CC :1188-1203 and compact.ts#createSkillAttachmentIfNeeded:
        // builtin init is recorded too, scoped to the current agent.
        let invoked = crate::bootstrap::state::get_invoked_skills_for_agent(Some(agent));
        assert_eq!(invoked.len(), 1);
        assert_eq!(invoked[0].skill_name, "init");
        assert_eq!(invoked[0].skill_path, "builtin:init");
        let preserved =
            crate::services::compact::compact::create_skill_attachment_if_needed(Some(agent))
                .unwrap();
        let preserved = serde_json::to_value(preserved.attachment).unwrap();
        assert_eq!(preserved["type"], "invoked_skills");
        assert_eq!(preserved["skills"][0]["name"], "init");
        assert_eq!(preserved["skills"][0]["path"], "builtin:init");
        assert!(
            crate::services::compact::compact::create_skill_attachment_if_needed(Some(
                "different-init-agent"
            ))
            .is_none()
        );
        assert!(matches!(&result.messages[2].kind,
            RenderableMessageKind::Attachment(crate::utils::attachments::Attachment::CommandPermissions {
                allowed_tools, model: None,
            }) if allowed_tools.is_empty()
        ));
        crate::bootstrap::state::clear_invoked_skills_for_agent(agent);
    }
}
