#!/usr/bin/env bash
# One test entrypoint for the developer machine and CI.
#
# It owns the three layers of host-state pinning the justfile used to inline,
# so `just test` and the shard jobs cannot drift apart:
#
#   settings root   NYAN_CONFIG_DIR -> target/test-home (also moves
#                   state.json, which lives inside it)
#   project settings NYAN_TEST_PROJECT_DIR -> tracked hook-free fixture
#                   (projectSettings is process state, not reachable by env)
#   host session    empty privacy vars, and the model/gateway variables a
#                   host Claude Code session injects are removed — empty
#                   strings would be adopted as model NAMES (justfile:86)
#
# Usage:
#   scripts/ci/test.sh [--archive FILE] [--partition SPEC] [nextest args...]
#
#   --archive    run a `cargo nextest archive` instead of building; the
#                extraction directory is created because nextest does not
#                create it
#   --partition  hash:k/n shard selection
#
# In CI (CI=true) the `ci` nextest profile is selected, which writes
# target/nextest/ci/junit.xml and disables retries. Locally the default
# profile is kept, so `just test` behaves exactly as before. The profile is
# resolved before CI is removed from the environment: the variable itself
# changes product behaviour (`auth.rs` short-circuits API-key resolution under
# CI), so the tests must not inherit it.

set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

archive=""
partition=""
extra=()
while (($#)); do
    case "$1" in
        --archive) archive="$2"; shift 2 ;;
        --archive=*) archive="${1#*=}"; shift ;;
        --partition) partition="$2"; shift 2 ;;
        --partition=*) partition="${1#*=}"; shift ;;
        *) extra+=("$1"); shift ;;
    esac
done

ci_profile="${CI:-}"

python3 "$root/scripts/ci/prepare-test-home.py" --root "$root" >/dev/null

export NYAN_CONFIG_DIR="$root/target/test-home"
export NYAN_DISABLE_NONESSENTIAL_TRAFFIC=
export NODE_ENV=
export NYAN_BASH_MAINTAIN_PROJECT_WORKING_DIR=
export NYAN_TEST_PROJECT_DIR="$root/tests/fixtures/isolated-project"

# Not `VAR=`: the model resolvers read these without an is-empty filter, so an
# empty value becomes a model name. They must be absent. Provider model
# selection comes only from settings, so ambient provider variables are gone.
unset NYAN_MODEL BAT_THEME

# A hosted runner has no TTY and usually no TERM, so crossterm's size probe
# falls back to tput, which fails without a terminal database lookup; the
# render loop then lays out at width 0. Pin the dimensions the suite is
# verified against (`tput` honours COLUMNS/LINES), so a shard renders exactly
# like a developer terminal and does not depend on runner state.
export TERM=xterm-256color
export COLUMNS=80
export LINES=24

# Locale and zone drift would otherwise leak into collation (`sys_locale`),
# timestamps and `Local::now()` assertions. C.UTF-8 maps to the en-US collator
# in `javascript_locale_compare`, the same ordering the suite is written
# against.
export LANG=C.UTF-8
export TZ=UTC

# Product code branches on CI (auth resolution in particular), so a hosted
# shard must not run with it set. Tests that need the CI branch set it through
# EnvVarGuard themselves.
unset CI

args=(cargo nextest run --no-fail-fast)
if [[ -z "$archive" ]]; then
    args+=(--lib)
fi
if [[ -n "$ci_profile" ]]; then
    args+=(--profile ci)
fi
if [[ -n "$archive" ]]; then
    # nextest refuses to extract over an existing destination, and this
    # directory is ours alone (a prior local shard run leaves it behind).
    rm -rf "$root/.ci-extract"
    mkdir -p "$root/.ci-extract"
    args+=(--archive-file "$archive" --workspace-remap "$root" --extract-to "$root/.ci-extract")
fi
if [[ -n "$partition" ]]; then
    args+=(--partition "$partition")
fi
args+=("${extra[@]}")

exec "${args[@]}"
