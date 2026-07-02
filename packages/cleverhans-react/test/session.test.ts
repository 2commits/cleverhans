import { describe, expect, it } from "vitest";

import { SPEC_VERSION } from "../src/envelope";
import { AgentSession } from "../src/session";
import { FakeTransport, proposalEvent } from "./fake-transport";

function openSession() {
  const transport = new FakeTransport();
  const session = new AgentSession(transport, {
    context: { route: "/transactions/tx_581", selected_record_id: "tx_581" },
  });
  return { transport, session };
}

describe("AgentSession", () => {
  it("sends init first, with the spec version", () => {
    const { transport } = openSession();

    expect(transport.sent[0]).toMatchObject({ type: "init", spec_version: SPEC_VERSION });
  });

  it("sendMessage appends a user transcript entry and sends user_message", () => {
    const { transport, session } = openSession();

    session.sendMessage("remove the co-buyer");

    expect(session.getSnapshot().transcript).toEqual([
      { id: "c-1", role: "user", text: "remove the co-buyer" },
    ]);
    expect(transport.sent[1]).toMatchObject({ type: "user_message", client_msg_id: "c-1" });
  });

  it("updateContext manages a monotonic context_seq", () => {
    const { transport, session } = openSession();

    session.updateContext({ route: "/a" });
    session.updateContext({ route: "/b" });

    expect(transport.sent.slice(1)).toMatchObject([
      { type: "context_update", context_seq: 1 },
      { type: "context_update", context_seq: 2 },
    ]);
  });

  it("an action_proposal arrives as pending", () => {
    const { transport, session } = openSession();

    transport.emit(proposalEvent("prop-1"));

    const snapshot = session.getSnapshot();
    expect(snapshot.pending).toHaveLength(1);
    expect(snapshot.pending[0]?.state).toBe("validated");
  });

  it("executed state carries the result and leaves pending", () => {
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-1"));

    transport.emit({
      type: "proposal_state_changed",
      proposal_id: "prop-1",
      state: "executed",
      result: { removed: true },
    });

    const snapshot = session.getSnapshot();
    expect(snapshot.pending).toHaveLength(0);
    expect(snapshot.proposals[0]).toMatchObject({
      state: "executed",
      result: { removed: true },
    });
  });

  it("an unrecognized wire state fails closed as terminal unknown", () => {
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-1"));

    transport.emit({
      type: "proposal_state_changed",
      proposal_id: "prop-1",
      state: "half_executed_v2",
    });

    const snapshot = session.getSnapshot();
    expect(snapshot.proposals[0]?.state).toBe("unknown");
    expect(snapshot.pending).toHaveLength(0);
  });

  it("chat deltas accumulate and the final message is authoritative", () => {
    const { transport, session } = openSession();

    transport.emit({ type: "chat_message", msg_id: "msg-1", text: "Hel", done: false });
    transport.emit({ type: "chat_message", msg_id: "msg-1", text: "lo.", done: false });
    transport.emit({ type: "chat_message", msg_id: "msg-1", text: "Hello.", done: true });

    expect(session.getSnapshot().transcript).toEqual([
      { id: "msg-1", role: "assistant", text: "Hello." },
    ]);
  });

  it("confirm and reject send the matching client events", () => {
    const { transport, session } = openSession();

    session.confirm("prop-1");
    session.reject("prop-2", "changed my mind");

    expect(transport.sent.slice(1)).toEqual([
      { type: "confirm_action", proposal_id: "prop-1" },
      { type: "reject_action", proposal_id: "prop-2", reason: "changed my mind" },
    ]);
  });

  it("stream errors land in lastError", () => {
    const { transport, session } = openSession();

    transport.emit({
      type: "error",
      code: "llm_error",
      message: "provider down",
      recoverable: true,
    });

    expect(session.getSnapshot().lastError).toEqual({
      code: "llm_error",
      message: "provider down",
      recoverable: true,
    });
  });
});
