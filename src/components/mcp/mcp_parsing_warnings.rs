//! Maps to: CC `components/mcp/McpParsingWarnings.tsx`.
//!
//! Renders read-only diagnostics from `services/mcp/config.ts#getMcpConfigsByScope`.
//! This component does not mutate config, approve MCP servers, or start clients.

use crate::services::mcp::config::{
    McpConfigErrorSeverity, McpConfigValidationError, get_mcp_configs_by_scope_readonly,
};
use crate::services::mcp::types::ConfigScope;
use crate::services::mcp::utils::{describe_mcp_config_file_path, get_scope_label};
use crate::utils::config::{GlobalConfig, ProjectConfig, normalize_project_path};
use iocraft::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigDiagnosticSection {
    pub scope: ConfigScope,
    pub parsing_errors: Vec<McpConfigValidationError>,
    pub warnings: Vec<McpConfigValidationError>,
}

fn project_config_for_current_cwd(global_config: &GlobalConfig) -> ProjectConfig {
    let cwd = std::env::current_dir().unwrap_or_default();
    let key = normalize_project_path(&cwd.to_string_lossy());
    global_config
        .projects
        .get(&key)
        .cloned()
        .unwrap_or_default()
}

/// Maps to: CC `McpParsingWarnings.tsx#filterErrors`.
pub fn filter_mcp_config_errors(
    errors: &[McpConfigValidationError],
    severity: McpConfigErrorSeverity,
) -> Vec<McpConfigValidationError> {
    errors
        .iter()
        .filter(|error| error.mcp_error_metadata.severity == severity)
        .cloned()
        .collect()
}

/// Maps to: CC `McpParsingWarnings.tsx` `scopes = useMemo(...)`.
pub fn mcp_config_diagnostic_sections(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
) -> Vec<McpConfigDiagnosticSection> {
    [ConfigScope::User, ConfigScope::Project, ConfigScope::Local]
        .into_iter()
        .map(|scope| {
            let config = get_mcp_configs_by_scope_readonly(scope, global_config, project_config);
            McpConfigDiagnosticSection {
                scope,
                parsing_errors: filter_mcp_config_errors(
                    &config.errors,
                    McpConfigErrorSeverity::Fatal,
                ),
                warnings: filter_mcp_config_errors(&config.errors, McpConfigErrorSeverity::Warning),
            }
        })
        .collect()
}

fn format_error_detail(error: &McpConfigValidationError) -> String {
    let server_name = error
        .mcp_error_metadata
        .server_name
        .as_deref()
        .map(|name| format!("[{name}] "))
        .unwrap_or_default();
    let path = if error.path.is_empty() {
        String::new()
    } else {
        format!("{}: ", error.path)
    };
    format!("{server_name}{path}{}", error.message)
}

fn render_section(
    section: McpConfigDiagnosticSection,
    theme: crate::utils::theme::Theme,
) -> Option<AnyElement<'static>> {
    let has_errors = !section.parsing_errors.is_empty();
    let has_warnings = !section.warnings.is_empty();
    if !has_errors && !has_warnings {
        return None;
    }
    let heading = if has_errors {
        "[Failed to parse] "
    } else {
        "[Contains warnings] "
    };
    let heading_color = if has_errors {
        theme.error
    } else {
        theme.warning
    };
    let scope = section.scope;
    Some(
        element! {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: heading.to_string(), color: heading_color, wrap: TextWrap::NoWrap)
                    Text(content: get_scope_label(scope).to_string(), wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Location: ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: describe_mcp_config_file_path(scope), dim: true, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Column, margin_left: 1u32) {
                    #(section.parsing_errors.into_iter().map(|error| {
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "└ ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                Text(content: "[Error]".to_string(), color: theme.error, wrap: TextWrap::NoWrap)
                                Text(content: format!(" {}", format_error_detail(&error)), dim: true, wrap: TextWrap::Wrap)
                            }
                        }
                    }))
                    #(section.warnings.into_iter().map(|warning| {
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "└ ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                Text(content: "[Warning]".to_string(), color: theme.warning, wrap: TextWrap::NoWrap)
                                Text(content: format!(" {}", format_error_detail(&warning)), dim: true, wrap: TextWrap::Wrap)
                            }
                        }
                    }))
                }
            }
        }
        .into_any(),
    )
}

#[component]
pub fn McpParsingWarnings(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    // Maps to: CC reading `getGlobalConfig()` directly — cached read;
    // globalConfig is not part of CC AppState.
    let global_config = crate::utils::config::load_global_config();
    let sections = hooks.use_memo(
        move || {
            let project_config = project_config_for_current_cwd(&global_config);
            mcp_config_diagnostic_sections(&global_config, &project_config)
        },
        (),
    );
    let has_diagnostics = sections
        .iter()
        .any(|section| !section.parsing_errors.is_empty() || !section.warnings.is_empty());
    if !has_diagnostics {
        return element! { View {} }.into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32, margin_bottom: 1u32) {
            Text(content: "MCP Config Diagnostics".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
            View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                Text(content: "For help configuring MCP servers, see: ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Link(url: "https://code.claude.com/docs/en/mcp".to_string())
            }
            #(sections.into_iter().filter_map(|section| render_section(section, theme)))
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_parsing_warnings_filter_errors_by_official_severity() {
        let warning = McpConfigValidationError {
            file: None,
            path: "mcpServers.docs".to_string(),
            message: "Missing environment variables: TOKEN".to_string(),
            suggestion: None,
            mcp_error_metadata: crate::services::mcp::config::McpConfigErrorMetadata {
                scope: ConfigScope::User,
                server_name: Some("docs".to_string()),
                severity: McpConfigErrorSeverity::Warning,
            },
        };
        let fatal = McpConfigValidationError {
            file: None,
            path: "mcpServers.bad".to_string(),
            message: "Does not adhere to MCP server configuration schema".to_string(),
            suggestion: None,
            mcp_error_metadata: crate::services::mcp::config::McpConfigErrorMetadata {
                scope: ConfigScope::User,
                server_name: Some("bad".to_string()),
                severity: McpConfigErrorSeverity::Fatal,
            },
        };
        let errors = vec![warning.clone(), fatal.clone()];

        assert_eq!(
            filter_mcp_config_errors(&errors, McpConfigErrorSeverity::Warning),
            vec![warning]
        );
        assert_eq!(
            filter_mcp_config_errors(&errors, McpConfigErrorSeverity::Fatal),
            vec![fatal]
        );
    }
}
