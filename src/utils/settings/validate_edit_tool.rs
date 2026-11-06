//! Settings-file validation used by Edit.
//!
//! Maps to: CC `utils/settings/validateEditTool.ts` — a thin gate over
//! `validateSettingsFileContent`: only Claude settings paths are checked, an
//! already-invalid file never blocks the edit (repairs stay possible), and a
//! valid→invalid transition returns the full error + schema message.

use std::path::Path;

use super::validation::{SettingsFileValidation, validate_settings_file_content};

/// Maps to: CC `validateInputForSettingsFileEdit(filePath, originalContent,
/// getUpdatedContent)`. Returns the blocking message when the edit would turn
/// a valid settings file invalid; `None` allows the edit. (The Rust seam
/// passes the updated content by value and the cwd for the path guard.)
pub fn validate_input_for_settings_file_edit(
    file_path: &Path,
    original_content: &str,
    updated_content: &str,
    cwd: &Path,
) -> Option<String> {
    // Only validate Claude settings files.
    if !crate::utils::permissions::filesystem::is_claude_settings_path(
        &file_path.display().to_string(),
        cwd,
    ) {
        return None;
    }

    // If the before version is invalid, allow the edit (don't block repairs).
    if matches!(
        validate_settings_file_content(original_content),
        SettingsFileValidation::Invalid { .. }
    ) {
        return None;
    }

    // If the before version is valid, ensure the after version is also valid.
    match validate_settings_file_content(updated_content) {
        SettingsFileValidation::Valid => None,
        SettingsFileValidation::Invalid { error, full_schema } => Some(format!(
            "nyan settings.json validation failed after edit:\n{error}\n\nFull schema:\n{full_schema}\nIMPORTANT: Do not update the env unless explicitly instructed to do so."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".claude/settings.json")
    }

    fn cwd() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    #[test]
    fn settings_edit_validation_blocks_valid_to_invalid_but_allows_repairs() {
        // valid → invalid is blocked with the full message.
        let message = validate_input_for_settings_file_edit(
            &settings_path(),
            r#"{"language": "zh"}"#,
            r#"{"language": 5}"#,
            &cwd(),
        )
        .expect("valid-to-invalid must block");
        assert!(message.starts_with("nyan settings.json validation failed after edit:\n"));
        assert!(message.contains("- language: Expected string, but received number"));
        assert!(message.contains("Full schema:\n{\n  \"$schema\""));
        assert!(
            message.ends_with(
                "IMPORTANT: Do not update the env unless explicitly instructed to do so."
            )
        );

        // invalid → anything is allowed (repairs must not be blocked).
        assert!(
            validate_input_for_settings_file_edit(
                &settings_path(),
                r#"{"language": 5}"#,
                r#"{"language": 6}"#,
                &cwd(),
            )
            .is_none()
        );

        // Non-settings paths are never validated.
        assert!(
            validate_input_for_settings_file_edit(
                std::path::Path::new("/tmp/other.json"),
                r#"{"language": "zh"}"#,
                "{ not json",
                &cwd(),
            )
            .is_none()
        );
    }

    #[test]
    fn strict_settings_schema_rejects_unknown_top_level_fields() {
        let message = validate_input_for_settings_file_edit(
            &settings_path(),
            r#"{"language": "zh"}"#,
            r#"{"language": "zh", "definitelyNotAClaudeSetting": true}"#,
            &cwd(),
        )
        .expect("unknown key must block under .strict()");
        assert!(message.contains("Unrecognized field: definitelyNotAClaudeSetting"));
    }

    #[test]
    fn zod_catch_fields_accept_invalid_or_future_values() {
        // .catch(undefined) fields degrade instead of failing the edit.
        assert!(
            validate_input_for_settings_file_edit(
                &settings_path(),
                r#"{"language": "zh"}"#,
                r#"{"language": "zh", "strictPluginOnlyCustomization": "skills"}"#,
                &cwd(),
            )
            .is_none()
        );
    }

    #[test]
    fn source_zod_coercions_and_refinements_control_valid_to_invalid_gate() {
        // env values coerce, so numbers stay valid.
        assert!(
            validate_input_for_settings_file_edit(
                &settings_path(),
                r#"{"language": "zh"}"#,
                r#"{"env": {"PORT": 8080}}"#,
                &cwd(),
            )
            .is_none()
        );
        // A marketplace refinement failure blocks (reserved settings name).
        let message = validate_input_for_settings_file_edit(
            &settings_path(),
            r#"{"language": "zh"}"#,
            r#"{"extraKnownMarketplaces": {"agent-skills": {"source": {"source": "settings", "name": "agent-skills", "plugins": []}}}}"#,
            &cwd(),
        )
        .expect("reserved marketplace name must block");
        assert!(
            message.contains(
                "Reserved official marketplace names cannot be used with settings sources"
            )
        );
    }
}
