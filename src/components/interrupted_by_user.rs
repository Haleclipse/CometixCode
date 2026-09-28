//! Maps to: CC `components/InterruptedByUser.tsx`.
//!
//! The upstream component renders the shared interrupted-turn copy used by
//! assistant aborts and fallback rejected tool-use rows. Cometix mirrors the
//! external-build branch; the ANT-only `/issue` suffix remains unavailable in
//! this external clone build.

use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub const INTERRUPTED_BY_USER_TEXT: &str = "Interrupted · What should Claude do instead?";

#[component]
pub fn InterruptedByUser(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();

    element! {
        View(flex_direction: FlexDirection::Row, overflow: Overflow::Hidden) {
            Text(content: "Interrupted ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
            Text(content: "· What should Claude do instead?".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_by_user_matches_official_external_copy() {
        let rendered = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                InterruptedByUser
            }
        }
        .render(None)
        .to_string();

        assert_eq!(rendered.trim_end(), INTERRUPTED_BY_USER_TEXT);
    }
}
