/** Self-contained confirm flow — no API key, no network. `pnpm smoke`. */

import { DOCS, makeAgent, pickLlm } from "./registry.ts";

const agent = makeAgent(pickLlm());
const session = agent.session({ user_id: "alex", roles: ["editor"] });

await session.handleCollect({
  type: "init",
  spec_version: "0.1.0-draft",
  context: { route: "/docs/doc-7", selected_record_id: "doc-7" },
});

console.log("user: publish this document");
const events = await session.handleCollect({
  type: "user_message",
  text: "publish this document",
  client_msg_id: "c-1",
});
for (const event of events) {
  console.log("agent →", JSON.stringify(event));
}

const proposal = events.find((event) => event.type === "action_proposal");
if (!proposal) {
  console.log("(no proposal — nothing to confirm)");
  process.exit(0);
}

console.log(`\nuser confirms ${String(proposal.proposal_id)}`);
for (const event of await session.handleCollect({
  type: "confirm_action",
  proposal_id: proposal.proposal_id ?? null,
})) {
  console.log("agent →", JSON.stringify(event));
}
console.log("\nstore says:", DOCS.get("doc-7"));

// The native agent's callbacks keep the event loop alive (what a server
// wants); a one-shot script exits explicitly.
process.exit(0);
