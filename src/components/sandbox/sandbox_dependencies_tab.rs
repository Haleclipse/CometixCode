//! Maps to: CC `components/sandbox/SandboxDependenciesTab.tsx`.

use crate::utils::sandbox::sandbox_adapter::SandboxDependencyCheck;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct SandboxDependenciesTabProps {
    pub dep_check: SandboxDependencyCheck,
}

/// `SandboxDependenciesTab.tsx:11` `getPlatform()`.
fn platform_name() -> &'static str {
    crate::utils::platform::get_platform().as_str()
}

pub fn dependency_flags(
    dep_check: &SandboxDependencyCheck,
) -> (bool, bool, bool, bool, Vec<String>) {
    let rg_missing = dep_check
        .errors
        .iter()
        .any(|error| error.contains("ripgrep"));
    let bwrap_missing = dep_check.errors.iter().any(|error| error.contains("bwrap"));
    let socat_missing = dep_check.errors.iter().any(|error| error.contains("socat"));
    let seccomp_missing = !dep_check.warnings.is_empty();
    let other_errors = dep_check
        .errors
        .iter()
        .filter(|error| {
            !error.contains("ripgrep") && !error.contains("bwrap") && !error.contains("socat")
        })
        .cloned()
        .collect::<Vec<_>>();
    (
        rg_missing,
        bwrap_missing,
        socat_missing,
        seccomp_missing,
        other_errors,
    )
}

#[component]
pub fn SandboxDependenciesTab(
    props: &SandboxDependenciesTabProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let platform = platform_name();
    let is_mac = platform == "macos";
    let (rg_missing, bwrap_missing, socat_missing, seccomp_missing, other_errors) =
        dependency_flags(&props.dep_check);
    let rg_install_hint = if is_mac {
        "brew install ripgrep"
    } else {
        "apt install ripgrep"
    };

    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
            #(if is_mac {
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "seatbelt: built-in (macOS)".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
                    }
                })
            } else { None })
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "ripgrep (rg): ".to_string(), wrap: TextWrap::NoWrap)
                    Text(content: if rg_missing { "not found".to_string() } else { "found".to_string() }, color: if rg_missing { theme.error } else { theme.success }, wrap: TextWrap::NoWrap)
                }
                #(if rg_missing { Some(element! { Text(content: format!("  · {rg_install_hint}"), dim: true, wrap: TextWrap::NoWrap) }) } else { None })
            }
            #(if !is_mac {
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "bubblewrap (bwrap): ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: if bwrap_missing { "not installed".to_string() } else { "installed".to_string() }, color: if bwrap_missing { theme.error } else { theme.success }, wrap: TextWrap::NoWrap)
                        }
                        #(if bwrap_missing { Some(element! { Text(content: "  · apt install bubblewrap".to_string(), dim: true, wrap: TextWrap::NoWrap) }) } else { None })
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "socat: ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: if socat_missing { "not installed".to_string() } else { "installed".to_string() }, color: if socat_missing { theme.error } else { theme.success }, wrap: TextWrap::NoWrap)
                        }
                        #(if socat_missing { Some(element! { Text(content: "  · apt install socat".to_string(), dim: true, wrap: TextWrap::NoWrap) }) } else { None })
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "seccomp filter: ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: if seccomp_missing { "not installed".to_string() } else { "installed".to_string() }, color: if seccomp_missing { theme.warning } else { theme.success }, wrap: TextWrap::NoWrap)
                            #(if seccomp_missing { Some(element! { Text(content: " (required to block unix domain sockets)".to_string(), dim: true, wrap: TextWrap::NoWrap) }) } else { None })
                        }
                        #(if seccomp_missing {
                            Some(element! {
                                View(flex_direction: FlexDirection::Column) {
                                    Text(content: "  · npm install -g @anthropic-ai/sandbox-runtime".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                    Text(content: "  · or copy vendor/seccomp/* from sandbox-runtime and set".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                    Text(content: "    sandbox.seccomp.bpfPath and applyPath in settings.json".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                }
                            })
                        } else { None })
                    }
                })
            } else { None })
            #(other_errors.into_iter().map(|error| element! {
                Text(content: error, color: theme.error, wrap: TextWrap::Wrap)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn sandbox_dependencies_flags_match_official_filtering() {
        let dep_check = SandboxDependencyCheck {
            errors: vec![
                "ripgrep missing".to_string(),
                "bwrap missing".to_string(),
                "Unsupported platform".to_string(),
            ],
            warnings: vec!["seccomp missing".to_string()],
        };
        let (rg, bwrap, socat, seccomp, other) = dependency_flags(&dep_check);
        assert!(rg);
        assert!(bwrap);
        assert!(!socat);
        assert!(seccomp);
        assert_eq!(other, vec!["Unsupported platform".to_string()]);
    }

    #[test]
    fn sandbox_dependencies_tab_renders_official_rows() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxDependenciesTab(dep_check: SandboxDependencyCheck {
                    errors: vec!["ripgrep missing".to_string(), "bwrap missing".to_string()],
                    warnings: vec!["seccomp missing".to_string()],
                })
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("ripgrep (rg):"), "canvas=\n{text}");
        assert!(text.contains("not found"), "canvas=\n{text}");
    }
}
