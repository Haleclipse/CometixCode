//! Maps to: CC `utils/releaseNotes.ts` and `utils/logoV2Utils.ts` release-note helpers.
//!
//! Official Claude Code fetches the public changelog in the background and
//! stores it under `~/.claude/cache/changelog.md`, then render paths read an
//! in-memory cache. Cometix keeps this runtime producer read-only: it only reads
//! an existing cache file and never fetches from GitHub, writes cache/config, or
//! updates `lastReleaseNotesSeen`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_RELEASE_NOTES_SHOWN: usize = 5;
const LOGO_RECENT_VERSION_COUNT: usize = 3;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseNotesStatus {
    pub has_release_notes: bool,
    pub release_notes: Vec<String>,
}

/// Maps to: CC `utils/releaseNotes.ts` `getChangelogCachePath`.
pub fn changelog_cache_path_from_config_home(config_home: &Path) -> PathBuf {
    config_home.join("cache").join("changelog.md")
}

/// Maps to: CC `utils/releaseNotes.ts` `getChangelogCachePath`.
pub fn changelog_cache_path() -> PathBuf {
    changelog_cache_path_from_config_home(&crate::utils::env_utils::get_claude_config_home_dir())
}

/// Maps to: CC `utils/releaseNotes.ts` `getStoredChangelogFromMemory` render seam.
/// Cometix reads the cache file directly as a read-only substitute for the
/// startup-populated memory cache.
pub fn read_cached_changelog_readonly() -> String {
    std::fs::read_to_string(changelog_cache_path()).unwrap_or_default()
}

/// Maps to: CC `utils/releaseNotes.ts` `parseChangelog`.
pub fn parse_changelog(content: &str) -> HashMap<String, Vec<String>> {
    let mut release_notes: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_version: Option<String> = None;
    let mut current_notes: Vec<String> = Vec::new();

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(version) = current_version.take() {
                if !current_notes.is_empty() {
                    release_notes.insert(version, std::mem::take(&mut current_notes));
                }
            }

            let version = rest
                .split(" - ")
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            current_version = version;
            continue;
        }

        let trimmed = line.trim();
        if let Some(note) = trimmed.strip_prefix("- ") {
            let note = note.trim();
            if !note.is_empty() && current_version.is_some() {
                current_notes.push(note.to_string());
            }
        }
    }

    if let Some(version) = current_version {
        if !current_notes.is_empty() {
            release_notes.insert(version, current_notes);
        }
    }

    release_notes
}

/// Maps to: CC `utils/releaseNotes.ts` `getRecentReleaseNotes`.
pub fn get_recent_release_notes_from_changelog(
    current_version: &str,
    previous_version: Option<&str>,
    changelog_content: &str,
) -> Vec<String> {
    let release_notes = parse_changelog(changelog_content);
    let base_current = coerce_semver_triplet(current_version);
    let base_previous = previous_version.and_then(coerce_semver_triplet);

    let should_show = base_previous.is_none()
        || base_current
            .zip(base_previous)
            .is_some_and(|(current, previous)| current > previous);
    if !should_show {
        return Vec::new();
    }

    let mut versions = release_notes
        .keys()
        .filter(|version| {
            base_previous.is_none()
                || coerce_semver_triplet(version)
                    .is_some_and(|version_triplet| Some(version_triplet) > base_previous)
        })
        .cloned()
        .collect::<Vec<_>>();
    sort_versions_newest_first(&mut versions);

    versions
        .into_iter()
        .filter_map(|version| release_notes.get(&version))
        .flat_map(|notes| notes.iter())
        .filter(|note| !note.is_empty())
        .take(MAX_RELEASE_NOTES_SHOWN)
        .cloned()
        .collect()
}

/// Maps to: CC `utils/releaseNotes.ts` `checkForReleaseNotesSync`.
pub fn check_for_release_notes_sync_readonly(
    current_version: &str,
    last_seen_version: Option<&str>,
    changelog_content: &str,
) -> ReleaseNotesStatus {
    let release_notes = get_recent_release_notes_from_changelog(
        current_version,
        last_seen_version,
        changelog_content,
    );
    ReleaseNotesStatus {
        has_release_notes: !release_notes.is_empty(),
        release_notes,
    }
}

