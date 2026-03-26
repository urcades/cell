#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rustRoot = path.resolve(__dirname, "..");
const defaultTsRepo = path.resolve(rustRoot, "..");

function parseArgs(argv) {
  const args = {
    tsRepo: process.env.PI_TS_REPO || defaultTsRepo,
    input: null,
    output: path.join(
      rustRoot,
      "crates",
      "pi-rust-models",
      "src",
      "generated_catalog.rs",
    ),
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if ((arg === "--ts-repo" || arg === "--input" || arg === "--output") && index + 1 < argv.length) {
      const value = argv[index + 1];
      index += 1;
      if (arg === "--ts-repo") {
        args.tsRepo = value;
      } else if (arg === "--input") {
        args.input = value;
      } else {
        args.output = value;
      }
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      printHelp();
      process.exit(0);
    }
  }

  if (!args.input) {
    args.input = path.join(args.tsRepo, "packages", "ai", "src", "models.generated.ts");
  }

  return args;
}

function printHelp() {
  process.stdout.write(
    [
      "Usage:",
      "  node scripts/seed_model_catalog_from_ts.mjs [--ts-repo <path>] [--input <path>] [--output <path>]",
      "",
      "Seeds the Rust generated model catalog from the TypeScript generated catalog.",
      "This is an optional bridge step for maintainers; the checked-in Rust output remains authoritative.",
      "",
      "Options:",
      "  --ts-repo <path>   Path to the TypeScript repo (default: ../ or PI_TS_REPO)",
      "  --input <path>     Explicit models.generated.ts path",
      "  --output <path>    Explicit generated_catalog.rs path",
      "",
    ].join("\n"),
  );
}

function rustString(value) {
  return JSON.stringify(String(value));
}

function rustNumber(value) {
  if (!Number.isFinite(value)) {
    throw new Error(`Non-finite number in catalog: ${value}`);
  }
  const number = Number(value);
  return Number.isInteger(number) ? `${number}.0` : number.toString();
}

function rustInputs(inputs) {
  const normalized = inputs.map((value) => {
    if (value === "text") {
      return "ModelInput::Text";
    }
    if (value === "image") {
      return "ModelInput::Image";
    }
    throw new Error(`Unsupported model input kind: ${value}`);
  });

  if (normalized.length === 1 && normalized[0] === "ModelInput::Text") {
    return "TEXT_INPUTS";
  }
  if (
    normalized.length === 2 &&
    normalized[0] === "ModelInput::Text" &&
    normalized[1] === "ModelInput::Image"
  ) {
    return "TEXT_IMAGE_INPUTS";
  }
  return `&[${normalized.join(", ")}]`;
}

function extractModelsObject(source) {
  const marker = "export const MODELS =";
  const start = source.indexOf(marker);
  if (start === -1) {
    throw new Error("Could not find `export const MODELS =` in models.generated.ts");
  }

  const objectStart = source.indexOf("{", start + marker.length);
  if (objectStart === -1) {
    throw new Error("Could not find MODELS object start");
  }

  let depth = 0;
  let end = -1;
  for (let index = objectStart; index < source.length; index += 1) {
    const char = source[index];
    if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth -= 1;
      if (depth === 0) {
        end = index;
        break;
      }
    }
  }

  if (end === -1) {
    throw new Error("Could not find MODELS object end");
  }

  return source
    .slice(objectStart, end + 1)
    .replace(/\s+satisfies\s+Model<[^>]+>/g, "");
}

function loadTsCatalog(inputPath) {
  const source = fs.readFileSync(inputPath, "utf8");
  const objectLiteral = extractModelsObject(source);
  return vm.runInNewContext(`(${objectLiteral})`, Object.create(null), {
    timeout: 10_000,
  });
}

