//! Maps to: CC `components/sandbox/SandboxDoctorSection.tsx`.
//!
//! Doctor-only sandbox status panel. The official component reads
//! `SandboxManager.isSupportedPlatform()`, `isSandboxEnabledInSettings()`, and
//! `checkDependencies()`. This port uses the safe read-only sandbox adapter
//! equivalents: no sandbox runtime is initialized and no commands are wrapped.

use crate::utils::sandbox::sandbox_adapter::{
    SandboxDependencyCheck, check_dependencies_readonly, get_sandbox_enabled_setting,
    sandbox_unavailable_message_for_platform,
};
use crate::utils::settings::types::SettingsJson;
use iocraft::prelude::*;

/// Maps to CC `SandboxManager.isSupportedPlatform()` as consumed by Doctor.
pub fn sandbox_doctor_is_supported_platform() -> bool {
    crate::utils::sandbox::sandbox_adapter::is_supported_platform()
}

/// Maps to CC `SandboxDoctorSection` render gating.
pub fn sandbox_doctor_should_render(
    supported_platform: bool,
    sandbox_enabled: bool,
    dep_check: &SandboxDependencyCheck,
) -> bool {
    supported_platform
        && sandbox_enabled
        && (!dep_check.errors.is_empty() || !dep_check.warnings.is_empty())
}

#[derive(Default, Props)]
pub struct SandboxDoctorSectionProps {
    /// Optional explicit dependency snapshot for tests/Doctor callers that have
    /// already performed the official safe read-only check.
    pub dependency_check: Option<SandboxDependencyCheck>,
    /// Optional settings snapshot. When omitted, mirrors CC by reading current
    /// merged settings at render time.
    pub settings: Option<SettingsJson>,
    pub supported_platform: Option<bool>,
    pub platform: Option<String>,
}

#[component]
pub fn SandboxDoctorSection(
    props: &SandboxDoctorSectionProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let supported_platform = props
        .supported_platform
        .unwrap_or_else(sandbox_doctor_is_supported_platform);
    let settings = props
        .settings
        .clone()
        .unwrap_or_else(crate::utils::settings::get_initial_settings);
    let sandbox_enabled = get_sandbox_enabled_setting(&settings);
    let platform = props
        .platform
        .as_deref()
        .unwrap_or_else(|| crate::utils::platform::get_platform().as_str());
    let dep_check = props
        .dependency_check
        .clone()
        .unwrap_or_else(check_dependencies_readonly);

    if let Some(message) = sandbox_unavailable_message_for_platform(platform, sandbox_enabled) {
        return element! {
            Text(content: format!("└ Sandbox: {message}"), color: theme.warning, wrap: TextWrap::Wrap)
        }
        .into_any();
    }

    if !sandbox_doctor_should_render(supported_platform, sandbox_enabled, &dep_check) {
        return element! { View {} }.into_any();
    }

    let has_errors = !dep_check.errors.is_empty();
    let status_text = if has_errors {
        "Missing dependencies"
    } else {
        "Available (with warnings)"
    };
    let status_color = if has_errors {
        theme.error
    } else {
        theme.warning
    };

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Sandbox".to_string(), weight: Weight::Bold)
            View(flex_direction: FlexDirection::Row) {
                Text(content: "└ Status: ".to_string(), wrap: TextWrap::NoWrap)
                Text(content: status_text.to_string(), color: status_color, wrap: TextWrap::NoWrap)
            }
            #(dep_check.errors.into_iter().map(|error| element! {
                Text(content: format!("└ {error}"), color: theme.error, wrap: TextWrap::Wrap)
            }))
            #(dep_check.warnings.into_iter().map(|warning| element! {
                Text(content: format!("└ {warning}"), color: theme.warning, wrap: TextWrap::Wrap)
            }))
            #(if has_errors {
                Some(element! { Text(content: "└ Run /sandbox for install instructions".to_string(), color: theme.inactive) })
            } else {
                None
            })
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render(dep_check: SandboxDependencyCheck, enabled: bool) -> String {
        render_for_platform(dep_check, enabled, "linux")
    }

    fn render_for_platform(
        dep_check: SandboxDependencyCheck,
        enabled: bool,
        platform: &str,
    ) -> String {
        let mut settings = SettingsJson::default();
        settings.sandbox = Some(serde_json::json!({"enabled": enabled}));
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxDoctorSection(
                    dependency_check: Some(dep_check),
                    settings: Some(settings),
                    supported_platform: Some(platform != "windows"),
                    platform: Some(platform.to_string()),
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn sandbox_doctor_render_gate_matches_official_conditions() {
        let deps = SandboxDependencyCheck::default();
        assert!(!sandbox_doctor_should_render(true, true, &deps));
        assert!(!sandbox_doctor_should_render(
            false,
            true,
            &SandboxDependencyCheck {
                errors: vec!["missing".to_string()],
                warnings: Vec::new(),
            }
        ));
        assert!(!sandbox_doctor_should_render(
            true,
            false,
            &SandboxDependencyCheck {
                errors: vec!["missing".to_string()],
                warnings: Vec::new(),
            }
        ));
        assert!(sandbox_doctor_should_render(
            true,
            true,
            &SandboxDependencyCheck {
                errors: Vec::new(),
                warnings: vec!["warn".to_string()],
            }
        ));
    }

    #[test]
    fn sandbox_doctor_reports_windows_unavailable_without_dependency_status() {
        let text = render_for_platform(SandboxDependencyCheck::default(), true, "windows");
        assert!(
            text.contains("Windows sandbox unavailable，命令未隔离执行"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Available"), "canvas=\n{text}");
        assert!(!text.contains("dependencies"), "canvas=\n{text}");
    }

    #[test]
    fn sandbox_doctor_renders_missing_dependencies_copy() {
        let text = render(
            SandboxDependencyCheck {
                errors: vec!["ripgrep (rg) not found".to_string()],
                warnings: Vec::new(),
            },
            true,
        );
        assert!(text.contains("Sandbox"), "canvas=\n{text}");
        assert!(
            text.contains("└ Status: Missing dependencies"),
            "canvas=\n{text}"
        );
        assert!(text.contains("└ ripgrep (rg) not found"), "canvas=\n{text}");
        assert!(
            text.contains("└ Run /sandbox for install instructions"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn sandbox_doctor_renders_warning_status_without_install_hint() {
        let text = render(
            SandboxDependencyCheck {
                errors: Vec::new(),
                warnings: vec!["weaker isolation".to_string()],
            },
            true,
        );
        assert!(
            text.contains("Available (with warnings)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("└ weaker isolation"), "canvas=\n{text}");
        assert!(!text.contains("install instructions"), "canvas=\n{text}");
    }
}
