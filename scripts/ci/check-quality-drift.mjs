#!/usr/bin/env node
// Quality drift check for PRs.
//
// Prevents AI agents and contributors from weakening tests or test gates:
// 1. Prohibits introducing new `#[ignore]` attributes in code or tests.
//    (Bypassing failing tests by ignoring them is forbidden; fix the defect instead.)
// 2. Prohibits reading or writing new `CLAUDE_*` / `ANTHROPIC_*` environment
//    variables. nyan's switches live in the `NYAN_*` namespace; the names
//    below still belong to Anthropic-account features being removed.
//
// Usage (CI):   node scripts/ci/check-quality-drift.mjs
// Usage (local): node scripts/ci/check-quality-drift.mjs --base master --head HEAD

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

function arg(name) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? undefined : process.argv[index + 1];
}

function endpoints() {
  const event = process.env.GITHUB_EVENT_NAME ?? "";
  const eventPath = process.env.GITHUB_EVENT_PATH;
  const payload = eventPath ? JSON.parse(readFileSync(eventPath, "utf8")) : {};

  const fromArgs = { base: arg("base"), head: arg("head") };
  if (fromArgs.base && fromArgs.head) return fromArgs;

  switch (event) {
    case "pull_request":
      return {
        base: payload.pull_request?.base?.sha,
        head: payload.pull_request?.head?.sha,
      };
    case "push":
      return { base: payload.before, head: payload.after };
    case "merge_group":
      return { base: payload.merge_group?.base_sha, head: payload.merge_group?.head_sha };
    default:
      return {};
  }
}

const zero = /^0+$/;

// Anthropic/CC environment names still present while their features are removed.
const LEGACY_ENV_ALLOW = new Set([
  // Provider variables tests still set to prove nyan ignores the ambient
  // Anthropic environment.
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "ANTHROPIC_BETAS",
  "ANTHROPIC_CUSTOM_HEADERS",
  "ANTHROPIC_CUSTOM_MODEL_OPTION",
  "ANTHROPIC_FOUNDRY_API_KEY",
  "ANTHROPIC_MODEL",
  "ANTHROPIC_SMALL_FAST_MODEL",
  "ANTHROPIC_UNIX_SOCKET",
  "CLAUDE_AGENT_SDK_CLIENT_APP",
  "CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS",
  "CLAUDE_AGENT_SDK_MCP_NO_PREFIX",
  "CLAUDE_AGENT_SDK_VERSION",
  "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
  "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
  "CLAUDE_CODE_CONTAINER_ID",
  "CLAUDE_CODE_CUSTOM_OAUTH_URL",
  "CLAUDE_CODE_ENVIRONMENT_KIND",
  "CLAUDE_CODE_IS_COWORK",
  "CLAUDE_CODE_OAUTH_CLIENT_ID",
  "CLAUDE_CODE_OAUTH_TOKEN",
  "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
  "CLAUDE_CODE_ORGANIZATION_UUID",
  "CLAUDE_CODE_REMOTE",
  "CLAUDE_CODE_REMOTE_MEMORY_DIR",
  "CLAUDE_CODE_REMOTE_SESSION_ID",
  "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
  "CLAUDE_CODE_USE_COWORK_PLUGINS",
  "CLAUDE_COWORK_MEMORY_EXTRA_GUIDELINES",
  "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
  "CLAUDE_LOCAL_OAUTH_API_BASE",
  "CLAUDE_LOCAL_OAUTH_APPS_BASE",
  "CLAUDE_LOCAL_OAUTH_CONSOLE_BASE",
  "CLAUDE_MOCK_HEADERLESS_429",
  "CLAUDE_OVERRIDE_SENTINEL",
  "CLAUDE_SESSION_INGRESS_TOKEN_FILE",
  "CLAUDE_SIG",
]);

const envAccess =
  /(?:process_env::(?:var|var_os|set|remove)|EnvVarGuard::(?:set|unset|preserve))\(\s*"((?:CLAUDE|ANTHROPIC)_[A-Z0-9_]+)"/;

function diffLines() {
  const { base, head } = endpoints();
  if (!base || !head || zero.test(base) || zero.test(head)) return null;
  try {
    return execFileSync("git", ["diff", "--unified=0", `${base}...${head}`, "--", "src", "tests"], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch (error) {
    console.error(`quality-drift: git diff failed: ${error.message}`);
    return null;
  }
}

function main() {
  const diff = diffLines();
  if (!diff) {
    console.log("quality-drift: no comparative diff to analyze, skipping.");
    return;
  }

  const violations = [];
  const lines = diff.split("\n");

  for (const line of lines) {
    // Only check added lines; ignore file diff header `+++`
    if (!line.startsWith("+") || line.startsWith("+++")) continue;

    const content = line.slice(1).trim();
    if (content.startsWith("//") || content.startsWith("/*")) continue;

    // Detect new #[ignore]
    if (/^#\s*\[\s*ignore\b/.test(content)) {
      violations.push(`Adding '#[ignore]' attribute is forbidden: "${content}"`);
    }

    // Detect new CC/Anthropic environment names in code.
    const env = envAccess.exec(content);
    if (env && !LEGACY_ENV_ALLOW.has(env[1])) {
      violations.push(
        `Use the NYAN_* namespace instead of "${env[1]}": "${content}"`,
      );
    }
  }

  if (violations.length > 0) {
    console.error("quality-drift: FAILED — test gate weakening detected:");
    for (const v of violations) {
      console.error(`  - ${v}`);
    }
    console.error(
      "\nResolution: fix the failing test or underlying defect instead of ignoring it; use NYAN_* for nyan switches.",
    );
    process.exit(1);
  }

  console.log("quality-drift: passed (no test gate weakening detected).");
}

main();
