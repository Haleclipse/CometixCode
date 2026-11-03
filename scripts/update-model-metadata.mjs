#!/usr/bin/env node

/**
 * Model metadata generator for nyan (provider redesign PR 2, decisions 13-15).
 *
 * Converts the models.dev v2 catalog into nyan's checked-in snapshot
 * src/utils/model/model_metadata.json. Normalization follows Pi Agent
 * packages/ai/catalog/sources/models-dev.ts at PI_COMMIT; the logic is
 * re-implemented here, Pi code is not executed.
 *
 * Output is deterministic (no timestamps, sorted keys), so the weekly CI job
 * only opens a PR when models or pins actually change.
 *
 * The hash is SHA-256 over canonical JSON of `models` (keys sorted recursively,
 * integral numbers written without a fraction). src/utils/model/metadata.rs
 * recomputes the same canonical form; keep the two in sync.
 *
 * Usage:
 *   node scripts/update-model-metadata.mjs [--source <url|file>] [--output <file>]
 *     [--pi-commit <40-hex sha>] [--models-dev-version <40-hex sha>]
 */

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT_DIR = resolve(__dirname, "..");
const DEFAULT_OUTPUT = resolve(ROOT_DIR, "src/utils/model/model_metadata.json");
const NOTICE_PATH = resolve(ROOT_DIR, "NOTICE");
const DEFAULT_URL = "https://models.dev/experimental/v2.0/api.json";
const DEFAULT_PI_COMMIT = "1f8604f84f968814fc84523557c01d704a6a8f83";
// models.dev `dev` HEAD resolved when the catalog was fetched. The live API is
// not commit-addressed, so this is a best-effort record, not a verified content
// pin; the PR body states the fetch time.
const DEFAULT_MODELS_DEV_VERSION = "c630900e53c472132194987aa702d22e392197db";
const SCHEMA_VERSION = 1;
// Guards against an upstream shape change silently producing an empty snapshot.
const MIN_MODELS = 100;

// Provider catalogs in Pi Agent packages/ai/catalog/providers at PI_COMMIT.
const PI_PROVIDERS = [
  "amazon-bedrock",
  "ant-ling",
  "anthropic",
  "azure",
  "baseten",
  "cerebras",
  "cloudflare-ai-gateway",
  "cloudflare-workers-ai",
  "deepseek",
  "fireworks",
  "github-copilot",
  "google-vertex",
  "google",
  "groq",
  "huggingface",
  "kimi-coding",
  "meta",
  "minimax-cn",
  "minimax",
  "mistral",
  "moonshotai-cn",
  "moonshotai",
  "nvidia",
  "openai-codex",
  "openai",
  "opencode-go",
  "opencode",
  "openrouter",
  "qwen-token-plan-cn",
  "qwen-token-plan-individual",
  "qwen-token-plan",
  "radius",
  "together",
  "typesafe",
  "vercel-ai-gateway",
  "xai",
  "xiaomi-token-plan-ams",
  "xiaomi-token-plan-cn",
  "xiaomi-token-plan-sgp",
  "xiaomi",
  "zai-coding-cn",
  "zai",
];

function parseArgs() {
  const args = process.argv.slice(2);
  const options = {
    output: DEFAULT_OUTPUT,
    source: DEFAULT_URL,
    piCommit: DEFAULT_PI_COMMIT,
    modelsDevVersion: DEFAULT_MODELS_DEV_VERSION,
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--output" && i + 1 < args.length) {
      options.output = resolve(args[++i]);
    } else if (arg === "--source" && i + 1 < args.length) {
      options.source = args[++i];
    } else if (arg === "--pi-commit" && i + 1 < args.length) {
      options.piCommit = args[++i];
    } else if (arg === "--models-dev-version" && i + 1 < args.length) {
      options.modelsDevVersion = args[++i];
    }
  }

  for (const [flag, value] of [
    ["--pi-commit", options.piCommit],
    ["--models-dev-version", options.modelsDevVersion],
  ]) {
    if (!/^[0-9a-f]{40}$/.test(value)) {
      throw new Error(`${flag} must be a full 40-character commit SHA, got ${value}`);
    }
  }

  return options;
}

async function loadSourceData(source) {
  if (source.startsWith("http://") || source.startsWith("https://")) {
    const res = await fetch(source);
    if (!res.ok) {
      throw new Error(`HTTP ${res.status} from ${source}`);
    }
    return await res.json();
  }
  return JSON.parse(readFileSync(source, "utf8"));
}

