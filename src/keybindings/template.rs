//! Maps to: CC `keybindings/template.ts`.

use super::default_bindings::default_binding_blocks;
use super::reserved_shortcuts::{NON_REBINDABLE, normalize_key_for_comparison};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Maps to: CC `filterReservedShortcuts(...)`.
pub fn filter_reserved_shortcuts() -> Vec<super::types::KeybindingBlock> {
    let reserved = NON_REBINDABLE
        .iter()
        .map(|shortcut| normalize_key_for_comparison(shortcut.key))
        .collect::<HashSet<_>>();
    default_binding_blocks()
        .into_iter()
        .filter(|block| block.include_in_template)
        .filter_map(|mut block| {
            block
                .bindings
                .retain(|(key, _)| !reserved.contains(&normalize_key_for_comparison(key)));
            (!block.bindings.is_empty()).then_some(block)
        })
        .collect()
}

/// Maps to: CC `generateKeybindingsTemplate()`.
pub fn generate_keybindings_template() -> String {
    let bindings = filter_reserved_shortcuts()
        .into_iter()
        .map(|block| {
            let mut object = Map::new();
            object.insert(
                "context".to_string(),
                Value::String(block.context.as_official_str().to_string()),
            );
            let mut binding_map = Map::new();
            for (key, action) in block.bindings {
                binding_map.insert(key, action.map(Value::String).unwrap_or(Value::Null));
            }
            object.insert("bindings".to_string(), Value::Object(binding_map));
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    // @nyan: no `$schema` or `$docs` (CC: the SchemaStore Claude Code
    // keybindings schema and its hosted docs page).
    let mut config = Map::new();
    config.insert("bindings".to_string(), Value::Array(bindings));
    format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(config))
            .expect("keybindings template is serializable")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybindings_template_matches_official_wrapper_order_and_reserved_filter() {
        let template = generate_keybindings_template();
        let parsed: Value = serde_json::from_str(&template).unwrap();
        assert!(template.ends_with('\n'));
        assert!(parsed.get("$schema").is_none());
        assert!(parsed.get("$docs").is_none());
        let blocks = parsed["bindings"].as_array().unwrap();
        assert_eq!(blocks[0]["context"], "Global");
        let global = blocks[0]["bindings"].as_object().unwrap();
        assert!(!global.contains_key("ctrl+c"));
        assert!(!global.contains_key("ctrl+d"));
        assert_eq!(global["ctrl+l"], "app:redraw");
        assert!(template.contains("\"ctrl+x ctrl+k\": \"chat:killAgents\""));
        // Template.ts only removes NON_REBINDABLE; platform/terminal warnings
        // remain visible for customization and /doctor validation.
        assert!(template.contains("\"cmd+c\": \"selection:copy\""));
        assert!(!template.contains("\"CopyPicker\""));
        assert!(!template.contains("\"select:index1\""));
        assert!(!template.contains("\"agent:saveAndEdit\""));
    }
}
