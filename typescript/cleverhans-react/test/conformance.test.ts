/**
 * Client conformance vectors (spec/vectors/client/*.json): drives
 * `AgentSession` over a fake transport per the steps in each vector,
 * asserting snapshot state and outbound wire shapes. The vectors are the
 * language-neutral contract; this suite is the TypeScript runner.
 */

import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

import type { AppContext, ServerEvent } from "../src/envelope";
import { AgentSession, type SessionSnapshot } from "../src/session";
import { FakeTransport } from "./fake-transport";
import { type Bindings, matchValue } from "./vector-matcher";

const VECTORS_DIR = path.join(__dirname, "../../../spec/vectors/client");

interface ClientAction {
  send_message?: string;
  update_context?: AppContext;
  confirm?: string;
  reject?: string;
}

interface ClientVectorStep {
  client?: ClientAction;
  server?: ServerEvent;
  assert?: Record<string, unknown>;
  assert_sent?: Record<string, unknown>;
  assert_sent_first?: Record<string, unknown>;
}

interface ClientVector {
  name: string;
  description?: string;
  spec?: string[];
  steps: ClientVectorStep[];
}

const CONTEXT: AppContext = { route: "/transactions/tx_581", selected_record_id: "tx_581" };

function loadVectors(): ClientVector[] {
  return readdirSync(VECTORS_DIR)
    .filter((file) => file.endsWith(".json"))
    .sort()
    .map((file) => {
      const vector = JSON.parse(readFileSync(path.join(VECTORS_DIR, file), "utf8")) as ClientVector;
      expect(vector.name, `vector name must match file stem: ${file}`).toBe(
        file.replace(/\.json$/, ""),
      );
      return vector;
    });
}

/**
 * Projects the snapshot into the vector-visible shape: `pending_count`
 * replaces the pending array, transcript/proposals keep only the fields
 * vectors assert on.
 */
function project(snapshot: SessionSnapshot): Record<string, unknown> {
  return {
    busy: snapshot.busy,
    pending_count: snapshot.pending.length,
    transcript: snapshot.transcript.map(({ role, text }) => ({ role, text })),
    proposals: snapshot.proposals.map((view) => ({
      state: view.state,
      working: view.working,
      ...(view.reason !== undefined ? { reason: view.reason } : {}),
      ...(view.result !== undefined ? { result: view.result } : {}),
    })),
    last_error: snapshot.lastError
      ? {
          code: snapshot.lastError.code,
          message: snapshot.lastError.message,
          recoverable: snapshot.lastError.recoverable,
        }
      : null,
  };
}

function runClientAction(session: AgentSession, action: ClientAction): void {
  if (action.send_message !== undefined) {
    session.sendMessage(action.send_message);
  } else if (action.update_context !== undefined) {
    session.updateContext(action.update_context);
  } else if (action.confirm !== undefined) {
    session.confirm(action.confirm);
  } else if (action.reject !== undefined) {
    session.reject(action.reject);
  } else {
    throw new Error(`unknown client action: ${JSON.stringify(action)}`);
  }
}

describe("client conformance vectors", () => {
  for (const vector of loadVectors()) {
    it(vector.name, () => {
      const transport = new FakeTransport();
      const session = new AgentSession(transport, {
        context: CONTEXT,
        workingTimeoutMs: null,
      });
      const bindings: Bindings = new Map();
      try {
        vector.steps.forEach((step, index) => {
          const at = `step ${index}`;
          if (step.client) {
            runClientAction(session, step.client);
          } else if (step.server) {
            transport.emit(step.server);
          } else if (step.assert) {
            matchValue(step.assert, project(session.getSnapshot()), bindings, at);
          } else if (step.assert_sent) {
            matchValue(step.assert_sent, transport.sent.at(-1), bindings, at);
          } else if (step.assert_sent_first) {
            matchValue(step.assert_sent_first, transport.sent[0], bindings, at);
          } else {
            throw new Error(`${at}: empty step`);
          }
        });
      } finally {
        session.dispose();
      }
    });
  }
});
