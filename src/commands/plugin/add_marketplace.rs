//! Maps to: CC commands/plugin/AddMarketplace.tsx.
use super::plugin_settings::ViewState;
use crate::components::{
    configurable_shortcut_hint::ConfigurableShortcutHint,
    design_system::{byline::Byline, keyboard_shortcut_hint::KeyboardShortcutHint},
    text_input::TextInput,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct AddMarketplaceProps {
    pub input_value: String,
    pub set_input_value: Handler<String>,
    pub cursor_offset: usize,
    pub set_cursor_offset: Handler<usize>,
    pub error: Option<String>,
    pub set_error: Handler<Option<String>>,
    pub result: Option<String>,
    pub set_result: Handler<Option<String>>,
    pub set_view_state: Handler<ViewState>,
    pub on_add_complete: Handler<()>,
    pub cli_mode: bool,
}
/// Maps to: CC AddMarketplace.tsx#AddMarketplace.
#[component]
pub fn AddMarketplace(
    props: &mut AddMarketplaceProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut has_attempted_auto_add = hooks.use_ref(|| false);
    let is_loading = super::plugin_settings::use_plugin_ui_state(&mut hooks, || false);
    let progress_message = super::plugin_settings::use_plugin_ui_state(&mut hooks, String::new);
    let mut input = hooks.use_state(|| props.input_value.clone());
    let mut cursor = hooks.use_state(|| props.cursor_offset);
    if *input.read() != props.input_value {
        input.set(props.input_value.clone());
    }
    let mut cursor_prop = hooks.use_state(|| props.cursor_offset);
    let mut previous_cursor = hooks.use_state(|| props.cursor_offset);
    if cursor_prop.get() != props.cursor_offset {
        cursor_prop.set(props.cursor_offset);
        cursor.set(props.cursor_offset);
        previous_cursor.set(props.cursor_offset);
    }
    // Controlled cursor changes transported back only when the child edits them.
    if previous_cursor.get() != cursor.get() {
        previous_cursor.set(cursor.get());
        (props.set_cursor_offset)(cursor.get());
    }
    let set_error = props.set_error.clone();
    let set_result = props.set_result.clone();
    let set_view_state = props.set_view_state.clone();
    let on_add_complete = props.on_add_complete.clone();
    let cli_mode = props.cli_mode;
    // Maps to source async handleAdd. Captured callbacks survive unmount, as in TS.
    let handle_add = Handler::from(move |value: String| {
        let set_error = set_error.clone();
        let set_result = set_result.clone();
        let set_view_state = set_view_state.clone();
        let on_add_complete = on_add_complete.clone();
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move{
            let input=value.trim_matches(|c:char|matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
            if input.is_empty(){set_error(Some("Please enter a marketplace source".into()));return;}
            let Some(parsed)=crate::utils::plugins::parse_marketplace_input::parse_marketplace_input(input).await else{set_error(Some("Invalid marketplace source format. Try: owner/repo, https://..., or ./path".into()));return;};
            if let Some(error)=parsed.get("error").and_then(|v|v.as_str()){set_error(Some(error.into()));return;}
            set_error(None);is_loading.set(true);progress_message.set(String::new());
            let progress=move|message:&str|{let mut state=progress_message;state.set(message.into());Ok(())};
            let result: anyhow::Result<String>=async{
                let added=crate::utils::plugins::marketplace_manager::add_marketplace_source(&parsed,Some(&progress)).await?;
                crate::utils::plugins::marketplace_manager::save_marketplace_to_settings(&added.name,&serde_json::json!({"source":added.resolved_source}),crate::utils::settings::SettingSource::User)?;
                crate::utils::plugins::cache_utils::clear_all_caches();
                let source=if parsed["source"]=="github"{parsed["repo"].clone()}else{parsed["source"].clone()};
                crate::services::analytics::log_event("tengu_marketplace_added",serde_json::json!({"source_type":source}));
                on_add_complete(());Ok(added.name)
            }.await;
            match result{
                Ok(name)=>{progress_message.set(String::new());is_loading.set(false);if cli_mode{set_result(Some(format!("Successfully added marketplace: {name}")));}else{set_view_state(ViewState::BrowseMarketplace{target_marketplace:Some(name),target_plugin:None});}},
                Err(error)=>{crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));set_error(Some(error.to_string()));progress_message.set(String::new());is_loading.set(false);set_result(cli_mode.then(||format!("Error: {error}")));},
            }
        });
    });
    let auto_add = handle_add.clone();
    let initial = props.input_value.clone();
    let initial_error = props.error.clone();
    let initial_result = props.result.clone();
    hooks.use_effect(
        move || {
            if !initial.is_empty()
                && !*has_attempted_auto_add.read()
                && initial_error.as_deref().is_none_or(str::is_empty)
                && initial_result.as_deref().is_none_or(str::is_empty)
            {
                *has_attempted_auto_add.write() = true;
                auto_add(initial);
            }
        },
        (),
    );
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let set_input = props.set_input_value.clone();
    let progress = progress_message.read().clone();
    element! {View(flex_direction:FlexDirection::Column){
        View(flex_direction:FlexDirection::Column,padding_left:1u32,padding_right:1u32,border_style:BorderStyle::Round){
            View(margin_bottom:1u32){Text(content:"Add Marketplace",weight:Weight::Bold)}
            View(flex_direction:FlexDirection::Column){Text(content:"Enter marketplace source:") Text(content:"Examples:",dim:true) Text(content:" · owner/repo (GitHub)",dim:true) Text(content:" · git@github.com:owner/repo.git (SSH)",dim:true) Text(content:" · https://example.com/marketplace.json",dim:true) Text(content:" · ./path/to/marketplace",dim:true)
                View(margin_top:1u32){TextInput(value:Some(input),cursor_offset:Some(cursor),columns:80usize,focus:Some(true),show_cursor:true,escape_event_passthrough:true,on_change:move|value|set_input(value),on_submit:move|value|handle_add(value))}
            }
            #(is_loading.get().then(||element!{View(margin_top:1u32){crate::components::spinner::Spinner Text(content:if progress.is_empty(){"Adding marketplace to configuration…".into()}else{progress})}}))
            #(props.error.clone().filter(|s|!s.is_empty()).map(|error|element!{View(margin_top:1u32){Text(content:error,color:theme.error)}}))
            #(props.result.clone().filter(|s|!s.is_empty()).map(|result|element!{View(margin_top:1u32){Text(content:result)}}))
        }
        View(margin_left:3u32){Byline{KeyboardShortcutHint(shortcut:"Enter",action:"add",dim:true,italic:true) ConfigurableShortcutHint(action:"confirm:no",context:"Settings",fallback:"Esc",description:"cancel",dim:true,italic:true)}}
    }}
}
