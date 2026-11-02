//! Plugin / marketplace schema definitions.
//!
//! Maps to: CC `utils/plugins/schemas.ts` — the marketplace-family subset the
//! settings schema reaches (`MarketplaceSourceSchema` and its dependency
//! chain), plus the plugin-management `PluginScopeSchema` and inferred type.
//! Manifest, marketplace entry and plugin hooks schemas are also restored for
//! `/plugin validate`; KnownMarketplace/File schemas serve marketplace list.
//! Installed V1/V2 registry schemas and their inferred typed data are restored;
//! other unconsumed exports remain partial.
//!
//! Each `*_schema()` function is CC's `lazySchema(() => ...)` in its settled
//! Rust shape: a `static OnceLock<Schema>` built on first access, returning a
//! stable `&'static Schema` (the exemption recorded in MODULE_MAP for
//! `lazySchema.ts`). Non-`pub` functions mirror CC's non-exported consts.

use std::sync::{LazyLock, OnceLock};

use crate::utils::zod::{self, Schema, Value};

/// Maps to: CC `utils/plugins/schemas.ts#PluginManifest` inferred schema type.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginManifest {
    /// Maps to CC `PluginManifest.name`.
    pub name: String,
    /// Maps to CC `PluginManifest.description`.
    pub description: Option<String>,
    /// Maps to CC `PluginManifest.version`.
    pub version: Option<String>,
    /// Maps to CC `PluginManifest.agents` path-or-array component declaration.
    #[serde(deserialize_with = "deserialize_manifest_paths")]
    pub agents: Option<Vec<String>>,
    /// Maps to CC `PluginManifest.userConfig` for content-safe agent prompt
    /// substitution.
    pub user_config: Option<serde_json::Value>,
    /// Maps to CC `PluginManifest.hooks`; carried for shared plugin load-result
    /// parity, not executed by the agent-loader slice.
    pub hooks: Option<serde_json::Value>,
    /// Maps to CC `PluginManifest.mcpServers`; carried for shared plugin
    /// source parity, not connected from the agent-loader slice.
    pub mcp_servers: Option<serde_json::Value>,
    /// Maps to CC `PluginManifest.lspServers`; carried for plugin LSP
    /// source parity and consumed by `utils/plugins/lsp_plugin_integration.rs`.
    pub lsp_servers: Option<serde_json::Value>,
    /// Maps to CC `PluginManifest.channels`; carried for plugin MCP
    /// channel-specific user config resolution.
    pub channels: Option<serde_json::Value>,
    pub author: Option<serde_json::Value>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub dependencies: Vec<String>,
    pub commands: Option<serde_json::Value>,
    pub skills: Option<serde_json::Value>,
    pub output_styles: Option<serde_json::Value>,
    pub settings: Option<serde_json::Value>,
}
// Native representation of the schema's string | string[] field. Validation
// remains PluginManifestSchema's responsibility; this is only its typed DTO.
fn deserialize_manifest_paths<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    use serde::Deserialize;
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::String(path)) => Ok(Some(vec![path])),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Maps to: CC `utils/plugins/schemas.ts:1672-1672#PluginScope`, inferred from
/// `PluginScopeSchema` at 1506-1508.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginScope {
    Managed,
    User,
    Project,
    Local,
}

/// Maps to: CC `utils/plugins/schemas.ts:1665-1665#InstalledPlugin`.
/// Typed representation of the canonical schema's validated result; schema
/// validation, including null rejection, precedes serde projection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub version: String,
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    pub install_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
}

/// Maps to: CC `utils/plugins/schemas.ts:1666-1668#InstalledPluginsFileV1`.
/// IndexMap preserves source record insertion order; PluginIdSchema excludes
/// integer-only keys, so ECMAScript numeric-key reordering cannot arise here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPluginsFileV1 {
    pub version: u8,
    pub plugins: indexmap::IndexMap<String, InstalledPlugin>,
}

/// Maps to: CC `utils/plugins/schemas.ts:1673-1675#PluginInstallationEntry`.
/// Paths, versions, timestamps and SHA values are source strings, not validated
/// filesystem paths, semantic versions, dates or hashes. Optional projectPath
/// stays optional for every scope; CC describes but does not refine it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallationEntry {
    pub scope: PluginScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    pub install_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
}

/// Maps to: CC `utils/plugins/schemas.ts:1669-1671#InstalledPluginsFileV2`.
/// As for V1, serde is a validated-data carrier, not a second schema parser.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPluginsFileV2 {
    pub version: u8,
    pub plugins: indexmap::IndexMap<String, Vec<PluginInstallationEntry>>,
}

/// Maps to: CC `utils/plugins/schemas.ts:1506-1508#PluginScopeSchema`.
pub fn plugin_scope_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| zod::enumeration(vec!["managed", "user", "project", "local"]))
}

/// Maps to: CC `utils/plugins/schemas.ts:19-28`.
///
/// Official marketplace names that are reserved for Anthropic/Claude official
/// use. These names are allowed ONLY for official marketplaces and blocked for
/// third parties. (CC keeps a `Set`; membership is checked on the lowercased
/// name at every call site.)
pub const ALLOWED_OFFICIAL_MARKETPLACE_NAMES: &[&str] = &[
    "claude-code-marketplace",
    "claude-code-plugins",
    "claude-plugins-official",
    "anthropic-marketplace",
    "anthropic-plugins",
    "agent-skills",
    "life-sciences",
    "knowledge-work-plugins",
];

/// Maps to: CC `utils/plugins/schemas.ts:71-72`.
///
/// Pattern to detect names that impersonate official Anthropic/Claude
/// marketplaces ("official" combined with "anthropic"/"claude" in either
/// order, or a leading "anthropic"/"claude" followed by an official-sounding
/// term). Case-insensitive.
pub static BLOCKED_OFFICIAL_NAME_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:official[^a-z0-9]*(anthropic|claude)|(?:anthropic|claude)[^a-z0-9]*official|^(?:anthropic|claude)[^a-z0-9]*(marketplace|plugins|official))",
    )
    .expect("source marketplace-name regex compiles")
});

/// Maps to: CC `utils/plugins/schemas.ts:77` — non-ASCII characters that could
/// be used for homograph attacks (e.g. Cyrillic 'а' instead of Latin 'a').
fn contains_non_ascii(name: &str) -> bool {
    // CC's /[^ -~]/ — anything outside printable ASCII.
    !name.chars().all(|c| (' '..='~').contains(&c))
}

/// Maps to: CC `utils/plugins/schemas.ts:87-101` `isBlockedOfficialName`.
///
/// Check if a marketplace name impersonates an official Anthropic/Claude
/// marketplace: allowed-list names are never blocked; non-ASCII names are
/// always blocked (homograph protection); otherwise the blocked pattern
/// decides.
pub fn is_blocked_official_name(name: &str) -> bool {
    if ALLOWED_OFFICIAL_MARKETPLACE_NAMES.contains(&name.to_lowercase().as_str()) {
        return false;
    }
    if contains_non_ascii(name) {
        return true;
    }
    BLOCKED_OFFICIAL_NAME_PATTERN.is_match(name)
}

/// Maps to: CC `utils/plugins/schemas.ts:162` `RelativePath`.
fn relative_path() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| zod::string().starts_with("./"))
}

