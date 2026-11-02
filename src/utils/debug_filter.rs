//! Debug category filter — Maps to: CC `utils/debugFilter.ts`.

/// Maps to CC `DebugFilter`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub is_exclusive: bool,
}

/// Maps to CC `parseDebugFilter`.
///
/// Examples:
/// - `"api,hooks"` → include only api and hooks
/// - `"!1p,!file"` → exclude those categories
/// - empty / mixed include+exclude → `None` (show all)
pub fn parse_debug_filter(filter_string: Option<&str>) -> Option<DebugFilter> {
    let raw = filter_string?.trim();
    if raw.is_empty() {
        return None;
    }

    let filters: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect();
    if filters.is_empty() {
        return None;
    }

    let has_exclusive = filters.iter().any(|f| f.starts_with('!'));
    let has_inclusive = filters.iter().any(|f| !f.starts_with('!'));
    if has_exclusive && has_inclusive {
        // CC: mixed filters are treated as error → show all.
        return None;
    }

    let clean: Vec<String> = filters
        .iter()
        .map(|f| f.trim_start_matches('!').to_lowercase())
        .collect();

    Some(DebugFilter {
        include: if has_exclusive {
            Vec::new()
        } else {
            clean.clone()
        },
        exclude: if has_exclusive { clean } else { Vec::new() },
        is_exclusive: has_exclusive,
    })
}

/// Maps to CC `extractDebugCategories`.
pub fn extract_debug_categories(message: &str) -> Vec<String> {
    let mut categories = Vec::new();

    let mcp_re = regex_lite_mcp_server(message);
    if let Some(name) = mcp_re {
        categories.push("mcp".to_string());
        categories.push(name);
    } else if let Some(prefix) = prefix_category(message) {
        categories.push(prefix);
    }

    if let Some(bracket) = bracket_category(message) {
        categories.push(bracket);
    }

    if message.to_lowercase().contains("1p event:") {
        categories.push("1p".to_string());
    }

    if let Some(secondary) = secondary_category(message)
        && secondary.len() < 30
        && !secondary.contains(' ')
    {
        categories.push(secondary);
    }

    categories.sort();
    categories.dedup();
    categories
}

fn regex_lite_mcp_server(message: &str) -> Option<String> {
    // MCP server "name" or 'name'
    let rest = message.strip_prefix("MCP server ")?;
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_lowercase())
}

fn prefix_category(message: &str) -> Option<String> {
    let end = message.find(':').or_else(|| message.find('['))?;
    let prefix = message[..end].trim();
    if prefix.is_empty() || prefix.contains('[') {
        return None;
    }
    Some(prefix.to_lowercase())
}

fn bracket_category(message: &str) -> Option<String> {
    let rest = message.strip_prefix('[')?;
    let end = rest.find(']')?;
    let inner = rest[..end].trim();
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_lowercase())
}

fn secondary_category(message: &str) -> Option<String> {
    // ": something type:" / mode: / status: / event:
    let after_first = message.split_once(':')?.1.trim();
    let (candidate, _) = after_first.split_once(':')?;
    let candidate = candidate
        .trim()
        .trim_end_matches(" type")
        .trim_end_matches(" mode")
        .trim_end_matches(" status")
        .trim_end_matches(" event")
        .trim();
    if candidate.is_empty() {
        return None;
    }
    Some(candidate.to_lowercase())
}

/// Maps to CC `shouldShowDebugCategories`.
pub fn should_show_debug_categories(categories: &[String], filter: Option<&DebugFilter>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if categories.is_empty() {
        return false;
    }
    if filter.is_exclusive {
        !categories.iter().any(|cat| filter.exclude.contains(cat))
    } else {
        categories.iter().any(|cat| filter.include.contains(cat))
    }
}

/// Maps to CC `shouldShowDebugMessage`.
pub fn should_show_debug_message(message: &str, filter: Option<&DebugFilter>) -> bool {
    if filter.is_none() {
        return true;
    }
    let categories = extract_debug_categories(message);
    should_show_debug_categories(&categories, filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_include_and_exclude_filters() {
        let include = parse_debug_filter(Some("api,hooks")).unwrap();
        assert!(!include.is_exclusive);
        assert_eq!(include.include, vec!["api", "hooks"]);

        let exclude = parse_debug_filter(Some("!1p,!file")).unwrap();
        assert!(exclude.is_exclusive);
        assert_eq!(exclude.exclude, vec!["1p", "file"]);

        assert!(parse_debug_filter(Some("api,!file")).is_none());
        assert!(parse_debug_filter(Some("")).is_none());
    }

    #[test]
    fn should_show_respects_include_filter() {
        let filter = parse_debug_filter(Some("api")).unwrap();
        assert!(should_show_debug_message(
            "api: request sent",
            Some(&filter)
        ));
        assert!(!should_show_debug_message("hooks: ran", Some(&filter)));
        assert!(!should_show_debug_message("uncategorized", Some(&filter)));
    }
}