function normalizeCost(cost) {
  const input = cost?.text?.input;
  const output = cost?.text?.output;
  if (!input && !output) return null;

  let cacheWrite = null;
  const writeVal = input?.cache?.write;
  if (typeof writeVal === "number") {
    cacheWrite = writeVal;
  } else if (typeof writeVal === "object" && writeVal !== null) {
    const values = Object.values(writeVal);
    if (values.length > 0 && typeof values[0] === "number") {
      cacheWrite = values[0];
    }
  }

  return {
    input: input?.price ?? 0,
    output: output?.price ?? 0,
    cacheRead: input?.cache?.read ?? null,
    cacheWrite: cacheWrite ?? null,
  };
}

// Canonical form shared with metadata.rs: object keys sorted, null members
// omitted (Rust drops None fields when re-serializing), integral numbers
// written without a fraction (JSON.stringify does this already).
export function canonicalize(value) {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .filter((key) => value[key] !== null)
        .sort()
        .map((key) => [key, canonicalize(value[key])]),
    );
  }
  return value;
}

export function computeModelsHash(models) {
  return createHash("sha256").update(JSON.stringify(canonicalize(models)), "utf8").digest("hex");
}

export function updateNoticeCommits(notice, piCommit, modelsDevVersion) {
  const pins = [piCommit, modelsDevVersion];
  const matches = notice.match(/^Commit: [0-9a-f]+$/gm) ?? [];
  if (matches.length !== pins.length) {
    throw new Error(`NOTICE must contain ${pins.length} "Commit:" lines, found ${matches.length}`);
  }
  let index = 0;
  return notice.replace(/^Commit: [0-9a-f]+$/gm, () => `Commit: ${pins[index++]}`);
}

export function transformCatalog(catalog, options) {
  const models = {};

  for (const provider of PI_PROVIDERS) {
    const providerData = catalog[provider];
    if (!providerData?.models) continue;

    for (const [modelId, model] of Object.entries(providerData.models)) {
      if (model.type !== "chat") continue;

      const contextWindow = model.limit?.context;
      const maxOutputTokens = model.limit?.output;
      const input = model.modalities?.input;
      const reasoning = model.capabilities?.reasoning;
      const hasApi = model.api && Object.keys(model.api).length > 0;

      // Same required set as the Rust validator; a missing field drops the entry.
      if (!contextWindow || !maxOutputTokens || !input?.length || reasoning === undefined || !hasApi) {
        continue;
      }

      const hasImage = input.some((x) => x === "image" || x.startsWith("image/"));
      const toolsSupported = model.capabilities?.tools?.supported === true;
      const structuredOutput = model.capabilities?.structured_output === true;
      const hasCache = !!model.capabilities?.cache;

      const entry = {
        provider,
        model: modelId,
        displayName: model.name || modelId,
        contextWindow,
        maxOutputTokens,
        capabilities: {
          contextWindow,
          maxOutputTokens,
          tools: toolsSupported ? "native" : "none",
          thinking: reasoning?.supported === true ? "adaptive" : "none",
          images: hasImage ? "input" : "none",
          structuredOutput,
          streaming: true,
          promptCache: hasCache ? "native" : "none",
        },
        cost: normalizeCost(model.cost),
        input: [...input],
      };
      models[`${provider}/${modelId}`] = entry;

      // nyan's provider kind is "gemini"; Pi files the same models under "google".
      if (provider === "google") {
        models[`gemini/${modelId}`] = { ...entry, provider: "gemini" };
      }
    }
  }

  const sorted = Object.fromEntries(Object.keys(models).sort().map((key) => [key, models[key]]));
  const count = Object.keys(sorted).length;
  if (count < MIN_MODELS) {
    throw new Error(`Only ${count} runnable models after transform; expected at least ${MIN_MODELS}`);
  }

  return {
    schemaVersion: SCHEMA_VERSION,
    piCommit: options.piCommit,
    modelsDevVersion: options.modelsDevVersion,
    hash: computeModelsHash(sorted),
    models: sorted,
  };
}

async function main() {
  const options = parseArgs();
  console.log(`Loading model catalog from ${options.source}...`);
  const rawCatalog = await loadSourceData(options.source);

  const snapshot = transformCatalog(rawCatalog, options);
  writeFileSync(options.output, JSON.stringify(snapshot, null, 2) + "\n", "utf8");
  console.log(`Wrote ${Object.keys(snapshot.models).length} models to ${options.output}, hash ${snapshot.hash}`);

  const notice = readFileSync(NOTICE_PATH, "utf8");
  writeFileSync(NOTICE_PATH, updateNoticeCommits(notice, options.piCommit, options.modelsDevVersion), "utf8");
}

if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error("Failed to generate model metadata:", err);
    process.exit(1);
  });
}
