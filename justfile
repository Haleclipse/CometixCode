# nyan task runner.
#
# Tests run under `cargo nextest`, which gives each test its own process.
# Measured on one commit, no test-code changes:
#
#   cargo test --lib                        530 failed   15s
#   cargo test --lib + the env pinning      522 failed   15s
#   cargo nextest run --lib                  23 failed  132s
#   just test                                19 failed  129s
#
# Nearly all of it is nextest: inside one process 164 files call `set_var` and
# 200 share `TEST_ENV_LOCK`, where one panic while holding it poisons the lock
# for every later test that unwraps it. Pinning the environment cannot fix
# tests stepping on each other; separate processes can. The cost is ~8x wall
# clock, which is worth it for a result you can act on.
#
# The env pinning below is worth the remaining handful, in both directions:
#   NYAN_CONFIG_DIR    redirects the settings root and the state.json inside
#                      it (utils/env.rs `get_global_state_file`), so tests stop
#                      asserting against the developer's own config.
#   privacy vars       gate analytics, which decides whether growthbook
#                      features are read at all (services/analytics/config.rs:6).
#                      A dev shell exporting CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC
#                      turned those tests into no-ops that passed for the wrong
#                      reason.
#   CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR
#                      short-circuits `reset_cwd_if_outside_project`
#                      (bash_tool/utils.rs:184) to always report the project
#                      root, whatever the shell actually did. A dev shell
#                      exporting it made `foreground_shell_reports_persistent_
#                      physical_cwd` fail against correct product behaviour —
#                      it read as a cwd bug for as long as it was in the
#                      known-failures list.
# It is recipe-scoped, so `cargo run` still uses the real config.
#
# What the pinning does NOT cover: `projectSettings`. Settings merge from five
# sources (utils/settings/mod.rs:1-8) and NYAN_CONFIG_DIR only moves the USER
# one. `projectSettings` is `${cwd}/.nyan/settings.json`, rooted at
# `get_original_cwd()` — process state, not an env var, so no recipe can pin it.
# A test that reads settings therefore reads THIS repository's
# `.nyan/settings.json`, which configures hooks; that is how a
# `UserPromptSubmit` assertion in process_user_input silently picked up the
# repo's own Trellis hook and failed. Tests that care must pin it themselves
# with `bootstrap::state::set_original_cwd(...)` plus a Drop guard that restores
# it — point it at the tracked `tests/fixtures/isolated-project`, the same
# hook-free workdir used by the recipes. `IsolatedProjectSettings` is the
# reference guard.
#
# Mutable harness state lives under `target/test-home`, so a clean checkout has
# every immutable input and `cargo clean` reliably resets generated state. The
# settings root and the global state file both live in the config home, so one
# variable covers both.

home    := justfile_directory() / "target/test-home"
project := justfile_directory() / "tests/fixtures/isolated-project"
test_runner := justfile_directory() / "scripts/ci/test.sh"

# The pinning layers (scratch config home, project-settings fixture, host
# model/gateway variables) live in `scripts/ci/test.sh` so CI runs the exact
# same harness. The recipes below are thin wrappers; the script's header is
# the reference for what each layer protects against. `home` and `project`
# stay declared because other recipes and docs reference them.
#
# Rebuild generated state; the script does this on every run, and keeping a
# standalone recipe means `just _prep` still works for callers that only want
# the scratch home rebuilt.
[private]
_prep:
    python3 {{quote(justfile_directory() / "scripts/ci/prepare-test-home.py")}} --root {{quote(justfile_directory())}}

default:
    @just --list

# Type/borrow check, tests included. The edit-loop companion to `just test`:
# it links nothing, so it is the fast way to close a refactor, but it proves
# only that the code compiles — it is NOT a gate.
#
# No env pinning: nothing here executes a test body, so the host state the
# test recipes pin cannot be read.
check *args:
    cargo check --lib --tests {{args}}

# Same, for the binary and its release profile.
check-release *args:
    cargo check --release {{args}}

# Full suite — the gate. Trustworthy, so batches no longer scope themselves to
# the directly-touched modules.
test *args:
    {{quote(test_runner)}} {{args}}

