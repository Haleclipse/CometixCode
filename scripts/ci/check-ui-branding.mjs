#!/usr/bin/env node
// UI branding check.
//
// nyan is provider-agnostic: user-visible copy names the product "nyan" and
// refers to the model neutrally. This scans every Rust string literal under
// the UI trees and fails on the word "Claude" and on links to the Claude Code
// docs site, which documents a different product.
//
// Not checked, so the gate does not trip on non-copy text:
// - comments (line, block, doc) — provenance notes such as `Maps to: CC ...`
//   stay;
// - `#[cfg(test)] mod ... { ... }` blocks — tests may assert that the old copy
//   is gone;
// - model ids (`claude-sonnet-4-6`), paths and wire fields: they are
//   lowercase, and the match is case-sensitive on the whole word, so
//   identifiers such as `ClaudeMd` do not match either.
//
// Usage: node scripts/ci/check-ui-branding.mjs

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const SCANNED = ["src/components", "src/screens", "src/commands"];

const RULES = [
  { pattern: /\bClaude\b/, reason: 'mentions "Claude"' },
  { pattern: /\b(?:code|docs)\.claude\.com\b/, reason: "links to the Claude Code docs" },
];
const TEST_MOD = /^#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{/;

function* rustFiles(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* rustFiles(path);
    else if (entry.name.endsWith(".rs")) yield path;
  }
}

const isIdent = (c) => c !== undefined && /[A-Za-z0-9_]/.test(c);

function decodeString(value) {
  const escapes = { n: "\n", r: "\r", t: "\t", 0: "\0", "\\": "\\", '"': '"', "'": "'" };
  return value.replace(
    /\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F_]+)\}|(\r?\n)[\t\n\r ]*|([nrt0\\'"]))/g,
    (_, hex, unicode, continuation, simple) => {
      if (hex !== undefined) return String.fromCodePoint(parseInt(hex, 16));
      if (unicode !== undefined) return String.fromCodePoint(parseInt(unicode.replaceAll("_", ""), 16));
      if (continuation !== undefined) return "";
      return escapes[simple];
    },
  );
}

// Yields { value, line } for every string literal outside comments and
// `#[cfg(test)]` modules. Handles nested block comments, raw strings
// (`r#"..."#`), byte/C-string prefixes, char literals and lifetimes.
function* stringLiterals(src) {
  let i = 0;
  let line = 1;
  let braceDepth = 0;
  let skipUntilDepth = null;
  let pendingTestMod = false;

  const advance = (to) => {
    for (; i < to; i++) if (src[i] === "\n") line++;
  };

  while (i < src.length) {
    const c = src[i];
    const next = src[i + 1];

    if (c === "/" && next === "/") {
      const end = src.indexOf("\n", i);
      advance(end === -1 ? src.length : end);
      continue;
    }
    if (c === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < src.length && depth > 0) {
        if (src[j] === "/" && src[j + 1] === "*") (depth++, (j += 2));
        else if (src[j] === "*" && src[j + 1] === "/") (depth--, (j += 2));
        else j++;
      }
      advance(j);
      continue;
    }

    if (src.startsWith("#[cfg(test)]", i)) {
      // Only a whole test module is skipped; the next `{` is its body.
      pendingTestMod = TEST_MOD.test(src.slice(i, i + 400));
      advance(i + "#[cfg(test)]".length);
      continue;
    }

    // String literal, optionally prefixed (b, c, r, br, cr) with raw hashes.
    const prefix = /^(?:b|c)?r(#*)"|^(?:b|c)?"/.exec(src.slice(i, i + 260));
    if (prefix && !isIdent(src[i - 1])) {
      const startLine = line;
      const start = i + prefix[0].length;
      let end;
      if (prefix[1] !== undefined) {
        end = src.indexOf(`"${prefix[1]}`, start);
        if (end === -1) end = src.length;
        advance(end + 1 + prefix[1].length);
      } else {
        let j = start;
        while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
        end = j;
        advance(j + 1);
      }
      if (skipUntilDepth === null) {
        const value = src.slice(start, end);
        // Match ordinary strings as rendered, but preserve raw string backslashes.
        yield { value: prefix[1] !== undefined ? value : decodeString(value), line: startLine };
      }
      continue;
    }

    // Char literal vs lifetime: 'x', '\n', '\u{..}' are literals; 'a is not.
    if (c === "'") {
      const m = /^'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'/su.exec(src.slice(i, i + 16));
      advance(m ? i + m[0].length : i + 1);
      continue;
    }

    if (c === "{") {
      if (pendingTestMod && skipUntilDepth === null) skipUntilDepth = braceDepth;
      pendingTestMod = false;
      braceDepth++;
    } else if (c === "}") {
      braceDepth--;
      if (skipUntilDepth === braceDepth) skipUntilDepth = null;
    } else if (c === ";") {
      pendingTestMod = false;
    }
    advance(i + 1);
  }
}

const violations = [];
for (const dir of SCANNED) {
  for (const file of rustFiles(resolve(root, dir))) {
    const src = readFileSync(file, "utf8");
    for (const { value, line } of stringLiterals(src)) {
      const rule = RULES.find(({ pattern }) => pattern.test(value));
      if (rule) {
        const shown = value.length > 120 ? `${value.slice(0, 117)}...` : value;
        violations.push(`${relative(root, file)}:${line}: ${rule.reason}: ${shown}`);
      }
    }
  }
}

if (violations.length > 0) {
  console.error("ui-branding: FAILED — UI string literals carry Claude branding:");
  for (const v of violations) console.error(`  - ${v}`);
  console.error(
    '\nResolution: name the product "nyan", refer to the model neutrally ("the model"), and link to docs/nyan/ or drop the link.',
  );
  process.exit(1);
}

console.log("ui-branding: passed.");
