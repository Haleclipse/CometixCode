//! Maps to: CC `utils/staticRender.tsx`.

use iocraft::prelude::*;

/// Maps to: CC `utils/staticRender.tsx#renderToAnsiString`.
/// L1 React/Ink → iocraft: the existing off-terminal `ElementExt::render`
/// commits exactly one tree and returns its canvas, so RenderOnceAndExit's
/// timer and extractFirstFrame's DEC framing have no transport counterpart.
/// No render loop, terminal modes, stdin or process stdout is opened here.
///
/// Partial framework boundary: `columns` constrains layout, but iocraft's
/// `use_terminal_size` still reads the process terminal on a one-shot render;
/// unlike Ink's stream.columns it does not publish this width to that hook.
/// Async leaf hooks likewise remain at their first committed fallback frame.
pub async fn render_to_ansi_string(node: AnyElement<'static>, columns: Option<usize>) -> String {
    // The source's imported render wraps every tree in ThemeProvider
    // (`ink.ts:12-23`), which reads the configured theme.
    let mut themed = element! {
        crate::components::design_system::theme_provider::ThemeProvider {
            #(node)
        }
    };
    let canvas = themed.render(Some(columns.unwrap_or(80)));
    let mut output = Vec::new();
    // Vec<u8>'s Write implementation cannot fail.
    canvas
        .write_ansi(&mut output)
        .expect("write canvas to memory");
    // Ink log-update.ts#renderFullFrame joins rows with LF, without a final
    // separator. Canvas adds CRLF after every row, including the last one.
    // Remove exactly that final separator, preserving real empty trailing rows.
    if output.ends_with(b"\r\n") {
        output.truncate(output.len() - 2);
    }
    String::from_utf8(output)
        .expect("canvas emits UTF-8")
        .replace("\r\n", "\n")
}

/// Maps to: CC `utils/staticRender.tsx#renderToString`.
pub async fn render_to_string(node: AnyElement<'static>, columns: Option<usize>) -> String {
    strip_ansi_escapes::strip_str(render_to_ansi_string(node, columns).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CC staticRender.tsx#renderToString strips ANSI after rendering, preserving LF.
    #[tokio::test]
    async fn static_render_matches_official_plain_text_and_width() {
        let text = render_to_string(
            element! { Text(content: "hello world") }.into_any(),
            Some(7),
        )
        .await;
        assert_eq!(text, "hello\nworld");
        let text =
            render_to_string(element! { Text(content: "hello world") }.into_any(), None).await;
        assert_eq!(text, "hello world");
    }

    /// CC staticRender.tsx#RenderOnceAndExit/extractFirstFrame captures one committed frame only.
    #[tokio::test]
    async fn static_render_matches_official_empty_and_single_frame() {
        let empty = render_to_string(element! { Fragment }.into_any(), None).await;
        assert_eq!(empty, "");
        let ansi =
            render_to_ansi_string(element! { Text(content: "one frame") }.into_any(), None).await;
        assert_eq!(strip_ansi_escapes::strip_str(ansi), "one frame");
    }

    /// CC ink/log-update.ts#renderFullFrame uses lines.join('\n').
    #[tokio::test]
    async fn static_render_preserves_real_empty_trailing_rows() {
        for blank_rows in 0..=2_u32 {
            let text = render_to_string(
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "A")
                        View(height: blank_rows)
                    }
                }
                .into_any(),
                Some(10),
            )
            .await;
            assert_eq!(text, format!("A{}", "\n".repeat(blank_rows as usize)));
        }
    }
}
