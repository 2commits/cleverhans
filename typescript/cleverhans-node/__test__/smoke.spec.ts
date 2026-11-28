/** Bridge smoke tests beyond the shared vectors. */

import { describe, expect, it } from "vitest";

import {
  Agent,
  type AgentSocket,
  type JsonObject,
  Rejected,
  SUBPROTOCOL,
  bindAgentSocket,
} from "../src/index";

const REGISTRY: JsonObject = {
  spec_version: "0.1",
  blocks: [
    { block_type: "confirm", slots: [{ name: "title", type: "string", required: false }] },
  ],
  actions: [
    {
      id: "record.touch",
      description: "Touch the selected record",
      params: [{ name: "recordId", type: "string", source: "context", required: true }],
      block_type: "confirm",
      mutates: true,
      authz_key: "record.touch",
    },
  ],
  context_params: { recordId: "selected_record_id" },
};

const INIT = {
  type: "init",
  spec_version: "0.1.0-draft",
  context: { route: "/records/r-1", selected_record_id: "r-1" },
};
const ASK = { type: "user_message", text: "touch it", client_msg_id: "c-1" };
const SCRIPT = [[{ tool_call: { name: "record.touch", arguments: {} } }]];

function makeAgent(
  handler: (params: JsonObject, principal: JsonObject) => unknown,
  overrides: Partial<ConstructorParameters<typeof Agent>[0]> = {},
): Agent {
  return new Agent({
    registry: REGISTRY,
    handlers: { "record.touch": handler },
    dryRuns: {
      "record.touch": async (params) => ({ affected_count: 1, sample_ids: [params.recordId] }),
    },
    slotBuilders: { "record.touch": { title: { const: "Touch record" } } },
    llm: { provider: "scripted", script: SCRIPT },
    ...overrides,
  });
}

async function proposeAndConfirm(agent: Agent, principal: JsonObject) {
  const session = agent.session(principal);
  await session.handleCollect(INIT);
  const events = await session.handleCollect(ASK);
  const proposal = events.find((event) => event.type === "action_proposal");
  expect(proposal, JSON.stringify(events)).toBeDefined();
  return session.handleCollect({
    type: "confirm_action",
    proposal_id: proposal!.proposal_id as string,
  });
}

