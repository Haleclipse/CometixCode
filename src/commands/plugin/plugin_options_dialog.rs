//! Maps to: CC commands/plugin/PluginOptionsDialog.tsx.
use crate::components::design_system::dialog::Dialog;
use crate::keybindings::{
    keybinding_context::KeybindingRuntime,
    types::ContextName,
    use_keybinding::{use_keybinding, use_keybindings},
};
use crate::utils::plugins::plugin_options_storage::{PluginOptionSchema, PluginOptionValues};
use indexmap::IndexMap;
use iocraft::prelude::*;
use serde_json::Value;
use unicode_width::UnicodeWidthStr;
/// Maps to: CC PluginOptionsDialog.tsx#buildFinalValues.
pub fn build_final_values(
    fields: &[String],
    collected: &IndexMap<String, String>,
    schema: &PluginOptionSchema,
    initial: Option<&PluginOptionValues>,
) -> PluginOptionValues {
    let mut result = PluginOptionValues::new();
    for key in fields {
        let field = schema.get(key);
        let value = collected.get(key).map_or("", String::as_str);
        if field
            .and_then(|f| f.get("sensitive"))
            .and_then(Value::as_bool)
            == Some(true)
            && value.is_empty()
            && initial.is_some_and(|i| i.contains_key(key))
        {
            continue;
        }
        let value = match field.and_then(|f| f.get("type")).and_then(Value::as_str) {
            Some("number") => {
                if value
                    .trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
                    .is_empty()
                {
                    continue;
                }
                let number = crate::utils::json::JsoncValue::from_json(Value::String(value.into()))
                    .to_number()
                    .unwrap_or(f64::NAN);
                if number.is_nan() {
                    Value::String(value.into())
                } else {
                    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
                }
            }
            Some("boolean") => Value::Bool(crate::utils::env_utils::is_env_truthy(Some(value))),
            _ => Value::String(value.into()),
        };
        result.insert(key.clone(), value);
    }
    result
}
#[derive(Default, Props)]
pub struct PluginOptionsDialogProps {
    pub title: String,
    pub subtitle: String,
    pub config_schema: PluginOptionSchema,
    pub initial_values: Option<PluginOptionValues>,
    pub on_save: Handler<PluginOptionValues>,
    pub on_cancel: Handler<()>,
}
/// Maps to: CC PluginOptionsDialog.tsx#PluginOptionsDialog.
#[component]
pub fn PluginOptionsDialog(
    props: &mut PluginOptionsDialogProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let fields: Vec<String> = props
        .config_schema
        .as_object()
        .map(|m| {
            crate::utils::process_env::ecmascript_object_entries(m)
                .into_iter()
                .map(|(k, _)| k.to_owned())
                .collect()
        })
        .unwrap_or_default();
    // Source initialFor, retained as a props-derived closure. UTF-16 buffer
    // preserves slice(0,-1) navigation; rendering uses native replacement for
    // an unmatched surrogate. Value save carrier retains its existing boundary.
    let schema = props.config_schema.clone();
    let initial = props.initial_values.clone();
    let initial_for = move |key: &str| -> Vec<u16> {
        if schema
            .get(key)
            .and_then(|v| v.get("sensitive"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Vec::new();
        }
        initial
            .as_ref()
            .and_then(|m| m.get(key))
            .map(|v| {
                if v.is_null() {
                    "null".into()
                } else {
                    crate::utils::json::JsoncValue::from_json(v.clone())
                        .array_string()
                        .expect("Cannot convert object to primitive value")
                }
            })
            .unwrap_or_default()
            .encode_utf16()
            .collect()
    };
    let mut current_field_index = hooks.use_state(|| 0usize);
    let mut values = hooks.use_state(IndexMap::<String, String>::new);
    let mut current_input =
        hooks.use_state(|| fields.first().map_or_else(Vec::new, |k| initial_for(k)));
    let mut pending = hooks.use_state(|| None::<bool>);
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|v| v.clone());
    let on_cancel = props.on_cancel.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Settings,
        || true,
        move || {
            on_cancel(());
            true
        },
    );
    use_keybindings(
        &mut hooks,
        runtime,
        vec![
            (
                "confirm:nextField".into(),
                Box::new(move || {
                    pending.set(Some(false));
                    true
                }),
            ),
            (
                "confirm:yes".into(),
                Box::new(move || {
                    pending.set(Some(true));
                    true
                }),
            ),
        ],
        ContextName::Confirmation,
        || true,
    );
    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Backspace | KeyCode::Delete => {
                let mut text = current_input.read().clone();
                text.pop();
                current_input.set(text);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let mut text = current_input.read().clone();
                text.extend(c.to_string().encode_utf16());
                current_input.set(text);
            }
            _ => {}
        },
        TerminalEvent::Paste(text) => {
            let mut value = current_input.read().clone();
            value.extend(text.encode_utf16());
            current_input.set(value);
        }
        _ => {}
    });
    if let Some(confirm) = pending.get() {
        pending.set(None);
        let index = current_field_index.get();
        if let Some(field) = fields.get(index).filter(|field| !field.is_empty()) {
            if confirm || index + 1 < fields.len() {
                let mut collected = values.read().clone();
                collected.insert(
                    field.clone(),
                    String::from_utf16_lossy(&current_input.read()),
                );
                if confirm && index + 1 == fields.len() {
                    (props.on_save)(build_final_values(
                        &fields,
                        &collected,
                        &props.config_schema,
                        props.initial_values.as_ref(),
                    ));
                } else {
                    values.set(collected);
                    current_field_index.set(index + 1);
                    current_input.set(
                        fields
                            .get(index + 1)
                            .map_or_else(Vec::new, |k| initial_for(k)),
                    );
                }
            }
        }
    }
    let Some(field) = fields
        .get(current_field_index.get())
        .filter(|field| !field.is_empty())
    else {
        return element! {View}.into_any();
    };
    let Some(field_schema) = props.config_schema.get(field) else {
        return element! {View}.into_any();
    };
    let text = String::from_utf16_lossy(&current_input.read());
    let display = if field_schema.get("sensitive").and_then(Value::as_bool) == Some(true) {
        "*".repeat(text.width())
    } else {
        text
    };
    let title = field_schema
        .get("title")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .unwrap_or(field)
        .to_owned();
    let required = field_schema.get("required").and_then(Value::as_bool) == Some(true);
    let description = field_schema
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let next = current_field_index.get() + 1 < fields.len();
    let on_cancel = props.on_cancel.clone();
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    element!{Dialog(title:props.title.clone(),subtitle:Some(props.subtitle.clone()),is_cancel_active:Some(false),on_cancel:move |_|on_cancel(())){
        View(flex_direction:FlexDirection::Column){
            View {Text(content:title,weight:Weight::Bold) #(required.then(||element!{Text(content:" *",color:theme.error)}))}
            #((!description.is_empty()).then(||element!{Text(content:description,dim:true)}))
            View(margin_top:1u32){Text(content:format!("{} ",crate::constants::figures::figures().pointer_small)) Text(content:display) Text(content:"█")}
        }
        View(flex_direction:FlexDirection::Column){Text(content:format!("Field {} of {}",current_field_index.get()+1,fields.len()),dim:true) Text(content:if next{"Tab: Next field · Enter: Save and continue"}else{"Enter: Save configuration"},dim:true)}
    }}.into_any()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn number_values_and_serialization_match_official_bun() {
        let oracle: Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-ui-complete-0914/options-number-oracle.json")).unwrap();
        for case in oracle.as_array().unwrap() {
            let values = build_final_values(
                &["value".into()],
                &IndexMap::from_iter([("value".into(), case["input"].as_str().unwrap().into())]),
                &serde_json::json!({"value":{"type":"number"}}),
                None,
            );
            let number = values["value"].as_f64().unwrap();
            assert_eq!(number, case["result"]["value"].as_f64().unwrap());
            assert_eq!(
                number == 0.0 && number.is_sign_negative(),
                case["negativeZero"].as_bool().unwrap()
            );
            assert_eq!(
                crate::utils::slow_operations::json_stringify(&Value::Object(values.clone()), 0),
                case["serialized"]
            );
            // Exercise the real downstream String(value) substitution boundary,
            // not serde_json's distinct integer/float Value comparison.
            assert_eq!(
                crate::utils::plugins::plugin_options_storage::substitute_user_config_variables(
                    "${user_config.value}",
                    &values
                )
                .unwrap(),
                case["text"]
            );
        }
    }
    #[test]
    fn number_input_whitespace_matches_official_bun() {
        let oracle:Value=serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-ui-complete-0914/options-values-oracle.json")).unwrap();
        for case in oracle.as_array().unwrap() {
            let schema = serde_json::json!({"value":{"type":"number"}});
            let collected =
                IndexMap::from_iter([("value".into(), case["input"].as_str().unwrap().into())]);
            assert_eq!(
                crate::utils::slow_operations::json_stringify(
                    &Value::Object(build_final_values(
                        &["value".into()],
                        &collected,
                        &schema,
                        None
                    )),
                    0
                ),
                crate::utils::slow_operations::json_stringify(&case["result"], 0)
            );
        }
    }
    #[test]
    fn final_values_matches_official_blank_numbers_and_sensitive_omission() {
        // Unchanged original function: plugin-panel-0914/pure-oracle.json.
        let schema = serde_json::json!({"token":{"sensitive":true},"count":{"type":"number"},"flag":{"type":"boolean"}});
        let fields = vec!["token".into(), "count".into(), "flag".into()];
        let initial = serde_json::json!({"token":"keep"});
        let collected = IndexMap::from_iter([
            ("token".into(), "".into()),
            ("count".into(), "  ".into()),
            ("flag".into(), "YES".into()),
        ]);
        assert_eq!(
            build_final_values(&fields, &collected, &schema, initial.as_object()),
            serde_json::json!({"flag":true})
                .as_object()
                .unwrap()
                .clone()
        );
        let mut collected = collected;
        collected.insert("count".into(), "0x10".into());
        assert_eq!(
            build_final_values(&fields, &collected, &schema, initial.as_object())["count"].as_f64(),
            Some(16.0)
        );
    }
}
