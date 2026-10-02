//! Maps to: CC utils/plugins/parseMarketplaceInput.ts.
use serde_json::{Value, json};
/// Maps to: CC parseMarketplaceInput.ts#parseMarketplaceInput.
/// Option<Value> carries source/schema input, {error:string}, or null separately.
pub async fn parse_marketplace_input(input: &str) -> Option<Value> {
    let trimmed=input.trim_matches(|c:char|matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
    let ssh = regex::Regex::new(r"^([a-zA-Z0-9._-]+@[^:]+:.+?(?:\.git)?)(#(.+))?$").unwrap();
    if let Some(c) = ssh.captures(trimmed) {
        let mut result = json!({"source":"git","url":&c[1]});
        if let Some(r) = c.get(3) {
            result["ref"] = r.as_str().into();
        }
        return Some(result);
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let regex = regex::Regex::new(r"^([^#]+)(#(.+))?$").unwrap();
        let capture = regex.captures(trimmed);
        let base = capture
            .as_ref()
            .and_then(|c| c.get(1))
            .map_or(trimmed, |m| m.as_str());
        let git_ref = capture.as_ref().and_then(|c| c.get(3)).map(|m| m.as_str());
        if base.ends_with(".git") || base.contains("/_git/") {
            let mut result = json!({"source":"git","url":base});
            if let Some(r) = git_ref {
                result["ref"] = r.into();
            }
            return Some(result);
        }
        if let Ok(url) = url::Url::parse(base) {
            if matches!(url.host_str(), Some("github.com" | "www.github.com"))
                && regex::Regex::new(r"^/([^/]+/[^/]+?)(/|\.git|$)")
                    .unwrap()
                    .is_match(url.path())
            {
                let mut result = json!({"source":"git","url":format!("{base}.git")});
                if let Some(r) = git_ref {
                    result["ref"] = r.into();
                }
                return Some(result);
            }
        }
        return Some(json!({"source":"url","url":base}));
    }
    let windows = cfg!(windows)
        && (trimmed.starts_with(".\\")
            || trimmed.starts_with("..\\")
            || regex::Regex::new(r"^[a-zA-Z]:[/\\]")
                .unwrap()
                .is_match(trimmed));
    if trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with('~')
        || windows
    {
        let raw = if let Some(rest) = trimmed.strip_prefix('~') {
            format!("{}{rest}", crate::utils::node_os::homedir().display())
        } else {
            trimmed.to_owned()
        };
        let raw = std::path::PathBuf::from(raw);
        let absolute = if raw.is_absolute() {
            raw
        } else {
            std::env::current_dir().unwrap_or_default().join(raw)
        };
        // Native representation of path.resolve: lexical, no realpath or NFC.
        let mut path = std::path::PathBuf::new();
        for c in absolute.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    path.pop();
                }
                c => path.push(c.as_os_str()),
            }
        }
        let display = path.to_string_lossy().into_owned();
        let metadata = match crate::utils::fs_operations::get_fs_implementation()
            .stat(&path)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                let message = if e.kind() == std::io::ErrorKind::NotFound {
                    format!("Path does not exist: {display}")
                } else {
                    let code = match e.raw_os_error() {
                        Some(libc::EACCES) => "EACCES".into(),
                        Some(libc::EPERM) => "EPERM".into(),
                        Some(libc::ENOTDIR) => "ENOTDIR".into(),
                        Some(libc::ELOOP) => "ELOOP".into(),
                        _ => e.to_string(),
                    };
                    format!("Cannot access path: {display} ({code})")
                };
                return Some(json!({"error":message}));
            }
        };
        return Some(if metadata.is_file() {
            if display.ends_with(".json") {
                json!({"source":"file","path":display})
            } else {
                json!({"error":format!("File path must point to a .json file (marketplace.json), but got: {display}")})
            }
        } else if metadata.is_dir() {
            json!({"source":"directory","path":display})
        } else {
            json!({"error":format!("Path is neither a file nor a directory: {display}")})
        });
    }
    if trimmed.contains('/') && !trimmed.starts_with('@') {
        if trimmed.contains(':') {
            return None;
        }
        let regex = regex::Regex::new(r"^([^#@]+)(?:[#@](.+))?$").unwrap();
        let captures = regex.captures(trimmed);
        let repo = captures
            .as_ref()
            .and_then(|c| c.get(1))
            .map_or(trimmed, |m| m.as_str());
        let mut result = json!({"source":"github","repo":repo});
        if let Some(r) = captures.as_ref().and_then(|c| c.get(2)) {
            result["ref"] = r.as_str().into();
        }
        return Some(result);
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn marketplace_inputs_match_official_protocol_and_fragment_precedence() {
        // Source native oracle: plugin-panel-0914/parse-input-oracle.json.
        for (input, expected) in [
            (
                "owner/repo@main",
                json!({"source":"github","repo":"owner/repo","ref":"main"}),
            ),
            (
                "owner/repo#",
                json!({"source":"github","repo":"owner/repo#"}),
            ),
            (
                "https://a/_git/r#dev",
                json!({"source":"git","url":"https://a/_git/r","ref":"dev"}),
            ),
            (
                "https://github.com/o/r/tree/main",
                json!({"source":"git","url":"https://github.com/o/r/tree/main.git"}),
            ),
            (
                "https://a/x.json#r",
                json!({"source":"url","url":"https://a/x.json"}),
            ),
        ] {
            assert_eq!(parse_marketplace_input(input).await, Some(expected));
        }
        assert_eq!(parse_marketplace_input("@scope/pkg").await, None);
    }
}
