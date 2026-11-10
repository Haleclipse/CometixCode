// Run independently: node --test scripts/ci/check-ui-branding.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

const checker = new URL("./check-ui-branding.mjs", import.meta.url);
const BRAND = 'mentions "Claude"';
const DOCS = "links to the Claude Code docs";
const copy = (literal) => `const COPY: &str = ${literal};`;

function check(source, expected, file = "src/components/copy.rs") {
  const root = mkdtempSync(join(tmpdir(), "nyan-ui-branding-"));
  try {
    for (const dir of ["scripts/ci", "src/components", "src/screens", "src/commands"]) {
      mkdirSync(join(root, dir), { recursive: true });
    }
    const script = join(root, "scripts/ci/check-ui-branding.mjs");
    copyFileSync(checker, script);
    mkdirSync(dirname(join(root, file)), { recursive: true });
    writeFileSync(join(root, file), source);
    const result = spawnSync(process.execPath, [script], { encoding: "utf8" });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    assert.equal(result.status, expected.length > 0 ? 1 : 0, result.stderr);
    if (expected.length === 0) {
      assert.equal(result.stdout, "ui-branding: passed.\n");
      assert.equal(result.stderr, "");
    } else {
      assert.equal(result.stdout, "");
      assert.match(result.stderr, /^ui-branding: FAILED/);
      assert.equal((result.stderr.match(/^  - /gm) ?? []).length, expected.length);
      for (const [line, reason] of expected) {
        assert.ok(result.stderr.includes(`  - ${file}:${line}: ${reason}: `), result.stderr);
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const cases = [
  ["ordinary brand", '"Claude wants to edit"', BRAND],
  ["ordinary code docs", '"code.claude.com/docs/en/mcp"', DOCS],
  ["ordinary docs host", '"docs.claude.com/docs/en/mcp"', DOCS],
  ["raw brand", 'r#"Claude wants to edit"#', BRAND],
  ["raw docs", 'r##"code.claude.com/docs/en/mcp"##', DOCS],
  ["escaped quotes", String.raw`"say \"Claude\" now"`, BRAND],
  ["escaped quote before docs", String.raw`"see \"code.claude.com/docs/en/mcp\""`, DOCS],
  ["escaped newline before brand", String.raw`"\nClaude wants to edit"`, BRAND],
  ["escaped tab before brand", String.raw`"\tClaude wants to edit"`, BRAND],
  ["escaped newline before code docs", String.raw`"\ncode.claude.com/docs/en/mcp"`, DOCS],
  ["escaped tab before code docs", String.raw`"\tcode.claude.com/docs/en/mcp"`, DOCS],
  ["escaped newline before docs host", String.raw`"\ndocs.claude.com/docs/en/mcp"`, DOCS],
  ["escaped tab before docs host", String.raw`"\tdocs.claude.com/docs/en/mcp"`, DOCS],
  ["escaped carriage return", String.raw`"\rClaude"`, BRAND],
  ["escaped NUL", String.raw`"\0Claude"`, BRAND],
  ["hex escape boundary", String.raw`"\x20Claude"`, BRAND],
  ["hex escape inside brand", String.raw`"\x43laude"`, BRAND],
  ["unicode escape boundary", String.raw`"\u{20}Claude"`, BRAND],
  ["unicode escape inside docs", String.raw`"\u{63}ode.claude.com"`, DOCS],
  ["unicode escape with underscores", String.raw`"\u{0_043}laude"`, BRAND],
  ["byte string", String.raw`b"\nClaude"`, BRAND],
  ["C string", String.raw`c"\tClaude"`, BRAND],
  ["raw byte string", 'br#"Claude"#', BRAND],
  ["raw C string", 'cr#"code.claude.com"#', DOCS],
  ["neutral copy", '"nyan wants to edit"', null],
  ["lowercase model ID", '"claude-sonnet-4-6"', null],
  ["model ID after escaped newline", String.raw`"\nclaude-sonnet-4-6"`, null],
  ["raw model ID", 'r#"claude-sonnet-4-6"#', null],
  ["model ID does not hide a brand", '"claude-sonnet-4-6 via Claude"', BRAND],
  ["model ID does not hide docs", '"claude-sonnet-4-6: code.claude.com"', DOCS],
  ["identifier is not a whole word", '"ClaudeMd"', null],
  ["raw newline is not decoded", String.raw`r"\nClaude"`, null],
  ["raw tab is not decoded", String.raw`r#"\tClaude"#`, null],
  ["raw docs newline is not decoded", String.raw`r#"\ncode.claude.com"#`, null],
  ["raw docs tab is not decoded", String.raw`r#"\tdocs.claude.com"#`, null],
  ["raw string with a real boundary", String.raw`r#"\n Claude"#`, BRAND],
  ["raw docs with a real boundary", String.raw`r#"\t code.claude.com"#`, DOCS],
  ["escaped backslash is not decoded twice", String.raw`"\\nClaude"`, null],
  ["escaped backslash keeps docs boundary semantics", String.raw`"\\tcode.claude.com"`, null],
  ["decoded word prefix is not a boundary", String.raw`"\x6eClaude"`, null],
  ["decoded unicode word prefix is not a boundary", String.raw`"\u{6e}code.claude.com"`, null],
];

for (const [name, literal, reason] of cases) {
  test(name, () => check(copy(literal), reason ? [[1, reason]] : []));
}

test("escaped quote does not terminate a string or swallow the following literal", () => {
  check(`${copy(String.raw`"a trailing quote: \""`)}\n${copy('"Claude"')}`, [[2, BRAND]]);
});

test("line continuation removes whitespace rather than adding a boundary", () => {
  check(copy('"n\\\n    Claude"'), []);
  check(copy('"\\\n    Claude"'), [[1, BRAND]]);
  check(copy('"n\\\r\n \tcode.claude.com"'), []);
});

test("decoded newlines do not change source line numbers", () => {
  check(`${copy(String.raw`"\nnyan"`)}\n${copy(String.raw`"\tClaude"`)}`, [[2, BRAND]]);
});

test("comments, including nested block comments, are excluded", () => {
  check(String.raw`// "Claude"
/// "code.claude.com"
//! "docs.claude.com"
/* "Claude" /* "code.claude.com" */ "Claude" */
const COPY: &str = "nyan";`, []);
});

test("comment markers inside a string do not hide branding", () => {
  check(copy('"// Claude /* code.claude.com */"'), [[1, BRAND]]);
});

test("test modules are skipped and subsequent production code is scanned", () => {
  check(String.raw`#[cfg(test)]
mod tests {
    const OLD: &str = "\nClaude";
    mod nested { const DOC: &str = r#"code.claude.com { }"#; }
    // } "Claude"
    /* } "code.claude.com" */
    const QUOTED: &str = "\" } Claude";
}
const COPY: &str = "\tClaude";
const DOC: &str = "\ncode.claude.com/docs/en/mcp";`, [[9, BRAND], [10, DOCS]]);
});

test("char literals and lifetimes do not swallow production strings", () => {
  check(String.raw`fn copy<'a>() -> &'a str {
    let quote = '"';
    let brace = '}';
    "Claude"
}`, [[4, BRAND]]);
});

for (const dir of ["src/components/nested", "src/screens", "src/commands"]) {
  test(`scan scope includes ${dir}`, () => check(copy('"Claude"'), [[1, BRAND]], `${dir}/copy.rs`));
}

test("scan scope does not expand to services", () => {
  check(copy('"Claude"'), [], "src/services/copy.rs");
});
