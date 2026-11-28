#!/usr/bin/env node
// Checks spec/asyncapi.yaml, the AsyncAPI description of the WebSocket + JSON
// binding, against the official parser and against the envelope schemas:
//
//   node scripts/check-asyncapi.mjs
//
// Run it through `pnpm spec:check` from typescript/; CI's web job runs it.
// It exits 1 and lists every problem when:
//
// 1. @asyncapi/parser reports an error-level diagnostic (warnings are printed);
// 2. the send operation's messages are not exactly the alternatives of
//    ClientEvent's `oneOf`, or the receive operation's not exactly those of
//    ServerEvent's; or a channel or `components.messages` holds a message
//    neither lists;
// 3. a message is not named after its schema entry, or its payload is not a
//    reference to that entry (`./schemas/<file>#/$defs/<Name>`, declared as
//    draft 2020-12), or the entry does not exist;
// 4. a message has no example, or an example does not validate against its
//    schema entry. The parser does not validate AsyncAPI 3 examples, so this
//    script does it with Ajv's draft 2020-12 implementation.
//
// The script reads the files and never writes them.

import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { Parser, fromFile, DiagnosticSeverity } from "@asyncapi/parser";
import Ajv2020 from "ajv/dist/2020.js";
import yaml from "js-yaml";

const WORKSPACE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const REPO = path.resolve(WORKSPACE, "..");
const DOCUMENT = path.join(REPO, "spec/asyncapi.yaml");

/**
 * The schema format every payload declares. AsyncAPI 3 names only draft-07 for
 * JSON Schema; this identifier follows its `;version=` pattern for the draft
 * the schema files are written in. The parser has no schema parser for it and
 * passes the schema through unvalidated, which is why step 4 exists.
 */
const SCHEMA_FORMAT = "application/schema+json;version=2020-12";

/** The two directions: operation action, schema file (relative to spec/), root type. */
const DIRECTIONS = [
  { action: "send", file: "schemas/client-event.json", root: "ClientEvent" },
  { action: "receive", file: "schemas/server-event.json", root: "ServerEvent" },
];

const problems = [];
/** The document as YAML, before the parser resolves references. */
let raw;
const problem = (message) => problems.push(message);

/** Names of the `$defs` entries a schema's root `oneOf` lists, in order. */
function alternatives(schema, file) {
  const names = [];
  for (const branch of schema.oneOf ?? []) {
    const match = /^#\/\$defs\/([^/]+)$/.exec(branch.$ref ?? "");
    if (match) {
      names.push(match[1]);
    } else {
      problem(`${file}: oneOf branch ${JSON.stringify(branch)} is not a reference to a $defs entry`);
    }
  }
  return names;
}

/** Problems between two name sets, phrased from the document's side. */
function compareSets(where, actual, expected) {
  for (const name of expected) {
    if (!actual.includes(name)) problem(`${where}: missing message ${name}`);
  }
  for (const name of actual) {
    if (!expected.includes(name)) problem(`${where}: message ${name} is not in the schema`);
  }
  const duplicates = actual.filter((name, i) => actual.indexOf(name) !== i);
  for (const name of new Set(duplicates)) problem(`${where}: message ${name} is listed twice`);
}

/** The target name of a local reference `#/<prefix>/<Name>`, or undefined. */
function localTarget(node, prefix) {
  const ref = node?.$ref;
  if (typeof ref !== "string" || !ref.startsWith(`#/${prefix}/`)) return undefined;
  return ref.slice(prefix.length + 3);
}

// --- 1. The official parser -------------------------------------------------

const parser = new Parser();
const { document, diagnostics } = await fromFile(parser, DOCUMENT).parse();
for (const diagnostic of diagnostics) {
  const where = diagnostic.path.length > 0 ? ` at ${diagnostic.path.join(".")}` : "";
  const line = `${DiagnosticSeverity[diagnostic.severity].toLowerCase()} ${diagnostic.code}${where}: ${diagnostic.message}`;
  if (diagnostic.severity === DiagnosticSeverity.Error) {
    problem(`parser: ${line}`);
  } else {
    console.warn(`parser: ${line}`);
  }
}
if (document === undefined) {
  if (problems.length === 0) problem("parser: returned no document");
  report();
}

// --- 2. and 3. Parity with the schemas --------------------------------------

raw = yaml.load(await readFile(DOCUMENT, "utf8"));
const specDir = path.dirname(DOCUMENT);
const schemas = new Map();
for (const direction of DIRECTIONS) {
  const schema = JSON.parse(await readFile(path.join(specDir, direction.file), "utf8"));
  schemas.set(direction.file, schema);
  direction.names = alternatives(schema, direction.file);
}
const allNames = DIRECTIONS.flatMap((direction) => direction.names);
/** Message name -> its direction. */
const directionOf = new Map(
  DIRECTIONS.flatMap((direction) => direction.names.map((name) => [name, direction])),
);