describe("napi bridge", () => {
  it("executes the handler under the JSON principal", async () => {
    const agent = makeAgent(async (params, principal) => ({
      touched: params.recordId,
      by: principal.user_id,
    }));

    const events = await proposeAndConfirm(agent, { user_id: "alex" });

    expect(events.at(-1)).toMatchObject({
      state: "executed",
      result: { touched: "r-1", by: "alex" },
    });
  });

  it("maps the Rejected class to failed-with-reason", async () => {
    const agent = makeAgent(async () => {
      throw new Rejected("record is locked");
    });

    const events = await proposeAndConfirm(agent, { user_id: "alex" });

    expect(events.at(-1)).toMatchObject({ state: "failed" });
    expect(String(events.at(-1)!.reason)).toContain("record is locked");
  });

  it("declarative slot builders fill proposal slots", async () => {
    const agent = makeAgent(async () => ({}));
    const session = agent.session({ user_id: "a" });
    await session.handleCollect(INIT);

    const events = await session.handleCollect(ASK);

    const proposal = events.find((event) => event.type === "action_proposal");
    expect(proposal!.slots).toEqual({ title: "Touch record" });
  });

  it("authorize deny string declines without a proposal", async () => {
    const agent = makeAgent(
      async () => {
        throw new Error("must never run");
      },
      { authorize: () => "editors only" },
    );
    const session = agent.session({ user_id: "a" });
    await session.handleCollect(INIT);

    const events = await session.handleCollect(ASK);

    expect(events.every((event) => event.type !== "action_proposal")).toBe(true);
    expect(events.at(-1)).toMatchObject({ type: "chat_message", done: true });
  });

  it("drives a turn through a custom llm callback", async () => {
    const agent = makeAgent(async () => ({ ok: true }), {
      llm: (request) => {
        expect((request.tools as JsonObject[])[0]!.name).toBe("record.touch");
        return [{ tool_call: { name: "record.touch", arguments: {} } }];
      },
    });

    const events = await proposeAndConfirm(agent, { user_id: "a" });

    expect(events.at(-1)).toMatchObject({ state: "executed", result: { ok: true } });
  });

  it("a handler result shaped like the rejection sentinel executes normally", async () => {
    // The result envelope is total, so this value cannot be misclassified.
    const agent = makeAgent(async () => ({ __cleverhans_rejected: "just data" }));

    const events = await proposeAndConfirm(agent, { user_id: "a" });

    expect(events.at(-1)).toMatchObject({
      state: "executed",
      result: { __cleverhans_rejected: "just data" },
    });
  });

  it("a Rejected subclass still maps to failed-with-reason", async () => {
    class DocumentLocked extends Rejected {}
    const agent = makeAgent(async () => {
      throw new DocumentLocked("locked");
    });

    const events = await proposeAndConfirm(agent, { user_id: "a" });

    expect(events.at(-1)).toMatchObject({ state: "failed" });
    expect(String(events.at(-1)!.reason)).toContain("locked");
  });

  it("init-first violation closes the session and reports via closed", async () => {
    const agent = makeAgent(async () => ({}));
    const session = agent.session({ user_id: "a" });

    const events = await session.handleCollect(ASK); // no init first
    const later = await session.handleCollect(INIT); // post-close: nothing

    expect(session.closed).toBe(true);
    expect(events.at(-1)).toMatchObject({ code: "init_required", recoverable: false });
    expect(later).toEqual([]);
  });

  it("an unsupported spec version ends the session with a close code and reason", async () => {
    const agent = makeAgent(async () => ({}));
    const session = agent.session({ user_id: "a" });
    expect([session.closed, session.closeCode, session.closeReason]).toEqual([false, null, null]);

    const events = await session.handleCollect({ ...INIT, spec_version: "9.9" });
    const later = await session.handleCollect(ASK);

    expect(events).toEqual([expect.objectContaining({ code: "unsupported_spec_version", recoverable: false })]);
    expect(later).toEqual([]);
    expect([session.closed, session.closeCode, session.closeReason]).toEqual([
      true,
      1008,
      "unsupported_spec_version",
    ]);
  });

  it("an unknown message type is ignored and init is still required first", async () => {
    const agent = makeAgent(async () => ({}));
    const session = agent.session({ user_id: "a" });

    const ignored = await session.handleCollect({ type: "frobnicate" });
    const opened = await session.handleCollect(INIT);

    expect([ignored, opened, session.closed]).toEqual([[], [], false]);
  });

  it("exports the binding's subprotocol name", () => {
    expect(SUBPROTOCOL).toBe("cleverhans.v0.1");
  });

  it("sync throws in handlers become internal failures, not crashes", async () => {
    const agent = makeAgent(() => {
      throw new Error("boom");
    });

    const events = await proposeAndConfirm(agent, { user_id: "a" });

    expect(events.at(-1)).toMatchObject({ state: "failed" });
    expect(String(events.at(-1)!.reason)).toContain("boom");
  });
});

class FakeSocket implements AgentSocket {
  sent: JsonObject[] = [];
  closes: [number | undefined, string | undefined][] = [];

  send(data: string): void {
    this.sent.push(JSON.parse(data) as JsonObject);
  }

  close(code?: number, reason?: string): void {
    this.closes.push([code, reason]);
  }
}

async function until(condition: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 200 && !condition(); attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe("bindAgentSocket", () => {
  it("closes the socket with 1008 and init_required after an init-first violation", async () => {
    const socket = new FakeSocket();
    const onMessage = bindAgentSocket(makeAgent(async () => ({})), { user_id: "a" }, socket);

    onMessage(JSON.stringify(ASK));
    await until(() => socket.closes.length > 0);

    expect(socket.sent).toEqual([expect.objectContaining({ code: "init_required", recoverable: false })]);
    expect(socket.closes).toEqual([[1008, "init_required"]]);
  });

  it("closes the socket with 1008 and unsupported_spec_version, and answers nothing later", async () => {
    const socket = new FakeSocket();
    const onMessage = bindAgentSocket(makeAgent(async () => ({})), { user_id: "a" }, socket);

    onMessage(JSON.stringify({ ...INIT, spec_version: "9.9" }));
    onMessage(JSON.stringify(ASK));
    await until(() => socket.closes.length > 0);
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(socket.sent).toEqual([
      expect.objectContaining({ code: "unsupported_spec_version", recoverable: false }),
    ]);
    expect(socket.closes).toEqual([[1008, "unsupported_spec_version"]]);
  });

  it("leaves an open session's socket open", async () => {
    const socket = new FakeSocket();
    const onMessage = bindAgentSocket(makeAgent(async () => ({})), { user_id: "a" }, socket);

    onMessage(JSON.stringify(INIT));
    onMessage(JSON.stringify(ASK));
    await until(() => socket.sent.length > 0);

    expect(socket.sent.at(-1)).toMatchObject({ type: "action_proposal" });
    expect(socket.closes).toEqual([]);
  });
});
