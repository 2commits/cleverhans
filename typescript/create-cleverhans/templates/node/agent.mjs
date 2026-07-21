// CleverHans Node host — scaffolded starter. The agent never acts on your
// system: it proposes actions from registry.json; the handlers below run
// only after explicit user confirmation.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { Agent, Rejected, bindAgentSocket } from "@cleverhans/node";
import { WebSocketServer } from "ws";

const here = dirname(fileURLToPath(import.meta.url));

// Replace with your real data layer.
const RECORDS = new Map([["r-1", { title: "First record", archived: false }]]);

function pickLlm() {
  if (process.env.ANTHROPIC_API_KEY) {
    return { provider: "anthropic", api_key: process.env.ANTHROPIC_API_KEY };
  }
  if (process.env.OLLAMA_MODEL) {
    return { provider: "ollama", model: process.env.OLLAMA_MODEL };
  }
  console.log("(no provider configured — using the scripted model)");
  return {
    provider: "scripted",
    script: [[{ tool_call: { name: "record.archive", arguments: {} } }]],
  };
}

const agent = new Agent({
  registry: readFileSync(join(here, "registry.json"), "utf8"),
  handlers: {
    "record.archive": (params, principal) => {
      const record = RECORDS.get(params.recordId);
      if (!record) throw new Rejected("record not found");
      record.archived = true; // your app's normal execution path
      return { archived: params.recordId, by: principal.user_id };
    },
  },
  dryRuns: {
    "record.archive": (params) => {
      const record = RECORDS.get(params.recordId);
      if (!record) throw new Rejected("record not found");
      return { affected_count: 1, sample_ids: [params.recordId], summary: `Archive “${record.title}”` };
    },
  },
  slotBuilders: {
    "record.archive": { title: { const: "Archive record" }, detail: { preview: "summary" } },
  },
  authorize: (principal) => (principal.roles.includes("editor") ? null : "editors only"),
  llm: pickLlm(),
});

/** Replace with your session-cookie / bearer-token auth (spec §10). */
function authenticate(request) {
  void request;
  return { user_id: "demo", roles: ["editor"] };
}

const wss = new WebSocketServer({ port: 8788, path: "/agent" });
wss.on("connection", (ws, request) => {
  ws.on("message", bindAgentSocket(agent, authenticate(request), ws));
});
console.log("envelope stream at ws://127.0.0.1:8788/agent");
