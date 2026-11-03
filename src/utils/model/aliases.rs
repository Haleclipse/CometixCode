//! Model alias tables.
//! Maps to CC `utils/model/aliases.ts`.

/// Maps to: CC `utils/model/aliases.ts:1-9` `MODEL_ALIASES`.
pub const MODEL_ALIASES: &[&str] = &[
    "sonnet",
    "opus",
    "haiku",
    "best",
    "sonnet[1m]",
    "opus[1m]",
    "opusplan",
];

/// Maps to: CC `utils/model/aliases.ts:12-14` `isModelAlias`.
pub fn is_model_alias(model_input: &str) -> bool {
    MODEL_ALIASES.contains(&model_input)
}

/// Maps to: CC `utils/model/aliases.ts:21` `MODEL_FAMILY_ALIASES` — bare family
/// names retained only for legacy display helpers.
pub const MODEL_FAMILY_ALIASES: &[&str] = &["sonnet", "opus", "haiku"];

/// Maps to: CC `utils/model/aliases.ts:23-25` `isModelFamilyAlias`.
pub fn is_model_family_alias(model: &str) -> bool {
    MODEL_FAMILY_ALIASES.contains(&model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_tables_match_the_official_lists() {
        assert_eq!(
            MODEL_ALIASES,
            [
                "sonnet",
                "opus",
                "haiku",
                "best",
                "sonnet[1m]",
                "opus[1m]",
                "opusplan"
            ]
        );
        assert_eq!(MODEL_FAMILY_ALIASES, ["sonnet", "opus", "haiku"]);
    }

    /// CC's `Array.includes` is case-sensitive and exact — callers lowercase
    /// before the lookup (`model.ts:449`, `modelAllowlist.ts:111`).
    #[test]
    fn alias_predicates_are_exact_matches() {
        assert!(is_model_alias("opusplan"));
        assert!(!is_model_alias("OpusPlan"));
        assert!(!is_model_alias("claude-opus-4-6"));
        assert!(is_model_family_alias("opus"));
        assert!(!is_model_family_alias("opus[1m]"));
        assert!(!is_model_family_alias("best"));
    }
}
