//! Maps to: CC `components/PromptInput/VoiceIndicator.tsx:1-71`.

use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoiceState {
    #[default]
    Idle,
    Recording,
    Processing,
}

#[derive(Default, Props)]
pub struct VoiceIndicatorProps {
    pub voice_state: VoiceState,
}

#[component]
pub fn VoiceIndicator(
    props: &VoiceIndicatorProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Every hook runs before any branch. iocraft resolves hooks by call index,
    // so calling them only in the `Processing` arm shifts the whole sequence
    // the moment `voice_state` changes, and the next render panics with
    // "Unexpected hook type!" — a render-time panic freezes the TUI. The
    // animation is still gated: a `None` period leaves the frame clock idle,
    // so the cost of hoisting is a stopped clock, not a running one.
    let reduced = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.settings.prefers_reduced_motion.unwrap_or(false)
    });
    let is_processing = matches!(props.voice_state, VoiceState::Processing);
    let frame =
        hooks.use_animation_frame((is_processing && !reduced).then_some(Duration::from_millis(50)));
    let theme = hooks.use_context::<Theme>();

    if !cfg!(feature = "voice_mode") {
        return element! { Fragment }.into_any();
    }
    match props.voice_state {
        VoiceState::Idle => element! { Fragment }.into_any(),
        VoiceState::Recording => {
            element! { Text(content: "listening…".to_string(), dim: true) }.into_any()
        }
        VoiceState::Processing => {
            let pulse = if reduced {
                false
            } else {
                ((frame.time_ms / 500) as usize).is_multiple_of(2)
            };
            element! { Text(content: "Voice: processing…".to_string(), color: theme.warning, dim: pulse) }.into_any()
        }
    }
}

#[component]
pub fn VoiceWarmupHint() -> impl Into<AnyElement<'static>> {
    if cfg!(feature = "voice_mode") {
        element! { Text(content: "keep holding…".to_string(), dim: true) }.into_any()
    } else {
        element! { Fragment }.into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_build_is_null_and_feature_copy_is_stable() {
        // The component reads `Theme` from context on every path — production
        // always mounts the provider at the launcher root. The harness has to
        // supply it too; it previously got away without one only because the
        // read sat inside the `Processing` arm, which this state never reaches.
        // Same story for AppState (`settings.prefers_reduced_motion`), which
        // went strict in P7: default state is the fixture, since this asserts
        // the external build renders nothing at all.
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        VoiceIndicator(voice_state: VoiceState::Recording)
                    }.into_any()),
                )
            }
        }
        .render(Some(40))
        .to_string();
        if cfg!(feature = "voice_mode") {
            assert!(text.contains("listening…"));
        } else {
            assert!(text.trim().is_empty());
        }
    }
}
