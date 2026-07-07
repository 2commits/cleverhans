import { afterEach, describe, expect, it, vi } from "vitest";

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
  afterEach(() => {
    vi.useRealTimers();
  });

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

  it("confirm marks the proposal working until the state change lands", () => {
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-1"));

    session.confirm("prop-1");
    expect(session.getSnapshot().proposals[0]).toMatchObject({
      state: "validated",
      working: true,
    });

    transport.emit({ type: "proposal_state_changed", proposal_id: "prop-1", state: "executed" });
    expect(session.getSnapshot().proposals[0]).toMatchObject({
      state: "executed",
      working: false,
    });
  });

  it("a working proposal reverts to actionable when the server never answers", () => {
    vi.useFakeTimers();
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-1"));
    session.confirm("prop-1");
    expect(session.getSnapshot().proposals[0]?.working).toBe(true);

    vi.advanceTimersByTime(10_000);

    expect(session.getSnapshot().proposals[0]).toMatchObject({
      state: "validated",
      working: false,
    });
  });

  it("a server answer within the window cancels the working timeout", () => {
    vi.useFakeTimers();
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-1"));
    session.confirm("prop-1");
    transport.emit({ type: "proposal_state_changed", proposal_id: "prop-1", state: "executed" });

    vi.advanceTimersByTime(60_000);

    expect(session.getSnapshot().proposals[0]).toMatchObject({
      state: "executed",
      working: false,
    });
  });

  it("proposals anchor to the user turn they answered", () => {
    const { transport, session } = openSession();
    transport.emit(proposalEvent("prop-0"));
    session.sendMessage("first");
    transport.emit(proposalEvent("prop-1"));
    session.sendMessage("second");
    transport.emit(proposalEvent("prop-2"));

    const [before, first, second] = session.getSnapshot().proposals;
    expect(before?.turnId).toBeNull();
    expect(first?.turnId).toBe("c-1");
    expect(second?.turnId).toBe("c-2");
  });

  it("busy walks thinking → streaming → idle across a turn", () => {
    const { transport, session } = openSession();
    expect(session.getSnapshot().busy).toBe("idle");

    session.sendMessage("rename this");
    expect(session.getSnapshot().busy).toBe("thinking");

    transport.emit({ type: "chat_message", msg_id: "m-1", text: "Sure", done: false });
    expect(session.getSnapshot().busy).toBe("streaming");

    transport.emit({ type: "chat_message", msg_id: "m-1", text: "Sure thing.", done: true });
    expect(session.getSnapshot().busy).toBe("idle");
  });

  it("a proposal arriving without chat text also clears thinking", () => {
    const { transport, session } = openSession();
    session.sendMessage("rename this");

    transport.emit(proposalEvent("prop-1"));

    expect(session.getSnapshot().busy).toBe("idle");
  });

  it("a stream error aborts any in-flight assistant message", () => {
    const { transport, session } = openSession();
    session.sendMessage("rename this");
    transport.emit({ type: "chat_message", msg_id: "m-1", text: "Sur", done: false });

    transport.emit({ type: "error", code: "llm_error", message: "down", recoverable: true });

    expect(session.getSnapshot().busy).toBe("idle");
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
