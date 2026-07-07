/** The demo app: a tiny document store + its CleverHans registry/seams. */

import { Agent, type JsonObject, Rejected } from "@cleverhans/node";

export const DOCS = new Map<string, { title: string; published: boolean }>([
  ["doc-7", { title: "Q3 Roadmap", published: false }],
]);

const REGISTRY: JsonObject = {
  spec_version: "0.1",
  blocks: [
    {
      block_type: "confirm",
      slots: [
        { name: "title", type: "string", required: true },
        { name: "detail", type: "string", required: false },
      ],
    },
  ],
  actions: [
    {
      id: "doc.publish",
      description: "Publish the currently open document so readers can see it.",
      params: [{ name: "docId", type: "string", source: "context", required: true }],
      block_type: "confirm",
      mutates: true,
      authz_key: "doc.publish",
    },
  ],
  context_params: { docId: "selected_record_id" },
};

export function makeAgent(llm: ConstructorParameters<typeof Agent>[0]["llm"]): Agent {
  return new Agent({
    registry: REGISTRY,
    handlers: {
      "doc.publish": async (params, principal) => {
        const doc = DOCS.get(String(params.docId));
        if (!doc) {
          throw new Rejected("document not found");
        }
        doc.published = true;
        return { published: params.docId, by: principal.user_id };
      },
    },
    dryRuns: {
      "doc.publish": async (params) => {
        const doc = DOCS.get(String(params.docId));
        if (!doc) {
          throw new Rejected("document not found");
        }
        return {
          affected_count: 1,
          sample_ids: [params.docId],
          summary: `Publish “${doc.title}”`,
        };
      },
    },
    slotBuilders: {
      "doc.publish": { title: { const: "Publish document" }, detail: { preview: "summary" } },
    },
    authorize: (principal) =>
      Array.isArray(principal.roles) && principal.roles.includes("editor")
        ? null
        : "editors only",
    llm,
  });
}

export function pickLlm(): ConstructorParameters<typeof Agent>[0]["llm"] {
  const apiKey = process.env.ANTHROPIC_API_KEY;
  if (apiKey) {
    return { provider: "anthropic", api_key: apiKey };
  }
  const model = process.env.OLLAMA_MODEL;
  if (model) {
    return { provider: "ollama", model };
  }
  console.log("(no provider configured — using the scripted model)\n");
  return {
    provider: "scripted",
    script: [[{ tool_call: { name: "doc.publish", arguments: {} } }]],
  };
}