# Quick substring filter: `just t state::store`
t pattern:
    {{quote(test_runner)}} -E {{quote("test(/" + pattern + "/)")}}

# Full nextest filterset: `just test-one 'binary(nyan) and test(foo)'`
test-one filter:
    {{quote(test_runner)}} -E {{quote(filter)}}

# nextest does not run doctests.
test-doc:
    cargo test --doc

# --- Anthropic-internal build profile ---------------------------------------
#
# CC gates ~510 sites on `USER_TYPE === 'ant'`, which its bundler substitutes at
# build time (`"external" === 'ant'` in the external source). Nyan models it
# as a Cargo feature (utils/build_profile.rs). The recipes above only ever build
# `external`, which leaves a hole in the gate that the env pinning above does
# not cover: the audience is a THIRD axis, next to host state and process
# isolation.
#
# Measured 2026-08-05:
#   131  production gate sites
#    50  `#[cfg(feature = "anthropic_internal")]`  — NOT COMPILED by `just check`
#    13  `cfg!(feature = "anthropic_internal")`    — compiled, branch never taken
#    29  tests that exist only under the feature   — invisible to `just test`
#
# The 50 attribute-gated sites are the real hazard: a signature change in shared
# code cannot break them visibly, because rustc never sees them. Run `just
# check-ant` after any cross-cutting refactor; run `just test-ant` before
# claiming a batch is green.
check-ant *args:
    cargo check --lib --tests --features anthropic_internal {{args}}

test-ant *args:
    {{quote(test_runner)}} --features anthropic_internal {{args}}

# Substring filter under the internal build: `just t-ant sandbox_permission`.
# `test-ant *args` word-splits, so a filterset containing `(` needs this form.
t-ant pattern:
    {{quote(test_runner)}} --features anthropic_internal -E {{quote("test(/" + pattern + "/)")}}

# Both audiences, for a release-shaped gate. Runs BOTH even when the first
# fails — a bare `just test; just test-ant` aborts on the first non-zero exit,
# which would silently skip the audience this recipe exists to cover.
test-all-audiences:
    #!/usr/bin/env bash
    set -uo pipefail
    rc=0
    just test || rc=1
    just test-ant || rc=1
    exit $rc

# EMPTY as of 2026-08-05. `just test` is expected to be fully green; a red test
# is a red test.
#
# The list held five. NONE was a product defect — every one was a broken fixture
# or a leaked host variable, and each had been sitting behind a note telling the
# next person not to look at it:
#
#   validation_error_is_reported_as_non_blocking_failure
#     Fed syntactically invalid JSON and asserted on the schema-validation path.
#     CC separates the two: `parseHookOutput` catches a `jsonParse` throw and
#     returns bare `{ plainText }` (utils/hooks.ts:447-450), so malformed JSON
#     is ordinary output; only valid JSON failing the schema returns
#     `validationError` (:446). Rust already matched CC — the payload was wrong.
#   cwd_changed_updates_dynamic_paths_restarts_and_notifies
#   file_event_updates_dynamic_paths_and_reports_failed_output
#     No workspace-trust fixture, so both measured the hook executor's untrusted
#     early return (env.rs:165-175). "Expected 2 results, got 0" read as a
#     product bug for as long as the note stood.
#   foreground_shell_reports_persistent_physical_cwd
#     CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR leaked from the developer's
#     shell and short-circuits reset_cwd_if_outside_project. Cleared by the
#     test harness (scripts/ci/test.sh).
#   status_notice_context_uses_official_api_key_approval_for_conflict_source
#     `api_key_source` resolves through `load_global_config()` and the process
#     environment (auth.rs:534), not the arguments the test passed. The sibling
#     test in the same file documented this and injected both fixtures; this one
#     did neither.
#
# So: a failure that survives a rerun is a defect somewhere — in the product, the
# fixture, or the harness. Find which. Do not re-open this list to park it.
#
# Load-sensitive flakes (repl::permissions_*, repl_context_command,
# shell_command::pipe_mode) are a separate matter: they pass on an idle machine
# and fail under contention, so compare same-filter runs on a quiet machine
# before calling one a regression.
known-failures := ''

test-known-failures:
    @echo 'known-failures is empty — `just test` should be green. See the justfile note.'
