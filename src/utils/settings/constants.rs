//! Setting-source constants and selection policy.
//!
//! Maps to CC `utils/settings/constants.ts`.

/// Maps to: CC `utils/settings/constants.ts:201-203` — the `$schema` value
/// settings.json files may carry (editable at SchemaStore).
pub const CLAUDE_CODE_SETTINGS_SCHEMA_URL: &str =
    "https://json.schemastore.org/claude-code-settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingSource {
    User,
    Project,
    Local,
    Flag,
    Policy,
}

/// Maps to: CC `utils/settings/constants.ts:182-185#EditableSettingSource`.
/// Rust carries the source's excluded union as a separate enum so functions
/// accepting editable destinations cannot receive policy or flag settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditableSettingSource {
    User,
    Project,
    Local,
}

/// Representation-only widening of CC `EditableSettingSource` to `SettingSource`.
impl From<EditableSettingSource> for SettingSource {
    fn from(source: EditableSettingSource) -> Self {
        match source {
            EditableSettingSource::User => Self::User,
            EditableSettingSource::Project => Self::Project,
            EditableSettingSource::Local => Self::Local,
        }
    }
}

/// Maps to: CC `utils/settings/constants.ts:191-195#SOURCES`.
/// Editable destinations in the source permission/hook save UI order.
pub const SOURCES: [SettingSource; 3] = [
    SettingSource::Local,
    SettingSource::Project,
    SettingSource::User,
];

/// Maps to: CC `utils/settings/constants.ts#getSettingSourceName`.
pub fn get_setting_source_name(source: SettingSource) -> &'static str {
    match source {
        SettingSource::User => "user",
        SettingSource::Project => "project",
        SettingSource::Local => "project, gitignored",
        SettingSource::Flag => "cli flag",
        SettingSource::Policy => "managed",
    }
}

/// Maps to: CC
/// `utils/settings/constants.ts#getSettingSourceDisplayNameCapitalized`.
pub(crate) fn get_setting_source_display_name_capitalized(source: SettingSource) -> &'static str {
    match source {
        SettingSource::User => "User settings",
        SettingSource::Project => "Shared project settings",
        SettingSource::Local => "Project local settings",
        SettingSource::Flag => "Command line arguments",
        SettingSource::Policy => "Enterprise managed settings",
    }
}

/// Maps to: CC `utils/settings/constants.ts#parseSettingSourcesFlag`.
pub fn parse_setting_sources_flag(flag: &str) -> Result<Vec<SettingSource>, String> {
    if flag.is_empty() {
        return Ok(Vec::new());
    }
    flag.split(',')
        .map(str::trim)
        .map(|name| match name {
            "user" => Ok(SettingSource::User),
            "project" => Ok(SettingSource::Project),
            "local" => Ok(SettingSource::Local),
            _ => Err(format!(
                "Invalid setting source: {name}. Valid options are: user, project, local"
            )),
        })
        .collect()
}

/// Maps to: CC `utils/settings/constants.ts#getEnabledSettingSources`.
pub fn get_enabled_setting_sources() -> Vec<SettingSource> {
    let allowed = crate::bootstrap::state::get_allowed_setting_sources();
    let mut sources = Vec::new();
    for (name, source) in [
        ("userSettings", SettingSource::User),
        ("projectSettings", SettingSource::Project),
        ("localSettings", SettingSource::Local),
    ] {
        if allowed.iter().any(|allowed| allowed == name) {
            sources.push(source);
        }
    }
    // Policy and CLI flag settings cannot be disabled.
    sources.push(SettingSource::Flag);
    sources.push(SettingSource::Policy);
    sources
}

/// Maps to: CC `utils/settings/constants.ts#isSettingSourceEnabled`.
pub fn is_setting_source_enabled(source: SettingSource) -> bool {
    get_enabled_setting_sources().contains(&source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    struct AllowedSourcesRestore(Vec<String>);

    impl AllowedSourcesRestore {
        fn capture() -> Self {
            Self(crate::bootstrap::state::get_allowed_setting_sources())
        }
    }

    impl Drop for AllowedSourcesRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
        }
    }

    #[test]
    fn setting_source_selection_gates_editable_sources_but_not_policy_or_flag() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = AllowedSourcesRestore::capture();

        crate::bootstrap::state::set_allowed_setting_sources(vec![
            "userSettings".to_string(),
            "localSettings".to_string(),
        ]);
        assert_eq!(
            get_enabled_setting_sources(),
            vec![
                SettingSource::User,
                SettingSource::Local,
                SettingSource::Flag,
                SettingSource::Policy,
            ]
        );
        assert!(is_setting_source_enabled(SettingSource::User));
        assert!(!is_setting_source_enabled(SettingSource::Project));
        assert!(is_setting_source_enabled(SettingSource::Flag));
        assert!(is_setting_source_enabled(SettingSource::Policy));

        crate::bootstrap::state::set_allowed_setting_sources(Vec::new());
        assert_eq!(
            get_enabled_setting_sources(),
            vec![SettingSource::Flag, SettingSource::Policy]
        );
    }

    #[test]
    fn setting_sources_flag_parser_matches_official_values_and_error() {
        assert_eq!(
            parse_setting_sources_flag("user, project,local").unwrap(),
            vec![
                SettingSource::User,
                SettingSource::Project,
                SettingSource::Local,
            ]
        );
        assert!(parse_setting_sources_flag("").unwrap().is_empty());
        assert_eq!(
            parse_setting_sources_flag("workspace").unwrap_err(),
            "Invalid setting source: workspace. Valid options are: user, project, local"
        );
    }
}
