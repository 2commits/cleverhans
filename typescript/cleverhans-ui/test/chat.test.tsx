import {
  AgentSession,
  type AgentTransport,
  type ClientEvent,
  type ServerEvent,
} from "@cleverhans/react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AgentChat } from "../src";

class FakeTransport implements AgentTransport {
  sent: ClientEvent[] = [];
  #handlers = new Set<(event: ServerEvent) => void>();

  send(event: ClientEvent): void {
    this.sent.push(event);
  }

  subscribe(onEvent: (event: ServerEvent) => void): () => void {
    this.#handlers.add(onEvent);
    return () => this.#handlers.delete(onEvent);
  }

  emit(event: ServerEvent): void {
    act(() => {
      for (const handler of this.#handlers) {
        handler(event);
      }
    });
  }
}

function mountChat() {
  const transport = new FakeTransport();
  const session = new AgentSession(transport, { context: { route: "/" } });
  render(<AgentChat session={session} />);
  return transport;
}

describe("AgentChat", () => {
  it("sends the composer text as a user_message and echoes it in the log", () => {
    const transport = mountChat();

    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "remove the co-buyer" },
    });
    fireEvent.click(screen.getByText("Send"));

    expect(transport.sent.at(-1)).toMatchObject({
      type: "user_message",
      text: "remove the co-buyer",
    });
    expect(screen.getByText("remove the co-buyer")).toBeDefined();
  });

  it("renders assistant chat messages", () => {
    const transport = mountChat();

    transport.emit({ type: "chat_message", msg_id: "msg-1", text: "Done looking.", done: true });

    expect(screen.getByText("Done looking.")).toBeDefined();
  });

  it("renders a confirm proposal with the styled default block and confirms it", () => {
    const transport = mountChat();

    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "transaction.coBuyer.remove",
      params: {},
      block_type: "confirm",
      slots: { title: "Remove co-buyer", detail: "Jane Doe · TX-581" },
      preview: { affected_count: 1, summary: "Remove co-buyer Jane Doe from TX-581" },
      context_seq: 0,
    });
    fireEvent.click(screen.getByText("Confirm"));

    expect(screen.getByText("Remove co-buyer")).toBeDefined();
    expect(screen.getByText("Jane Doe · TX-581")).toBeDefined();
    expect(transport.sent.at(-1)).toEqual({
      type: "confirm_action",
      proposal_id: "prop-1",
    });
  });

  it("renders bulk previews with the sample record ids", () => {
    const transport = mountChat();

    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-2",
      action_id: "contacts.deleteByPredicate",
      params: {},
      block_type: "bulk_preview",
      slots: { title: "Delete stale contacts" },
      preview: { affected_count: 3, sample_ids: ["c_1", "c_2", "c_3"] },
      context_seq: 0,
    });

    expect(screen.getByText("3")).toBeDefined();
    expect(screen.getByText("c_2")).toBeDefined();
  });

  it("the full conversation stays visible as a flow", () => {
    const transport = mountChat();

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "first ask" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({ type: "chat_message", msg_id: "m-1", text: "first answer", done: true });

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "second ask" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({ type: "chat_message", msg_id: "m-2", text: "second answer", done: true });

    expect(screen.getByText("first ask")).toBeDefined();
    expect(screen.getByText("first answer")).toBeDefined();
    expect(screen.getByText("second ask")).toBeDefined();
    expect(screen.getByText("second answer")).toBeDefined();
  });

  it("a confirmed action shows the working state, then stays as a receipt", () => {
    const transport = mountChat();
    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });

    fireEvent.click(screen.getByText("Confirm"));

    // The agent is executing: buttons gone, working indicator on.
    expect(screen.getByText("On it")).toBeDefined();
    expect(screen.queryByText("Confirm")).toBeNull();

    transport.emit({ type: "proposal_state_changed", proposal_id: "prop-1", state: "executed" });

    // The card stays on stage as a receipt.
    expect(screen.getByText("Rename document")).toBeDefined();
    expect(screen.getByText("✓ Done")).toBeDefined();
    expect(screen.queryByText("On it")).toBeNull();
  });

  it("a failed execution surfaces its reason on the receipt", () => {
    const transport = mountChat();
    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });
    fireEvent.click(screen.getByText("Confirm"));

    transport.emit({
      type: "proposal_state_changed",
      proposal_id: "prop-1",
      state: "failed",
      reason: "record locked",
    });

    expect(screen.getByText("✗ Failed — record locked")).toBeDefined();
  });

  it("multiple actions leave a complete audit trail in order", () => {
    const transport = mountChat();
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "rename it" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });
    transport.emit({ type: "proposal_state_changed", proposal_id: "prop-1", state: "executed" });

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "publish it" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-2",
      action_id: "document.publish",
      params: {},
      block_type: "confirm",
      slots: { title: "Publish document" },
      context_seq: 0,
    });
    transport.emit({
      type: "proposal_state_changed",
      proposal_id: "prop-2",
      state: "failed",
      reason: "owner approval missing",
    });

    // Both turns and both receipts visible at once — the audit trail.
    expect(screen.getByText("Rename document")).toBeDefined();
    expect(screen.getByText("✓ Done")).toBeDefined();
    expect(screen.getByText("Publish document")).toBeDefined();
    expect(screen.getByText("✗ Failed — owner approval missing")).toBeDefined();

    // Receipts sit with the turn that produced them, in conversation order.
    const titles = screen
      .getAllByText(/Rename document|Publish document/)
      .map((node) => node.textContent);
    expect(titles).toEqual(["Rename document", "Publish document"]);
  });

  it("a proposal arriving before any chat still reaches the stage", () => {
    const transport = mountChat();

    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-0",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });

    expect(screen.getByText("Rename document")).toBeDefined();
  });

  it("the actions filter strips prose and keeps receipts under their asks", () => {
    const transport = mountChat();
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "rename it" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({ type: "chat_message", msg_id: "m-1", text: "prose reply", done: true });
    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "just chatting" } });
    fireEvent.click(screen.getByText("Send"));
    transport.emit({ type: "chat_message", msg_id: "m-2", text: "chatty reply", done: true });

    fireEvent.click(screen.getByText("Actions (1)"));

    // Card + its ask stay; prose and action-less turns go.
    expect(screen.getByText("Rename document")).toBeDefined();
    expect(screen.getByText("rename it")).toBeDefined();
    expect(screen.queryByText("prose reply")).toBeNull();
    expect(screen.queryByText("just chatting")).toBeNull();

    fireEvent.click(screen.getByText("All"));
    expect(screen.getByText("prose reply")).toBeDefined();
  });

  it("the filter toggle only appears once a proposal exists", () => {
    const transport = mountChat();
    expect(screen.queryByText("All")).toBeNull();

    transport.emit({
      type: "action_proposal",
      proposal_id: "prop-1",
      action_id: "document.rename",
      params: {},
      block_type: "confirm",
      slots: { title: "Rename document" },
      context_seq: 0,
    });

    expect(screen.getByText("All")).toBeDefined();
    expect(screen.getByText("Actions (1)")).toBeDefined();
  });

  it("shows stream errors as an alert", () => {
    const transport = mountChat();

    transport.emit({
      type: "error",
      code: "llm_error",
      message: "provider unavailable",
      recoverable: true,
    });

    expect(screen.getByRole("alert").textContent).toBe("provider unavailable");
  });
});