/// Maps to: CC `utils/plugins/schemas.ts:216-246` `MarketplaceNameSchema`.
///
/// Shared marketplace-name validation, used by the settings arm of
/// `MarketplaceSourceSchema` (and by `PluginMarketplaceSchema` when the
/// marketplace-manifest port lands — CC keeps the two in sync through this one
/// schema so no name can pass the settings arm and then fail post-write).
fn marketplace_name_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::string()
            .min_with_message(1, "Marketplace must have a name")
            .refine(
                |v| v.as_str().is_some_and(|name| !name.contains(' ')),
                "Marketplace name cannot contain spaces. Use kebab-case (e.g., \"my-marketplace\")",
            )
            .refine(
                |v| {
                    v.as_str().is_some_and(|name| {
                        !name.contains('/')
                            && !name.contains('\\')
                            && !name.contains("..")
                            && name != "."
                    })
                },
                "Marketplace name cannot contain path separators (/ or \\), \"..\" sequences, or be \".\"",
            )
            .refine(
                |v| v.as_str().is_some_and(|name| !is_blocked_official_name(name)),
                "Marketplace name impersonates an official Anthropic/Claude marketplace",
            )
            .refine(
                |v| v.as_str().is_some_and(|name| name.to_lowercase() != "inline"),
                "Marketplace name \"inline\" is reserved for --plugin-dir session plugins",
            )
            .refine(
                |v| v.as_str().is_some_and(|name| name.to_lowercase() != "builtin"),
                "Marketplace name \"builtin\" is reserved for built-in plugins",
            )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:251-267` `PluginAuthorSchema`.
pub fn plugin_author_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "name",
                zod::string()
                    .min_with_message(1, "Author name cannot be empty")
                    .describe("Display name of the plugin author or organization"),
            ),
            (
                "email",
                zod::string()
                    .optional()
                    .describe("Contact email for support or feedback"),
            ),
            (
                "url",
                zod::string()
                    .optional()
                    .describe("Website, GitHub profile, or organization URL"),
            ),
        ])
    })
}

/// The scoped/regular npm-name formats from CC's second `NpmPackageNameSchema`
/// refine. The character class is written `[a-z0-9._-]` (hyphen last) — same
/// set as CC's `[a-z0-9-._]`, unambiguous to the regex crate.
fn npm_package_name_format_ok(v: &Value) -> bool {
    static SCOPED: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^@[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$")
            .expect("source scoped-package regex compiles")
    });
    static REGULAR: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^[a-z0-9][a-z0-9._-]*$").expect("source regular-package regex compiles")
    });
    v.as_str()
        .is_some_and(|name| SCOPED.is_match(name) || REGULAR.is_match(name))
}

/// Maps to: CC `utils/plugins/schemas.ts:837-850` `NpmPackageNameSchema`.
///
/// Validates npm package names including scoped packages; prevents path
/// traversal by disallowing `..` and `//`.
fn npm_package_name_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::string()
            .refine(
                |v| {
                    v.as_str()
                        .is_some_and(|name| !name.contains("..") && !name.contains("//"))
                },
                "Package name cannot contain path traversal patterns",
            )
            .refine(
                npm_package_name_format_ok,
                "Invalid npm package name format",
            )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:906-1052` `MarketplaceSourceSchema`.
///
/// Marketplace source locations: direct URLs, GitHub repos, git URLs, npm
/// packages, local paths, the strictKnownMarketplaces patterns, and the inline
/// settings manifest.
pub fn marketplace_source_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::discriminated_union(
            "source",
            vec![
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("url"))),
                    (
                        "url",
                        zod::string()
                            .url()
                            .describe("Direct URL to marketplace.json file"),
                    ),
                    (
                        "headers",
                        zod::record(zod::string())
                            .optional()
                            .describe("Custom HTTP headers (e.g., for authentication)"),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("github"))),
                    (
                        "repo",
                        zod::string().describe("GitHub repository in owner/repo format"),
                    ),
                    (
                        "ref",
                        zod::string().optional().describe(
                            "Git branch or tag to use (e.g., \"main\", \"v1.0.0\"). Defaults to repository default branch.",
                        ),
                    ),
                    (
                        "path",
                        zod::string().optional().describe(
                            "Path to marketplace.json within repo (defaults to .claude-plugin/marketplace.json)",
                        ),
                    ),
                    (
                        "sparsePaths",
                        zod::array(zod::string()).optional().describe(
                            "Directories to include via git sparse-checkout (cone mode). Use for monorepos where the marketplace lives in a subdirectory. Example: [\".claude-plugin\", \"plugins\"]. If omitted, the full repository is cloned.",
                        ),
                    ),
                ]),
                // No .endsWith('.git') on the git arm — that's a
                // GitHub/GitLab/Bitbucket convention, not a git requirement
                // (Azure DevOps / CodeCommit URLs carry no suffix; gh-31256).
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("git"))),
                    ("url", zod::string().describe("Full git repository URL")),
                    (
                        "ref",
                        zod::string().optional().describe(
                            "Git branch or tag to use (e.g., \"main\", \"v1.0.0\"). Defaults to repository default branch.",
                        ),
                    ),
                    (
                        "path",
                        zod::string().optional().describe(
                            "Path to marketplace.json within repo (defaults to .claude-plugin/marketplace.json)",
                        ),
                    ),
                    (
                        "sparsePaths",
                        zod::array(zod::string()).optional().describe(
                            "Directories to include via git sparse-checkout (cone mode). Use for monorepos where the marketplace lives in a subdirectory. Example: [\".claude-plugin\", \"plugins\"]. If omitted, the full repository is cloned.",
                        ),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("npm"))),
                    (
                        "package",
                        npm_package_name_schema()
                            .clone()
                            .describe("NPM package containing marketplace.json"),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("file"))),
                    (
                        "path",
                        zod::string().describe("Local file path to marketplace.json"),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("directory"))),
                    (
                        "path",
                        zod::string()
                            .describe("Local directory containing .claude-plugin/marketplace.json"),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("hostPattern"))),
                    (
                        "hostPattern",
                        zod::string().describe(
                            "Regex pattern to match the host/domain extracted from any marketplace source type. For github sources, matches against \"github.com\". For git sources (SSH or HTTPS), extracts the hostname from the URL. Use in strictKnownMarketplaces to allow all marketplaces from a specific host (e.g., \"^github\\.mycompany\\.com$\").",
                        ),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("pathPattern"))),
                    (
                        "pathPattern",
                        zod::string().describe(
                            "Regex pattern matched against the .path field of file and directory sources. Use in strictKnownMarketplaces to allow filesystem-based marketplaces alongside hostPattern restrictions for network sources. Use \".*\" to allow all filesystem paths, or a narrower pattern (e.g., \"^/opt/approved/\") to restrict to specific directories.",
                        ),
                    ),
                ]),
                zod::object(vec![
                    ("source", zod::literal(serde_json::json!("settings"))),
                    (
                        "name",
                        marketplace_name_schema()
                            .clone()
                            .refine(
                                |v| {
                                    v.as_str().is_some_and(|name| {
                                        !ALLOWED_OFFICIAL_MARKETPLACE_NAMES
                                            .contains(&name.to_lowercase().as_str())
                                    })
                                },
                                "Reserved official marketplace names cannot be used with settings sources. validateOfficialNameSource only accepts github/git sources from anthropics/* for these names; a settings source would be rejected after loadAndCacheMarketplace has already written to disk with cleanupNeeded=false.",
                            )
                            .describe(
                                "Marketplace name. Must match the extraKnownMarketplaces key (enforced); the synthetic manifest is written under this name. Same validation as PluginMarketplaceSchema plus reserved-name rejection — validateOfficialNameSource runs after the disk write, too late to clean up.",
                            ),
                    ),
                    (
                        "plugins",
                        zod::array(settings_marketplace_plugin_schema().clone())
                            .describe("Plugin entries declared inline in settings.json"),
                    ),
                    ("owner", plugin_author_schema().clone().optional()),
                ])
                .describe(
                    "Inline marketplace manifest defined directly in settings.json. The reconciler writes a synthetic marketplace.json to the cache; diffMarketplaces detects edits via isEqual on the stored source (the plugins array is inside this object, so edits surface as sourceChanged).",
                ),
            ],
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1054-1060` `gitSha`.
pub fn git_sha() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::string().length(40).regex_with_message(
            r"^[a-f0-9]{40}$",
            "Must be a full 40-character lowercase git commit SHA",
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1062-1160` `PluginSourceSchema`.
///
/// Plugin source locations: a marketplace-relative path, or npm / pip / git
/// URL / GitHub / git-subdir descriptors.
pub fn plugin_source_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            relative_path().clone().describe(
                "Path to the plugin root, relative to the marketplace root (the directory containing .claude-plugin/, not .claude-plugin/ itself)",
            ),
            zod::object(vec![
                ("source", zod::literal(serde_json::json!("npm"))),
                (
                    "package",
                    // NpmPackageNameSchema().or(z.string()) — URLs and local
                    // paths are allowed as well.
                    zod::union(vec![npm_package_name_schema().clone(), zod::string()]).describe(
                        "Package name (or url, or local path, or anything else that can be passed to `npm` as a package)",
                    ),
                ),
                (
                    "version",
                    zod::string()
                        .optional()
                        .describe("Specific version or version range (e.g., ^1.0.0, ~2.1.0)"),
                ),
                (
                    "registry",
                    zod::string().url().optional().describe(
                        "Custom NPM registry URL (defaults to using system default, likely npmjs.org)",
                    ),
                ),
            ])
            .describe("NPM package as plugin source"),
            zod::object(vec![
                ("source", zod::literal(serde_json::json!("pip"))),
                (
                    "package",
                    zod::string().describe("Python package name as it appears on PyPI"),
                ),
                (
                    "version",
                    zod::string()
                        .optional()
                        .describe("Version specifier (e.g., ==1.0.0, >=2.0.0, <3.0.0)"),
                ),
                (
                    "registry",
                    zod::string().url().optional().describe(
                        "Custom PyPI registry URL (defaults to using system default, likely pypi.org)",
                    ),
                ),
            ])
            .describe("Python package as plugin source"),
            // See note on MarketplaceSourceSchema source:'git' re .endsWith —
            // dropped to support Azure DevOps / CodeCommit URLs (gh-31256).
            zod::object(vec![
                ("source", zod::literal(serde_json::json!("url"))),
                (
                    "url",
                    zod::string().describe("Full git repository URL (https:// or git@)"),
                ),
                (
                    "ref",
                    zod::string().optional().describe(
                        "Git branch or tag to use (e.g., \"main\", \"v1.0.0\"). Defaults to repository default branch.",
                    ),
                ),
                (
                    "sha",
                    git_sha()
                        .clone()
                        .optional()
                        .describe("Specific commit SHA to use"),
                ),
            ]),
            zod::object(vec![
                ("source", zod::literal(serde_json::json!("github"))),
                (
                    "repo",
                    zod::string().describe("GitHub repository in owner/repo format"),
                ),
                (
                    "ref",
                    zod::string().optional().describe(
                        "Git branch or tag to use (e.g., \"main\", \"v1.0.0\"). Defaults to repository default branch.",
                    ),
                ),
                (
                    "sha",
                    git_sha()
                        .clone()
                        .optional()
                        .describe("Specific commit SHA to use"),
                ),
            ]),
            zod::object(vec![
                ("source", zod::literal(serde_json::json!("git-subdir"))),
                (
                    "url",
                    zod::string().describe(
                        "Git repository: GitHub owner/repo shorthand, https://, or git@ URL",
                    ),
                ),
                (
                    "path",
                    zod::string().min(1).describe(
                        "Subdirectory within the repo containing the plugin (e.g., \"tools/claude-plugin\"). Cloned sparsely using partial clone (--filter=tree:0) to minimize bandwidth for monorepos.",
                    ),
                ),
                (
                    "ref",
                    zod::string().optional().describe(
                        "Git branch or tag to use (e.g., \"main\", \"v1.0.0\"). Defaults to repository default branch.",
                    ),
                ),
                (
                    "sha",
                    git_sha()
                        .clone()
                        .optional()
                        .describe("Specific commit SHA to use"),
                ),
            ])
            .describe(
                "Plugin located in a subdirectory of a larger repository (monorepo). Only the specified subdirectory is materialized; the rest of the repo is not downloaded.",
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1182-1208` `SettingsMarketplacePluginSchema`.
///
/// Narrow plugin entry for settings-sourced marketplaces: only what
/// `loadPluginFromMarketplaceEntry` reads (name, source, version, strict) plus
/// description. The synthetic marketplace.json is re-parsed with the full
/// `PluginMarketplaceSchema` downstream, so the narrowness is
/// settings-surface-only.
fn settings_marketplace_plugin_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "name",
                zod::string()
                    .min_with_message(1, "Plugin name cannot be empty")
                    .refine(
                        |v| v.as_str().is_some_and(|name| !name.contains(' ')),
                        "Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")",
                    )
                    .describe("Plugin name as it appears in the target repository"),
            ),
            (
                "source",
                plugin_source_schema().clone().describe(
                    "Where to fetch the plugin from. Must be a remote source — relative paths have no marketplace repository to resolve against.",
                ),
            ),
            ("description", zod::string().optional()),
            ("version", zod::string().optional()),
            ("strict", zod::boolean().optional()),
        ])
        .refine(
            |v| v.get("source").is_none_or(|s| !s.is_string()),
            "Plugins in a settings-sourced marketplace must use remote sources (github, git-subdir, npm, url, pip). Relative-path sources like \"./foo\" have no marketplace repository to resolve against.",
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:167-167#RelativeJSONPath`.
fn relative_json_path() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| relative_path().clone().ends_with(".json"))
}

