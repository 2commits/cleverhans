#!/usr/bin/env node
// Registry codegen CLI over the native binding — no Rust toolchain needed.
//
//   npx cleverhans-codegen --schema registry.json --ts src/generated/registry.ts
//   npx cleverhans-codegen --schema registry.json --py app/generated/registry.py
//   npx cleverhans-codegen --schema registry.json --check   # freshness gate
//
// With no output flag the TypeScript module goes to stdout. `--check`
// writes nothing and exits 1 if any named output is stale.

"use strict";

const { readFileSync, writeFileSync } = require("node:fs");
const { generateTypes } = require("../index.js");

const TARGETS = { "--ts": "typescript", "--py": "python", "--rs": "rust" };

function main(argv) {
  let schema = null;
  let check = false;
  const outputs = [];
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--check") {
      check = true;
    } else if (flag === "--schema" || flag in TARGETS) {
      const value = argv[(i += 1)];
      if (value === undefined) throw new Error(`${flag} needs a value`);
      if (flag === "--schema") {
        if (schema !== null) throw new Error("--schema given twice");
        schema = value;
      } else {
        outputs.push({ target: TARGETS[flag], path: value });
      }
    } else {
      throw new Error(`unknown flag \`${flag}\``);
    }
  }
  if (schema === null) throw new Error("--schema <file> is required");

  const registryJson = readFileSync(schema, "utf8");
  if (outputs.length === 0) {
    if (check) throw new Error("--check needs at least one output flag");
    process.stdout.write(generateTypes(registryJson, "typescript"));
    return;
  }
  for (const { target, path } of outputs) {
    const module = generateTypes(registryJson, target);
    if (check) {
      let current = "";
      try {
        current = readFileSync(path, "utf8");
      } catch {
        // missing counts as stale
      }
      if (current !== module) throw new Error(`${path} is stale — re-run without --check`);
    } else {
      writeFileSync(path, module);
    }
  }
}

try {
  main(process.argv.slice(2));
} catch (err) {
  console.error(`cleverhans-codegen: ${err.message}`);
  console.error(
    "usage: cleverhans-codegen --schema <registry.json> [--ts <out.ts>] [--py <out.py>] [--rs <out.rs>] [--check]",
  );
  process.exit(1);
}
