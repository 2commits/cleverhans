// Smoke test: scaffold each host into a temp dir and assert the files land,
// idempotently (second run must skip, not overwrite).

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const bin = join(import.meta.dirname, "index.mjs");

function scaffold(args, cwd) {
  return execFileSync(process.execPath, [bin, ...args], { cwd, encoding: "utf8" });
}

const EXPECT = {
  rust: ["registry.json", "eval-cases.json", "agent.rs", "README.md"],
  node: ["registry.json", "eval-cases.json", "agent.mjs", "README.md"],
  python: ["registry.json", "eval-cases.json", "agent.py", "README.md"],
};

for (const [host, files] of Object.entries(EXPECT)) {
  const dir = mkdtempSync(join(tmpdir(), "create-cleverhans-"));
  try {
    scaffold(["--host", host, "--react"], dir);
    for (const file of [...files, "AgentWidget.tsx", "README-react.md"]) {
      assert.ok(existsSync(join(dir, "cleverhans", file)), `${host}: missing ${file}`);
    }
    // Registry parses and carries the starter action.
    const registry = JSON.parse(readFileSync(join(dir, "cleverhans", "registry.json"), "utf8"));
    assert.equal(registry.actions[0].id, "record.archive");

    // Second run skips existing files instead of overwriting.
    const marker = join(dir, "cleverhans", "registry.json");
    writeFileSync(marker, "{ \"edited\": true }");
    const output = scaffold(["--host", host], dir);
    assert.match(output, /skip\s+registry\.json/);
    assert.match(readFileSync(marker, "utf8"), /edited/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

// Bad flags fail loudly.
for (const args of [[], ["--host", "ruby"], ["--wat"]]) {
  assert.throws(() => scaffold(args, tmpdir()), `expected failure for ${args.join(" ")}`);
}

console.log("create-cleverhans: all smoke tests passed");
