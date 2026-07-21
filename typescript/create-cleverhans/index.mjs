#!/usr/bin/env node
// Scaffolds CleverHans into an existing project:
//
//   npm create cleverhans -- --host node            # or rust | python
//   npm create cleverhans -- --host rust --react    # + frontend wiring
//   npm create cleverhans -- --host node --dir agent
//
// Emits a `cleverhans/` directory (or --dir) with a starter registry
// document, a host stub for your stack, eval cases, and a README with the
// exact next steps. Never overwrites existing files.

import { cpSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HOSTS = ["rust", "node", "python"];
const templatesRoot = join(dirname(fileURLToPath(import.meta.url)), "templates");

function fail(message) {
  console.error(`create-cleverhans: ${message}`);
  console.error("usage: npm create cleverhans -- --host <rust|node|python> [--react] [--dir <path>]");
  process.exit(1);
}

function parseArgs(argv) {
  const options = { host: null, react: false, dir: "cleverhans" };
  for (let i = 0; i < argv.length; i += 1) {
    switch (argv[i]) {
      case "--host":
        options.host = argv[(i += 1)];
        break;
      case "--react":
        options.react = true;
        break;
      case "--dir":
        options.dir = argv[(i += 1)];
        break;
      default:
        fail(`unknown flag \`${argv[i]}\``);
    }
  }
  if (!HOSTS.includes(options.host)) {
    fail(`--host must be one of ${HOSTS.join(" | ")}`);
  }
  if (!options.dir) {
    fail("--dir needs a value");
  }
  return options;
}

function copyTemplate(name, target) {
  const source = join(templatesRoot, name);
  for (const entry of readdirSync(source, { recursive: true, withFileTypes: true })) {
    if (!entry.isFile()) {
      continue;
    }
    const relative = join(entry.parentPath.slice(source.length + 1) || "", entry.name);
    const destination = join(target, relative);
    if (existsSync(destination)) {
      console.log(`  skip  ${relative} (exists)`);
      continue;
    }
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(join(entry.parentPath, entry.name), destination);
    console.log(`  write ${relative}`);
  }
}

const options = parseArgs(process.argv.slice(2));
const target = resolve(options.dir);
mkdirSync(target, { recursive: true });

console.log(`Scaffolding CleverHans (${options.host}${options.react ? " + react" : ""}) into ${target}`);
copyTemplate("shared", target);
copyTemplate(options.host, target);
if (options.react) {
  copyTemplate("react", target);
}

console.log(`\nDone. Next steps are in ${join(options.dir, "README.md")}.`);
