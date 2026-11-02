//! Maps to: CC `components/shell/ExpandShellOutputContext.tsx:1-32`.

use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpandShellOutputContext(pub bool);

#[derive(Default, Props)]
pub struct ExpandShellOutputProviderProps {
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn ExpandShellOutputProvider(
    props: &mut ExpandShellOutputProviderProps,
) -> impl Into<AnyElement<'static>> {
    let children = std::mem::take(&mut props.children);
    element! {
        ContextProvider(value: Context::owned(ExpandShellOutputContext(true))) {
            #(children)
        }
    }
}

pub fn use_expand_shell_output(hooks: &mut Hooks) -> bool {
    hooks
        .try_use_context::<ExpandShellOutputContext>()
        .is_some_and(|context| context.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[component]
    fn Consumer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        element! { Text(content: use_expand_shell_output(&mut hooks).to_string()) }
    }

    #[test]
    fn provider_changes_default_false_to_true() {
        assert_eq!(
            element! { Consumer }.render(Some(20)).to_string().trim(),
            "false"
        );
        assert_eq!(
            element! { ExpandShellOutputProvider { Consumer } }
                .render(Some(20))
                .to_string()
                .trim(),
            "true"
        );
    }
}
