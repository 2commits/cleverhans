import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ServerEvent } from "../src/envelope";

import { AgentProvider } from "../src/context";
import { Proposal } from "../src/primitives";
import { PendingProposals, type BlockProps } from "../src/router";
import { AgentSession } from "../src/session";
import { FakeTransport, proposalEvent } from "./fake-transport";

function ConfirmBlock(props: BlockProps) {
  return (
    <Proposal.Root {...props}>
      <Proposal.Title />
      <Proposal.Preview />
      <Proposal.Confirm />
      <Proposal.Reject />
    </Proposal.Root>
  );
}

function mount(transport: FakeTransport, session: AgentSession) {
  return render(
    <AgentProvider session={session}>
      <PendingProposals components={{ confirm: ConfirmBlock }} />
    </AgentProvider>,
  );
}

function openSession() {
  const transport = new FakeTransport();
  const session = new AgentSession(transport, { context: { route: "/" } });
  const emit = (event: ServerEvent) => {
    act(() => transport.emit(event));
  };
  return { transport, session, emit };
}

describe("PendingProposals + BlockRouter", () => {
  it("mounts the registered component with slots and preview", () => {
    const { transport, session, emit } = openSession();
    mount(transport, session);

    emit(proposalEvent("prop-1"));

    expect(screen.getByText("Remove co-buyer")).toBeDefined();
    expect(screen.getByText("Remove co-buyer Jane Doe")).toBeDefined();
  });

  it("renders nothing for an unregistered block type (fail closed)", () => {
    const { transport, session, emit } = openSession();
    const { container } = mount(transport, session);

    emit(proposalEvent("prop-1", "sneaky_block"));

    expect(container.innerHTML).toBe("");
  });

  it("confirm click sends confirm_action for the right proposal", () => {
    const { transport, session, emit } = openSession();
    mount(transport, session);
    emit(proposalEvent("prop-1"));

    fireEvent.click(screen.getByText("Confirm"));

    expect(transport.sent.at(-1)).toEqual({
      type: "confirm_action",
      proposal_id: "prop-1",
    });
  });

  it("an executed proposal unmounts from the pending list", () => {
    const { transport, session, emit } = openSession();
    const { container } = mount(transport, session);
    emit(proposalEvent("prop-1"));

    emit({ type: "proposal_state_changed", proposal_id: "prop-1", state: "executed" });

    expect(container.innerHTML).toBe("");
  });
});
