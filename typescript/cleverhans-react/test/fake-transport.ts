import type { AgentTransport, ClientEvent, ServerEvent } from "../src/envelope";

/** In-memory transport: records what the client sends, replays server events. */
export class FakeTransport implements AgentTransport {
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
    for (const handler of this.#handlers) {
      handler(event);
    }
  }
}

export function proposalEvent(id: string, blockType = "confirm"): ServerEvent {
  return {
    type: "action_proposal",
    proposal_id: id,
    action_id: "transaction.coBuyer.remove",
    params: { transactionId: "tx_581" },
    block_type: blockType,
    slots: { title: "Remove co-buyer" },
    preview: { affected_count: 1, summary: "Remove co-buyer Jane Doe" },
    context_seq: 0,
  };
}
