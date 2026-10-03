//! Grep tool metadata, execution, and UI.
//!
//! Maps to:
//! - CC `tools/GrepTool/GrepTool.ts` (schema + `call` execution)
//! - CC `tools/GrepTool/prompt.ts`
//! - CC `tools/GrepTool/UI.tsx`
//!
//! Execution shells out to ripgrep via `utils/ripgrep.rs` (CC `utils/ripgrep.ts`).

pub mod prompt;
pub mod ui;

/// Maps to: CC `GrepTool.ts:33-90` `inputSchema` — the carrier schema, one
/// definition behind validation and the JSON Schema projection. Built once per
/// process, matching CC's `lazySchema()` one-instance-per-session guarantee.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::semantic_boolean::semantic_boolean;
        use crate::utils::semantic_number::semantic_number;
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "pattern",
                zod::string()
                    .describe("The regular expression pattern to search for in file contents"),
            ),
            (
                "path",
                zod::string().optional().describe(
                    "File or directory to search in (rg PATH). Defaults to current working directory.",
                ),
            ),
            (
                "glob",
                zod::string().optional().describe(
                    "Glob pattern to filter files (e.g. \"*.js\", \"*.{ts,tsx}\") - maps to rg --glob",
                ),
            ),
            (
                "output_mode",
                zod::enumeration(vec!["content", "files_with_matches", "count"])
                    .optional()
                    .describe(
                        "Output mode: \"content\" shows matching lines (supports -A/-B/-C context, -n line numbers, head_limit), \"files_with_matches\" shows file paths (supports head_limit), \"count\" shows match counts (supports head_limit). Defaults to \"files_with_matches\".",
                    ),
            ),
            (
                "-B",
                semantic_number(zod::number().optional()).describe(
                    "Number of lines to show before each match (rg -B). Requires output_mode: \"content\", ignored otherwise.",
                ),
            ),
            (
                "-A",
                semantic_number(zod::number().optional()).describe(
                    "Number of lines to show after each match (rg -A). Requires output_mode: \"content\", ignored otherwise.",
                ),
            ),
            (
                "-C",
                semantic_number(zod::number().optional()).describe("Alias for context."),
            ),
            (
                "context",
                semantic_number(zod::number().optional()).describe(
                    "Number of lines to show before and after each match (rg -C). Requires output_mode: \"content\", ignored otherwise.",
                ),
            ),
            (
                "-n",
                semantic_boolean(zod::boolean().optional()).describe(
                    "Show line numbers in output (rg -n). Requires output_mode: \"content\", ignored otherwise. Defaults to true.",
                ),
            ),
            (
                "-i",
                semantic_boolean(zod::boolean().optional())
                    .describe("Case insensitive search (rg -i)"),
            ),
            (
                "type",
                zod::string().optional().describe(
                    "File type to search (rg --type). Common types: js, py, rust, go, java, etc. More efficient than include for standard file types.",
                ),
            ),
            (
                "head_limit",
                semantic_number(zod::number().optional()).describe(
                    "Limit output to first N lines/entries, equivalent to \"| head -N\". Works across all output modes: content (limits output lines), files_with_matches (limits file paths), count (limits count entries). Defaults to 250 when unspecified. Pass 0 for unlimited (use sparingly — large result sets waste context).",
                ),
            ),
            (
                "offset",
                semantic_number(zod::number().optional()).describe(
                    "Skip first N lines/entries before applying head_limit, equivalent to \"| tail -n +N | head -N\". Works across all output modes. Defaults to 0.",
                ),
            ),
            (
                "multiline",
                semantic_boolean(zod::boolean().optional()).describe(
                    "Enable multiline mode where . matches newlines and patterns can span lines (rg -U --multiline-dotall). Default: false.",
                ),
            ),
        ])
    })
}

