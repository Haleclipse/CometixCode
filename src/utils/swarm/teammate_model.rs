//! Teammate model fallback.
//! Maps to: CC `utils/swarm/teammateModel.ts`.
//!
//! CC's file declares EXACTLY ONE symbol. `getDefaultTeammateModel` and
//! `resolveTeammateModel` are declared in `tools/shared/spawnMultiAgent.ts`
//! (`:72` / `:93`) and live in `tools/shared/spawn_multi_agent.rs` here — a
//! symbol belongs to the Rust file named after the CC file that DECLARES it,
//! regardless of which file references it.

/// Maps to: CC `utils/swarm/teammateModel.ts#getHardcodedTeammateModelFallback`.
///
/// @nyan: CC falls back to Opus 4.6. A teammate model is a profile ref, and
/// nyan has no built-in Anthropic default, so a teammate follows its leader.
pub fn get_hardcoded_teammate_model_fallback() -> String {
    "inherit".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardcoded_teammate_model_fallback_follows_the_leader() {
        assert_eq!(get_hardcoded_teammate_model_fallback(), "inherit");
    }
}
