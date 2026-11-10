//! Maps to: CC commands/plugin/PluginTrustWarning.tsx.
use iocraft::prelude::*;
/// Maps to: CC PluginTrustWarning.tsx:6-20#PluginTrustWarning.
#[component]
pub fn PluginTrustWarning(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    element! { View(margin_bottom:1u32) {
        Text(content:format!("{} ",crate::constants::figures::figures().warning),color:theme.claude)
        Text(content:"Make sure you trust a plugin before installing, updating, or using it. nyan does not control what MCP servers, files, or other software are included in plugins and cannot verify that they will work as intended or that they won't change. See each plugin's homepage for more information.",dim:true,italic:true)
    } }
}
