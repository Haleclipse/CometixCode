//! Maps to CC `tools/FileReadTool/prompt.ts`.

pub const FILE_READ_TOOL_NAME: &str = "Read";

pub const FILE_UNCHANGED_STUB: &str = "File unchanged since last read. The content from the earlier Read tool_result in this conversation is still current — refer to that instead of re-reading.";

pub const MAX_LINES_TO_READ: usize = 2000;

pub const DESCRIPTION: &str = "Read a file from the local filesystem.";

pub const LINE_FORMAT_INSTRUCTION: &str =
    "- Results are returned using cat -n format, with line numbers starting at 1";

pub const OFFSET_INSTRUCTION_DEFAULT: &str = "- You can optionally specify a line offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters";

pub const OFFSET_INSTRUCTION_TARGETED: &str = "- When you already know which part of the file you need, only read that part. This can be important for larger files.";

/// @nyan: the paragraphs nyan adds on top of CC's prompt, each tied to the
/// switch that owns it (docs/nyan/tool-io.md T1/T4/T5/T6). Empty strings in,
/// empty strings out — the off state keeps the base prompt without extras.
#[derive(Clone, Copy, Debug, Default)]
pub struct NyanPromptExtras {
    pub completion_notes: bool,
    pub batch: bool,
    pub encoding: bool,
    pub hex_view: bool,
}

impl NyanPromptExtras {
    fn render(&self) -> String {
        let mut lines = Vec::new();
        if self.completion_notes {
            lines.push(
                "- The last line of every result is a completion note: `(Complete: ...)` when everything in range was shown, or `(Partial: ... Continue with offset=N.)` when it was cut. Only continue with the exact parameters a Partial note provides.",
            );
        }
        if self.batch {
            lines.push(
                "- To read several text files at once, pass `files` (1-16 entries, each with `path` and optional `offset`, `limit`, `encoding`) instead of `file_path`. The files share one token budget; a member that fails only reports inside its own section. Images, PDFs and notebooks must be read on their own.",
            );
        }
        if self.encoding {
            lines.push(
                "- Text is decoded strictly; a non-UTF-8 file is decoded from its detected source encoding and the note `(Note: decoded from <encoding>; output is UTF-8.)` names it. If detection is ambiguous the read fails and lists the candidates: retry with `encoding` set to one of them.",
            );
        }
        if self.hex_view {
            lines.push(
                "- For binary files set `view` to `\"hex\"` to read 16-byte rows with an ASCII column instead of text.",
            );
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("\n{}", lines.join("\n"))
        }
    }
}

pub fn render_prompt_template(
    line_format: &str,
    max_size_instruction: &str,
    offset_instruction: &str,
    pdf_supported: bool,
    nyan: NyanPromptExtras,
) -> String {
    let nyan_prompt = nyan.render();
    let pdf_instruction = if pdf_supported {
        "\n- This tool can read PDF files (.pdf). For large PDFs (more than 10 pages), you MUST provide the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Reading a large PDF without the pages parameter will fail. Maximum 20 pages per request."
    } else {
        ""
    };
    format!(
        "Reads a file from the local filesystem. You can access any file directly by using this tool.\nAssume this tool is able to read all files on the machine. If the User provides a path to a file assume that path is valid. It is okay to read a file that does not exist; an error will be returned.\n\nUsage:\n- The file_path parameter must be an absolute path, not a relative path\n- By default, it reads up to {MAX_LINES_TO_READ} lines starting from the beginning of the file{max_size_instruction}\n{offset_instruction}\n{line_format}\n- This tool allows you to read images (eg PNG, JPG, etc). When reading an image file the contents are presented visually to the model.{pdf_instruction}\n- This tool can read Jupyter notebooks (.ipynb files) and returns all cells with their outputs, combining code, text, and visualizations.\n- This tool can only read files, not directories. To read a directory, use an ls command via the Bash tool.\n- You will regularly be asked to read screenshots. If the user provides a path to a screenshot, ALWAYS use this tool to view the file at the path. This tool will work with all temporary file paths.\n- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents.{nyan_prompt}"
    )
}
