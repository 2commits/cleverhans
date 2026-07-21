/**
 * WebSocket server hosting the agent — the backend counterpart of
 * `@cleverhans/react`'s `createWebSocketTransport` (same JSON-frame wire as
 * the Rust `cleverhans-ws` binding). `pnpm serve`, then point the playground
 * transport at ws://127.0.0.1:8788/agent.
 */

import { WebSocketServer } from "ws";

import { bindAgentSocket } from "@cleverhans/node";

import { makeAgent, pickLlm } from "./registry.ts";

const agent = makeAgent(pickLlm());
const wss = new WebSocketServer({ port: 8788, path: "/agent" });

wss.on("connection", (ws, request) => {
  // Demo-only principal: a real app maps its session cookie / bearer token
  // here (spec §10) and refuses the upgrade when unauthenticated.
  void request;
  ws.on("message", bindAgentSocket(agent, { user_id: "demo", roles: ["editor"] }, ws));
});

console.log("envelope stream at ws://127.0.0.1:8788/agent");
