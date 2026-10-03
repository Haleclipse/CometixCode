//! Maps to: CC `hooks/useMainLoopModel.ts`.
//!
//! The returned value is a full model name that can be used directly in API
//! calls. Prefer this over [`crate::utils::model::model::get_main_loop_model`]
//! when the caller already has AppState model fields (including session
//! overrides) and needs alias → `ANTHROPIC_DEFAULT_*_MODEL` resolution.

use crate::utils::model::model::{get_default_main_loop_model_setting, parse_user_specified_model};

/// Maps to: CC `hooks/useMainLoopModel.ts` `useMainLoopModel()`.
///
/// Resolves `mainLoopModelForSession ?? mainLoopModel ?? defaultSetting`
/// through [`parse_user_specified_model`], matching the official React hook
/// (GrowthBook refresh re-render is not ported; env/settings already drive
/// alias targets via `ANTHROPIC_DEFAULT_*_MODEL`).
pub fn use_main_loop_model(
    main_loop_model: Option<&str>,
    main_loop_model_for_session: Option<&str>,
) -> String {
    let default_setting = get_default_main_loop_model_setting();
    let setting = main_loop_model_for_session
        .filter(|value| !value.trim().is_empty())
        .or_else(|| main_loop_model.filter(|value| !value.trim().is_empty()))
        .unwrap_or(default_setting.as_str());
    parse_user_specified_model(setting)
}

/// Convenience wrapper over AppState model fields.
///
/// Maps to the `useAppState(s => s.mainLoopModel*)` reads inside
/// CC `useMainLoopModel()`.
pub fn use_main_loop_model_from_app_state(
    main_loop_model: Option<&str>,
    main_loop_model_for_session: Option<&str>,
) -> String {
    use_main_loop_model(main_loop_model, main_loop_model_for_session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn use_main_loop_model_resolves_opus_alias_via_default_opus_env() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "grok-4.5");

        assert_eq!(use_main_loop_model(Some("opus"), None), "grok-4.5");
        assert_eq!(
            use_main_loop_model(Some("opus"), Some("sonnet")),
            parse_user_specified_model("sonnet")
        );
        assert_eq!(
            use_main_loop_model(None, None),
            parse_user_specified_model(&get_default_main_loop_model_setting())
        );

        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
    }
}
