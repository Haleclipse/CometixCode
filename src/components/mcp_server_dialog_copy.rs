//! Maps to: CC `components/MCPServerDialogCopy.tsx`.
//!
//! @nyan: CC ends the copy with "Learn more in the" and a link to its hosted
//! MCP docs; nyan has no such page, so the sentence and link are dropped.

use iocraft::prelude::*;

pub const MCP_SERVER_DIALOG_COPY: &str =
    "MCP servers may execute code or access system resources. All tool calls require approval.";

#[component]
pub fn MCPServerDialogCopy() -> impl Into<AnyElement<'static>> {
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: MCP_SERVER_DIALOG_COPY.to_string(), wrap: TextWrap::Wrap)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_server_dialog_copy_renders_text_without_docs_link() {
        let text = element! { MCPServerDialogCopy() }
            .render(Some(100))
            .to_string();
        assert!(
            text.contains(
                "MCP servers may execute code or access system resources. All tool calls require approval."
            ),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Learn more"), "canvas=\n{text}");
    }
}