function flattenCatalog(catalog) {
  const specs = [];
  for (const [provider, models] of Object.entries(catalog)) {
    for (const [id, model] of Object.entries(models)) {
      specs.push({
        provider,
        api: model.api,
        id,
        name: model.name,
        reasoning: Boolean(model.reasoning),
        input: Array.isArray(model.input) ? model.input : ["text"],
        cost: {
          input: Number(model.cost?.input ?? 0),
          output: Number(model.cost?.output ?? 0),
          cacheRead: Number(model.cost?.cacheRead ?? 0),
          cacheWrite: Number(model.cost?.cacheWrite ?? 0),
        },
        contextWindow: Number(model.contextWindow ?? 0),
        maxTokens: Number(model.maxTokens ?? 0),
        baseUrl: model.baseUrl ?? "",
      });
    }
  }

  specs.sort((left, right) => {
    const leftKey = `${left.provider}/${left.id}`;
    const rightKey = `${right.provider}/${right.id}`;
    return leftKey.localeCompare(rightKey);
  });
  return specs;
}

function renderRust(specs, inputPath) {
  const renderedSpecs = specs
    .map(
      (spec) => `    BuiltInModelSpec {
        provider: ${rustString(spec.provider)},
        api: ${rustString(spec.api)},
        id: ${rustString(spec.id)},
        name: ${rustString(spec.name)},
        reasoning: ${spec.reasoning},
        input: ${rustInputs(spec.input)},
        cost: ModelCost {
            input: ${rustNumber(spec.cost.input)},
            output: ${rustNumber(spec.cost.output)},
            cache_read: ${rustNumber(spec.cost.cacheRead)},
            cache_write: ${rustNumber(spec.cost.cacheWrite)},
        },
        context_window: ${spec.contextWindow},
        max_tokens: ${spec.maxTokens},
        base_url: ${rustString(spec.baseUrl)},
    },`,
    )
    .join("\n");

  return `// This file is generated by scripts/seed_model_catalog_from_ts.mjs\n// Source: ${inputPath.replace(/\\/g, "/")}\n// Do not edit manually.\n\nuse pi_rust_ai_core::{ApiId, Model, ModelCost, ModelInput, ProviderId};\n\n#[derive(Clone, Debug)]\npub(crate) struct BuiltInModelSpec {\n    pub provider: &'static str,\n    pub api: &'static str,\n    pub id: &'static str,\n    pub name: &'static str,\n    pub reasoning: bool,\n    pub input: &'static [ModelInput],\n    pub cost: ModelCost,\n    pub context_window: u32,\n    pub max_tokens: u32,\n    pub base_url: &'static str,\n}\n\nimpl BuiltInModelSpec {\n    pub(crate) fn to_model(&self) -> Model {\n        Model {\n            id: self.id.to_string(),\n            name: self.name.to_string(),\n            api: ApiId::new(self.api),\n            provider: ProviderId::new(self.provider),\n            base_url: self.base_url.to_string(),\n            reasoning: self.reasoning,\n            input: self.input.to_vec(),\n            cost: self.cost.clone(),\n            context_window: self.context_window,\n            max_tokens: self.max_tokens,\n            headers: None,\n            compat: None,\n        }\n    }\n}\n\nconst TEXT_INPUTS: &[ModelInput] = &[ModelInput::Text];\nconst TEXT_IMAGE_INPUTS: &[ModelInput] = &[ModelInput::Text, ModelInput::Image];\n\npub(crate) const BUILT_IN_MODEL_SPECS: &[BuiltInModelSpec] = &[\n${renderedSpecs}\n];\n\npub(crate) fn built_in_model_specs() -> &'static [BuiltInModelSpec] {\n    BUILT_IN_MODEL_SPECS\n}\n`;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const catalog = loadTsCatalog(args.input);
  const specs = flattenCatalog(catalog);
  const output = renderRust(specs, args.input);
  fs.writeFileSync(args.output, output);
  process.stdout.write(
    `Wrote ${specs.length} model specs to ${path.resolve(args.output)}\n`,
  );
}

main();
