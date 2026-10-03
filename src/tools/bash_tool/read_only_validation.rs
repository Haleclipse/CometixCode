//! Read-only constraint validation for Bash commands.
//! Maps to: CC `tools/BashTool/readOnlyValidation.ts`.
//!
//! CC's `checkReadOnlyConstraints` (:1876) parses the command with
//! shell-quote, rejects operator tokens and `$` expansion
//! (`isCommandSafeViaFlagParsing` :1246), then validates every subcommand
//! against per-flag configs: `COMMAND_ALLOWLIST` (:128),
//! `READONLY_COMMANDS` (:1432), and `GIT_READ_ONLY_COMMANDS`
//! (`utils/shell/readOnlyCommandValidation.ts`:107).
//!
//! The parser below mirrors the production legacy path: quoted operators stay
//! in argv, while unquoted pipelines/compounds are split and every leaf must
//! independently satisfy the complete private + shared flag maps. Unknown
//! syntax fails closed.

use regex::Regex;
use std::sync::LazyLock;

const PRIVATE_COMMAND_NAMES: &[&str] = &[
    "xargs",
    "file",
    "sed",
    "sort",
    "man",
    "help",
    "netstat",
    "ps",
    "base64",
    "grep",
    "sha256sum",
    "sha1sum",
    "md5sum",
    "tree",
    "date",
    "hostname",
    "info",
    "lsof",
    "pgrep",
    "tput",
    "ss",
    "fd",
    "fdfind",
];
const ANT_PRIVATE_COMMAND_NAMES: &[&str] = &["aki"];