/// Version control directories excluded from searches (CC `VCS_DIRECTORIES_TO_EXCLUDE`).
const VCS_DIRECTORIES_TO_EXCLUDE: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// Maps to CC `GrepTool.inputSchema`.
pub fn grep_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::GREP_TOOL_NAME.to_string(),
        description: prompt::get_description(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

/// Maps to CC `DEFAULT_HEAD_LIMIT` (`GrepTool.ts:108`).
const DEFAULT_HEAD_LIMIT: f64 = 250.0;

fn javascript_number(value: f64) -> serde_json::Number {
    debug_assert!(value.is_finite());
    ryu_js::Buffer::new()
        .format(value)
        .parse()
        .expect("finite JavaScript number must be valid JSON")
}

pub(crate) fn javascript_number_text(number: &serde_json::Number) -> String {
    let Some(value) = number.as_f64() else {
        return number.to_string();
    };
    ryu_js::Buffer::new().format(value).to_string()
}

fn input_number(args: &serde_json::Value, key: &str) -> Option<serde_json::Number> {
    let value = args.get(key)?.as_number()?.as_f64()?;
    value.is_finite().then(|| javascript_number(value))
}

fn usize_number(value: usize) -> serde_json::Number {
    serde_json::Number::from(u64::try_from(value).unwrap_or(u64::MAX))
}

/// JavaScript `Array.prototype.slice` index coercion (`ToIntegerOrInfinity`).
fn slice_index(length: usize, value: f64) -> usize {
    if value.is_nan() || value == 0.0 || value == f64::NEG_INFINITY {
        return 0;
    }
    if value == f64::INFINITY {
        return length;
    }
    let integer = value.trunc();
    if integer < 0.0 {
        ((length as f64 + integer).max(0.0).min(length as f64)) as usize
    } else {
        integer.min(length as f64) as usize
    }
}

/// Maps to CC `applyHeadLimit` (`GrepTool.ts:110-129`). This deliberately
/// preserves Zod `number()` semantics: fractional/negative values are coerced
/// by JavaScript `slice`, while the original number is reported in pagination.
fn apply_head_limit<T: Clone>(
    items: &[T],
    limit: Option<&serde_json::Number>,
    offset: &serde_json::Number,
) -> (Vec<T>, Option<serde_json::Number>) {
    let offset_value = offset.as_f64().unwrap_or(0.0);
    let explicit_limit = limit.and_then(serde_json::Number::as_f64);
    if explicit_limit == Some(0.0) {
        let start = slice_index(items.len(), offset_value);
        return (items[start..].to_vec(), None);
    }

    let effective_limit = explicit_limit.unwrap_or(DEFAULT_HEAD_LIMIT);
    let start = slice_index(items.len(), offset_value);
    let end = slice_index(items.len(), offset_value + effective_limit);
    let sliced = if end < start {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    let was_truncated = items.len() as f64 - offset_value > effective_limit;
    let applied_limit = was_truncated.then(|| {
        limit
            .cloned()
            .unwrap_or_else(|| javascript_number(DEFAULT_HEAD_LIMIT))
    });
    (sliced, applied_limit)
}

/// Maps to CC `formatLimitInfo` (`GrepTool.ts:134-142`).
fn format_limit_info(
    applied_limit: Option<&serde_json::Number>,
    applied_offset: Option<&serde_json::Number>,
) -> String {
    let mut parts = Vec::new();
    if let Some(limit) = applied_limit {
        parts.push(format!("limit: {}", javascript_number_text(limit)));
    }
    if let Some(offset) = applied_offset.filter(|offset| {
        offset
            .as_f64()
            .is_some_and(|value| value != 0.0 && !value.is_nan())
    }) {
        parts.push(format!("offset: {}", javascript_number_text(offset)));
    }
    parts.join(", ")
}

/// Maps to: CC `tools/GrepTool/GrepTool.ts:158` `type Output =
/// z.infer<OutputSchema>` (schema at :144-155) — the single type the tool
/// yields from `call()`, records as the message's `toolUseResult`, and the
/// render path recovers via `outputSchema.safeParse` ([`parse_output`] is the
/// Rust stand-in). Numbers remain unrestricted Zod `number()` values so cold
/// JSONL keeps exact fidelity; live execution fills integral counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    /// CC `mode` — optional in the schema; both consumers default it to
    /// `files_with_matches` on destructuring (UI.tsx:128, GrepTool.ts:256).
    pub(crate) mode: Option<crate::types::message::SearchResultMode>,
    /// CC `numFiles`.
    pub(crate) num_files: serde_json::Number,
    /// CC `filenames` (files_with_matches mode only; other modes emit `[]`).
    pub(crate) filenames: Vec<String>,
    /// CC `content` (content/count modes).
    pub(crate) content: Option<String>,
    /// CC `numLines` (content mode).
    pub(crate) num_lines: Option<serde_json::Number>,
    /// CC `numMatches` (count mode).
    pub(crate) num_matches: Option<serde_json::Number>,
    /// CC `appliedLimit` — only when truncation occurred.
    pub(crate) applied_limit: Option<serde_json::Number>,
    /// CC `appliedOffset` — only when offset > 0.
    pub(crate) applied_offset: Option<serde_json::Number>,
}

impl Output {
    /// CC's destructuring default `mode = 'files_with_matches'`.
    pub(crate) fn effective_mode(&self) -> crate::types::message::SearchResultMode {
        self.mode.unwrap_or_default()
    }
}

/// Maps to CC `utils/path.ts#toRelativePath` relative to project cwd (`getCwd`).
fn to_relative_path(absolute_path: &str, cwd: &std::path::Path) -> String {
    let path = std::path::Path::new(absolute_path);
    if let Ok(relative) = path.strip_prefix(cwd) {
        // Node `path.relative` uses native separators and preserves literal
        // backslashes on POSIX; do not apply the UI-only slash normalizer.
        let relative_text = relative.to_string_lossy().to_string();
        if !relative_text.starts_with("..") {
            return relative_text;
        }
    }
    absolute_path.to_string()
}

fn relativize_content_line(line: &str, cwd: &std::path::Path) -> String {
    // `/absolute/path:rest` or `/absolute/path:num:content`
    let Some(colon) = line.find(':') else {
        return line.to_string();
    };
    let (file_path, rest) = line.split_at(colon);
    format!("{}{rest}", to_relative_path(file_path, cwd))
}

fn relativize_count_line(line: &str, cwd: &std::path::Path) -> String {
    // `/absolute/path:count` — last colon separates count.
    let Some(colon) = line.rfind(':') else {
        return line.to_string();
    };
    let (file_path, count) = line.split_at(colon);
    format!("{}{count}", to_relative_path(file_path, cwd))
}

fn read_deny_globs(
    permission_context: &crate::tool::ToolPermissionContext,
    project_cwd: &std::path::Path,
    search_root: &std::path::Path,
) -> Vec<String> {
    use crate::utils::permissions::filesystem::{
        get_file_read_ignore_patterns, normalize_patterns_to_path, paths_to_check,
    };

    let patterns_by_root = get_file_read_ignore_patterns(permission_context);
    let mut normalized = indexmap::IndexSet::new();
    let mut roots = indexmap::IndexSet::new();
    roots.insert(project_cwd.display().to_string());
    // DEVIATION(SECURITY): CC normalizes against `getCwd()` only
    // (`GrepTool.ts:413`). Also anchor the logical and resolved traversal roots
    // so a directory symlink cannot expose Read-denied files.
    roots.extend(paths_to_check(
        &search_root.display().to_string(),
        project_cwd,
    ));
    for root in roots {
        normalized.extend(normalize_patterns_to_path(&patterns_by_root, &root));
    }
    normalized
        .into_iter()
        .map(|pattern| {
            let pattern = if pattern.starts_with('/') {
                pattern
            } else {
                format!("**/{pattern}")
            };
            // DEVIATION(SECURITY): CC pushes the normalized spelling verbatim
            // (`GrepTool.ts:423`). For a search root outside the process cwd
            // ripgrep matches a root-prefixed spelling instead, so the anchored
            // rewrite is what actually excludes the Read-denied file.
            crate::utils::glob::ripgrep_read_deny_pattern(&pattern, search_root)
        })
        .collect()
}

/// Maps to CC `GrepTool.ts:529-540`: start every async stat before awaiting
/// settlement, including filename-only test mode. A failed stat contributes 0.
pub(crate) async fn file_match_mtimes(results: &[String]) -> Vec<f64> {
    let pending = results
        .iter()
        .map(|path| {
            crate::utils::fs_operations::get_fs_implementation().stat(std::path::Path::new(path))
        })
        .collect::<Vec<_>>();
    futures::future::join_all(pending)
        .await
        .into_iter()
        .map(|result| result.map_or(0.0, |stats| stats.mtime_ms))
        .collect()
}

fn sort_file_matches(
    results: Vec<String>,
    filename_only: bool,
    mut modified_time: impl FnMut(&str) -> f64,
) -> Vec<String> {
    let mut matches_with_mtime = results
        .into_iter()
        .map(|path| {
            let mtime = if filename_only {
                0.0
            } else {
                modified_time(&path)
            };
            (path, mtime)
        })
        .collect::<Vec<_>>();
    matches_with_mtime.sort_by(|(left, left_mtime), (right, right_mtime)| {
        if filename_only {
            javascript_locale_compare(left, right)
        } else {
            let difference = right_mtime - left_mtime;
            if difference == 0.0 {
                javascript_locale_compare(left, right)
            } else {
                // Array.sort treats NaN as equality; it does not take CC's
                // zero-difference filename branch in that case.
                difference
                    .partial_cmp(&0.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        }
    });
    matches_with_mtime
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// JavaScript `String.prototype.localeCompare(other)` — the runtime primitive,
/// not a CC source file, so it carries no `Maps to:` of its own.
///
/// Shared crate-wide from the module where the need first surfaced, matching the
/// `javascript_number_to_string` funnel precedent (PORTING.md `Zod v4 runtime
/// carrier` clause 7). Second consumer: `tools/mod.rs#assemble_tool_pool`, whose
/// CC counterpart sorts with `(a, b) => a.name.localeCompare(b.name)`
/// (`tools.ts:362`, `utils/toolPool.ts:69`).
pub(crate) fn javascript_locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    static COLLATOR: std::sync::LazyLock<icu_collator::CollatorBorrowed<'static>> =
        std::sync::LazyLock::new(|| {
            let raw_locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
            let normalized = raw_locale
                .split(['.', '@'])
                .next()
                .unwrap_or("en-US")
                .replace('_', "-");
            // Node/ICU resolves C/POSIX locales to en-US for Intl.Collator.
            let normalized = if matches!(normalized.as_str(), "C" | "POSIX") {
                "en-US"
            } else {
                normalized.as_str()
            };
            let locale = normalized
                .parse::<icu_locale::Locale>()
                .unwrap_or_else(|_| "en-US".parse().expect("en-US is a valid ICU locale"));
            icu_collator::CollatorBorrowed::try_new(
                locale.into(),
                icu_collator::options::CollatorOptions::default(),
            )
            .expect("compiled ICU collation data must contain the process locale")
        });
    COLLATOR.compare(left, right)
}

fn is_javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn javascript_parse_int_base10(text: &str) -> Option<f64> {
    let text = text.trim_start_matches(is_javascript_whitespace);
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let digit_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    (end > digit_start)
        .then(|| text[..end].parse::<f64>().ok())
        .flatten()
}

/// Execute a permitted Grep tool use, producing the CC-shaped output.
/// Maps to: CC `tools/GrepTool/GrepTool.ts` `call` (:310) — builds ripgrep
/// argv, calls `ripGrep`, then content (:466-474) / count (:514-522) /
/// files_with_matches (:565-571) post-processing + `applyHeadLimit`.
pub(crate) fn grep_output(
    args: &serde_json::Value,
    cwd: Option<&std::path::Path>,
    abort: &crate::tool::AbortController,
) -> Result<Output, String> {
    let permission_context = crate::tool::ToolPermissionContext::default();
    grep_output_with_context(args, cwd, abort, &permission_context)
}

fn grep_output_with_context(
    args: &serde_json::Value,
    cwd: Option<&std::path::Path>,
    abort: &crate::tool::AbortController,
    permission_context: &crate::tool::ToolPermissionContext,
) -> Result<Output, String> {
    use crate::types::message::SearchResultMode;
    use crate::utils::ripgrep::rip_grep;

    let Some(pattern) = args.get("pattern").and_then(|value| value.as_str()) else {
        return Err("Error searching files: missing pattern".to_string());
    };
    let output_mode_str = args
        .get("output_mode")
        .and_then(|value| value.as_str())
        .unwrap_or("files_with_matches");
    let output_mode = match output_mode_str {
        "content" => SearchResultMode::Content,
        "count" => SearchResultMode::Count,
        _ => SearchResultMode::FilesWithMatches,
    };
    let search_cwd = cwd
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(crate::bootstrap::state::get_original_cwd);
    // Official `toRelativePath` and deny normalization use `getCwd()`, not the
    // optional Grep `path` root. `effective_cwd` is that owner in Rust.
    let project_cwd = search_cwd.clone();
    let absolute_path = match args
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
    {
        Some(path) => crate::utils::path::expand_path(path, Some(&search_cwd))?,
        None => search_cwd.clone(),
    };

    let mut rg_args = vec!["--hidden".to_string()];
    for dir in VCS_DIRECTORIES_TO_EXCLUDE {
        rg_args.push("--glob".to_string());
        rg_args.push(format!("!{dir}"));
    }
    rg_args.push("--max-columns".to_string());
    rg_args.push("500".to_string());

    if args
        .get("multiline")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        rg_args.push("-U".to_string());
        rg_args.push("--multiline-dotall".to_string());
    }
    if args
        .get("-i")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        rg_args.push("-i".to_string());
    }
    match output_mode {
        SearchResultMode::FilesWithMatches => rg_args.push("-l".to_string()),
        SearchResultMode::Count => rg_args.push("-c".to_string()),
        SearchResultMode::Content => {
            let show_line_numbers = args
                .get("-n")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            if show_line_numbers {
                rg_args.push("-n".to_string());
            }
            let context = input_number(args, "context");
            let context_c = input_number(args, "-C");
            let context_before = input_number(args, "-B");
            let context_after = input_number(args, "-A");
            if let Some(context) = context.or(context_c) {
                rg_args.push("-C".to_string());
                rg_args.push(javascript_number_text(&context));
            } else {
                if let Some(before) = context_before {
                    rg_args.push("-B".to_string());
                    rg_args.push(javascript_number_text(&before));
                }
                if let Some(after) = context_after {
                    rg_args.push("-A".to_string());
                    rg_args.push(javascript_number_text(&after));
                }
            }
        }
    }

    if pattern.starts_with('-') {
        rg_args.push("-e".to_string());
        rg_args.push(pattern.to_string());
    } else {
        rg_args.push(pattern.to_string());
    }

    if let Some(file_type) = args
        .get("type")
        .and_then(serde_json::Value::as_str)
        .filter(|file_type| !file_type.is_empty())
    {
        rg_args.push("--type".to_string());
        rg_args.push(file_type.to_string());
    }

    if let Some(glob) = args.get("glob").and_then(|value| value.as_str()) {
        // CC: split on JavaScript whitespace, preserve `{...}` brace groups,
        // then split non-brace groups on commas.
        let mut glob_patterns = Vec::new();
        for raw_pattern in glob
            .split(is_javascript_whitespace)
            .filter(|pattern| !pattern.is_empty())
        {
            if raw_pattern.contains('{') && raw_pattern.contains('}') {
                glob_patterns.push(raw_pattern.to_string());
            } else {
                glob_patterns.extend(
                    raw_pattern
                        .split(',')
                        .filter(|part| !part.is_empty())
                        .map(str::to_string),
                );
            }
        }
        for glob_pattern in glob_patterns {
            rg_args.push("--glob".to_string());
            rg_args.push(glob_pattern);
        }
    }

    for ignore_pattern in read_deny_globs(permission_context, &project_cwd, &absolute_path) {
        rg_args.push("--glob".to_string());
        rg_args.push(format!("!{ignore_pattern}"));
    }
    for exclusion in
        crate::utils::plugins::orphaned_plugin_filter::get_glob_exclusions_for_plugin_cache(Some(
            &absolute_path,
        ))
    {
        rg_args.push("--glob".to_string());
        rg_args.push(exclusion);
    }

    let results = rip_grep(&rg_args, &absolute_path, abort).map_err(|error| error.to_string())?;

    let head_limit = input_number(args, "head_limit");
    let offset = input_number(args, "offset").unwrap_or_else(|| javascript_number(0.0));
    let applied_offset = offset
        .as_f64()
        .is_some_and(|value| value > 0.0)
        .then(|| offset.clone());

    Ok(match output_mode {
        SearchResultMode::Content => {
            let (limited, applied_limit) = apply_head_limit(&results, head_limit.as_ref(), &offset);
            let final_lines = limited
                .iter()
                .map(|line| relativize_content_line(line, &project_cwd))
                .collect::<Vec<_>>();
            Output {
                mode: Some(output_mode),
                num_files: usize_number(0),
                filenames: Vec::new(),
                num_lines: Some(usize_number(final_lines.len())),
                content: Some(final_lines.join("\n")),
                num_matches: None,
                applied_limit,
                applied_offset,
            }
        }
        SearchResultMode::Count => {
            let (limited, applied_limit) = apply_head_limit(&results, head_limit.as_ref(), &offset);
            let final_count_lines = limited
                .iter()
                .map(|line| relativize_count_line(line, &project_cwd))
                .collect::<Vec<_>>();
            let mut total_matches = 0.0_f64;
            let mut file_count = 0usize;
            for line in &final_count_lines {
                if let Some((_, count_str)) = line.rsplit_once(':') {
                    if let Some(count) = javascript_parse_int_base10(count_str) {
                        total_matches += count;
                        file_count += 1;
                    }
                }
            }
            Output {
                mode: Some(output_mode),
                num_files: usize_number(file_count),
                filenames: Vec::new(),
                content: Some(final_count_lines.join("\n")),
                num_lines: None,
                num_matches: Some(javascript_number(total_matches)),
                applied_limit,
                applied_offset,
            }
        }
        SearchResultMode::FilesWithMatches => {
            // Maps to CC `GrepTool.ts:540-562`: production mtime-descending,
            // filename tiebreaker; NODE_ENV=test uses filename-only ordering.
            let filename_only_sort =
                cfg!(test) || crate::utils::process_env::var("NODE_ENV").is_some_and(|value| value == "test");
            let mtimes = futures::executor::block_on(file_match_mtimes(&results));
            let mut times = mtimes.into_iter();
            let sorted =
                sort_file_matches(results, filename_only_sort, |_| times.next().unwrap_or(0.0));
            let (limited, applied_limit) = apply_head_limit(&sorted, head_limit.as_ref(), &offset);
            let relative_matches = limited
                .iter()
                .map(|path| to_relative_path(path, &project_cwd))
                .collect::<Vec<_>>();
            Output {
                mode: Some(output_mode),
                num_files: usize_number(relative_matches.len()),
                filenames: relative_matches,
                content: None,
                num_lines: None,
                num_matches: None,
                applied_limit,
                applied_offset,
            }
        }
    })
}

/// Behavioral half of CC `GrepTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct GrepTool;

impl crate::tool::ToolCall for GrepTool {
    fn name(&self) -> &'static str {
        "Grep"
    }

    /// Maps to: CC `GrepTool.ts:241-243` `async prompt() { return
    /// getDescription() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_description()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("search file contents with regex (ripgrep)")
    }

    /// Maps to: CC `GrepTool.ts:166-168` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::get_description()
    }

    /// Maps to: CC `GrepTool.ts:169-171` `userFacingName()`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Search".to_string()
    }

    /// Maps to: CC `GrepTool.ts:250-253` `extractSearchText` — content mode
    /// indexes the content; other modes index the filename list.
    fn extract_search_text(&self, data: &crate::tool::ToolOutput) -> Option<String> {
        match data {
            crate::tool::ToolOutput::Grep(output) => Some(
                match (&output.effective_mode(), output.content.as_deref()) {
                    (crate::types::message::SearchResultMode::Content, Some(content))
                        if !content.is_empty() =>
                    {
                        content.to_string()
                    }
                    _ => output.filenames.join("\n"),
                },
            ),
            _ => None,
        }
    }

    /// Maps to: CC `GrepTool.ts:27,172` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::grep_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to CC `GrepTool.ts:173-176`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::grep_tool::ui::get_tool_use_summary(Some(args))
                .map(|summary| format!("Searching for {summary}"))
                .unwrap_or_else(|| "Searching".to_string()),
        )
    }

    fn max_result_size_chars(&self) -> usize {
        20_000
    }

    /// Maps to: CC `GrepTool.ts:33-89` Zod semantic preprocessors.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        for field in ["-B", "-A", "-C", "context", "head_limit", "offset"] {
            crate::utils::semantic_number::preprocess_object_field(&mut parsed, field);
        }
        for field in ["-n", "-i", "multiline"] {
            crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, field);
        }
        parsed
    }

    /// Maps to CC `GrepTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_search_or_read_command(
        &self,
        _args: &serde_json::Value,
    ) -> Option<crate::tool::SearchOrReadCommand> {
        Some(crate::tool::SearchOrReadCommand {
            is_search: true,
            is_read: false,
        })
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            args.get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    crate::bootstrap::state::get_original_cwd()
                        .display()
                        .to_string()
                }),
        )
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let pattern = args
            .get("pattern")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        args.get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map_or_else(
                || pattern.to_string(),
                |path| format!("{pattern} in {path}"),
            )
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
        let pattern = args.get("pattern")?.as_str()?.to_string();
        Some(Box::new(move |rule_pattern| {
            crate::utils::permissions::shell_rule_matching::match_wildcard_pattern(
                rule_pattern,
                &pattern,
                false,
            )
        }))
    }

    /// Maps to CC `GrepTool.ts:201-230` `validateInput`.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let Some(path) = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            return crate::tool::ValidationResult::Ok;
        };
        let cwd = context.effective_cwd();
        let absolute_path = match crate::utils::path::expand_path(path, Some(&cwd)) {
            Ok(path) => path,
            Err(message) => return crate::tool::ValidationResult::Fatal { message },
        };
        let absolute_text = absolute_path.display().to_string();
        // DEVIATION(SAFETY): inspect raw Windows spelling on POSIX as well, so
        // the CC UNC no-I/O guard runs before stat or symlink resolution.
        if path.starts_with("\\\\")
            || path.starts_with("//")
            || absolute_text.starts_with("\\\\")
            || absolute_text.starts_with("//")
        {
            return crate::tool::ValidationResult::Ok;
        }
        match futures::executor::block_on(
            crate::utils::fs_operations::get_fs_implementation().stat(&absolute_path),
        ) {
            Ok(_) => crate::tool::ValidationResult::Ok,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut message = format!(
                    "Path does not exist: {path}. {} {}.",
                    crate::utils::file::FILE_NOT_FOUND_CWD_NOTE,
                    cwd.display()
                );
                if let Some(suggestion) =
                    crate::utils::file::suggest_path_under_cwd(&absolute_path, &cwd)
                {
                    message.push_str(&format!(" Did you mean {}?", suggestion.display()));
                }
                crate::tool::ValidationResult::Error {
                    message,
                    error_code: 1,
                }
            }
            Err(error) => crate::tool::ValidationResult::Fatal {
                message: error.to_string(),
            },
        }
    }

    /// Maps to CC `GrepTool.ts:233-240` via `checkReadPermissionForTool`.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let parsed = self.normalize_input(args);
        let cwd = context.effective_cwd();
        let path = parsed
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| cwd.display().to_string());
        crate::utils::permissions::filesystem::check_read_permission_for_tool(
            &path,
            &parsed,
            &context.tool_permission_context,
            &cwd,
        )
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        let args = args.clone();
        let cwd = context.effective_cwd();
        let abort = context.abort_controller.clone();
        let permission_context = context.tool_permission_context.clone();
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || {
                grep_output_with_context(&args, Some(&cwd), &abort, &permission_context)
            })
            .await
            .map_err(|error| format!("Grep worker failed: {error}"))
            .and_then(|result| result);
            match result {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Grep(output),
                    new_messages: Vec::new(),
                },
                Err(message) => crate::tool::ToolResult {
                    // The `Error: …` raw string rides the row via the
                    // `tool_use_result` trait projection below, not a display
                    // variant.
                    data: crate::tool::ToolOutput::Composed {
                        content: message.clone(),
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `tools/GrepTool/GrepTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:254) — content mode (:267),
    /// count summary (:280-285), files_with_matches header (:293-303),
    /// including `formatLimitInfo` pagination hints.
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::{SearchResultMode, ToolResultStatus};
        match data {
            crate::tool::ToolOutput::Grep(output) => {
                let limit_info = format_limit_info(
                    output.applied_limit.as_ref(),
                    output.applied_offset.as_ref(),
                );
                let content = match output.effective_mode() {
                    SearchResultMode::Content => {
                        let result = output
                            .content
                            .clone()
                            .filter(|content| !content.is_empty())
                            .unwrap_or_else(|| "No matches found".to_string());
                        if limit_info.is_empty() {
                            result
                        } else {
                            format!("{result}\n\n[Showing results with pagination = {limit_info}]")
                        }
                    }
                    SearchResultMode::Count => {
                        let raw = output
                            .content
                            .clone()
                            .filter(|content| !content.is_empty())
                            .unwrap_or_else(|| "No matches found".to_string());
                        let zero = javascript_number(0.0);
                        let matches = output.num_matches.as_ref().unwrap_or(&zero);
                        let match_text = javascript_number_text(matches);
                        let file_text = javascript_number_text(&output.num_files);
                        let occurrence_label = if matches.as_f64() == Some(1.0) {
                            "occurrence"
                        } else {
                            "occurrences"
                        };
                        let file_label = if output.num_files.as_f64() == Some(1.0) {
                            "file"
                        } else {
                            "files"
                        };
                        let pagination = if limit_info.is_empty() {
                            String::new()
                        } else {
                            format!(" with pagination = {limit_info}")
                        };
                        format!(
                            "{raw}\n\nFound {match_text} total {occurrence_label} across {file_text} {file_label}.{pagination}"
                        )
                    }
                    SearchResultMode::FilesWithMatches => {
                        if output.num_files.as_f64() == Some(0.0) {
                            "No files found".to_string()
                        } else {
                            let file_text = javascript_number_text(&output.num_files);
                            let file_label = if output.num_files.as_f64() == Some(1.0) {
                                "file"
                            } else {
                                "files"
                            };
                            let pagination = if limit_info.is_empty() {
                                String::new()
                            } else {
                                format!(" {limit_info}")
                            };
                            format!(
                                "Found {file_text} {file_label}{pagination}\n{}",
                                output.filenames.join("\n")
                            )
                        }
                    }
                };
                (content, ToolResultStatus::Success)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Grep carries no display shape — the raw the trait projects below
    /// is what renders (`renderToolResultMessage` parses it with the tool's
    /// own output schema). A live `Output` is typed, so its projection
    /// always parses; the emit gate never fires here, unlike the cold paths
    /// that must gate on `parse_output`.
    /// Maps to: CC recording GrepTool's `Output` as the message's
    /// `toolUseResult`. A failure records the `Error: …` string, matching the
    /// string `toolUseResult` CC keeps for errored searches.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Grep(output) => {
                Some(crate::tools::grep_tool::ui::output_to_value(output))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => Some(serde_json::Value::String(format!("Error: {content}"))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    #[test]
    fn grep_tool_schema_matches_official_input_shape() {
        let schema = grep_tool_schema();
        assert_eq!(schema.name, "Grep");
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["pattern"])
        );
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert_eq!(schema.strict, Some(true));
        assert_eq!(
            schema.input_schema["properties"]
                .as_object()
                .map(serde_json::Map::len),
            Some(14)
        );
        for key in [
            "pattern",
            "path",
            "glob",
            "output_mode",
            "-B",
            "-A",
            "-C",
            "context",
            "-n",
            "-i",
            "type",
            "head_limit",
            "offset",
            "multiline",
        ] {
            assert!(
                schema
                    .input_schema
                    .pointer(&format!("/properties/{key}"))
                    .is_some(),
                "missing {key}"
            );
        }
        assert!(schema.description.contains("ALWAYS use Grep"));
    }

    #[test]
    fn grep_semantic_preprocessing_matches_official_schema() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &GrepTool,
            &serde_json::json!({
                "pattern": "needle",
                "-B": "2",
                "head_limit": "3.5",
                "-i": "true",
                "multiline": "false"
            }),
        );
        assert_eq!(parsed["-B"], serde_json::json!(2));
        assert_eq!(parsed["head_limit"], serde_json::json!(3.5));
        assert_eq!(parsed["-i"], serde_json::json!(true));
        assert_eq!(parsed["multiline"], serde_json::json!(false));
    }

    #[test]
    fn grep_output_uses_cwd_override_when_path_is_omitted() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-grep-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("needle.txt"), "alpha\nneedle\n").unwrap();
        let args = serde_json::json!({"pattern": "needle"});

        let output = grep_output(&args, Some(&dir), &crate::tool::AbortController::default())
            .expect("grep succeeds");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            output
                .filenames
                .iter()
                .any(|path| path.ends_with("needle.txt"))
        );
    }

    #[test]
    fn apply_head_limit_defaults_to_250_and_zero_means_unlimited() {
        let items: Vec<_> = (0..300).collect();
        let zero = javascript_number(0.0);
        let five = javascript_number(5.0);
        let ten = javascript_number(10.0);
        let (sliced, applied) = apply_head_limit(&items, None, &zero);
        assert_eq!(sliced.len(), 250);
        assert_eq!(applied, Some(javascript_number(250.0)));

        let (sliced, applied) = apply_head_limit(&items, Some(&zero), &zero);
        assert_eq!(sliced.len(), 300);
        assert_eq!(applied, None);

        let (sliced, applied) = apply_head_limit(&items, Some(&ten), &five);
        assert_eq!(sliced, (5..15).collect::<Vec<_>>());
        assert_eq!(applied, Some(ten));

        let small = vec![1, 2, 3];
        let (sliced, applied) = apply_head_limit(&small, None, &zero);
        assert_eq!(sliced, small);
        assert_eq!(applied, None);
    }

    #[test]
    fn grep_output_applies_head_limit_and_offset_like_official() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-grep-page-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}.txt")), format!("needle line {i}\n")).unwrap();
        }

        let abort = crate::tool::AbortController::default();
        let limited = grep_output(
            &serde_json::json!({
                "pattern": "needle",
                "output_mode": "files_with_matches",
                "head_limit": 2,
                "offset": 1,
            }),
            Some(&dir),
            &abort,
        )
        .expect("grep pages");
        assert_eq!(limited.num_files, usize_number(2));
        assert_eq!(limited.filenames.len(), 2);
        assert_eq!(limited.applied_limit, Some(usize_number(2)));
        assert_eq!(limited.applied_offset, Some(usize_number(1)));

        let content = grep_output(
            &serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "head_limit": 2,
            }),
            Some(&dir),
            &abort,
        )
        .expect("content pages");
        assert_eq!(content.num_lines, Some(usize_number(2)));
        assert_eq!(content.applied_limit, Some(usize_number(2)));

        let (mapped, _) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &GrepTool,
            &crate::tool::ToolOutput::Grep(content),
            "toolu_grep",
        );
        assert!(mapped.contains("[Showing results with pagination = limit: 2]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_tool_metadata_matches_official_contract() {
        use crate::tool::ToolCall as _;

        let tool = GrepTool;
        let input = serde_json::json!({"pattern": "needle", "path": "src"});
        assert_eq!(
            tool.search_hint(),
            Some("search file contents with regex (ripgrep)")
        );
        assert_eq!(tool.max_result_size_chars(), 20_000);
        assert_eq!(
            tool.get_activity_description(&input),
            Some("Searching for needle".to_string())
        );
        assert_eq!(
            tool.get_activity_description(&serde_json::json!({})),
            Some("Searching".to_string())
        );
        assert!(tool.is_concurrency_safe(&input));
        assert!(tool.is_read_only(&input));
        assert_eq!(
            tool.is_search_or_read_command(&input),
            Some(crate::tool::SearchOrReadCommand {
                is_search: true,
                is_read: false,
            })
        );
        assert_eq!(tool.get_path(&input), Some("src".to_string()));
        assert_eq!(tool.to_auto_classifier_input(&input), "needle in src");
        assert_eq!(
            tool.to_auto_classifier_input(&serde_json::json!({"pattern": "needle"})),
            "needle"
        );
        let matcher = tool
            .prepare_permission_matcher(&input)
            .expect("Grep exposes a hook matcher");
        assert!(matcher("need*"));
        assert!(!matcher("other*"));
    }

    #[test]
    fn grep_validation_matches_official_path_errors_and_unc_guard() {
        use crate::tool::ToolCall as _;

        let cwd = std::env::temp_dir().join(format!(
            "cometix-grep-validation-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let suggested_name = format!("suggested-dir-{}", uuid::Uuid::new_v4().simple());
        std::fs::create_dir_all(cwd.join(&suggested_name)).unwrap();
        std::fs::write(cwd.join("file.txt"), "needle").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(cwd.clone()),
            ..crate::tool::ToolUseContext::default()
        };

        for input in [
            serde_json::json!({"pattern": "needle"}),
            serde_json::json!({"pattern": "needle", "path": "file.txt"}),
            serde_json::json!({"pattern": "needle", "path": "."}),
            serde_json::json!({"pattern": "needle", "path": "//server/share"}),
            serde_json::json!({"pattern": "needle", "path": "\\\\server\\share"}),
        ] {
            assert_eq!(
                GrepTool.validate_input(&input, &context),
                crate::tool::ValidationResult::Ok
            );
        }
        assert_eq!(
            GrepTool.validate_input(
                &serde_json::json!({"pattern": "needle", "path": "missing"}),
                &context,
            ),
            crate::tool::ValidationResult::Error {
                message: format!(
                    "Path does not exist: missing. Note: your current working directory is {}.",
                    cwd.display()
                ),
                error_code: 1,
            }
        );
        let mistaken = cwd.parent().unwrap().join(&suggested_name);
        assert_eq!(
            GrepTool.validate_input(
                &serde_json::json!({
                    "pattern": "needle",
                    "path": mistaken.display().to_string()
                }),
                &context,
            ),
            crate::tool::ValidationResult::Error {
                message: format!(
                    "Path does not exist: {}. Note: your current working directory is {}. Did you mean {}?",
                    mistaken.display(),
                    cwd.display(),
                    cwd.join(&suggested_name).display(),
                ),
                error_code: 1,
            }
        );
        assert_eq!(
            GrepTool.validate_input(
                &serde_json::json!({"pattern": "needle", "path": "bad\0path"}),
                &context,
            ),
            crate::tool::ValidationResult::Fatal {
                message: "Path contains null bytes".to_string(),
            }
        );
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn grep_permissions_cover_cwd_outside_deny_bypass_and_directory_symlinks() {
        use crate::tool::ToolCall as _;
        use crate::utils::permissions::permission_result::PermissionResult;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-permission-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-grep-permission-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("blocked")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // CC filesystem.ts:667-674 authorizes original cwd; the symlink target stays outside.
        let _project = PinnedProjectDir::at(&root);
        let mut context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        assert!(matches!(
            GrepTool.check_permissions(&serde_json::json!({"pattern": "x"}), &context),
            PermissionResult::Allow { .. }
        ));
        assert!(matches!(
            GrepTool.check_permissions(
                &serde_json::json!({"pattern": "x", "-i": "true", "head_limit": "1"}),
                &context,
            ),
            PermissionResult::Allow {
                updated_input: Some(ref input),
                ..
            } if input["-i"] == true && input["head_limit"] == 1
        ));
        // CC rejects this input at the zod boundary before `checkPermissions`
        // runs; the permission decision itself still resolves against getCwd().
        assert!(matches!(
            GrepTool.check_permissions(
                &serde_json::json!({"pattern": "x", "head_limit": "invalid"}),
                &context,
            ),
            PermissionResult::Allow { .. }
        ));
        assert!(matches!(
            GrepTool.check_permissions(
                &serde_json::json!({"pattern": "x", "path": outside.display().to_string()}),
                &context,
            ),
            PermissionResult::Ask { .. }
        ));

        context.tool_permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read",
                Some("blocked/**".to_string()),
            )],
        );
        assert!(matches!(
            GrepTool.check_permissions(
                &serde_json::json!({"pattern": "x", "path": "blocked"}),
                &context,
            ),
            PermissionResult::Deny { .. }
        ));

        let mut bypass = context.clone();
        bypass.tool_permission_context.mode =
            crate::types::permissions::PermissionMode::BypassPermissions;
        // `checkReadPermissionForTool` itself still reports Ask; the generic
        // permission orchestrator applies bypass mode after this tool decision.
        assert!(matches!(
            GrepTool.check_permissions(
                &serde_json::json!({"pattern": "x", "path": outside.display().to_string()}),
                &bypass,
            ),
            PermissionResult::Ask { .. }
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
            let clean_context = crate::tool::ToolUseContext {
                cwd_override: Some(root.clone()),
                ..crate::tool::ToolUseContext::default()
            };
            assert!(matches!(
                GrepTool.check_permissions(
                    &serde_json::json!({"pattern": "x", "path": "linked"}),
                    &clean_context,
                ),
                PermissionResult::Ask { .. }
            ));
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn filename_tiebreaker_tracks_javascript_locale_compare_for_common_paths() {
        let mut paths = vec!["B.rs", "a.rs", "A.rs", "b.rs", "ä.rs", "10.rs", "2.rs"];
        paths.sort_by(|left, right| javascript_locale_compare(left, right));
        assert_eq!(
            paths,
            vec!["10.rs", "2.rs", "a.rs", "A.rs", "ä.rs", "b.rs", "B.rs"]
        );
        assert_eq!(
            javascript_locale_compare("é.txt", "e\u{301}.txt"),
            std::cmp::Ordering::Equal
        );
        let mut canonically_equal = vec!["e\u{301}.txt", "é.txt"];
        canonically_equal.sort_by(|left, right| javascript_locale_compare(left, right));
        assert_eq!(canonically_equal, vec!["e\u{301}.txt", "é.txt"]);

        let production = sort_file_matches(
            vec![
                "old.rs".into(),
                "B.rs".into(),
                "new.rs".into(),
                "a.rs".into(),
            ],
            false,
            |path| match path {
                "new.rs" => 10.0,
                "a.rs" | "B.rs" => 5.0,
                _ => 1.0,
            },
        );
        assert_eq!(production, vec!["new.rs", "a.rs", "B.rs", "old.rs"]);
        let test_order = sort_file_matches(
            vec![
                "old.rs".into(),
                "B.rs".into(),
                "new.rs".into(),
                "a.rs".into(),
            ],
            true,
            |_| panic!("filename-only sort must not stat files"),
        );
        assert_eq!(test_order, vec!["a.rs", "B.rs", "new.rs", "old.rs"]);
    }

    #[test]
    fn javascript_slice_pagination_preserves_fractional_and_negative_numbers() {
        let items = (0..10).collect::<Vec<_>>();
        let zero = javascript_number(0.0);
        let fractional_limit = javascript_number(2.9);
        let fractional_offset = javascript_number(1.9);
        let (sliced, applied) =
            apply_head_limit(&items, Some(&fractional_limit), &fractional_offset);
        assert_eq!(sliced, vec![1, 2, 3]);
        assert_eq!(applied, Some(fractional_limit));

        let negative_offset = javascript_number(-3.0);
        let two = javascript_number(2.0);
        assert_eq!(
            apply_head_limit(&items, Some(&two), &negative_offset).0,
            vec![7, 8]
        );
        let negative_limit = javascript_number(-2.0);
        let (sliced, applied) = apply_head_limit(&items, Some(&negative_limit), &zero);
        assert_eq!(sliced, (0..8).collect::<Vec<_>>());
        assert_eq!(applied, Some(negative_limit));

        let unlimited = apply_head_limit(&items, Some(&zero), &javascript_number(-2.0));
        assert_eq!(unlimited, (vec![8, 9], None));
    }

    #[test]
    fn grep_execution_covers_official_modes_filters_context_and_multiline() {
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-modes-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.rs"), "before\nNeedle\nafter\n").unwrap();
        std::fs::write(root.join("b.txt"), "needle one\nneedle two\n").unwrap();
        std::fs::write(root.join(".hidden.rs"), "needle hidden\n").unwrap();
        std::fs::write(root.join(".git/config"), "needle vcs\n").unwrap();
        std::fs::write(root.join("multiline.rs"), "BEGIN\nmiddle\nEND\n").unwrap();
        std::fs::write(root.join("dash.rs"), "-flag\n").unwrap();
        #[cfg(unix)]
        std::fs::write(root.join("a\\b.special"), "backslash-token\n").unwrap();
        let abort = crate::tool::AbortController::default();

        let files = grep_output(
            &serde_json::json!({"pattern": "needle", "-i": true}),
            Some(&root),
            &abort,
        )
        .expect("files mode");
        assert!(files.filenames.iter().any(|path| path == "a.rs"));
        assert!(files.filenames.iter().any(|path| path == ".hidden.rs"));
        assert!(!files.filenames.iter().any(|path| path.contains(".git")));

        let content = grep_output(
            &serde_json::json!({
                "pattern": "needle",
                "-i": true,
                "glob": "*.rs",
                "output_mode": "content",
                "context": 1,
                "-B": 9,
                "-A": 9
            }),
            Some(&root),
            &abort,
        )
        .expect("content mode");
        let text = content.content.as_deref().unwrap_or_default();
        assert!(text.contains("a.rs-1-before"));
        assert!(text.contains("a.rs:2:Needle"));
        assert!(text.contains("a.rs-3-after"));
        assert!(!text.contains("b.txt"));

        let no_lines = grep_output(
            &serde_json::json!({
                "pattern": "Needle",
                "path": "a.rs",
                "output_mode": "content",
                "-n": false
            }),
            Some(&root),
            &abort,
        )
        .expect("file target without line numbers");
        assert_eq!(no_lines.content.as_deref(), Some("Needle"));
        let normalized_path = grep_output(
            &serde_json::json!({
                "pattern": "Needle",
                "path": "missing-parent/../a.rs"
            }),
            Some(&root),
            &abort,
        )
        .expect("expandPath normalizes the requested target");
        assert_eq!(normalized_path.filenames, vec!["a.rs"]);

        #[cfg(unix)]
        for mode in ["files_with_matches", "content", "count"] {
            let backslash = grep_output(
                &serde_json::json!({
                    "pattern": "backslash-token",
                    "output_mode": mode
                }),
                Some(&root),
                &abort,
            )
            .expect("POSIX backslash filename grep");
            let observed = backslash
                .content
                .clone()
                .unwrap_or_else(|| backslash.filenames.join("\n"));
            assert!(observed.contains("a\\b.special"), "{observed:?}");
            assert!(!observed.contains("a/b.special"), "{observed:?}");
        }

        let count = grep_output(
            &serde_json::json!({"pattern": "needle", "-i": true, "output_mode": "count"}),
            Some(&root),
            &abort,
        )
        .expect("count mode");
        assert_eq!(count.num_matches, Some(usize_number(4)));
        assert_eq!(count.num_files, usize_number(3));

        let dash = grep_output(
            &serde_json::json!({"pattern": "-flag", "type": "rust"}),
            Some(&root),
            &abort,
        )
        .expect("dash pattern and type filter");
        assert_eq!(dash.filenames, vec!["dash.rs"]);

        let single_line = grep_output(
            &serde_json::json!({"pattern": "BEGIN.*END", "output_mode": "content"}),
            Some(&root),
            &abort,
        )
        .expect("single-line default");
        assert_eq!(single_line.num_lines, Some(usize_number(0)));
        let multiline = grep_output(
            &serde_json::json!({
                "pattern": "BEGIN.*END",
                "output_mode": "content",
                "multiline": true
            }),
            Some(&root),
            &abort,
        )
        .expect("multiline mode");
        assert!(
            multiline
                .num_lines
                .as_ref()
                .is_some_and(|count| count.as_f64().unwrap_or(0.0) > 0.0)
        );
        assert!(
            multiline
                .content
                .as_deref()
                .is_some_and(|text| text.contains("middle"))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn grep_read_deny_rules_filter_every_mode_and_symlink_traversal_roots() {
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-deny-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-grep-deny-target-{}",
            uuid::Uuid::new_v4().simple()
        ));
        for directory in [
            root.join("secret"),
            root.join("rootless"),
            outside.join("secret"),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(root.join("visible.txt"), "needle visible\n").unwrap();
        std::fs::write(root.join("secret/hidden.txt"), "needle secret\n").unwrap();
        std::fs::write(root.join("rootless/hidden.txt"), "needle rootless\n").unwrap();
        std::fs::write(outside.join("visible.txt"), "needle outside\n").unwrap();
        std::fs::write(outside.join("secret/hidden.txt"), "needle outside secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

        let mut permissions = crate::tool::ToolPermissionContext::default();
        permissions.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some(format!("/{}", root.join("secret/**").display())),
                ),
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some("rootless/**".to_string()),
                ),
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some(format!("/{}", outside.join("secret/**").display())),
                ),
            ],
        );
        let abort = crate::tool::AbortController::default();
        for mode in ["files_with_matches", "content", "count"] {
            let output = grep_output_with_context(
                &serde_json::json!({"pattern": "needle", "output_mode": mode}),
                Some(&root),
                &abort,
                &permissions,
            )
            .expect("denied paths are hidden, not surfaced");
            let observed = output
                .content
                .clone()
                .unwrap_or_else(|| output.filenames.join("\n"));
            assert!(observed.contains("visible.txt"));
            assert!(!observed.contains("secret"));
            assert!(!observed.contains("rootless"));
        }

        let linked = grep_output_with_context(
            &serde_json::json!({
                "pattern": "needle",
                "path": "linked",
                "output_mode": "files_with_matches"
            }),
            Some(&root),
            &abort,
            &permissions,
        )
        .expect("approved symlink-root grep");
        assert_eq!(linked.filenames, vec!["linked/visible.txt"]);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn grep_excludes_orphaned_plugin_versions_with_session_frozen_cache() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct PluginStateGuard(Option<EnvVarGuard>);
        impl Drop for PluginStateGuard {
            fn drop(&mut self) {
                drop(self.0.take());
                crate::utils::plugins::orphaned_plugin_filter::clear_plugin_cache_exclusions();
            }
        }
        let plugins = std::env::temp_dir().join(format!(
            "cometix-grep-plugins-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let orphaned = plugins.join("cache/market/tool/1.0");
        let current = plugins.join("cache/market/tool/2.0");
        std::fs::create_dir_all(&orphaned).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(orphaned.join(".orphaned_at"), "now").unwrap();
        std::fs::write(orphaned.join("old.txt"), "needle old\n").unwrap();
        std::fs::write(current.join("new.txt"), "needle new\n").unwrap();
        let _guard = PluginStateGuard(Some(EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            &plugins,
        )));
        crate::utils::plugins::orphaned_plugin_filter::clear_plugin_cache_exclusions();

        let output = grep_output(
            &serde_json::json!({"pattern": "needle"}),
            Some(&plugins.join("cache")),
            &crate::tool::AbortController::default(),
        )
        .expect("plugin-cache grep");
        assert_eq!(output.filenames, vec!["market/tool/2.0/new.txt"]);

        // Once warmed, disk mutations do not alter this session's exclusions.
        std::fs::remove_file(orphaned.join(".orphaned_at")).unwrap();
        let frozen = grep_output(
            &serde_json::json!({"pattern": "needle"}),
            Some(&plugins.join("cache")),
            &crate::tool::AbortController::default(),
        )
        .expect("frozen plugin-cache grep");
        assert_eq!(frozen.filenames, vec!["market/tool/2.0/new.txt"]);
        let _ = std::fs::remove_dir_all(plugins);
    }

    #[test]
    fn grep_model_mapping_and_display_preserve_all_output_fields() {
        use crate::tool::ToolCall as _;
        use crate::types::message::{SearchResultMode, ToolResultStatus};

        let content = Output {
            mode: Some(SearchResultMode::Content),
            num_files: usize_number(0),
            filenames: vec![],
            content: Some("src/a.rs:2:needle".to_string()),
            num_lines: Some(usize_number(1)),
            num_matches: None,
            applied_limit: Some(javascript_number(2.5)),
            applied_offset: Some(usize_number(1)),
        };
        assert_eq!(
            GrepTool.map_tool_result_to_tool_result_block_param(
                &crate::tool::ToolOutput::Grep(content.clone()),
                "toolu-content",
            ),
            (
                "src/a.rs:2:needle\n\n[Showing results with pagination = limit: 2.5, offset: 1]"
                    .to_string(),
                ToolResultStatus::Success,
            )
        );
        // No Grep display shape — the trait projects the raw
        // `toolUseResult` with every output field intact.
        let raw = GrepTool
            .tool_use_result(&crate::tool::ToolOutput::Grep(content))
            .expect("grep success projects raw output");
        assert_eq!(
            raw,
            serde_json::json!({
                "mode": "content",
                "numFiles": 0,
                "filenames": [],
                "content": "src/a.rs:2:needle",
                "numLines": 1,
                "appliedLimit": 2.5,
                "appliedOffset": 1
            })
        );
        // The live projection always satisfies the tool's own output schema,
        // so the render-time `safeParse` gate never suppresses it.
        assert!(crate::tools::grep_tool::ui::parse_output(&raw).is_some());

        let count = Output {
            mode: Some(SearchResultMode::Count),
            num_files: usize_number(1),
            filenames: vec![],
            content: Some("src/a.rs:1".to_string()),
            num_lines: None,
            num_matches: Some(usize_number(1)),
            applied_limit: None,
            applied_offset: None,
        };
        assert_eq!(
            GrepTool
                .map_tool_result_to_tool_result_block_param(
                    &crate::tool::ToolOutput::Grep(count),
                    "toolu-count",
                )
                .0,
            "src/a.rs:1\n\nFound 1 total occurrence across 1 file."
        );

        let empty_files = Output {
            mode: Some(SearchResultMode::FilesWithMatches),
            num_files: usize_number(0),
            filenames: vec![],
            content: None,
            num_lines: None,
            num_matches: None,
            applied_limit: None,
            applied_offset: None,
        };
        assert_eq!(
            GrepTool
                .map_tool_result_to_tool_result_block_param(
                    &crate::tool::ToolOutput::Grep(empty_files),
                    "toolu-empty",
                )
                .0,
            "No files found"
        );
    }

    #[tokio::test]
    async fn grep_call_preserves_usage_error_and_abort_behavior() {
        use crate::tool::ToolCall as _;
        use crate::types::message::ToolResultStatus;

        let root = std::env::temp_dir().join(format!(
            "cometix-grep-call-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "needle\n").unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        let request = crate::utils::permissions::permissions::mock_permission_request(
            "permission-grep",
            "toolu-grep",
            "Grep",
            "needle",
            crate::types::permissions::PermissionMode::Default,
        );
        let usage_error = GrepTool
            .call(
                &serde_json::json!({"pattern": "["}),
                &request,
                &context,
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(
            usage_error.data,
            crate::tool::ToolOutput::Grep(Output { ref num_files, .. })
                if num_files == &usize_number(0)
        ));

        let canceled = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        canceled.abort_controller.abort();
        let canceled_result = GrepTool
            .call(
                &serde_json::json!({"pattern": "needle"}),
                &request,
                &canceled,
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(
            canceled_result.data,
            crate::tool::ToolOutput::Composed {
                status: ToolResultStatus::Error,
                ref content,            } if content.starts_with("Ripgrep search timed out after ")
        ));
        // The `Error: …` raw string now rides the row via the trait.
        assert!(matches!(
            GrepTool.tool_use_result(&canceled_result.data),
            Some(serde_json::Value::String(ref raw))
                if raw.starts_with("Error: Ripgrep search timed out after ")
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
