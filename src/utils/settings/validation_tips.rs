//! Contextual tips attached to settings validation errors.
//!
//! Maps to: CC `utils/settings/validationTips.ts`.

/// Maps to: CC `ValidationTip`.
///
/// @nyan: no `docLink`. CC's links point at its hosted docs site, which
/// describes a different product.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValidationTip {
    pub suggestion: Option<String>,
}

/// Maps to: CC `TipContext` — what `formatZodError` knows about an issue.
pub struct TipContext<'a> {
    pub path: &'a str,
    pub code: &'a str,
    pub expected: Option<&'a str>,
    pub received: Option<&'a str>,
    pub enum_values: Option<&'a [String]>,
}

/// One `TIP_MATCHERS` entry: the match predicate and the tip copy, verbatim
/// from CC's table.
struct TipMatcher {
    matches: fn(&TipContext) -> bool,
    suggestion: Option<&'static str>,
}

const TIP_MATCHERS: &[TipMatcher] = &[
    TipMatcher {
        matches: |ctx| ctx.path == "permissions.defaultMode" && ctx.code == "invalid_value",
        suggestion: Some(
            "Valid modes: \"acceptEdits\" (ask before file changes), \"plan\" (analysis only), \"bypassPermissions\" (auto-accept all), or \"default\" (standard behavior)",
        ),
    },
    TipMatcher {
        matches: |ctx| ctx.path == "apiKeyHelper" && ctx.code == "invalid_type",
        suggestion: Some(
            "Provide a shell command that outputs your API key to stdout. The script should output only the API key. Example: \"/bin/generate_temp_api_key.sh\"",
        ),
    },
    TipMatcher {
        matches: |ctx| {
            ctx.path == "cleanupPeriodDays" && ctx.code == "too_small" && ctx.expected == Some("0")
        },
        suggestion: Some(
            "Must be 0 or greater. Set a positive number for days to retain transcripts (default is 30). Setting 0 disables session persistence entirely: no transcripts are written and existing transcripts are deleted at startup.",
        ),
    },
    TipMatcher {
        matches: |ctx| ctx.path.starts_with("env.") && ctx.code == "invalid_type",
        suggestion: Some(
            "Environment variables must be strings. Wrap numbers and booleans in quotes. Example: \"DEBUG\": \"true\", \"PORT\": \"3000\"",
        ),
    },
    TipMatcher {
        matches: |ctx| {
            (ctx.path == "permissions.allow" || ctx.path == "permissions.deny")
                && ctx.code == "invalid_type"
                && ctx.expected == Some("array")
        },
        suggestion: Some(
            "Permission rules must be in an array. Format: [\"Tool(specifier)\"]. Examples: [\"Bash(npm run build)\", \"Edit(docs/**)\", \"Read(~/.zshrc)\"]. Use * for wildcards.",
        ),
    },
    TipMatcher {
        matches: |ctx| ctx.path.contains("hooks") && ctx.code == "invalid_type",
        // gh-31187 / CC-282: the matcher is a string (exact, pipe-separated,
        // or empty), never an object — a prior tip showing an object format
        // sent users in circles.
        suggestion: Some(
            "Hooks use a matcher + hooks array. The matcher is a string: a tool name (\"Bash\"), pipe-separated list (\"Edit|Write\"), or empty to match all. Example: {\"PostToolUse\": [{\"matcher\": \"Edit|Write\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo Done\"}]}]}",
        ),
    },
    TipMatcher {
        matches: |ctx| ctx.code == "invalid_type" && ctx.expected == Some("boolean"),
        suggestion: Some(
            "Use true or false without quotes. Example: \"includeCoAuthoredBy\": true",
        ),
    },
    TipMatcher {
        matches: |ctx| ctx.code == "unrecognized_keys",
        suggestion: Some("Check for typos or refer to the documentation for valid fields"),
    },
    TipMatcher {
        matches: |ctx| ctx.code == "invalid_value" && ctx.enum_values.is_some(),
        suggestion: None,
    },
    TipMatcher {
        matches: |ctx| {
            ctx.code == "invalid_type"
                && ctx.expected == Some("object")
                && ctx.received == Some("null")
                && ctx.path.is_empty()
        },
        suggestion: Some(
            "Check for missing commas, unmatched brackets, or trailing commas. Use a JSON validator to identify the exact syntax error.",
        ),
    },
    TipMatcher {
        matches: |ctx| {
            ctx.path == "permissions.additionalDirectories" && ctx.code == "invalid_type"
        },
        suggestion: Some(
            "Must be an array of directory paths. Example: [\"~/projects\", \"/tmp/workspace\"]. You can also use --add-dir flag or /add-dir command",
        ),
    },
];

/// Maps to: CC `getValidationTip(context)`.
pub fn get_validation_tip(context: &TipContext) -> Option<ValidationTip> {
    let matcher = TIP_MATCHERS.iter().find(|m| (m.matches)(context))?;

    let mut tip = ValidationTip {
        suggestion: matcher.suggestion.map(str::to_string),
    };

    if context.code == "invalid_value"
        && tip.suggestion.is_none()
        && let Some(values) = context.enum_values
    {
        tip.suggestion = Some(format!(
            "Valid values: {}",
            values
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Some(tip)
}