fn private_command_config(
    words: &[String],
) -> Option<(
    &'static str,
    usize,
    crate::utils::shell::read_only_command_validation::CommandConfig,
)> {
    use crate::utils::shell::read_only_command_validation::{CommandConfig, FlagArgType};
    if words.get(0).map(String::as_str) == Some("xargs") {
        return Some((
            "xargs",
            1,
            CommandConfig {
                flags: &[
                    ("-I", FlagArgType::Braces),
                    ("-n", FlagArgType::Number),
                    ("-P", FlagArgType::Number),
                    ("-L", FlagArgType::Number),
                    ("-s", FlagArgType::Number),
                    ("-E", FlagArgType::Eof),
                    ("-0", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("-x", FlagArgType::None),
                    ("-d", FlagArgType::Char),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("file") {
        return Some((
            "file",
            1,
            CommandConfig {
                flags: &[
                    ("--brief", FlagArgType::None),
                    ("-b", FlagArgType::None),
                    ("--mime", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--mime-type", FlagArgType::None),
                    ("--mime-encoding", FlagArgType::None),
                    ("--apple", FlagArgType::None),
                    ("--check-encoding", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--exclude", FlagArgType::String),
                    ("--exclude-quiet", FlagArgType::String),
                    ("--print0", FlagArgType::None),
                    ("-0", FlagArgType::None),
                    ("-f", FlagArgType::String),
                    ("-F", FlagArgType::String),
                    ("--separator", FlagArgType::String),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("--no-dereference", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--dereference", FlagArgType::None),
                    ("-L", FlagArgType::None),
                    ("--magic-file", FlagArgType::String),
                    ("-m", FlagArgType::String),
                    ("--keep-going", FlagArgType::None),
                    ("-k", FlagArgType::None),
                    ("--list", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("--no-buffer", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("--preserve-date", FlagArgType::None),
                    ("-p", FlagArgType::None),
                    ("--raw", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--special-files", FlagArgType::None),
                    ("--uncompress", FlagArgType::None),
                    ("-z", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("sed") {
        return Some((
            "sed",
            1,
            CommandConfig {
                flags: &[
                    ("--expression", FlagArgType::String),
                    ("-e", FlagArgType::String),
                    ("--quiet", FlagArgType::None),
                    ("--silent", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("--regexp-extended", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("--posix", FlagArgType::None),
                    ("-E", FlagArgType::None),
                    ("--line-length", FlagArgType::Number),
                    ("-l", FlagArgType::Number),
                    ("--zero-terminated", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--separate", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--unbuffered", FlagArgType::None),
                    ("-u", FlagArgType::None),
                    ("--debug", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("sort") {
        return Some((
            "sort",
            1,
            CommandConfig {
                flags: &[
                    ("--ignore-leading-blanks", FlagArgType::None),
                    ("-b", FlagArgType::None),
                    ("--dictionary-order", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("--ignore-case", FlagArgType::None),
                    ("-f", FlagArgType::None),
                    ("--general-numeric-sort", FlagArgType::None),
                    ("-g", FlagArgType::None),
                    ("--human-numeric-sort", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--ignore-nonprinting", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--month-sort", FlagArgType::None),
                    ("-M", FlagArgType::None),
                    ("--numeric-sort", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("--random-sort", FlagArgType::None),
                    ("-R", FlagArgType::None),
                    ("--reverse", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("--sort", FlagArgType::String),
                    ("--stable", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--unique", FlagArgType::None),
                    ("-u", FlagArgType::None),
                    ("--version-sort", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--zero-terminated", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--key", FlagArgType::String),
                    ("-k", FlagArgType::String),
                    ("--field-separator", FlagArgType::String),
                    ("-t", FlagArgType::String),
                    ("--check", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--check-char-order", FlagArgType::None),
                    ("-C", FlagArgType::None),
                    ("--merge", FlagArgType::None),
                    ("-m", FlagArgType::None),
                    ("--buffer-size", FlagArgType::String),
                    ("-S", FlagArgType::String),
                    ("--parallel", FlagArgType::Number),
                    ("--batch-size", FlagArgType::Number),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("man") {
        return Some((
            "man",
            1,
            CommandConfig {
                flags: &[
                    ("-a", FlagArgType::None),
                    ("--all", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("-f", FlagArgType::None),
                    ("--whatis", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("-k", FlagArgType::None),
                    ("--apropos", FlagArgType::None),
                    ("-l", FlagArgType::String),
                    ("-w", FlagArgType::None),
                    ("-S", FlagArgType::String),
                    ("-s", FlagArgType::String),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("help") {
        return Some((
            "help",
            1,
            CommandConfig {
                flags: &[
                    ("-d", FlagArgType::None),
                    ("-m", FlagArgType::None),
                    ("-s", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("netstat") {
        return Some((
            "netstat",
            1,
            CommandConfig {
                flags: &[
                    ("-a", FlagArgType::None),
                    ("-L", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("-f", FlagArgType::String),
                    ("-g", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("-I", FlagArgType::String),
                    ("-s", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("-m", FlagArgType::None),
                    ("-v", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("ps") {
        return Some((
            "ps",
            1,
            CommandConfig {
                flags: &[
                    ("-e", FlagArgType::None),
                    ("-A", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("-N", FlagArgType::None),
                    ("--deselect", FlagArgType::None),
                    ("-f", FlagArgType::None),
                    ("-F", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("-j", FlagArgType::None),
                    ("-y", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("-ww", FlagArgType::None),
                    ("--width", FlagArgType::Number),
                    ("-c", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("--forest", FlagArgType::None),
                    ("--headers", FlagArgType::None),
                    ("--no-headers", FlagArgType::None),
                    ("-n", FlagArgType::String),
                    ("--sort", FlagArgType::String),
                    ("-L", FlagArgType::None),
                    ("-T", FlagArgType::None),
                    ("-m", FlagArgType::None),
                    ("-C", FlagArgType::String),
                    ("-G", FlagArgType::String),
                    ("-g", FlagArgType::String),
                    ("-p", FlagArgType::String),
                    ("--pid", FlagArgType::String),
                    ("-q", FlagArgType::String),
                    ("--quick-pid", FlagArgType::String),
                    ("-s", FlagArgType::String),
                    ("--sid", FlagArgType::String),
                    ("-t", FlagArgType::String),
                    ("--tty", FlagArgType::String),
                    ("-U", FlagArgType::String),
                    ("-u", FlagArgType::String),
                    ("--user", FlagArgType::String),
                    ("--help", FlagArgType::None),
                    ("--info", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("base64") {
        return Some((
            "base64",
            1,
            CommandConfig {
                flags: &[
                    ("-d", FlagArgType::None),
                    ("-D", FlagArgType::None),
                    ("--decode", FlagArgType::None),
                    ("-b", FlagArgType::Number),
                    ("--break", FlagArgType::Number),
                    ("-w", FlagArgType::Number),
                    ("--wrap", FlagArgType::Number),
                    ("-i", FlagArgType::String),
                    ("--input", FlagArgType::String),
                    ("--ignore-garbage", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: false,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("grep") {
        return Some((
            "grep",
            1,
            CommandConfig {
                flags: &[
                    ("-e", FlagArgType::String),
                    ("--regexp", FlagArgType::String),
                    ("-f", FlagArgType::String),
                    ("--file", FlagArgType::String),
                    ("-F", FlagArgType::None),
                    ("--fixed-strings", FlagArgType::None),
                    ("-G", FlagArgType::None),
                    ("--basic-regexp", FlagArgType::None),
                    ("-E", FlagArgType::None),
                    ("--extended-regexp", FlagArgType::None),
                    ("-P", FlagArgType::None),
                    ("--perl-regexp", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--ignore-case", FlagArgType::None),
                    ("--no-ignore-case", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("--invert-match", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--word-regexp", FlagArgType::None),
                    ("-x", FlagArgType::None),
                    ("--line-regexp", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--count", FlagArgType::None),
                    ("--color", FlagArgType::String),
                    ("--colour", FlagArgType::String),
                    ("-L", FlagArgType::None),
                    ("--files-without-match", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("--files-with-matches", FlagArgType::None),
                    ("-m", FlagArgType::Number),
                    ("--max-count", FlagArgType::Number),
                    ("-o", FlagArgType::None),
                    ("--only-matching", FlagArgType::None),
                    ("-q", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--silent", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--no-messages", FlagArgType::None),
                    ("-b", FlagArgType::None),
                    ("--byte-offset", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("--with-filename", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--no-filename", FlagArgType::None),
                    ("--label", FlagArgType::String),
                    ("-n", FlagArgType::None),
                    ("--line-number", FlagArgType::None),
                    ("-T", FlagArgType::None),
                    ("--initial-tab", FlagArgType::None),
                    ("-u", FlagArgType::None),
                    ("--unix-byte-offsets", FlagArgType::None),
                    ("-Z", FlagArgType::None),
                    ("--null", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--null-data", FlagArgType::None),
                    ("-A", FlagArgType::Number),
                    ("--after-context", FlagArgType::Number),
                    ("-B", FlagArgType::Number),
                    ("--before-context", FlagArgType::Number),
                    ("-C", FlagArgType::Number),
                    ("--context", FlagArgType::Number),
                    ("--group-separator", FlagArgType::String),
                    ("--no-group-separator", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--text", FlagArgType::None),
                    ("--binary-files", FlagArgType::String),
                    ("-D", FlagArgType::String),
                    ("--devices", FlagArgType::String),
                    ("-d", FlagArgType::String),
                    ("--directories", FlagArgType::String),
                    ("--exclude", FlagArgType::String),
                    ("--exclude-from", FlagArgType::String),
                    ("--exclude-dir", FlagArgType::String),
                    ("--include", FlagArgType::String),
                    ("-r", FlagArgType::None),
                    ("--recursive", FlagArgType::None),
                    ("-R", FlagArgType::None),
                    ("--dereference-recursive", FlagArgType::None),
                    ("--line-buffered", FlagArgType::None),
                    ("-U", FlagArgType::None),
                    ("--binary", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("sha256sum") {
        return Some((
            "sha256sum",
            1,
            CommandConfig {
                flags: &[
                    ("-b", FlagArgType::None),
                    ("--binary", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("--text", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--check", FlagArgType::None),
                    ("--ignore-missing", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--status", FlagArgType::None),
                    ("--strict", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--warn", FlagArgType::None),
                    ("--tag", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--zero", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("sha1sum") {
        return Some((
            "sha1sum",
            1,
            CommandConfig {
                flags: &[
                    ("-b", FlagArgType::None),
                    ("--binary", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("--text", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--check", FlagArgType::None),
                    ("--ignore-missing", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--status", FlagArgType::None),
                    ("--strict", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--warn", FlagArgType::None),
                    ("--tag", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--zero", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("md5sum") {
        return Some((
            "md5sum",
            1,
            CommandConfig {
                flags: &[
                    ("-b", FlagArgType::None),
                    ("--binary", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("--text", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--check", FlagArgType::None),
                    ("--ignore-missing", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--status", FlagArgType::None),
                    ("--strict", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--warn", FlagArgType::None),
                    ("--tag", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--zero", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("tree") {
        return Some((
            "tree",
            1,
            CommandConfig {
                flags: &[
                    ("-a", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("-f", FlagArgType::None),
                    ("-x", FlagArgType::None),
                    ("-L", FlagArgType::Number),
                    ("-P", FlagArgType::String),
                    ("-I", FlagArgType::String),
                    ("--gitignore", FlagArgType::None),
                    ("--gitfile", FlagArgType::String),
                    ("--ignore-case", FlagArgType::None),
                    ("--matchdirs", FlagArgType::None),
                    ("--metafirst", FlagArgType::None),
                    ("--prune", FlagArgType::None),
                    ("--info", FlagArgType::None),
                    ("--infofile", FlagArgType::String),
                    ("--noreport", FlagArgType::None),
                    ("--charset", FlagArgType::String),
                    ("--filelimit", FlagArgType::Number),
                    ("-q", FlagArgType::None),
                    ("-N", FlagArgType::None),
                    ("-Q", FlagArgType::None),
                    ("-p", FlagArgType::None),
                    ("-u", FlagArgType::None),
                    ("-g", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--si", FlagArgType::None),
                    ("--du", FlagArgType::None),
                    ("-D", FlagArgType::None),
                    ("--timefmt", FlagArgType::String),
                    ("-F", FlagArgType::None),
                    ("--inodes", FlagArgType::None),
                    ("--device", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("-U", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("--dirsfirst", FlagArgType::None),
                    ("--filesfirst", FlagArgType::None),
                    ("--sort", FlagArgType::String),
                    ("-i", FlagArgType::None),
                    ("-A", FlagArgType::None),
                    ("-S", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("-C", FlagArgType::None),
                    ("-X", FlagArgType::None),
                    ("-J", FlagArgType::None),
                    ("-H", FlagArgType::String),
                    ("--nolinks", FlagArgType::None),
                    ("--hintro", FlagArgType::String),
                    ("--houtro", FlagArgType::String),
                    ("-T", FlagArgType::String),
                    ("--hyperlink", FlagArgType::None),
                    ("--scheme", FlagArgType::String),
                    ("--authority", FlagArgType::String),
                    ("--fromfile", FlagArgType::None),
                    ("--fromtabfile", FlagArgType::None),
                    ("--fflinks", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("date") {
        return Some((
            "date",
            1,
            CommandConfig {
                flags: &[
                    ("-d", FlagArgType::String),
                    ("--date", FlagArgType::String),
                    ("-r", FlagArgType::String),
                    ("--reference", FlagArgType::String),
                    ("-u", FlagArgType::None),
                    ("--utc", FlagArgType::None),
                    ("--universal", FlagArgType::None),
                    ("-I", FlagArgType::None),
                    ("--iso-8601", FlagArgType::String),
                    ("-R", FlagArgType::None),
                    ("--rfc-email", FlagArgType::None),
                    ("--rfc-3339", FlagArgType::String),
                    ("--debug", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("hostname") {
        return Some((
            "hostname",
            1,
            CommandConfig {
                flags: &[
                    ("-f", FlagArgType::None),
                    ("--fqdn", FlagArgType::None),
                    ("--long", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--short", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--ip-address", FlagArgType::None),
                    ("-I", FlagArgType::None),
                    ("--all-ip-addresses", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--alias", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("--domain", FlagArgType::None),
                    ("-A", FlagArgType::None),
                    ("--all-fqdns", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("--verbose", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("info") {
        return Some((
            "info",
            1,
            CommandConfig {
                flags: &[
                    ("-f", FlagArgType::String),
                    ("--file", FlagArgType::String),
                    ("-d", FlagArgType::String),
                    ("--directory", FlagArgType::String),
                    ("-n", FlagArgType::String),
                    ("--node", FlagArgType::String),
                    ("-a", FlagArgType::None),
                    ("--all", FlagArgType::None),
                    ("-k", FlagArgType::String),
                    ("--apropos", FlagArgType::String),
                    ("-w", FlagArgType::None),
                    ("--where", FlagArgType::None),
                    ("--location", FlagArgType::None),
                    ("--show-options", FlagArgType::None),
                    ("--vi-keys", FlagArgType::None),
                    ("--subnodes", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("--usage", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("lsof") {
        return Some((
            "lsof",
            1,
            CommandConfig {
                flags: &[
                    ("-?", FlagArgType::None),
                    ("-h", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("-b", FlagArgType::None),
                    ("-C", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("-N", FlagArgType::None),
                    ("-O", FlagArgType::None),
                    ("-P", FlagArgType::None),
                    ("-Q", FlagArgType::None),
                    ("-R", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("-U", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("-X", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("-E", FlagArgType::None),
                    ("-F", FlagArgType::None),
                    ("-g", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("-K", FlagArgType::None),
                    ("-L", FlagArgType::None),
                    ("-o", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("-S", FlagArgType::None),
                    ("-T", FlagArgType::None),
                    ("-x", FlagArgType::None),
                    ("-A", FlagArgType::String),
                    ("-c", FlagArgType::String),
                    ("-d", FlagArgType::String),
                    ("-e", FlagArgType::String),
                    ("-k", FlagArgType::String),
                    ("-p", FlagArgType::String),
                    ("-u", FlagArgType::String),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("pgrep") {
        return Some((
            "pgrep",
            1,
            CommandConfig {
                flags: &[
                    ("-d", FlagArgType::String),
                    ("--delimiter", FlagArgType::String),
                    ("-l", FlagArgType::None),
                    ("--list-name", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--list-full", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("--inverse", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--lightweight", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--count", FlagArgType::None),
                    ("-f", FlagArgType::None),
                    ("--full", FlagArgType::None),
                    ("-g", FlagArgType::String),
                    ("--pgroup", FlagArgType::String),
                    ("-G", FlagArgType::String),
                    ("--group", FlagArgType::String),
                    ("-i", FlagArgType::None),
                    ("--ignore-case", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("--newest", FlagArgType::None),
                    ("-o", FlagArgType::None),
                    ("--oldest", FlagArgType::None),
                    ("-O", FlagArgType::String),
                    ("--older", FlagArgType::String),
                    ("-P", FlagArgType::String),
                    ("--parent", FlagArgType::String),
                    ("-s", FlagArgType::String),
                    ("--session", FlagArgType::String),
                    ("-t", FlagArgType::String),
                    ("--terminal", FlagArgType::String),
                    ("-u", FlagArgType::String),
                    ("--euid", FlagArgType::String),
                    ("-U", FlagArgType::String),
                    ("--uid", FlagArgType::String),
                    ("-x", FlagArgType::None),
                    ("--exact", FlagArgType::None),
                    ("-F", FlagArgType::String),
                    ("--pidfile", FlagArgType::String),
                    ("-L", FlagArgType::None),
                    ("--logpidfile", FlagArgType::None),
                    ("-r", FlagArgType::String),
                    ("--runstates", FlagArgType::String),
                    ("--ns", FlagArgType::String),
                    ("--nslist", FlagArgType::String),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("tput") {
        return Some((
            "tput",
            1,
            CommandConfig {
                flags: &[
                    ("-T", FlagArgType::String),
                    ("-V", FlagArgType::None),
                    ("-x", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("ss") {
        return Some((
            "ss",
            1,
            CommandConfig {
                flags: &[
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                    ("-n", FlagArgType::None),
                    ("--numeric", FlagArgType::None),
                    ("-r", FlagArgType::None),
                    ("--resolve", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--all", FlagArgType::None),
                    ("-l", FlagArgType::None),
                    ("--listening", FlagArgType::None),
                    ("-o", FlagArgType::None),
                    ("--options", FlagArgType::None),
                    ("-e", FlagArgType::None),
                    ("--extended", FlagArgType::None),
                    ("-m", FlagArgType::None),
                    ("--memory", FlagArgType::None),
                    ("-p", FlagArgType::None),
                    ("--processes", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--info", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--summary", FlagArgType::None),
                    ("-4", FlagArgType::None),
                    ("--ipv4", FlagArgType::None),
                    ("-6", FlagArgType::None),
                    ("--ipv6", FlagArgType::None),
                    ("-0", FlagArgType::None),
                    ("--packet", FlagArgType::None),
                    ("-t", FlagArgType::None),
                    ("--tcp", FlagArgType::None),
                    ("-M", FlagArgType::None),
                    ("--mptcp", FlagArgType::None),
                    ("-S", FlagArgType::None),
                    ("--sctp", FlagArgType::None),
                    ("-u", FlagArgType::None),
                    ("--udp", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("--dccp", FlagArgType::None),
                    ("-w", FlagArgType::None),
                    ("--raw", FlagArgType::None),
                    ("-x", FlagArgType::None),
                    ("--unix", FlagArgType::None),
                    ("--tipc", FlagArgType::None),
                    ("--vsock", FlagArgType::None),
                    ("-f", FlagArgType::String),
                    ("--family", FlagArgType::String),
                    ("-A", FlagArgType::String),
                    ("--query", FlagArgType::String),
                    ("--socket", FlagArgType::String),
                    ("-Z", FlagArgType::None),
                    ("--context", FlagArgType::None),
                    ("-z", FlagArgType::None),
                    ("--contexts", FlagArgType::None),
                    ("-b", FlagArgType::None),
                    ("--bpf", FlagArgType::None),
                    ("-E", FlagArgType::None),
                    ("--events", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("--no-header", FlagArgType::None),
                    ("-O", FlagArgType::None),
                    ("--oneline", FlagArgType::None),
                    ("--tipcinfo", FlagArgType::None),
                    ("--tos", FlagArgType::None),
                    ("--cgroup", FlagArgType::None),
                    ("--inet-sockopt", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("fd") {
        return Some((
            "fd",
            1,
            CommandConfig {
                flags: &[
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("--hidden", FlagArgType::None),
                    ("-I", FlagArgType::None),
                    ("--no-ignore", FlagArgType::None),
                    ("--no-ignore-vcs", FlagArgType::None),
                    ("--no-ignore-parent", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--case-sensitive", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--ignore-case", FlagArgType::None),
                    ("-g", FlagArgType::None),
                    ("--glob", FlagArgType::None),
                    ("--regex", FlagArgType::None),
                    ("-F", FlagArgType::None),
                    ("--fixed-strings", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--absolute-path", FlagArgType::None),
                    ("-L", FlagArgType::None),
                    ("--follow", FlagArgType::None),
                    ("-p", FlagArgType::None),
                    ("--full-path", FlagArgType::None),
                    ("-0", FlagArgType::None),
                    ("--print0", FlagArgType::None),
                    ("-d", FlagArgType::Number),
                    ("--max-depth", FlagArgType::Number),
                    ("--min-depth", FlagArgType::Number),
                    ("--exact-depth", FlagArgType::Number),
                    ("-t", FlagArgType::String),
                    ("--type", FlagArgType::String),
                    ("-e", FlagArgType::String),
                    ("--extension", FlagArgType::String),
                    ("-S", FlagArgType::String),
                    ("--size", FlagArgType::String),
                    ("--changed-within", FlagArgType::String),
                    ("--changed-before", FlagArgType::String),
                    ("-o", FlagArgType::String),
                    ("--owner", FlagArgType::String),
                    ("-E", FlagArgType::String),
                    ("--exclude", FlagArgType::String),
                    ("--ignore-file", FlagArgType::String),
                    ("-c", FlagArgType::String),
                    ("--color", FlagArgType::String),
                    ("-j", FlagArgType::Number),
                    ("--threads", FlagArgType::Number),
                    ("--max-buffer-time", FlagArgType::String),
                    ("--max-results", FlagArgType::Number),
                    ("-1", FlagArgType::None),
                    ("-q", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--show-errors", FlagArgType::None),
                    ("--strip-cwd-prefix", FlagArgType::None),
                    ("--one-file-system", FlagArgType::None),
                    ("--prune", FlagArgType::None),
                    ("--search-path", FlagArgType::String),
                    ("--base-directory", FlagArgType::String),
                    ("--path-separator", FlagArgType::String),
                    ("--batch-size", FlagArgType::Number),
                    ("--no-require-git", FlagArgType::None),
                    ("--hyperlink", FlagArgType::String),
                    ("--and", FlagArgType::String),
                    ("--format", FlagArgType::String),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if words.get(0).map(String::as_str) == Some("fdfind") {
        return Some((
            "fdfind",
            1,
            CommandConfig {
                flags: &[
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-V", FlagArgType::None),
                    ("--version", FlagArgType::None),
                    ("-H", FlagArgType::None),
                    ("--hidden", FlagArgType::None),
                    ("-I", FlagArgType::None),
                    ("--no-ignore", FlagArgType::None),
                    ("--no-ignore-vcs", FlagArgType::None),
                    ("--no-ignore-parent", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--case-sensitive", FlagArgType::None),
                    ("-i", FlagArgType::None),
                    ("--ignore-case", FlagArgType::None),
                    ("-g", FlagArgType::None),
                    ("--glob", FlagArgType::None),
                    ("--regex", FlagArgType::None),
                    ("-F", FlagArgType::None),
                    ("--fixed-strings", FlagArgType::None),
                    ("-a", FlagArgType::None),
                    ("--absolute-path", FlagArgType::None),
                    ("-L", FlagArgType::None),
                    ("--follow", FlagArgType::None),
                    ("-p", FlagArgType::None),
                    ("--full-path", FlagArgType::None),
                    ("-0", FlagArgType::None),
                    ("--print0", FlagArgType::None),
                    ("-d", FlagArgType::Number),
                    ("--max-depth", FlagArgType::Number),
                    ("--min-depth", FlagArgType::Number),
                    ("--exact-depth", FlagArgType::Number),
                    ("-t", FlagArgType::String),
                    ("--type", FlagArgType::String),
                    ("-e", FlagArgType::String),
                    ("--extension", FlagArgType::String),
                    ("-S", FlagArgType::String),
                    ("--size", FlagArgType::String),
                    ("--changed-within", FlagArgType::String),
                    ("--changed-before", FlagArgType::String),
                    ("-o", FlagArgType::String),
                    ("--owner", FlagArgType::String),
                    ("-E", FlagArgType::String),
                    ("--exclude", FlagArgType::String),
                    ("--ignore-file", FlagArgType::String),
                    ("-c", FlagArgType::String),
                    ("--color", FlagArgType::String),
                    ("-j", FlagArgType::Number),
                    ("--threads", FlagArgType::Number),
                    ("--max-buffer-time", FlagArgType::String),
                    ("--max-results", FlagArgType::Number),
                    ("-1", FlagArgType::None),
                    ("-q", FlagArgType::None),
                    ("--quiet", FlagArgType::None),
                    ("--show-errors", FlagArgType::None),
                    ("--strip-cwd-prefix", FlagArgType::None),
                    ("--one-file-system", FlagArgType::None),
                    ("--prune", FlagArgType::None),
                    ("--search-path", FlagArgType::String),
                    ("--base-directory", FlagArgType::String),
                    ("--path-separator", FlagArgType::String),
                    ("--batch-size", FlagArgType::Number),
                    ("--no-require-git", FlagArgType::None),
                    ("--hyperlink", FlagArgType::String),
                    ("--and", FlagArgType::String),
                    ("--format", FlagArgType::String),
                ],
                respects_double_dash: true,
            },
        ));
    }
    if cfg!(feature = "anthropic_internal") && words.get(0).map(String::as_str) == Some("aki") {
        return Some((
            "aki",
            1,
            CommandConfig {
                flags: &[
                    ("-h", FlagArgType::None),
                    ("--help", FlagArgType::None),
                    ("-k", FlagArgType::None),
                    ("--keyword", FlagArgType::None),
                    ("-s", FlagArgType::None),
                    ("--semantic", FlagArgType::None),
                    ("--no-adaptive", FlagArgType::None),
                    ("-n", FlagArgType::Number),
                    ("--limit", FlagArgType::Number),
                    ("-o", FlagArgType::Number),
                    ("--offset", FlagArgType::Number),
                    ("--source", FlagArgType::String),
                    ("--exclude-source", FlagArgType::String),
                    ("-a", FlagArgType::String),
                    ("--after", FlagArgType::String),
                    ("-b", FlagArgType::String),
                    ("--before", FlagArgType::String),
                    ("--collection", FlagArgType::String),
                    ("--drive", FlagArgType::String),
                    ("--folder", FlagArgType::String),
                    ("--descendants", FlagArgType::None),
                    ("-m", FlagArgType::String),
                    ("--meta", FlagArgType::String),
                    ("-t", FlagArgType::String),
                    ("--threshold", FlagArgType::String),
                    ("--kw-weight", FlagArgType::String),
                    ("--sem-weight", FlagArgType::String),
                    ("-j", FlagArgType::None),
                    ("--json", FlagArgType::None),
                    ("-c", FlagArgType::None),
                    ("--chunk", FlagArgType::None),
                    ("--preview", FlagArgType::None),
                    ("-d", FlagArgType::None),
                    ("--full-doc", FlagArgType::None),
                    ("-v", FlagArgType::None),
                    ("--verbose", FlagArgType::None),
                    ("--stats", FlagArgType::None),
                    ("-S", FlagArgType::Number),
                    ("--summarize", FlagArgType::Number),
                    ("--explain", FlagArgType::None),
                    ("--examine", FlagArgType::String),
                    ("--url", FlagArgType::String),
                    ("--multi-turn", FlagArgType::Number),
                    ("--multi-turn-model", FlagArgType::String),
                    ("--multi-turn-context", FlagArgType::String),
                    ("--no-rerank", FlagArgType::None),
                    ("--audit", FlagArgType::None),
                    ("--local", FlagArgType::None),
                    ("--staging", FlagArgType::None),
                ],
                respects_double_dash: true,
            },
        ));
    }
    None
}

static HOSTNAME_READ_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^hostname(?:\s+(?:-[A-Za-z]|--[A-Za-z-]+))*\s*$")
        .expect("valid hostname read-only regex")
});
static ECHO_READ_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)^echo(?:\s+(?:'[^']*'|"[^"$<>\n\r]*"|[^|;&`$(){}><#\\!"'\s]+))*(?:\s+2>&1)?\s*$"#,
    )
    .expect("valid echo read-only regex")
});

/// Maps to: CC `READONLY_COMMANDS` (readOnlyValidation.ts:1432) — commands
/// with no side-effecting flags at all; arbitrary arguments are safe once
/// the metacharacter gate has passed.
const READONLY_COMMANDS: &[&str] = &[
    // Time and date
    "cal", "uptime", // File content viewing
    "cat", "head", "tail", "wc", "stat", "strings", "hexdump", "od", "nl", // System info
    "id", "uname", "free", "df", "du", "locale", "groups", "nproc",
    // Path information
    "basename", "dirname", "realpath", "readlink", // Text processing
    "cut", "paste", "tr", "column", "tac", "rev", "fold", "expand", "unexpand", "fmt", "comm",
    "cmp", "numfmt", // File comparison
    "diff",   // true and false, used to silence or create errors
    "true", "false", // Misc. safe commands
    "sleep", "which", "type", "expr", "test", "getconf", "seq", "tsort", "pr",
];

/// Maps to CC `isCommandSafeViaFlagParsing(...)`'s shell-quote token
/// projection. Compound splitting remains owned by `utils/bash/commands.rs`.
fn parse_read_only_words(command: &str) -> Option<Vec<String>> {
    crate::utils::bash::shell_quote::try_parse_shell_command(command)
        .ok()?
        .into_iter()
        .map(|entry| match entry {
            crate::utils::bash::shell_quote::ParseEntry::String(value)
            | crate::utils::bash::shell_quote::ParseEntry::Glob(value) => Some(value),
            crate::utils::bash::shell_quote::ParseEntry::Operator(_)
            | crate::utils::bash::shell_quote::ParseEntry::Comment(_) => None,
        })
        .collect()
}

/// Maps to CC `containsUnquotedExpansion(command)`.
fn contains_unquoted_expansion(command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && !in_single_quote {
            escaped = true;
            continue;
        }
        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if in_single_quote {
            continue;
        }
        if character == '$'
            && chars
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphanumeric() || "_@*#?!$-".contains(*next))
        {
            return true;
        }
        if !in_double_quote && matches!(character, '?' | '*' | '[' | ']') {
            return true;
        }
    }
    false
}

fn private_command_is_dangerous(name: &str, raw: &str, args: &[String]) -> bool {
    match name {
        "sed" => !super::sed_validation::sed_command_is_allowed_by_allowlist(
            raw,
            super::sed_validation::SedValidationOptions {
                allow_file_writes: false,
            },
        ),
        "ps" => args.iter().any(|argument| {
            !argument.starts_with('-')
                && !argument.is_empty()
                && argument.bytes().all(|byte| byte.is_ascii_alphabetic())
                && argument.contains('e')
        }),
        "date" => {
            const FLAGS_WITH_ARGS: &[&str] = &[
                "-d",
                "--date",
                "-r",
                "--reference",
                "--iso-8601",
                "--rfc-3339",
            ];
            let mut index = 0usize;
            while index < args.len() {
                let token = args[index].as_str();
                if token.starts_with("--") && token.contains('=') {
                    index += 1;
                } else if token.starts_with('-') {
                    index += if FLAGS_WITH_ARGS.contains(&token) {
                        2
                    } else {
                        1
                    };
                } else {
                    if !token.starts_with('+') {
                        return true;
                    }
                    index += 1;
                }
            }
            false
        }
        "lsof" => args
            .iter()
            .any(|argument| argument == "+m" || argument.starts_with("+m")),
        "tput" => {
            const DANGEROUS: &[&str] = &[
                "init", "reset", "rs1", "rs2", "rs3", "is1", "is2", "is3", "iprog", "if", "rf",
                "clear", "flash", "mc0", "mc4", "mc5", "mc5i", "mc5p", "pfkey", "pfloc", "pfx",
                "pfxl", "smcup", "rmcup",
            ];
            let mut index = 0usize;
            let mut after_double_dash = false;
            while index < args.len() {
                let token = args[index].as_str();
                if token == "--" {
                    after_double_dash = true;
                    index += 1;
                } else if !after_double_dash && token.starts_with('-') {
                    if token == "-S"
                        || (!token.starts_with("--") && token.len() > 2 && token.contains('S'))
                    {
                        return true;
                    }
                    index += if token == "-T" { 2 } else { 1 };
                } else {
                    if DANGEROUS.contains(&token) {
                        return true;
                    }
                    index += 1;
                }
            }
            false
        }
        _ => false,
    }
}

fn validate_private_allowlist_command(raw: &str, words: &[String]) -> Option<bool> {
    let (name, command_tokens, config) = private_command_config(words)?;
    if cfg!(windows) && name == "xargs" {
        return Some(false);
    }
    let targets = ["echo", "printf", "wc", "grep", "head", "tail"];
    let valid = crate::utils::shell::read_only_command_validation::validate_flags(
        words,
        command_tokens,
        config,
        crate::utils::shell::read_only_command_validation::ValidateFlagsOptions {
            command_name: words.first().map(String::as_str),
            raw_command: Some(raw),
            xargs_target_commands: (name == "xargs").then_some(targets.as_slice()),
        },
    );
    if !valid {
        return Some(false);
    }
    if name == "hostname" && !HOSTNAME_READ_ONLY_RE.is_match(raw) {
        return Some(false);
    }
    if raw.contains('`')
        || (matches!(words.first().map(String::as_str), Some("grep" | "rg"))
            && raw.contains(['\n', '\r']))
    {
        return Some(false);
    }
    Some(!private_command_is_dangerous(
        name,
        raw,
        &words[command_tokens..],
    ))
}

fn raw_make_regex_safe(command: &str, expected_name: &str) -> bool {
    let mut parts = command.splitn(2, char::is_whitespace);
    if parts.next() != Some(expected_name) {
        return false;
    }
    !command.chars().any(|character| {
        matches!(
            character,
            '<' | '>' | '(' | ')' | '$' | '`' | '|' | '{' | '}' | '&' | ';' | '\n' | '\r'
        )
    })
}

fn uniq_regex_safe(args: &[&str]) -> bool {
    let mut index = 0usize;
    while index < args.len() {
        let token = args[index];
        let short_letters = token.starts_with('-')
            && !token.starts_with("--")
            && token.len() > 1
            && token[1..].bytes().all(|byte| byte.is_ascii_alphabetic());
        let long = token.starts_with("--")
            && token.len() > 2
            && token[2..].split_once('=').map_or(
                token[2..]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b'-'),
                |(flag, value)| {
                    !value.is_empty()
                        && flag
                            .bytes()
                            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
                },
            );
        if !short_letters && !long {
            return false;
        }
        if matches!(token, "-f" | "-s" | "-w")
            && args.get(index + 1).is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            index += 2;
        } else {
            index += 1;
        }
    }
    true
}

fn regex_allowlist_command(raw: &str, first: &str, rest: &[&str], unmatched_quote: bool) -> bool {
    for external in ["docker ps", "docker images"] {
        if (raw == external || raw.starts_with(&format!("{external} ")))
            && !raw.chars().any(|character| {
                matches!(
                    character,
                    '<' | '>' | '(' | ')' | '$' | '`' | '|' | '{' | '}' | '&' | ';' | '\n' | '\r'
                )
            })
        {
            return true;
        }
    }
    if READONLY_COMMANDS.contains(&first) {
        return raw_make_regex_safe(raw, first);
    }
    match first {
        "echo" => !unmatched_quote && ECHO_READ_ONLY_RE.is_match(raw),
        "claude" => matches!(raw.trim(), "claude -h" | "claude --help"),
        "uniq" => uniq_regex_safe(rest),
        "pwd" | "whoami" | "alias" => rest.is_empty(),
        "node" => matches!(rest, ["-v"] | ["--version"]),
        "python" | "python3" => rest == ["--version"],
        "history" => {
            rest.is_empty()
                || matches!(rest, [value] if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        }
        "arch" => rest.is_empty() || matches!(rest, ["-h"] | ["--help"]),
        "ip" => rest == ["addr"],
        "ifconfig" => {
            rest.is_empty()
                || matches!(rest, [name] if name.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
                    && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
        }
        "jq" => {
            !rest.is_empty()
                && rest.iter().any(|argument| !argument.starts_with('-'))
                && !rest.iter().any(|argument| {
                    matches!(
                        *argument,
                        "-f" | "--from-file"
                            | "--rawfile"
                            | "--slurpfile"
                            | "--run-tests"
                            | "-L"
                            | "--library-path"
                            | "env"
                    ) || argument.starts_with("--from-file=")
                        || argument.starts_with("--rawfile=")
                        || argument.starts_with("--slurpfile=")
                        || argument.starts_with("--library-path=")
                        || argument.contains("$ENV")
                })
                && !raw.contains('`')
        }
        "cd" => rest.len() <= 1,
        "ls" => true,
        "find" => !rest.iter().any(|argument| {
            matches!(
                *argument,
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fls"
                    | "-fprintf"
            )
        }),
        _ => false,
    }
}

/// Maps to CC `isCommandReadOnly(command)`.
fn command_is_read_only(raw: &str) -> bool {
    let mut effective_raw = raw.trim();
    if crate::utils::shell::read_only_command_validation::contains_vulnerable_unc_path(
        effective_raw,
    ) || contains_unquoted_expansion(effective_raw)
    {
        return false;
    }
    let Some(mut effective_words) = parse_read_only_words(effective_raw) else {
        return false;
    };
    if effective_words.iter().any(|word| word == "2>&1") {
        if effective_words.last().is_some_and(|word| word == "2>&1")
            && effective_raw.ends_with(" 2>&1")
        {
            effective_words.pop();
            effective_raw = effective_raw
                .strip_suffix(" 2>&1")
                .unwrap_or(effective_raw)
                .trim();
        } else {
            return false;
        }
    }
    let Some((first, rest)) = effective_words.split_first() else {
        return false;
    };
    let first = first.as_str();
    let rest = rest.iter().map(String::as_str).collect::<Vec<_>>();

    // Maps to the shared Git/GH/Docker/rg/pyright maps. Shared callbacks and
    // double-dash semantics remain in their source owner.
    if let Some(valid) = crate::utils::shell::read_only_command_validation::validate_shared_command(
        effective_raw,
        &effective_words,
        cfg!(feature = "anthropic_internal"),
    ) {
        return valid;
    }

    // Maps to BashTool's private COMMAND_ALLOWLIST (51 entries total after
    // shared spreads, plus ant-only `aki`).
    if effective_words.iter().skip(1).any(|word| {
        word.contains('$') || (word.contains('{') && (word.contains(',') || word.contains("..")))
    }) {
        // Regex fallback still gets a chance for literal `$` in echo's
        // single-quoted arguments, exactly like CC's containsUnquotedExpansion.
        return regex_allowlist_command(effective_raw, first, &rest, false);
    }
    if let Some(valid) = validate_private_allowlist_command(effective_raw, &effective_words) {
        return valid;
    }

    regex_allowlist_command(effective_raw, first, &rest, false)
}

/// Maps to CC `commandHasAnyGit(command)`.
fn command_has_any_git(command: &str) -> bool {
    crate::utils::bash::commands::split_command_deprecated(command)
        .into_iter()
        .any(|subcommand| super::bash_permissions::is_normalized_git_command(subcommand.trim()))
}

/// Maps to CC `isGitInternalPath(path)`.
fn is_git_internal_path(path: &str) -> bool {
    let normalized = path
        .strip_prefix("./")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path);
    normalized == "HEAD"
        || ["objects", "refs", "hooks"].iter().any(|directory| {
            normalized == *directory || normalized.starts_with(&format!("{directory}/"))
        })
}

/// Maps to CC `extractWritePathsFromSubcommand(subcommand)`.
fn extract_write_paths_from_subcommand(subcommand: &str) -> Vec<String> {
    let Ok(entries) = crate::utils::bash::shell_quote::try_parse_shell_command(subcommand) else {
        return Vec::new();
    };
    let tokens = entries
        .into_iter()
        .filter_map(|entry| match entry {
            crate::utils::bash::shell_quote::ParseEntry::String(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(command) = tokens
        .first()
        .and_then(|command| super::path_validation::PathCommand::parse(command))
    else {
        return Vec::new();
    };
    if !matches!(
        super::path_validation::command_operation_type(command),
        crate::utils::permissions::path_validation::FileOperationType::Write
            | crate::utils::permissions::path_validation::FileOperationType::Create
    ) || matches!(
        command,
        super::path_validation::PathCommand::Rm
            | super::path_validation::PathCommand::Rmdir
            | super::path_validation::PathCommand::Sed
    ) {
        return Vec::new();
    }
    super::path_validation::extract_paths(command, &tokens[1..])
}

/// Maps to CC `commandWritesToGitInternalPaths(command)`.
fn command_writes_to_git_internal_paths(command: &str) -> bool {
    crate::utils::bash::commands::split_command_deprecated(command)
        .into_iter()
        .any(|subcommand| {
            let subcommand = subcommand.trim();
            extract_write_paths_from_subcommand(subcommand)
                .iter()
                .any(|path| is_git_internal_path(path))
                || crate::utils::bash::commands::extract_output_redirections(subcommand)
                    .redirections
                    .iter()
                    .any(|redirection| is_git_internal_path(&redirection.target))
        })
}

fn is_bare_git_repo(cwd: &std::path::Path) -> bool {
    let valid_dot_git = cwd.join(".git");
    if valid_dot_git.join("HEAD").is_file() || valid_dot_git.is_file() {
        return false;
    }
    cwd.join("HEAD").is_file() || cwd.join("objects").is_dir() || cwd.join("refs").is_dir()
}

/// Maps to CC `checkReadOnlyConstraints(...)` reduced to the
/// `behavior === 'allow'` boolean consumed by `BashTool.isReadOnly(input)`.
pub(crate) fn check_read_only_constraints(command: &str, cwd: &std::path::Path) -> bool {
    let command = command.trim();
    if command.is_empty()
        || crate::utils::bash::shell_quote::try_parse_shell_command(command).is_err()
        || !matches!(
            super::bash_security::bash_command_is_safe_deprecated(command),
            crate::utils::permissions::permission_result::PermissionResult::Passthrough { .. }
        )
        || crate::utils::shell::read_only_command_validation::contains_vulnerable_unc_path(command)
    {
        return false;
    }

    let has_git = command_has_any_git(command);
    if has_git && super::bash_permissions::command_has_any_cd(command) {
        return false;
    }
    if has_git && is_bare_git_repo(cwd) {
        return false;
    }
    if has_git && command_writes_to_git_internal_paths(command) {
        return false;
    }
    if has_git
        && crate::utils::sandbox::sandbox_adapter::get_sandbox_enabled_setting(
            &crate::utils::settings::get_initial_settings(),
        )
        && cwd != crate::bootstrap::state::get_original_cwd()
    {
        return false;
    }

    let subcommands = crate::utils::bash::commands::split_command_deprecated(command);
    !subcommands.is_empty()
        && subcommands.iter().all(|subcommand| {
            matches!(
                super::bash_security::bash_command_is_safe_deprecated(subcommand),
                crate::utils::permissions::permission_result::PermissionResult::Passthrough { .. }
            ) && command_is_read_only(subcommand)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn check_read_only_constraints_allows_plain_read_only_commands() {
        for command in [
            "cat foo.txt",
            "head -n 50 src/main.rs",
            "git status",
            "git diff HEAD~1",
            "git branch",
            "git branch -a --verbose",
            "grep -r pattern src",
            "rg -n pattern src",
            "docker ps",
            "docker ps -a",
            "uname -a",
            "sort file.txt",
            "date",
            "head -n 5 \"my file.txt\"",
            "rg -n \"foo|bar\" src",
            "cat a | head -n 1",
            "git status && git diff",
            "ls -la",
            "find . -name \"*.rs\"",
            "sed -n \"1,2p\" f",
            "echo -e x",
            "pwd",
        ] {
            assert!(
                check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected allow: {command}"
            );
        }
    }

    #[test]
    fn source_shaped_shell_quote_and_compound_matrix_matches_cc_2_1_88() {
        // Generated against this repository's sole reference source with
        // `checkReadOnlyConstraints({ command }, commandHasAnyCd(command))`.
        for (command, expected) in [
            ("cat file", true),
            ("cat 'my file'", true),
            ("cat \"my file\"", true),
            ("cat my\\ file", false),
            ("cat *", false),
            ("cat '*'", true),
            ("cat \"$FILE\"", false),
            ("cat '$FILE'", false),
            ("cat ${FILE}", false),
            ("cat a | head -n 1", true),
            ("cat a || head b", true),
            ("cat a && head b", true),
            ("cat a; head b", true),
            ("cat a & head b", false),
            ("cat a |& head b", false),
            ("(cat a)", false),
            ("cat < input", false),
            ("cat > output", false),
            ("cat 2>&1", true),
            ("cat a 2>errors", false),
            ("cat a 2>&1 | head", true),
            ("echo hello", true),
            ("echo 'hello world'", true),
            ("echo \"hello world\"", true),
            ("echo 'unterminated", false),
            ("echo \"unterminated", false),
            ("echo $HOME", false),
            ("echo '$HOME'", true),
            ("echo foo # comment", false),
            ("echo foo#bar", false),
            ("printf %s hello", false),
            ("pwd", true),
            ("pwd extra", false),
            ("git status", true),
            ("env git status", false),
            ("sudo git status", false),
            ("xargs git status", false),
            ("cd /tmp && git status", false),
            ("pushd /tmp && git status", false),
            ("popd && git status", false),
            ("mkdir objects && git status", false),
            ("mkdir ./refs && git status", false),
            ("touch HEAD && git status", false),
            ("cp source hooks/pre-commit && git status", false),
            ("mv source objects/x && git status", false),
            ("rm -rf objects && git status", false),
            ("sed -i s/x/y/ hooks/x && git status", false),
            ("echo x > hooks/pre-commit && git status", false),
            ("echo x > ./HEAD && git status", false),
            ("echo x > safe && git status", false),
            ("git status && mkdir hooks", false),
            ("git status && cat HEAD", true),
            ("find . -name \"*.rs\"", true),
            ("find . -name *.rs", false),
            ("find . -exec cat {} \\;", false),
            ("rg \"foo|bar\" src", true),
            ("rg foo src | head", true),
            ("rg $PATTERN src", false),
            ("sed -n \"1,2p\" file", true),
            ("sed -n \"$p\" file", false),
            ("jq \".foo\" file.json", true),
            ("jq '$ENV.HOME' file.json", false),
            ("jq \"$ENV.HOME\" file.json", false),
            ("docker ps --format \"{{.ID}}\"", false),
            ("git diff --stat", true),
            ("git diff --output=/tmp/x", false),
            ("git -c core.fsmonitor=x status", false),
            ("git --config-env=x=y status", false),
            ("timeout 5 git status", false),
            ("command git status", false),
            ("nice git status", false),
            ("FOO=bar git status", false),
            ("cat <<'EOF'\nhello\nEOF", false),
            ("cat <<EOF\n$HOME\nEOF", false),
            ("cat <<'EOF' | head\nhello\nEOF", false),
            ("echo café | head -n 1", true),
            ("echo 😀 | head -n 1", true),
            ("cat \"a|b\"", false),
            ("cat a\\|b", false),
            ("cat a\\;b", false),
            ("cat a\\&b", false),
            ("cat a\\>b", false),
            ("cat a\\ b", false),
            ("cat \"$(whoami)\"", false),
            ("cat `whoami`", false),
            ("cat $((1+1))", false),
            ("cat {a,b}", false),
            ("cat foo\\", true),
        ] {
            assert_eq!(
                check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                expected,
                "CC 2.1.88 read-only differential for {command:?}"
            );
        }
    }

    #[test]
    fn private_and_shared_command_inventory_matches_cc_2_1_88() {
        assert_eq!(super::PRIVATE_COMMAND_NAMES.len(), 23);
        assert_eq!(
            super::PRIVATE_COMMAND_NAMES.len()
                + crate::utils::shell::read_only_command_validation::GIT_READ_ONLY_COMMAND_NAMES
                    .len()
                + 4,
            51,
            "Git + rg + pyright + two Docker configs complete COMMAND_ALLOWLIST"
        );
        assert_eq!(
            super::ANT_PRIVATE_COMMAND_NAMES.len()
                + crate::utils::shell::read_only_command_validation::GH_READ_ONLY_COMMAND_NAMES
                    .len(),
            23
        );
        assert_eq!(
            READONLY_COMMANDS.len()
                + crate::utils::shell::read_only_command_validation::EXTERNAL_READONLY_COMMANDS
                    .len(),
            50
        );
    }

    #[test]
    fn complete_flag_callbacks_and_parser_differentials_fail_closed() {
        for command in [
            "xargs -E= EOF echo foo",
            "xargs -rI echo sh -c id",
            "xargs sh -c id",
            "ps axe",
            "date 01011200",
            "hostname foo",
            "hostname -fs",
            "lsof +m/tmp/cache",
            "tput clear",
            "tput -xS cols",
            "pyright -- --watch",
            "git diff -S -- --output=/tmp/pwned",
            "git reflog expire --all",
            "git remote add origin url",
        ] {
            assert!(
                !check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected callback/parser rejection: {command}"
            );
        }
        for command in [
            "xargs -I {} echo {}",
            "ps aux",
            "date +%F",
            "hostname -f",
            "lsof -i",
            "tput cols",
            "git remote show origin",
            "git tag --list 'v*'",
        ] {
            assert!(
                check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected complete-map allow: {command}"
            );
        }
    }

    #[test]
    fn check_read_only_constraints_rejects_compounds_and_redirections() {
        for command in [
            "cat a.txt > b.txt",
            "ls && cargo build",
            "cat a; rm b",
            "git status | tee out.txt",
            "grep foo `whoami`",
            "cat $FILE",
        ] {
            assert!(
                !check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected reject: {command}"
            );
        }
    }

    #[test]
    fn check_read_only_constraints_rejects_dangerous_flags_and_mutations() {
        for command in [
            "sort -o out.txt in.txt",
            "sort -oout.txt in.txt",
            "sort -uoout.txt in.txt",
            "tree -o out.html",
            "tree -Coout .",
            "git log --output=x.patch",
            "git push",
            "git branch new-feature",
            "git branch -D main",
            "rg --pre cmd pattern",
            "git diff --ext-diff",
            "git show --textconv",
            "find . -delete",
            "sed -i s/a/b/ f",
            "uniq input output",
            "base64 -o output input",
            "date -s 20260101",
            "date -us 20260101",
        ] {
            assert!(
                !check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected reject: {command}"
            );
        }
    }

    #[test]
    fn sandboxed_git_outside_original_cwd_is_not_auto_allowed() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let config = std::env::temp_dir().join(format!(
            "cometix-readonly-sandbox-settings-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cwd = std::env::temp_dir().join(format!(
            "cometix-readonly-sandbox-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            config.join("settings.json"),
            r#"{"sandbox":{"enabled":true}}"#,
        )
        .unwrap();
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        // The sandbox gate reads merged settings, which are cached
        // process-wide (`settings_cache.rs:10-12`); without the reset an
        // earlier test's snapshot hides the `sandbox.enabled` written above.
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert!(!check_read_only_constraints("git status", &cwd));
        drop(_env);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(config);
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn git_read_only_commands_are_not_auto_allowed_in_bare_repo_shapes() {
        let root = std::env::temp_dir().join(format!(
            "cometix-bare-repo-readonly-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("objects")).unwrap();
        assert!(!check_read_only_constraints("git status", &root));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(check_read_only_constraints("git status", &root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn check_read_only_constraints_rejects_unlisted_commands() {
        for command in [
            "find . -delete",
            "awk -f evil.awk data",
            "sed -i s/a/b/ f",
            "",
            "   ",
        ] {
            assert!(
                !check_read_only_constraints(command, &std::env::current_dir().unwrap()),
                "expected reject: {command}"
            );
        }
    }
}