/// Maps to: CC `utils/logoV2Utils.ts` `getRecentReleaseNotesSync(maxItems)`.
pub fn get_recent_release_notes_for_logo_from_changelog(
    changelog_content: &str,
    max_items: usize,
) -> Vec<String> {
    let release_notes = parse_changelog(changelog_content);
    let mut versions = release_notes.keys().cloned().collect::<Vec<_>>();
    sort_versions_newest_first(&mut versions);

    versions
        .into_iter()
        .take(LOGO_RECENT_VERSION_COUNT)
        .filter_map(|version| release_notes.get(&version))
        .flat_map(|notes| notes.iter())
        .filter(|note| !note.is_empty())
        .take(max_items)
        .cloned()
        .collect()
}

fn sort_versions_newest_first(versions: &mut [String]) {
    versions.sort_by(
        |a, b| match (coerce_semver_triplet(a), coerce_semver_triplet(b)) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => b.cmp(a),
        },
    );
}

fn coerce_semver_triplet(value: &str) -> Option<[u64; 3]> {
    let start = value.find(|ch: char| ch.is_ascii_digit())?;
    let mut parts = Vec::new();
    let mut current = String::new();

    for ch in value[start..].chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if ch == '.' {
            if current.is_empty() {
                break;
            }
            parts.push(current.parse::<u64>().ok()?);
            current.clear();
            if parts.len() == 3 {
                break;
            }
        } else {
            break;
        }
    }

    if !current.is_empty() && parts.len() < 3 {
        parts.push(current.parse::<u64>().ok()?);
    }
    if parts.is_empty() {
        return None;
    }
    while parts.len() < 3 {
        parts.push(0);
    }
    Some([parts[0], parts[1], parts[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = r#"# Changelog

## 2.1.0 - 2026-01-02
- Added browser automation
- Improved MCP status

## 2.0.0
- New logo feed

## 1.9.0 - 2025-12-01
- Older note
"#;

    #[test]
    fn parse_changelog_matches_official_heading_and_bullet_shape() {
        let parsed = parse_changelog(CHANGELOG);
        assert_eq!(
            parsed.get("2.1.0").expect("2.1.0 notes"),
            &vec![
                "Added browser automation".to_string(),
                "Improved MCP status".to_string()
            ]
        );
        assert_eq!(
            parsed.get("2.0.0").expect("2.0.0 notes"),
            &vec!["New logo feed".to_string()]
        );
    }

    #[test]
    fn recent_release_notes_match_official_last_seen_gate_and_limit() {
        let unseen = get_recent_release_notes_from_changelog("2.1.0", Some("2.0.0"), CHANGELOG);
        assert_eq!(
            unseen,
            vec![
                "Added browser automation".to_string(),
                "Improved MCP status".to_string()
            ]
        );

        let none = get_recent_release_notes_from_changelog("2.1.0", Some("2.1.0"), CHANGELOG);
        assert!(none.is_empty());

        let first_run = get_recent_release_notes_from_changelog("2.1.0", None, CHANGELOG);
        assert_eq!(first_run.len(), 4);
    }

    #[test]
    fn logo_recent_release_notes_use_top_three_recent_versions() {
        let notes = get_recent_release_notes_for_logo_from_changelog(CHANGELOG, 3);
        assert_eq!(
            notes,
            vec![
                "Added browser automation".to_string(),
                "Improved MCP status".to_string(),
                "New logo feed".to_string()
            ]
        );
    }

    #[test]
    fn changelog_cache_path_matches_official_cache_location() {
        assert_eq!(
            changelog_cache_path_from_config_home(Path::new("/tmp/claude")),
            Path::new("/tmp/claude").join("cache").join("changelog.md")
        );
    }
}
