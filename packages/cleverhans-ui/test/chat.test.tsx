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
