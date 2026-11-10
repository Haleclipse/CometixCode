//! Maps to: CC `hooks/renderPlaceholder.ts`.
//!
//! Pure placeholder rendering helper for text inputs. The official helper uses
//! chalk.dim/chalk.inverse; Nyan emits equivalent ANSI SGR so callers can
//! render through iocraft `Ansi` without moving placeholder logic into the
//! input component.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderRender {
    pub rendered_placeholder: Option<String>,
    pub show_placeholder: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderRenderOptions {
    pub placeholder: Option<String>,
    pub value: String,
    pub show_cursor: bool,
    pub focus: bool,
    pub terminal_focus: bool,
}

fn dim(text: &str) -> String {
    format!("\x1b[2m{text}\x1b[22m")
}

fn invert(text: &str) -> String {
    format!("\x1b[7m{text}\x1b[27m")
}

/// Maps to: CC `hooks/renderPlaceholder.ts#renderPlaceholder`.
pub fn render_placeholder(options: PlaceholderRenderOptions) -> PlaceholderRender {
    let mut rendered_placeholder = None;

    if let Some(placeholder) = options.placeholder.as_deref() {
        let rendered = if options.show_cursor && options.focus && options.terminal_focus {
            if let Some(first) = placeholder.chars().next() {
                let first_len = first.len_utf8();
                format!(
                    "{}{}",
                    invert(&placeholder[..first_len]),
                    dim(&placeholder[first_len..])
                )
            } else {
                invert(" ")
            }
        } else {
            dim(placeholder)
        };
        rendered_placeholder = Some(rendered);
    }

    PlaceholderRender {
        rendered_placeholder,
        show_placeholder: options.value.is_empty() && options.placeholder.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_placeholder_matches_official_cursor_and_dim_branches() {
        let idle = render_placeholder(PlaceholderRenderOptions {
            placeholder: Some("Ask nyan".to_string()),
            value: String::new(),
            show_cursor: true,
            focus: false,
            terminal_focus: true,
        });
        assert_eq!(
            idle.rendered_placeholder,
            Some("\x1b[2mAsk nyan\x1b[22m".to_string())
        );
        assert!(idle.show_placeholder);

        let focused = render_placeholder(PlaceholderRenderOptions {
            placeholder: Some("Ask".to_string()),
            value: String::new(),
            show_cursor: true,
            focus: true,
            terminal_focus: true,
        });
        assert_eq!(
            focused.rendered_placeholder,
            Some("\x1b[7mA\x1b[27m\x1b[2msk\x1b[22m".to_string())
        );
    }

    #[test]
    fn render_placeholder_is_not_shown_when_value_is_present() {
        let with_value = render_placeholder(PlaceholderRenderOptions {
            placeholder: Some("Ask nyan".to_string()),
            value: "hello".to_string(),
            show_cursor: true,
            focus: true,
            terminal_focus: true,
        });
        assert!(!with_value.show_placeholder);
    }
}