const operations = Object.entries(raw.operations ?? {});
const channels = Object.entries(raw.channels ?? {});
if (channels.length !== 1) {
  problem(`channels: expected exactly one channel (one connection is one session), found ${channels.length}`);
}
const [channelKey, channel] = channels[0] ?? [];

for (const direction of DIRECTIONS) {
  const matching = operations.filter(([, operation]) => operation.action === direction.action);
  if (matching.length !== 1) {
    problem(`operations: expected exactly one "${direction.action}" operation, found ${matching.length}`);
    continue;
  }
  const [key, operation] = matching[0];
  if (operation.channel?.$ref !== `#/channels/${channelKey}`) {
    problem(`operations.${key}: channel must be #/channels/${channelKey}`);
  }
  const names = (operation.messages ?? []).map((reference, i) => {
    const name = localTarget(reference, `channels/${channelKey}/messages`);
    if (name === undefined) {
      problem(`operations.${key}.messages[${i}]: not a reference to a message of channel ${channelKey}`);
    }
    return name;
  });
  compareSets(
    `operations.${key} (${direction.action}, ${direction.root})`,
    names.filter((name) => name !== undefined),
    direction.names,
  );
}
if (operations.length !== DIRECTIONS.length) {
  problem(`operations: expected ${DIRECTIONS.length} operations (send and receive), found ${operations.length}`);
}

if (channel !== undefined) {
  const entries = Object.entries(channel.messages ?? {});
  compareSets(`channels.${channelKey}.messages`, entries.map(([key]) => key), allNames);
  for (const [key, reference] of entries) {
    if (localTarget(reference, "components/messages") !== key) {
      problem(`channels.${channelKey}.messages.${key}: must be a reference to #/components/messages/${key}`);
    }
  }
}

const components = Object.entries(raw.components?.messages ?? {});
compareSets("components.messages", components.map(([key]) => key), allNames);
for (const [key, message] of components) {
  const where = `components.messages.${key}`;
  if (message.name !== key) {
    problem(`${where}: name must be ${JSON.stringify(key)}, the schema entry's name`);
  }
  const direction = directionOf.get(key);
  if (direction === undefined) continue;
  const expectedRef = `./${direction.file}#/$defs/${key}`;
  const payload = message.payload ?? {};
  if (payload.schemaFormat !== SCHEMA_FORMAT) {
    problem(`${where}.payload.schemaFormat: must be ${SCHEMA_FORMAT}`);
  }
  const schema = payload.schema ?? {};
  if (schema.$ref !== expectedRef || Object.keys(schema).length !== 1) {
    problem(`${where}.payload.schema: must be exactly { $ref: '${expectedRef}' }, found ${JSON.stringify(schema)}`);
  }
  if (schemas.get(direction.file).$defs?.[key] === undefined) {
    problem(`${where}: ${direction.file} has no $defs entry ${key}`);
  }
}

// --- 4. Examples -------------------------------------------------------------

const ajv = new Ajv2020({ allErrors: true });
for (const schema of schemas.values()) ajv.addSchema(schema);
for (const [key, message] of components) {
  const direction = directionOf.get(key);
  if (direction === undefined) continue;
  const schema = schemas.get(direction.file);
  const validateEntry = ajv.getSchema(`${schema.$id}#/$defs/${key}`);
  const validateRoot = ajv.getSchema(schema.$id);
  if (validateEntry === undefined || validateRoot === undefined) continue;
  const examples = message.examples ?? [];
  if (examples.length === 0) {
    problem(`components.messages.${key}: has no example`);
  }
  examples.forEach((example, i) => {
    const where = `components.messages.${key}.examples[${i}]${example.name ? ` (${example.name})` : ""}`;
    if (example.payload === undefined) {
      problem(`${where}: has no payload`);
      return;
    }
    if (!validateEntry(example.payload)) {
      for (const error of validateEntry.errors) {
        problem(`${where}: payload${error.instancePath} ${error.message} (${key}${error.schemaPath.slice(1)})`);
      }
    } else if (!validateRoot(example.payload)) {
      problem(`${where}: payload is a valid ${key} but not exactly one ${direction.root}`);
    }
  });
}

report();

function report() {
  if (problems.length > 0) {
    console.error(`spec/asyncapi.yaml: ${problems.length} problem(s)`);
    for (const message of problems) console.error(`  - ${message}`);
    process.exit(1);
  }
  const counts = DIRECTIONS.map((direction) => `${direction.names.length} ${direction.action}`).join(", ");
  console.log(`spec/asyncapi.yaml: valid AsyncAPI ${raw?.asyncapi}; messages match the schemas (${counts}); examples validate`);
  process.exit(0);
}
