//! Output style prompt configuration.
//!
//! Maps to CC `constants/outputStyles.ts`.

use crate::utils::settings::types::SettingsJson;
use std::collections::BTreeMap;
use std::path::Path;

/// Maps to CC `constants/outputStyles.ts` `DEFAULT_OUTPUT_STYLE_NAME`.
pub const DEFAULT_OUTPUT_STYLE_NAME: &str = "default";

/// Maps to CC `constants/outputStyles.ts` `OutputStyleConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyleConfig {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub source: String,
    pub keep_coding_instructions: Option<bool>,
    pub force_for_plugin: Option<bool>,
}

pub(crate) fn built_in_output_styles_ordered() -> Vec<(String, Option<OutputStyleConfig>)> {
    // Maps to CC `constants/outputStyles.ts#OUTPUT_STYLE_CONFIG` insertion order.
    vec![
        (DEFAULT_OUTPUT_STYLE_NAME.to_string(), None),
        (
            "Explanatory".to_string(),
            Some(OutputStyleConfig {
                name: "Explanatory".to_string(),
                description: "Claude explains its implementation choices and codebase patterns"
                    .to_string(),
                keep_coding_instructions: Some(true),
                source: "built-in".to_string(),
                force_for_plugin: None,
                prompt: explanatory_prompt(),
            }),
        ),
        (
            "Learning".to_string(),
            Some(OutputStyleConfig {
                name: "Learning".to_string(),
                description:
                    "Claude pauses and asks you to write small pieces of code for hands-on practice"
                        .to_string(),
                keep_coding_instructions: Some(true),
                source: "built-in".to_string(),
                force_for_plugin: None,
                prompt: learning_prompt(),
            }),
        ),
    ]
}

fn upsert_style_ordered(
    styles: &mut Vec<(String, Option<OutputStyleConfig>)>,
    key: String,
    value: Option<OutputStyleConfig>,
) {
    if let Some((_, existing)) = styles.iter_mut().find(|(name, _)| name == &key) {
        *existing = value;
    } else {
        styles.push((key, value));
    }
}

/// Maps to CC `constants/outputStyles.ts#getAllOutputStyles` preserving the
/// `Object.entries(...)` order consumed by `OutputStylePicker`.
///
/// Plugin output style loading (`loadPluginOutputStyles`) remains an explicit
/// no-op until plugin runtime parity lands; directory styles are now loaded via
/// the official `.claude/output-styles/*.md` path and source priority.
pub fn get_all_output_styles_ordered(cwd: &Path) -> Vec<(String, Option<OutputStyleConfig>)> {
    let mut all_styles = built_in_output_styles_ordered();
    let custom_styles =
        crate::output_styles::load_output_styles_dir::get_output_style_dir_styles(cwd);
    for source in ["userSettings", "projectSettings", "policySettings"] {
        for style in custom_styles.iter().filter(|style| style.source == source) {
            upsert_style_ordered(&mut all_styles, style.name.clone(), Some(style.clone()));
        }
    }
    all_styles
}

/// Maps to CC `constants/outputStyles.ts#getAllOutputStyles` lookup shape.
pub fn get_all_output_styles(cwd: &Path) -> BTreeMap<String, Option<OutputStyleConfig>> {
    get_all_output_styles_ordered(cwd).into_iter().collect()
}

/// Maps to CC `constants/outputStyles.ts#getOutputStyleConfig()`.
pub fn get_output_style_config(settings: &SettingsJson) -> Option<OutputStyleConfig> {
    let style = settings
        .output_style
        .as_deref()
        .unwrap_or(DEFAULT_OUTPUT_STYLE_NAME)
        .trim();

    if style.eq_ignore_ascii_case(DEFAULT_OUTPUT_STYLE_NAME) || style.is_empty() {
        return None;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    get_all_output_styles(&cwd)
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(style))
        .and_then(|(_, config)| config)
}

/// Maps to CC `constants/outputStyles.ts` `EXPLANATORY_FEATURE_PROMPT`.
fn explanatory_feature_prompt() -> String {
    let star = crate::constants::figures::MAIN_SYMBOLS.star;
    format!(
        r#"
## Insights
In order to encourage learning, before and after writing code, always provide brief educational explanations about implementation choices using (with backticks):
"`{star} Insight ─────────────────────────────────────`
[2-3 key educational points]
`─────────────────────────────────────────────────`"

These insights should be included in the conversation, not in the codebase. You should generally focus on interesting insights that are specific to the codebase or the code you just wrote, rather than general programming concepts."#
    )
}

/// Maps to CC `constants/outputStyles.ts` `OUTPUT_STYLE_CONFIG.Explanatory.prompt`.
fn explanatory_prompt() -> String {
    format!(
        r#"You are an interactive CLI tool that helps users with software engineering tasks. In addition to software engineering tasks, you should provide educational insights about the codebase along the way.

You should be clear and educational, providing helpful explanations while remaining focused on the task. Balance educational content with task completion. When providing insights, you may exceed typical length constraints, but remain focused and relevant.

# Explanatory Style Active
{}"#,
        explanatory_feature_prompt()
    )
}