/// Maps to: CC `utils/plugins/schemas.ts:173-188#McpbPath`.
fn mcpb_path() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            relative_path()
                .clone()
                .refine(
                    |v| {
                        v.as_str()
                            .is_some_and(|s| s.ends_with(".mcpb") || s.ends_with(".dxt"))
                    },
                    "MCPB file path must end with .mcpb or .dxt",
                )
                .describe("Path to MCPB file relative to plugin root"),
            zod::string()
                .url()
                .refine(
                    |v| {
                        v.as_str()
                            .is_some_and(|s| s.ends_with(".mcpb") || s.ends_with(".dxt"))
                    },
                    "MCPB URL must end with .mcpb or .dxt",
                )
                .describe("URL to MCPB file"),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:193-193#RelativeMarkdownPath`.
fn relative_markdown_path() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| relative_path().clone().ends_with(".md"))
}

/// Maps to: CC `utils/plugins/schemas.ts:198-203#RelativeCommandPath`.
fn relative_command_path() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            relative_markdown_path().clone(),
            relative_path().clone(),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:274-320#PluginManifestMetadataSchema`.
fn plugin_manifest_metadata_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "name",
                zod::string()
                    .min_with_message(1, "Plugin name cannot be empty")
                    .refine(
                        |v| v.as_str().is_some_and(|s| !s.contains(' ')),
                        "Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")",
                    )
                    .describe("Unique identifier for the plugin, used for namespacing (prefer kebab-case)"),
            ),
            ("version", zod::string().optional().describe("Semantic version (e.g., 1.2.3) following semver.org specification")),
            ("description", zod::string().optional().describe("Brief, user-facing explanation of what the plugin provides")),
            ("author", plugin_author_schema().clone().optional().describe("Information about the plugin creator or maintainer")),
            ("homepage", zod::string().url().optional().describe("Plugin homepage or documentation URL")),
            ("repository", zod::string().optional().describe("Source code repository URL")),
            ("license", zod::string().optional().describe("SPDX license identifier (e.g., MIT, Apache-2.0)")),
            ("keywords", zod::array(zod::string()).optional().describe("Tags for plugin discovery and categorization")),
            (
                "dependencies",
                zod::array(dependency_ref_schema().clone())
                    .optional()
                    .describe("Plugins that must be enabled for this plugin to function. Bare names (no \"@marketplace\") are resolved against the declaring plugin's own marketplace."),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:328-340#PluginHooksSchema`.
pub fn plugin_hooks_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("description", zod::string().optional().describe("Brief, user-facing explanation of what these hooks provide")),
            (
                "hooks",
                crate::schemas::hooks::hooks_schema()
                    .clone()
                    .describe("The hooks provided by the plugin, in the same format as the one used for settings"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:348-373#PluginManifestHooksSchema`.
fn plugin_manifest_hooks_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "hooks",
            zod::union(vec![
                relative_json_path()
                    .clone()
                    .describe("Path to file with additional hooks (in addition to those in hooks/hooks.json, if it exists), relative to the plugin root"),
                crate::schemas::hooks::hooks_schema()
                    .clone()
                    .describe("Additional hooks (in addition to those in hooks/hooks.json, if it exists)"),
                zod::array(zod::union(vec![
                    relative_json_path()
                        .clone()
                        .describe("Path to file with additional hooks (in addition to those in hooks/hooks.json, if it exists), relative to the plugin root"),
                    crate::schemas::hooks::hooks_schema()
                        .clone()
                        .describe("Additional hooks (in addition to those in hooks/hooks.json, if it exists)"),
                ])),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:385-416#CommandMetadataSchema`.
pub fn command_metadata_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("source", relative_command_path().clone().optional().describe("Path to command markdown file, relative to plugin root")),
            ("content", zod::string().optional().describe("Inline markdown content for the command")),
            ("description", zod::string().optional().describe("Command description override")),
            ("argumentHint", zod::string().optional().describe("Hint for command arguments (e.g., \"[file]\")")),
            ("model", zod::string().optional().describe("Default model for this command")),
            ("allowedTools", zod::array(zod::string()).optional().describe("Tools allowed when command runs")),
        ])
        .refine(
            |data| data.get("source").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) != data.get("content").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
            "Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both",
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:429-452#PluginManifestCommandsSchema`.
fn plugin_manifest_commands_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "commands",
            zod::union(vec![
                relative_command_path()
                    .clone()
                    .describe("Path to additional command file or skill directory (in addition to those in the commands/ directory, if it exists), relative to the plugin root"),
                zod::array(
                    relative_command_path()
                        .clone()
                        .describe("Path to additional command file or skill directory (in addition to those in the commands/ directory, if it exists), relative to the plugin root"),
                )
                .describe("List of paths to additional command files or skill directories"),
                zod::record(command_metadata_schema().clone())
                    .describe("Object mapping of command names to their metadata and source files. Command name becomes the slash command name (e.g., \"about\" → \"/plugin:about\")"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:460-476#PluginManifestAgentsSchema`.
fn plugin_manifest_agents_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "agents",
            zod::union(vec![
                relative_markdown_path()
                    .clone()
                    .describe("Path to additional agent file (in addition to those in the agents/ directory, if it exists), relative to the plugin root"),
                zod::array(
                    relative_markdown_path()
                        .clone()
                        .describe("Path to additional agent file (in addition to those in the agents/ directory, if it exists), relative to the plugin root"),
                )
                .describe("List of paths to additional agent files"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:484-499#PluginManifestSkillsSchema`.
fn plugin_manifest_skills_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "skills",
            zod::union(vec![
                relative_path()
                    .clone()
                    .describe("Path to additional skill directory (in addition to those in the skills/ directory, if it exists), relative to the plugin root"),
                zod::array(
                    relative_path()
                        .clone()
                        .describe("Path to additional skill directory (in addition to those in the skills/ directory, if it exists), relative to the plugin root"),
                )
                .describe("List of paths to additional skill directories"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:507-524#PluginManifestOutputStylesSchema`.
fn plugin_manifest_output_styles_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "outputStyles",
            zod::union(vec![
                relative_path()
                    .clone()
                    .describe("Path to additional output styles directory or file (in addition to those in the output-styles/ directory, if it exists), relative to the plugin root"),
                zod::array(
                    relative_path()
                        .clone()
                        .describe("Path to additional output styles directory or file (in addition to those in the output-styles/ directory, if it exists), relative to the plugin root"),
                )
                .describe("List of paths to additional output styles directories or files"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:527-527#nonEmptyString`.
fn non_empty_string() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| zod::string().min(1))
}

/// Maps to: CC `utils/plugins/schemas.ts:528-535#fileExtension`.
fn file_extension() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::string().min(2).refine(
            |v| v.as_str().is_some_and(|s| s.starts_with('.')),
            "File extensions must start with dot (e.g., \".ts\", not \"ts\")",
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:543-572#PluginManifestMcpServerSchema`.
fn plugin_manifest_mcp_server_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "mcpServers",
            zod::union(vec![
                relative_json_path()
                    .clone()
                    .describe("MCP servers to include in the plugin (in addition to those in the .mcp.json file, if it exists)"),
                mcpb_path().clone().describe("Path or URL to MCPB file containing MCP server configuration"),
                zod::record(crate::services::mcp::types::mcp_server_config_schema().clone()).describe("MCP server configurations keyed by server name"),
                zod::array(zod::union(vec![
                    relative_json_path().clone().describe("Path to MCP servers configuration file"),
                    mcpb_path().clone().describe("Path or URL to MCPB file"),
                    zod::record(crate::services::mcp::types::mcp_server_config_schema().clone()).describe("Inline MCP server configurations"),
                ]))
                .describe("Array of MCP server configurations (paths, MCPB files, or inline definitions)"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:587-621#PluginUserConfigOptionSchema`.
fn plugin_user_config_option_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "type",
                zod::enumeration(vec!["string", "number", "boolean", "directory", "file"]).describe("Type of the configuration value"),
            ),
            ("title", zod::string().describe("Human-readable label shown in the config dialog")),
            ("description", zod::string().describe("Help text shown beneath the field in the config dialog")),
            ("required", zod::boolean().optional().describe("If true, validation fails when this field is empty")),
            (
                "default",
                zod::union(vec![zod::string(), zod::number(), zod::boolean(), zod::array(zod::string())])
                    .optional()
                    .describe("Default value used when the user provides nothing"),
            ),
            ("multiple", zod::boolean().optional().describe("For string type: allow an array of strings")),
            (
                "sensitive",
                zod::boolean()
                    .optional()
                    .describe("If true, masks dialog input and stores value in secure storage (keychain/credentials file) instead of settings.json"),
            ),
            ("min", zod::number().optional().describe("Minimum value (number type only)")),
            ("max", zod::number().optional().describe("Maximum value (number type only)")),
        ])
        .strict()
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:632-654#PluginManifestUserConfigSchema`.
fn plugin_manifest_user_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
            zod::object(vec![
                    (
                        "userConfig",
                        zod::record_with_key(zod::string().regex_with_flags_and_message("^[A-Za-z_]\\w*$","","Option keys must be valid identifiers (letters, digits, underscore; no leading digit) — they become CLAUDE_PLUGIN_OPTION_<KEY> env vars in hooks"),plugin_user_config_option_schema().clone()).optional()
                        .describe(
                            "User-configurable values this plugin needs. Prompted at enable time. Non-sensitive values saved to settings.json; sensitive values to secure storage (macOS keychain or .credentials.json). Available as ${user_config.KEY} in MCP/LSP server config, hook commands, and (non-sensitive only) skill/agent content. Note: sensitive values share a single keychain entry with OAuth tokens — keep secret counts small to stay under the ~2KB stdin-safe limit (see INC-3028).",
                        ),
                    )
                ])
        })
}

/// Maps to: CC `utils/plugins/schemas.ts:670-703#PluginManifestChannelsSchema`.
fn plugin_manifest_channels_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "channels",
            zod::array(
                zod::object(vec![
                    (
                        "server",
                        zod::string()
                            .min(1)
                            .describe("Name of the MCP server this channel binds to. Must match a key in this plugin's mcpServers."),
                    ),
                    (
                        "displayName",
                        zod::string()
                            .optional()
                            .describe("Human-readable name shown in the config dialog title (e.g., \"Telegram\"). Defaults to the server name."),
                    ),
                    (
                        "userConfig",
                        zod::record(plugin_user_config_option_schema().clone()).optional().describe(
                            "Fields to prompt the user for when enabling this plugin in assistant mode. Saved values are substituted into ${user_config.KEY} references in the mcpServers env.",
                        ),
                    ),
                ])
                .strict(),
            )
            .describe("Channels this plugin provides. Each entry declares an MCP server as a message channel and optionally specifies user configuration to prompt for at enable time."),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:708-788#LspServerConfigSchema`.
pub fn lsp_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::strict_object(vec![
            (
                "command",
                zod::string()
                    .min(1)
                    .refine(
                        |v| v.as_str().is_some_and(|s| !s.contains(' ') || s.starts_with('/')),
                        "Command should not contain spaces. Use args array for arguments.",
                    )
                    .describe("Command to execute the LSP server (e.g., \"typescript-language-server\")"),
            ),
            ("args", zod::array(non_empty_string().clone()).optional().describe("Command-line arguments to pass to the server")),
            (
                "extensionToLanguage",
                zod::record_with_key(file_extension().clone(), non_empty_string().clone())
                    .refine(|v| v.as_object().is_some_and(|o| !o.is_empty()), "extensionToLanguage must have at least one mapping")
                    .describe("Mapping from file extension to LSP language ID. File extensions and languages are derived from this mapping."),
            ),
            (
                "transport",
                zod::enumeration(vec!["stdio", "socket"])
                    .default(serde_json::json!("stdio"))
                    .describe("Communication transport mechanism"),
            ),
            ("env", zod::record(zod::string()).optional().describe("Environment variables to set when starting the server")),
            (
                "initializationOptions",
                zod::any().optional().describe("Initialization options passed to the server during initialization"),
            ),
            ("settings", zod::any().optional().describe("Settings passed to the server via workspace/didChangeConfiguration")),
            ("workspaceFolder", zod::string().optional().describe("Workspace folder path to use for the server")),
            (
                "startupTimeout",
                zod::number().int().positive().optional().describe("Maximum time to wait for server startup (milliseconds)"),
            ),
            (
                "shutdownTimeout",
                zod::number().int().positive().optional().describe("Maximum time to wait for graceful shutdown (milliseconds)"),
            ),
            ("restartOnCrash", zod::boolean().optional().describe("Whether to restart the server if it crashes")),
            (
                "maxRestarts",
                zod::number().int().nonnegative().optional().describe("Maximum number of restart attempts before giving up"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:797-820#PluginManifestLspServerSchema`.
fn plugin_manifest_lsp_server_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "lspServers",
            zod::union(vec![
                relative_json_path()
                    .clone()
                    .describe("Path to .lsp.json configuration file relative to plugin root"),
                zod::record(lsp_server_config_schema().clone())
                    .describe("LSP server configurations keyed by server name"),
                zod::array(zod::union(vec![
                    relative_json_path()
                        .clone()
                        .describe("Path to LSP configuration file"),
                    zod::record(lsp_server_config_schema().clone())
                        .describe("Inline LSP server configurations"),
                ]))
                .describe("Array of LSP server configurations (paths or inline definitions)"),
            ]),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:857-867#PluginManifestSettingsSchema`.
fn plugin_manifest_settings_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "settings",
            zod::record(zod::any())
                .optional()
                .describe("Settings to merge when plugin is enabled. Only allowlisted keys are kept (currently: agent)"),
        )])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:884-898#PluginManifestSchema`.
pub fn plugin_manifest_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        let mut fields = Vec::new();
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_metadata_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_hooks_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_commands_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_agents_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_skills_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_output_styles_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_channels_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_mcp_server_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_lsp_server_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_settings_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        fields.extend({
            let Schema::Object(fields) = plugin_manifest_user_config_schema().clone() else {
                unreachable!("source object schema")
            };
            fields
                .into_iter()
                .map(|(name, field)| (name, field.optional()))
                .collect::<Vec<_>>()
        });
        zod::object(fields)
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1254-1285#PluginMarketplaceEntrySchema`.
pub fn plugin_marketplace_entry_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        let mut fields = {
            let Schema::Object(fields) = plugin_manifest_schema().clone() else {
                unreachable!("source object schema")
            };
            fields.into_iter().map(|(name, field)| (name, field.optional())).collect::<Vec<_>>()
        };
        for (name, field) in [(
                "name",
                zod::string()
                    .min_with_message(1, "Plugin name cannot be empty")
                    .refine(
                        |v| v.as_str().is_some_and(|s| !s.contains(' ')),
                        "Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")",
                    )
                    .describe("Unique identifier matching the plugin name"),
            ),
            ("source", plugin_source_schema().clone().describe("Where to fetch the plugin from")),
            (
                "category",
                zod::string().optional().describe("Category for organizing plugins (e.g., \"productivity\", \"development\")"),
            ),
            ("tags", zod::array(zod::string()).optional().describe("Tags for searchability and discovery")),
            (
                "strict",
                zod::boolean()
                    .optional()
                    .default(serde_json::json!(true))
                    .describe("Require the plugin manifest to be present in the plugin folder. If false, the marketplace entry provides the manifest."),
            )] {
            if let Some((_, current)) = fields.iter_mut().find(|(key, _)| *key == name) {
                *current = field;
            } else {
                fields.push((name, field));
            }
        }
        zod::object(fields)
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1293-1326#PluginMarketplaceSchema`.
pub fn plugin_marketplace_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("name", marketplace_name_schema().clone()),
            ("owner", plugin_author_schema().clone().describe("Marketplace maintainer or curator information")),
            (
                "plugins",
                zod::array(plugin_marketplace_entry_schema().clone()).describe("Collection of available plugins in this marketplace"),
            ),
            (
                "forceRemoveDeletedPlugins",
                zod::boolean()
                    .optional()
                    .describe("When true, plugins removed from this marketplace will be automatically uninstalled and flagged for users"),
            ),
            (
                "metadata",
                zod::object(vec![
                    ("pluginRoot", zod::string().optional().describe("Base path for relative plugin sources")),
                    ("version", zod::string().optional().describe("Marketplace version")),
                    ("description", zod::string().optional().describe("Marketplace description")),
                ])
                .optional()
                .describe("Optional marketplace metadata"),
            ),
            (
                "allowCrossMarketplaceDependenciesOn",
                zod::array(zod::string())
                    .optional()
                    .describe("Marketplace names whose plugins may be auto-installed as dependencies. Only the root marketplace's allowlist applies — no transitive trust."),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1367-1391#DependencyRefSchema`.
pub fn dependency_ref_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            zod::string()
                .regex_with_flags_and_message(
                    "^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$",
                    "i",
                    "Dependency must be a plugin name, optionally qualified with @marketplace",
                )
                .transform(|v| {
                    let s = v.as_str().expect("validated dependency string");
                    Value::String(s.find("@^").map_or(s, |at| &s[..at]).to_string())
                }),
            zod::passthrough_object({
                let Schema::Object(fields) = zod::object(vec![
                    (
                        "name",
                        zod::string()
                            .min(1)
                            .regex_with_flags("^[a-z0-9][-a-z0-9._]*$", "i"),
                    ),
                    (
                        "marketplace",
                        zod::string()
                            .min(1)
                            .regex_with_flags("^[a-z0-9][-a-z0-9._]*$", "i")
                            .optional(),
                    ),
                ]) else {
                    unreachable!("source object schema")
                };
                fields
            })
            .transform(|v| {
                let name = v["name"].as_str().expect("validated dependency name");
                Value::String(match v.get("marketplace").and_then(Value::as_str) {
                    Some(m) if !m.is_empty() => format!("{name}@{m}"),
                    _ => name.to_string(),
                })
            }),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1339-1346#PluginIdSchema`.
pub fn plugin_id_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::string().regex_with_flags_and_message(
            "^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$",
            "i",
            "Plugin ID must be in format: plugin@marketplace",
        )
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1446-1462#InstalledPluginSchema`.
pub fn installed_plugin_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "version",
                zod::string().describe("Currently installed version"),
            ),
            (
                "installedAt",
                zod::string().describe("ISO 8601 timestamp of installation"),
            ),
            (
                "lastUpdated",
                zod::string()
                    .optional()
                    .describe("ISO 8601 timestamp of last update"),
            ),
            (
                "installPath",
                zod::string().describe("Absolute path to the installed plugin directory"),
            ),
            (
                "gitCommitSha",
                zod::string()
                    .optional()
                    .describe("Git commit SHA for git-based plugins (for version tracking)"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1482-1492#InstalledPluginsFileSchemaV1`.
pub fn installed_plugins_file_schema_v1() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "version",
                zod::literal(serde_json::json!(1)).describe("Schema version 1"),
            ),
            (
                "plugins",
                zod::record_with_key(
                    plugin_id_schema().clone(),
                    installed_plugin_schema().clone(),
                )
                .describe("Map of plugin IDs to their installation metadata"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1517-1542#PluginInstallationEntrySchema`.
pub fn plugin_installation_entry_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "scope",
                plugin_scope_schema().clone().describe("Installation scope"),
            ),
            (
                "projectPath",
                zod::string()
                    .optional()
                    .describe("Project path (required for project/local scopes)"),
            ),
            (
                "installPath",
                zod::string().describe("Absolute path to the versioned plugin directory"),
            ),
            (
                "version",
                zod::string()
                    .optional()
                    .describe("Currently installed version"),
            ),
            (
                "installedAt",
                zod::string()
                    .optional()
                    .describe("ISO 8601 timestamp of installation"),
            ),
            (
                "lastUpdated",
                zod::string()
                    .optional()
                    .describe("ISO 8601 timestamp of last update"),
            ),
            (
                "gitCommitSha",
                zod::string()
                    .optional()
                    .describe("Git commit SHA for git-based plugins"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1562-1569#InstalledPluginsFileSchemaV2`.
pub fn installed_plugins_file_schema_v2() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            (
                "version",
                zod::literal(serde_json::json!(2)).describe("Schema version 2"),
            ),
            (
                "plugins",
                zod::record_with_key(
                    plugin_id_schema().clone(),
                    zod::array(plugin_installation_entry_schema().clone()),
                )
                .describe("Map of plugin IDs to arrays of installation entries"),
            ),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1575-1577#InstalledPluginsFileSchema`.
pub fn installed_plugins_file_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            installed_plugins_file_schema_v1().clone(),
            installed_plugins_file_schema_v2().clone(),
        ])
    })
}

/// Maps to: CC `utils/plugins/schemas.ts:1592-1610#KnownMarketplaceSchema`.
pub fn known_marketplace_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| zod::object(vec![
        ("source", marketplace_source_schema().clone().describe("Where to fetch the marketplace from")),
        ("installLocation", zod::string().describe("Local cache path where marketplace manifest is stored")),
        ("lastUpdated", zod::string().describe("ISO 8601 timestamp of last marketplace refresh")),
        ("autoUpdate", zod::boolean().optional().describe("Whether to automatically update this marketplace and its installed plugins on startup")),
    ]))
}

/// Maps to: CC `utils/plugins/schemas.ts:1624-1629#KnownMarketplacesFileSchema`.
pub fn known_marketplaces_file_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| zod::record_with_key(zod::string(), known_marketplace_schema().clone()))
}

/// Maps to: CC `utils/plugins/schemas.ts:107-107#OFFICIAL_GITHUB_ORG`.
pub const OFFICIAL_GITHUB_ORG: &str = "anthropics";

/// Maps to: CC `utils/plugins/schemas.ts:119-157#validateOfficialNameSource`.
pub fn validate_official_name_source(name: &str, source: &serde_json::Value) -> Option<String> {
    if !ALLOWED_OFFICIAL_MARKETPLACE_NAMES.contains(&name.to_lowercase().as_str()) {
        return None;
    }
    if source["source"] == "github" {
        if source["repo"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .starts_with(&format!("{OFFICIAL_GITHUB_ORG}/"))
        {
            return None;
        }
    } else if source["source"] == "git" && source["url"].as_str().is_some_and(|s| !s.is_empty()) {
        let url = source["url"].as_str().unwrap().to_lowercase();
        if url.contains("github.com/anthropics/") || url.contains("git@github.com:anthropics/") {
            return None;
        }
    } else {
        return Some(format!(
            "The name '{name}' is reserved for official Anthropic marketplaces and can only be used with GitHub sources from the '{OFFICIAL_GITHUB_ORG}' organization."
        ));
    }
    Some(format!(
        "The name '{name}' is reserved for official Anthropic marketplaces. Only repositories from 'github.com/{OFFICIAL_GITHUB_ORG}/' can use this name."
    ))
}

/// Maps to: CC `schemas.ts:1221-1223#isLocalPluginSource`.
pub fn is_local_plugin_source(source: &serde_json::Value) -> bool {
    source
        .as_str()
        .is_some_and(|source| source.starts_with("./"))
}

/// Maps to: CC `utils/plugins/schemas.ts:1236-1240#isLocalMarketplaceSource`.
pub fn is_local_marketplace_source(source: &serde_json::Value) -> bool {
    source["source"] == "file" || source["source"] == "directory"
}

/// Maps to: CC `utils/plugins/schemas.ts:35-35#NO_AUTO_UPDATE_OFFICIAL_MARKETPLACES`.
const NO_AUTO_UPDATE_OFFICIAL_MARKETPLACES: &[&str] = &["knowledge-work-plugins"];

/// Maps to: CC `utils/plugins/schemas.ts:48-58#isMarketplaceAutoUpdate`.
pub fn is_marketplace_auto_update(marketplace_name: &str, entry: &Value) -> bool {
    let normalized_name = marketplace_name.to_lowercase();
    entry
        .get("autoUpdate")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            ALLOWED_OFFICIAL_MARKETPLACE_NAMES.contains(&normalized_name.as_str())
                && !NO_AUTO_UPDATE_OFFICIAL_MARKETPLACES.contains(&normalized_name.as_str())
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_scope_schema_and_typed_values_match_official_bun_oracle() {
        // CC schemas.ts:1506-1508,1672: exactly four lowercase persisted
        // scopes; flag belongs only to pluginIdentifier's extended union.
        use super::{PluginScope, plugin_scope_schema};
        use crate::utils::zod::safe_parse;
        use serde_json::json;

        for (name, scope) in [
            ("managed", PluginScope::Managed),
            ("user", PluginScope::User),
            ("project", PluginScope::Project),
            ("local", PluginScope::Local),
        ] {
            assert_eq!(
                safe_parse(plugin_scope_schema(), &json!(name)).unwrap(),
                json!(name)
            );
            assert_eq!(serde_json::to_value(scope).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<PluginScope>(json!(name)).unwrap(),
                scope
            );
        }
        for invalid in [
            json!("flag"),
            json!("USER"),
            json!(""),
            serde_json::Value::Null,
        ] {
            assert!(safe_parse(plugin_scope_schema(), &invalid).is_err());
            assert!(serde_json::from_value::<PluginScope>(invalid).is_err());
        }
    }

    use super::*;
    use crate::utils::zod::{safe_parse, to_json_schema};
    use serde_json::json;

    #[test]
    fn is_blocked_official_name_matches_official_semantics() {
        // Allowed-list names are never blocked, case-insensitively.
        assert!(!is_blocked_official_name("agent-skills"));
        assert!(!is_blocked_official_name("Anthropic-Marketplace"));
        // The blocked pattern: official+claude/anthropic in either order, or a
        // leading claude/anthropic with an official-sounding suffix.
        assert!(is_blocked_official_name("official-claude-plugins"));
        assert!(is_blocked_official_name("claude-official"));
        assert!(is_blocked_official_name("anthropic-marketplace-new"));
        // Homograph protection: non-ASCII is always blocked (Cyrillic а).
        assert!(is_blocked_official_name("аnthropic"));
        // Indirect variations are intentionally not blocked (CC's comment:
        // avoid false positives on legitimate names).
        assert!(!is_blocked_official_name("my-claude-marketplace"));
    }

    #[test]
    fn marketplace_name_refines_report_official_copy_verbatim() {
        let expect_message = |input: &str, message: &str| {
            let err =
                safe_parse(marketplace_name_schema(), &json!(input)).expect_err("name should fail");
            assert!(
                err.issues.iter().any(|i| i.message == message),
                "expected {message:?} for {input:?}, got {:?}",
                err.issues.iter().map(|i| &i.message).collect::<Vec<_>>()
            );
        };
        expect_message("", "Marketplace must have a name");
        expect_message(
            "my marketplace",
            "Marketplace name cannot contain spaces. Use kebab-case (e.g., \"my-marketplace\")",
        );
        expect_message(
            "a/b",
            "Marketplace name cannot contain path separators (/ or \\), \"..\" sequences, or be \".\"",
        );
        expect_message(
            "claude-official",
            "Marketplace name impersonates an official Anthropic/Claude marketplace",
        );
        expect_message(
            "Inline",
            "Marketplace name \"inline\" is reserved for --plugin-dir session plugins",
        );
        expect_message(
            "BUILTIN",
            "Marketplace name \"builtin\" is reserved for built-in plugins",
        );
        assert!(safe_parse(marketplace_name_schema(), &json!("my-marketplace")).is_ok());
    }

    #[test]
    fn git_sha_reports_both_failed_checks_like_official() {
        // zod's checks are non-aborting: a short non-hex input fails the
        // exact-length check AND the regex, in chain order.
        let err = safe_parse(git_sha(), &json!("abc")).expect_err("short sha should fail");
        assert_eq!(err.issues.len(), 2);
        assert_eq!(
            err.issues[0].message,
            "Too small: expected string to have >=40 characters"
        );
        assert_eq!(err.issues[0].exact, Some(true));
        assert_eq!(
            err.issues[1].message,
            "Must be a full 40-character lowercase git commit SHA"
        );
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(safe_parse(git_sha(), &json!(sha)).unwrap(), json!(sha));
    }

    #[test]
    fn plugin_source_union_accepts_relative_and_remote_forms() {
        let s = plugin_source_schema();
        assert!(safe_parse(s, &json!("./my-plugin")).is_ok());
        assert!(safe_parse(s, &json!({"source": "npm", "package": "@babel/core"})).is_ok());
        assert!(safe_parse(s, &json!({"source": "github", "repo": "o/r", "sha": "0123456789abcdef0123456789abcdef01234567"})).is_ok());
        // A malformed sha fails inside the github arm.
        assert!(
            safe_parse(
                s,
                &json!({"source": "github", "repo": "o/r", "sha": "HEAD"})
            )
            .is_err()
        );
        // A path not starting with ./ matches no union arm.
        assert!(safe_parse(s, &json!("plugins/foo")).is_err());
    }

    #[test]
    fn marketplace_source_settings_arm_validates_inline_manifest() {
        let s = marketplace_source_schema();
        let ok = json!({
            "source": "settings",
            "name": "my-market",
            "plugins": [{"name": "p", "source": {"source": "github", "repo": "o/r"}}],
        });
        assert!(safe_parse(s, &ok).is_ok());

        // Reserved official names are rejected by the settings-arm refine.
        let reserved = json!({
            "source": "settings",
            "name": "agent-skills",
            "plugins": [],
        });
        let err = safe_parse(s, &reserved).expect_err("reserved name should fail");
        assert!(err.issues.iter().any(|i| i.message
            == "Reserved official marketplace names cannot be used with settings sources. validateOfficialNameSource only accepts github/git sources from anthropics/* for these names; a settings source would be rejected after loadAndCacheMarketplace has already written to disk with cleanupNeeded=false."));

        // A relative-path plugin source parses as the string arm, then fails
        // the SettingsMarketplacePluginSchema object refine.
        let relative = json!({
            "source": "settings",
            "name": "my-market",
            "plugins": [{"name": "p", "source": "./local"}],
        });
        let err = safe_parse(s, &relative).expect_err("relative source should fail");
        assert!(err.issues.iter().any(|i| i.message
            == "Plugins in a settings-sourced marketplace must use remote sources (github, git-subdir, npm, url, pip). Relative-path sources like \"./foo\" have no marketplace repository to resolve against."));

        // The npm arm rejects traversal patterns with CC's copy.
        let traversal = json!({"source": "npm", "package": "../../../etc/passwd"});
        let err = safe_parse(s, &traversal).expect_err("traversal should fail");
        assert!(
            err.issues
                .iter()
                .any(|i| i.message == "Package name cannot contain path traversal patterns")
        );
    }

    #[test]
    fn marketplace_source_projects_nine_arms_with_described_settings_arm() {
        let projected = to_json_schema(marketplace_source_schema());
        let arms = projected["anyOf"].as_array().expect("anyOf");
        assert_eq!(arms.len(), 9);
        // The settings arm keeps its `.describe` in the projection; the
        // discriminator stays implicit in each arm's const tag.
        let settings_arm = arms
            .iter()
            .find(|a| a["properties"]["source"]["const"] == json!("settings"))
            .expect("settings arm");
        assert!(
            settings_arm["description"]
                .as_str()
                .unwrap()
                .starts_with("Inline marketplace manifest defined directly in settings.json.")
        );
        // RelativePath's startsWith projects as an anchored escaped prefix in
        // PluginSourceSchema's first arm (oracle shape: `^\.\/.*`).
        let plugin_projected = to_json_schema(plugin_source_schema());
        assert_eq!(plugin_projected["anyOf"][0]["pattern"], json!("^\\.\\/.*"));
    }
    #[test]
    fn plugin_manifest_schemas_match_official_bun_oracle() {
        // Actual Bun direct imports of CC utils/plugins/schemas.ts:162-1390 and
        // services/mcp/types.ts:29-136. Check parsed values AND complete ordered
        // Zod issue trees: defaults, transforms, unknown keys, paths and copy.
        // Oracle script: research/proof/plugin-schemas-0914/oracle.ts.
        let cases: serde_json::Value = serde_json::from_str(r###"[{"schema":"PluginManifestSchema","input":null,"issues":[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received null"}]},{"schema":"PluginManifestSchema","input":[],"issues":[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received array"}]},{"schema":"PluginManifestSchema","input":{},"issues":[{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}]},{"schema":"PluginManifestSchema","input":{"name":"p"},"data":{"name":"p"}},{"schema":"PluginManifestSchema","input":{"name":"p","extra":1},"data":{"name":"p"}},{"schema":"PluginManifestSchema","input":{"name":"","version":1},"issues":[{"origin":"string","code":"too_small","minimum":1,"inclusive":true,"path":["name"],"message":"Plugin name cannot be empty"},{"expected":"string","code":"invalid_type","path":["version"],"message":"Invalid input: expected string, received number"}]},{"schema":"PluginManifestSchema","input":{"name":"p p"},"issues":[{"code":"custom","path":["name"],"message":"Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")"}]},{"schema":"PluginManifestSchema","input":{"name":"p"},"data":{"name":"p"}},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":null},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received null"}],[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received null"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received null"}]],"path":["hooks"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":"a"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""},{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".json","path":[],"message":"Invalid string: must end with \".json\""}],[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received string"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["hooks"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":"./a"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".json","path":[],"message":"Invalid string: must end with \".json\""}],[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received string"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["hooks"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":"./a.json"},"data":{"name":"p","hooks":"./a.json"}},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":["./a.json"]},"data":{"name":"p","hooks":["./a.json"]}},{"schema":"PluginManifestSchema","input":{"name":"p","hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo ok"}]}]}},"data":{"name":"p","hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo ok"}]}]}}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":"./a.md"},"data":{"name":"p","commands":"./a.md"}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":"./dir"},"data":{"name":"p","commands":"./dir"}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":["./a.md"]},"data":{"name":"p","commands":["./a.md"]}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":{"x":{"source":"./a.md"}}},"data":{"name":"p","commands":{"x":{"source":"./a.md"}}}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":{"x":{"content":"hello"}}},"data":{"name":"p","commands":{"x":{"content":"hello"}}}},{"schema":"PluginManifestSchema","input":{"name":"p","commands":{"x":{"content":""}}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}]],"path":[],"message":"Invalid input"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received object"}],[{"code":"custom","path":["x"],"message":"Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both"}]],"path":["commands"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","commands":{"x":{"source":"./a.md","content":"x"}}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}]],"path":[],"message":"Invalid input"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received object"}],[{"code":"custom","path":["x"],"message":"Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both"}]],"path":["commands"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","commands":{"x":{"source":"a"}}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}]],"path":[],"message":"Invalid input"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received object"}],[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""},{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".md","path":[],"message":"Invalid string: must end with \".md\""}],[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}]],"path":["x","source"],"message":"Invalid input"}]],"path":["commands"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","agents":"./a.md"},"data":{"name":"p","agents":"./a.md"}},{"schema":"PluginManifestSchema","input":{"name":"p","agents":"./a"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".md","path":[],"message":"Invalid string: must end with \".md\""}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["agents"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","agents":["./a.md"]},"data":{"name":"p","agents":["./a.md"]}},{"schema":"PluginManifestSchema","input":{"name":"p","agents":["./a"]},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received array"}],[{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".md","path":[0],"message":"Invalid string: must end with \".md\""}]],"path":["agents"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","skills":"./dir"},"data":{"name":"p","skills":"./dir"}},{"schema":"PluginManifestSchema","input":{"name":"p","skills":"dir"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["skills"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","skills":["./dir"]},"data":{"name":"p","skills":["./dir"]}},{"schema":"PluginManifestSchema","input":{"name":"p","outputStyles":"./styles"},"data":{"name":"p","outputStyles":"./styles"}},{"schema":"PluginManifestSchema","input":{"name":"p","outputStyles":"styles"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["outputStyles"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","outputStyles":false},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received boolean"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received boolean"}]],"path":["outputStyles"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["p"]},"data":{"name":"p","dependencies":["p"]}},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["p@m"]},"data":{"name":"p","dependencies":["p@m"]}},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["p@m@^1.2"]},"data":{"name":"p","dependencies":["p@m"]}},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":[{"name":"p","version":"1"}]},"data":{"name":"p","dependencies":["p"]}},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":[{"name":"p","marketplace":"m"}]},"data":{"name":"p","dependencies":["p@m"]}},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["K"]},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":["dependencies",0],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["ſ"]},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":["dependencies",0],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","dependencies":["p\n"]},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":["dependencies",0],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","userConfig":{"A":{"type":"string","title":"Title","description":"Help"}}},"data":{"name":"p","userConfig":{"A":{"type":"string","title":"Title","description":"Help"}}}},{"schema":"PluginManifestSchema","input":{"name":"p","userConfig":{"0A":{"type":"string","title":"T","description":"D"}}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[A-Za-z_]\\w*$/","path":[],"message":"Option keys must be valid identifiers (letters, digits, underscore; no leading digit) — they become CLAUDE_PLUGIN_OPTION_<KEY> env vars in hooks"}],"path":["userConfig","0A"],"message":"Invalid key in record"}]},{"schema":"PluginManifestSchema","input":{"name":"p","userConfig":{"é":{"type":"string","title":"T","description":"D"}}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[A-Za-z_]\\w*$/","path":[],"message":"Option keys must be valid identifiers (letters, digits, underscore; no leading digit) — they become CLAUDE_PLUGIN_OPTION_<KEY> env vars in hooks"}],"path":["userConfig","é"],"message":"Invalid key in record"}]},{"schema":"PluginManifestSchema","input":{"name":"p","userConfig":{"A":{"type":"string","title":"T","description":"D","extra":1}}},"issues":[{"code":"unrecognized_keys","keys":["extra"],"path":["userConfig","A"],"message":"Unrecognized key: \"extra\""}]},{"schema":"PluginManifestSchema","input":{"name":"p","userConfig":{"A":{"type":"boolean"}}},"issues":[{"expected":"string","code":"invalid_type","path":["userConfig","A","title"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["userConfig","A","description"],"message":"Invalid input: expected string, received undefined"}]},{"schema":"PluginManifestSchema","input":{"name":"p","channels":[{"server":"s"}]},"data":{"name":"p","channels":[{"server":"s"}]}},{"schema":"PluginManifestSchema","input":{"name":"p","channels":[{"server":"s","unknown":1}]},"issues":[{"code":"unrecognized_keys","keys":["unknown"],"path":["channels",0],"message":"Unrecognized key: \"unknown\""}]},{"schema":"PluginManifestSchema","input":{"name":"p","channels":[{"server":""}]},"issues":[{"origin":"string","code":"too_small","minimum":1,"inclusive":true,"path":["channels",0,"server"],"message":"Too small: expected string to have >=1 characters"}]},{"schema":"PluginManifestSchema","input":{"name":"p","channels":null},"issues":[{"expected":"array","code":"invalid_type","path":["channels"],"message":"Invalid input: expected array, received null"}]},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":"./m.json"},"data":{"name":"p","mcpServers":"./m.json"}},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":"./m.mcpb"},"data":{"name":"p","mcpServers":"./m.mcpb"}},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":"https://example.com/m.dxt"},"data":{"name":"p","mcpServers":"https://example.com/m.dxt"}},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":"./bad.txt"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".json","path":[],"message":"Invalid string: must end with \".json\""}],[{"code":"invalid_union","errors":[[{"code":"custom","path":[],"message":"MCPB file path must end with .mcpb or .dxt"}],[{"code":"invalid_format","format":"url","path":[],"message":"Invalid URL"},{"code":"custom","path":[],"message":"MCPB URL must end with .mcpb or .dxt"}]],"path":[],"message":"Invalid input"}],[{"expected":"record","code":"invalid_type","path":[],"message":"Invalid input: expected record, received string"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received string"}]],"path":["mcpServers"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":{"s":{"command":"echo"}}},"data":{"name":"p","mcpServers":{"s":{"command":"echo","args":[]}}}},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":{"s":{"type":"http","url":"x"}}},"data":{"name":"p","mcpServers":{"s":{"type":"http","url":"x"}}}},{"schema":"PluginManifestSchema","input":{"name":"p","mcpServers":["./m.json",{"s":{"command":"x"}}]},"data":{"name":"p","mcpServers":["./m.json",{"s":{"command":"x","args":[]}}]}},{"schema":"PluginManifestSchema","input":{"name":"p","lspServers":"./l.json"},"data":{"name":"p","lspServers":"./l.json"}},{"schema":"PluginManifestSchema","input":{"name":"p","lspServers":{"l":{"command":"l","extensionToLanguage":{".rs":"rust"}}}},"data":{"name":"p","lspServers":{"l":{"command":"l","extensionToLanguage":{".rs":"rust"},"transport":"stdio"}}}},{"schema":"PluginManifestSchema","input":{"name":"p","lspServers":{"l":{"command":"l x","extensionToLanguage":{}}}},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"code":"custom","path":["l","command"],"message":"Command should not contain spaces. Use args array for arguments."},{"code":"custom","path":["l","extensionToLanguage"],"message":"extensionToLanguage must have at least one mapping"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received object"}]],"path":["lspServers"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","lspServers":{"l":{"command":"/some dir/l","extensionToLanguage":{"rs":"rust"}}}},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"origin":"record","code":"invalid_key","issues":[{"code":"custom","path":[],"message":"File extensions must start with dot (e.g., \".ts\", not \"ts\")"}],"path":["l","extensionToLanguage","rs"],"message":"Invalid key in record"}],[{"expected":"array","code":"invalid_type","path":[],"message":"Invalid input: expected array, received object"}]],"path":["lspServers"],"message":"Invalid input"}]},{"schema":"PluginManifestSchema","input":{"name":"p","settings":{"agent":"x","anything":1}},"data":{"name":"p","settings":{"agent":"x","anything":1}}},{"schema":"PluginManifestSchema","input":{"name":"p","homepage":"bad"},"issues":[{"code":"invalid_format","format":"url","path":["homepage"],"message":"Invalid URL"}]},{"schema":"PluginHooksSchema","input":{},"issues":[{"expected":"record","code":"invalid_type","path":["hooks"],"message":"Invalid input: expected record, received undefined"}]},{"schema":"PluginHooksSchema","input":{"hooks":{}},"data":{"hooks":{}}},{"schema":"PluginHooksSchema","input":{"description":"d","hooks":{}},"data":{"description":"d","hooks":{}}},{"schema":"PluginHooksSchema","input":{"hooks":{"X":[]}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":["PreToolUse","PostToolUse","PostToolUseFailure","Notification","UserPromptSubmit","SessionStart","SessionEnd","Stop","StopFailure","SubagentStart","SubagentStop","PreCompact","PostCompact","PermissionRequest","PermissionDenied","Setup","TeammateIdle","TaskCreated","TaskCompleted","Elicitation","ElicitationResult","ConfigChange","WorktreeCreate","WorktreeRemove","InstructionsLoaded","CwdChanged","FileChanged"],"path":[],"message":"Invalid option: expected one of \"PreToolUse\"|\"PostToolUse\"|\"PostToolUseFailure\"|\"Notification\"|\"UserPromptSubmit\"|\"SessionStart\"|\"SessionEnd\"|\"Stop\"|\"StopFailure\"|\"SubagentStart\"|\"SubagentStop\"|\"PreCompact\"|\"PostCompact\"|\"PermissionRequest\"|\"PermissionDenied\"|\"Setup\"|\"TeammateIdle\"|\"TaskCreated\"|\"TaskCompleted\"|\"Elicitation\"|\"ElicitationResult\"|\"ConfigChange\"|\"WorktreeCreate\"|\"WorktreeRemove\"|\"InstructionsLoaded\"|\"CwdChanged\"|\"FileChanged\""}],[{"expected":"never","code":"invalid_type","path":[],"message":"Invalid input: expected never, received string"}]],"path":[],"message":"Invalid input"}],"path":["hooks","X"],"message":"Invalid key in record"}]},{"schema":"PluginHooksSchema","input":{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo x"}]}]}},"data":{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo x"}]}]}}},{"schema":"PluginMarketplaceEntrySchema","input":{"name":"p","source":"./p"},"data":{"name":"p","source":"./p","strict":true}},{"schema":"PluginMarketplaceEntrySchema","input":{"name":"p","source":"./p","strict":false},"data":{"name":"p","source":"./p","strict":false}},{"schema":"PluginMarketplaceEntrySchema","input":{"name":"p","source":"./p","extra":1},"data":{"name":"p","source":"./p","strict":true}},{"schema":"PluginMarketplaceEntrySchema","input":{"source":"./p"},"issues":[{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}]},{"schema":"PluginMarketplaceEntrySchema","input":{"name":"p","source":"bad"},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":["source"],"message":"Invalid input"}]},{"schema":"PluginMarketplaceSchema","input":{"name":"m","owner":{"name":"a"},"plugins":[]},"data":{"name":"m","owner":{"name":"a"},"plugins":[]}},{"schema":"PluginMarketplaceSchema","input":{"name":"m","owner":{"name":"a"},"plugins":[{"name":"p","source":"./p"}],"extra":1},"data":{"name":"m","owner":{"name":"a"},"plugins":[{"name":"p","source":"./p","strict":true}]}},{"schema":"PluginMarketplaceSchema","input":{"name":"inline","owner":{"name":"a"},"plugins":[]},"issues":[{"code":"custom","path":["name"],"message":"Marketplace name \"inline\" is reserved for --plugin-dir session plugins"}]},{"schema":"PluginMarketplaceSchema","input":{"name":"m","plugins":[]},"issues":[{"expected":"object","code":"invalid_type","path":["owner"],"message":"Invalid input: expected object, received undefined"}]},{"schema":"PluginMarketplaceSchema","input":{"name":"m","owner":{"name":"a"},"plugins":[{"name":"p","source":"./p","extra":1}]},"data":{"name":"m","owner":{"name":"a"},"plugins":[{"name":"p","source":"./p","strict":true}]}},{"schema":"LspServerConfigSchema","input":{"command":"l","extensionToLanguage":{".rs":"rust"}},"data":{"command":"l","extensionToLanguage":{".rs":"rust"},"transport":"stdio"}},{"schema":"LspServerConfigSchema","input":{"command":"l","extensionToLanguage":{".r":""},"startupTimeout":0,"maxRestarts":-1},"issues":[{"origin":"string","code":"too_small","minimum":1,"inclusive":true,"path":["extensionToLanguage",".r"],"message":"Too small: expected string to have >=1 characters"},{"origin":"number","code":"too_small","minimum":0,"inclusive":false,"path":["startupTimeout"],"message":"Too small: expected number to be >0"},{"origin":"number","code":"too_small","minimum":0,"inclusive":true,"path":["maxRestarts"],"message":"Too small: expected number to be >=0"}]},{"schema":"LspServerConfigSchema","input":{"command":"l","extensionToLanguage":{"r":"rust"},"extra":1},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"too_small","minimum":2,"inclusive":true,"path":[],"message":"Too small: expected string to have >=2 characters"},{"code":"custom","path":[],"message":"File extensions must start with dot (e.g., \".ts\", not \"ts\")"}],"path":["extensionToLanguage","r"],"message":"Invalid key in record"},{"code":"unrecognized_keys","keys":["extra"],"path":[],"message":"Unrecognized key: \"extra\""}]},{"schema":"CommandMetadataSchema","input":{},"issues":[{"code":"custom","path":[],"message":"Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both"}]},{"schema":"CommandMetadataSchema","input":{"content":""},"issues":[{"code":"custom","path":[],"message":"Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both"}]},{"schema":"CommandMetadataSchema","input":{"content":"x","source":""},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""},{"origin":"string","code":"invalid_format","format":"ends_with","suffix":".md","path":[],"message":"Invalid string: must end with \".md\""}],[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"./","path":[],"message":"Invalid string: must start with \"./\""}]],"path":["source"],"message":"Invalid input"}]},{"schema":"CommandMetadataSchema","input":{"content":"x"},"data":{"content":"x"}},{"schema":"CommandMetadataSchema","input":{"source":"./x"},"data":{"source":"./x"}},{"schema":"CommandMetadataSchema","input":{"source":"./x","content":"x"},"issues":[{"code":"custom","path":[],"message":"Command must have either \"source\" (file path) or \"content\" (inline markdown), but not both"}]},{"schema":"DependencyRefSchema","input":"p","data":"p"},{"schema":"DependencyRefSchema","input":"p@m","data":"p@m"},{"schema":"DependencyRefSchema","input":"p@^1","data":"p"},{"schema":"DependencyRefSchema","input":"p@m@^1","data":"p@m"},{"schema":"DependencyRefSchema","input":"p@m@^","data":"p@m"},{"schema":"DependencyRefSchema","input":"p@m@^1\n","data":"p@m"},{"schema":"DependencyRefSchema","input":"p\n","issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":[],"message":"Invalid input"}]},{"schema":"DependencyRefSchema","input":"p\r","issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":[],"message":"Invalid input"}]},{"schema":"DependencyRefSchema","input":"K","issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":[],"message":"Invalid input"}]},{"schema":"DependencyRefSchema","input":"ſ","issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*(@[a-z0-9][-a-z0-9._]*)?(@\\^[^@]*)?$/i","path":[],"message":"Dependency must be a plugin name, optionally qualified with @marketplace"}],[{"expected":"object","code":"invalid_type","path":[],"message":"Invalid input: expected object, received string"}]],"path":[],"message":"Invalid input"}]},{"schema":"DependencyRefSchema","input":{"name":"p","marketplace":"m","version":"1"},"data":"p@m"},{"schema":"DependencyRefSchema","input":{"name":"p","marketplace":""},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"origin":"string","code":"too_small","minimum":1,"inclusive":true,"path":["marketplace"],"message":"Too small: expected string to have >=1 characters"},{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":["marketplace"],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}]],"path":[],"message":"Invalid input"}]},{"schema":"DependencyRefSchema","input":{"name":"K"},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":[],"message":"Invalid input: expected string, received object"}],[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*$/i","path":["name"],"message":"Invalid string: must match pattern /^[a-z0-9][-a-z0-9._]*$/i"}]],"path":[],"message":"Invalid input"}]}]"###).unwrap();
        for case in cases.as_array().unwrap() {
            let schema = match case["schema"].as_str().unwrap() {
                "PluginManifestSchema" => plugin_manifest_schema(),
                "PluginHooksSchema" => plugin_hooks_schema(),
                "PluginMarketplaceEntrySchema" => plugin_marketplace_entry_schema(),
                "PluginMarketplaceSchema" => plugin_marketplace_schema(),
                "LspServerConfigSchema" => lsp_server_config_schema(),
                "CommandMetadataSchema" => command_metadata_schema(),
                "DependencyRefSchema" => dependency_ref_schema(),
                other => panic!("unexpected oracle schema: {other}"),
            };
            match crate::utils::zod::safe_parse(schema, &case["input"]) {
                Ok(data) => {
                    assert!(case.get("data").is_some(), "expected error: {case}");
                    assert_eq!(data, case["data"], "{case}");
                }
                Err(error) => {
                    let issues: Vec<_> = error.issues.iter().map(|issue| issue.to_json()).collect();
                    assert_eq!(serde_json::json!(issues), case["issues"], "{case}");
                }
            }
        }
    }
    #[test]
    fn installed_registry_schemas_match_official_bun_oracle() {
        // Real Bun 1.3.14 imports of schemas.ts:1339-1346,1446-1577.
        // Preserve complete ordered issue trees as well as parsed data.
        // Reproduce with research/proof/plugin-installed-schemas-0914/oracle.ts.
        let cases: serde_json::Value = serde_json::from_str(r###"[{"name":"v1-minimal","schema":"InstalledPluginsFileSchemaV1","input":{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":""}}},"data":{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":""}}}},{"name":"v1-strip","schema":"InstalledPluginsFileSchemaV1","input":{"version":1,"extra":true,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":"","lastUpdated":"x","gitCommitSha":"not-a-sha","extra":true}}},"data":{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","lastUpdated":"x","installPath":"","gitCommitSha":"not-a-sha"}}}},{"name":"v2-empty","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m":[]}},"data":{"version":2,"plugins":{"p@m":[]}}},{"name":"v2-minimal-project","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m":[{"scope":"project","installPath":""}]}},"data":{"version":2,"plugins":{"p@m":[{"scope":"project","installPath":""}]}}},{"name":"v2-strip-order","schema":"InstalledPluginsFileSchemaV2","input":{"extra":3,"version":2,"plugins":{"z@m":[{"scope":"user","installPath":"relative","extra":7,"projectPath":"","version":"","installedAt":"x","lastUpdated":"x","gitCommitSha":"y"}],"A.a_1@M-2":[{"scope":"local","installPath":"a/../b"}],"b@m":[]}},"data":{"version":2,"plugins":{"z@m":[{"scope":"user","projectPath":"","installPath":"relative","version":"","installedAt":"x","lastUpdated":"x","gitCommitSha":"y"}],"A.a_1@M-2":[{"scope":"local","installPath":"a/../b"}],"b@m":[]}}},{"name":"v1-missing-version","schema":"InstalledPluginsFileSchemaV1","input":{"plugins":{}},"issues":[{"code":"invalid_value","values":[1],"path":["version"],"message":"Invalid input: expected 1"}]},{"name":"v2-string-version","schema":"InstalledPluginsFileSchemaV2","input":{"version":"2","plugins":{}},"issues":[{"code":"invalid_value","values":[2],"path":["version"],"message":"Invalid input: expected 2"}]},{"name":"v1-array","schema":"InstalledPluginsFileSchemaV1","input":{"version":1,"plugins":{"p@m":[{"version":"","installedAt":"not-a-date","installPath":""}]}},"issues":[{"expected":"object","code":"invalid_type","path":["plugins","p@m"],"message":"Invalid input: expected object, received array"}]},{"name":"v2-single","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m":{"scope":"user","installPath":"relative"}}},"issues":[{"expected":"array","code":"invalid_type","path":["plugins","p@m"],"message":"Invalid input: expected array, received object"}]},{"name":"v2-null-optional","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m":[{"scope":"user","installPath":"relative","projectPath":null}]}},"issues":[{"expected":"string","code":"invalid_type","path":["plugins","p@m",0,"projectPath"],"message":"Invalid input: expected string, received null"}]},{"name":"v2-invalid-scope","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m":[{"scope":"flag","installPath":"relative"}]}},"issues":[{"code":"invalid_value","values":["managed","user","project","local"],"path":["plugins","p@m",0,"scope"],"message":"Invalid option: expected one of \"managed\"|\"user\"|\"project\"|\"local\""}]},{"name":"v2-invalid-id","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p":[]}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"path":["plugins","p"],"message":"Invalid key in record"}]},{"name":"v2-newline-id","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"p@m\n":[]}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"path":["plugins","p@m\n"],"message":"Invalid key in record"}]},{"name":"v2-invalid-unicode-id","schema":"InstalledPluginsFileSchemaV2","input":{"version":2,"plugins":{"K@m":[]}},"issues":[{"origin":"record","code":"invalid_key","issues":[{"origin":"string","code":"invalid_format","format":"regex","pattern":"/^[a-z0-9][-a-z0-9._]*@[a-z0-9][-a-z0-9._]*$/i","path":[],"message":"Plugin ID must be in format: plugin@marketplace"}],"path":["plugins","K@m"],"message":"Invalid key in record"}]},{"name":"combined-v1","schema":"InstalledPluginsFileSchema","input":{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":""}}},"data":{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":""}}}},{"name":"combined-v2","schema":"InstalledPluginsFileSchema","input":{"version":2,"plugins":{"p@m":[{"scope":"user","installPath":"relative"}]}},"data":{"version":2,"plugins":{"p@m":[{"scope":"user","installPath":"relative"}]}}},{"name":"combined-invalid","schema":"InstalledPluginsFileSchema","input":{"version":3,"plugins":{}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":[1],"path":["version"],"message":"Invalid input: expected 1"}],[{"code":"invalid_value","values":[2],"path":["version"],"message":"Invalid input: expected 2"}]],"path":[],"message":"Invalid input"}]},{"name":"entry-null-projectPath","schema":"PluginInstallationEntrySchema","input":{"scope":"user","installPath":"relative","projectPath":null},"issues":[{"expected":"string","code":"invalid_type","path":["projectPath"],"message":"Invalid input: expected string, received null"}]},{"name":"entry-null-version","schema":"PluginInstallationEntrySchema","input":{"scope":"user","installPath":"relative","version":null},"issues":[{"expected":"string","code":"invalid_type","path":["version"],"message":"Invalid input: expected string, received null"}]},{"name":"entry-null-installedAt","schema":"PluginInstallationEntrySchema","input":{"scope":"user","installPath":"relative","installedAt":null},"issues":[{"expected":"string","code":"invalid_type","path":["installedAt"],"message":"Invalid input: expected string, received null"}]},{"name":"entry-null-lastUpdated","schema":"PluginInstallationEntrySchema","input":{"scope":"user","installPath":"relative","lastUpdated":null},"issues":[{"expected":"string","code":"invalid_type","path":["lastUpdated"],"message":"Invalid input: expected string, received null"}]},{"name":"entry-null-gitCommitSha","schema":"PluginInstallationEntrySchema","input":{"scope":"user","installPath":"relative","gitCommitSha":null},"issues":[{"expected":"string","code":"invalid_type","path":["gitCommitSha"],"message":"Invalid input: expected string, received null"}]},{"name":"v1-missing-version","schema":"InstalledPluginSchema","input":{"installedAt":"not-a-date","installPath":""},"issues":[{"expected":"string","code":"invalid_type","path":["version"],"message":"Invalid input: expected string, received undefined"}]},{"name":"v1-missing-installedAt","schema":"InstalledPluginSchema","input":{"version":"","installPath":""},"issues":[{"expected":"string","code":"invalid_type","path":["installedAt"],"message":"Invalid input: expected string, received undefined"}]},{"name":"v1-missing-installPath","schema":"InstalledPluginSchema","input":{"version":"","installedAt":"not-a-date"},"issues":[{"expected":"string","code":"invalid_type","path":["installPath"],"message":"Invalid input: expected string, received undefined"}]}]"###).unwrap();
        for case in cases.as_array().unwrap() {
            let schema = match case["schema"].as_str().unwrap() {
                "InstalledPluginSchema" => installed_plugin_schema(),
                "InstalledPluginsFileSchemaV1" => installed_plugins_file_schema_v1(),
                "PluginInstallationEntrySchema" => plugin_installation_entry_schema(),
                "InstalledPluginsFileSchemaV2" => installed_plugins_file_schema_v2(),
                "InstalledPluginsFileSchema" => installed_plugins_file_schema(),
                other => panic!("unexpected oracle schema: {other}"),
            };
            match safe_parse(schema, &case["input"]) {
                Ok(data) => {
                    assert!(case.get("data").is_some(), "expected error: {case}");
                    assert_eq!(data, case["data"], "{case}");
                }
                Err(error) => {
                    let issues: Vec<_> = error.issues.iter().map(|issue| issue.to_json()).collect();
                    assert_eq!(json!(issues), case["issues"], "{case}");
                }
            }
        }
    }

    #[test]
    fn installed_registry_typed_projection_matches_official_field_order_and_omission() {
        // schemas.ts:1446-1462,1517-1542,1665-1675: projectPath absence is
        // distinct from an empty string; metadata is plain string data.
        let v1 = safe_parse(installed_plugins_file_schema_v1(), &json!({
            "version": 1, "extra": true,
            "plugins": {"p@m": {"version": "", "installedAt": "not-a-date", "installPath": "", "extra": true}}
        })).unwrap();
        let typed_v1: InstalledPluginsFileV1 = serde_json::from_value(v1.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed_v1).unwrap(), v1);
        assert_eq!(
            serde_json::to_string(&typed_v1).unwrap(),
            r#"{"version":1,"plugins":{"p@m":{"version":"","installedAt":"not-a-date","installPath":""}}}"#
        );

        let v2 = safe_parse(installed_plugins_file_schema_v2(), &json!({
            "extra": 3, "version": 2,
            "plugins": {
                "z@m": [{"scope":"user","projectPath":"","installPath":"relative","version":"","installedAt":"x","lastUpdated":"x","gitCommitSha":"y","extra":7}],
                "A.a_1@M-2": [{"scope":"local","installPath":"a/../b"}],
                "b@m": []
            }
        })).unwrap();
        let typed_v2: InstalledPluginsFileV2 = serde_json::from_value(v2.clone()).unwrap();
        assert_eq!(
            typed_v2
                .plugins
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["z@m", "A.a_1@M-2", "b@m"]
        );
        assert_eq!(typed_v2.plugins["z@m"][0].project_path.as_deref(), Some(""));
        assert_eq!(typed_v2.plugins["A.a_1@M-2"][0].project_path, None);
        assert_eq!(serde_json::to_value(&typed_v2).unwrap(), v2);
        assert_eq!(
            serde_json::to_string(&typed_v2).unwrap(),
            r#"{"version":2,"plugins":{"z@m":[{"scope":"user","projectPath":"","installPath":"relative","version":"","installedAt":"x","lastUpdated":"x","gitCommitSha":"y"}],"A.a_1@M-2":[{"scope":"local","installPath":"a/../b"}],"b@m":[]}}"#
        );
    }
}