/// Maps to CC `constants/outputStyles.ts` `OUTPUT_STYLE_CONFIG.Learning.prompt`.
fn learning_prompt() -> String {
    let bullet = crate::constants::figures::MAIN_SYMBOLS.bullet;
    format!(
        r#"You are an interactive CLI tool that helps users with software engineering tasks. In addition to software engineering tasks, you should help users learn more about the codebase through hands-on practice and educational insights.

You should be collaborative and encouraging. Balance task completion with learning by requesting user input for meaningful design decisions while handling routine implementation yourself.   

# Learning Style Active
## Requesting Human Contributions
In order to encourage learning, ask the human to contribute 2-10 line code pieces when generating 20+ lines involving:
- Design decisions (error handling, data structures)
- Business logic with multiple valid approaches  
- Key algorithms or interface definitions

**TodoList Integration**: If using a TodoList for the overall task, include a specific todo item like "Request human input on [specific decision]" when planning to request human input. This ensures proper task tracking. Note: TodoList is not required for all tasks.

Example TodoList flow:
   ✓ "Set up component structure with placeholder for logic"
   ✓ "Request human collaboration on decision logic implementation"
   ✓ "Integrate contribution and complete feature"

### Request Format
```
{bullet} **Learn by Doing**
**Context:** [what's built and why this decision matters]
**Your Task:** [specific function/section in file, mention file and TODO(human) but do not include line numbers]
**Guidance:** [trade-offs and constraints to consider]
```

### Key Guidelines
- Frame contributions as valuable design decisions, not busy work
- You must first add a TODO(human) section into the codebase with your editing tools before making the Learn by Doing request      
- Make sure there is one and only one TODO(human) section in the code
- Don't take any action or output anything after the Learn by Doing request. Wait for human implementation before proceeding.

### Example Requests

**Whole Function Example:**
```
{bullet} **Learn by Doing**

**Context:** I've set up the hint feature UI with a button that triggers the hint system. The infrastructure is ready: when clicked, it calls selectHintCell() to determine which cell to hint, then highlights that cell with a yellow background and shows possible values. The hint system needs to decide which empty cell would be most helpful to reveal to the user.

**Your Task:** In sudoku.js, implement the selectHintCell(board) function. Look for TODO(human). This function should analyze the board and return {{row, col}} for the best cell to hint, or null if the puzzle is complete.

**Guidance:** Consider multiple strategies: prioritize cells with only one possible value (naked singles), or cells that appear in rows/columns/boxes with many filled cells. You could also consider a balanced approach that helps without making it too easy. The board parameter is a 9x9 array where 0 represents empty cells.
```

**Partial Function Example:**
```
{bullet} **Learn by Doing**

**Context:** I've built a file upload component that validates files before accepting them. The main validation logic is complete, but it needs specific handling for different file type categories in the switch statement.

**Your Task:** In upload.js, inside the validateFile() function's switch statement, implement the 'case "document":' branch. Look for TODO(human). This should validate document files (pdf, doc, docx).

**Guidance:** Consider checking file size limits (maybe 10MB for documents?), validating the file extension matches the MIME type, and returning {{valid: boolean, error?: string}}. The file object has properties: name, size, type.
```

**Debugging Example:**
```
{bullet} **Learn by Doing**

**Context:** The user reported that number inputs aren't working correctly in the calculator. I've identified the handleInput() function as the likely source, but need to understand what values are being processed.

**Your Task:** In calculator.js, inside the handleInput() function, add 2-3 console.log statements after the TODO(human) comment to help debug why number inputs fail.

**Guidance:** Consider logging: the raw input value, the parsed result, and any validation state. This will help us understand where the conversion breaks.
```

### After Contributions
Share one insight connecting their code to broader patterns or system effects. Avoid praise or repetition.

## Insights
{}"#,
        explanatory_feature_prompt()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn built_in_output_style_config_matches_official_names() {
        let settings = SettingsJson {
            output_style: Some("Explanatory".to_string()),
            ..Default::default()
        };
        let style = get_output_style_config(&settings).expect("built-in explanatory style");
        assert_eq!(style.name, "Explanatory");
        assert_eq!(style.keep_coding_instructions, Some(true));
        assert_eq!(style.source, "built-in");
        assert!(style.prompt.contains("# Explanatory Style Active"));

        let settings = SettingsJson {
            output_style: Some("Learning".to_string()),
            ..Default::default()
        };
        let style = get_output_style_config(&settings).expect("built-in learning style");
        assert_eq!(style.name, "Learning");
        assert!(style.prompt.contains("# Learning Style Active"));
        assert!(style.prompt.contains("Learn by Doing"));
    }

    #[test]
    fn default_output_style_falls_back_to_no_section_like_official() {
        for output_style in [None, Some("default".to_string())] {
            let settings = SettingsJson {
                output_style,
                ..Default::default()
            };
            assert!(get_output_style_config(&settings).is_none());
        }
    }

    #[test]
    fn custom_output_style_config_is_loaded_from_official_directory() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-output-style-config-{}",
            uuid::Uuid::new_v4()
        ));
        let config_home = root.join("config");
        let cwd = root.join("repo");
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude/output-styles")).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::write(
            cwd.join(".claude/output-styles/custom.md"),
            "---\nname: custom\ndescription: Custom style\nkeep-coding-instructions: false\n---\nPrompt body",
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&cwd).unwrap();
        let settings = SettingsJson {
            output_style: Some("custom".to_string()),
            ..Default::default()
        };
        let style = get_output_style_config(&settings).expect("custom style");
        std::env::set_current_dir(old_cwd).unwrap();
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(style.name, "custom");
        assert_eq!(style.description, "Custom style");
        assert_eq!(style.keep_coding_instructions, Some(false));
        assert_eq!(style.prompt, "Prompt body");
    }
}
