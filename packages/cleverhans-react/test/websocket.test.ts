import { describe, expect, it } from "vitest";

import type { ServerEvent } from "../src/envelope";
import { createWebSocketTransport } from "../src/websocket";

type Listener = (event: { data?: unknown }) => void;

class FakeWebSocket {
  sent: string[] = [];
  closed = false;
  #listeners = new Map<string, Set<Listener>>();

  addEventListener(type: string, listener: Listener): void {
    const set = this.#listeners.get(type) ?? new Set();
    set.add(listener);
    this.#listeners.set(type, set);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  dispatch(type: string, event: { data?: unknown } = {}): void {
    for (const listener of this.#listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

function openTransport() {
  const socket = new FakeWebSocket();
  const transport = createWebSocketTransport("wss://example/agent", {
    webSocketFactory: () => socket as unknown as WebSocket,
  });
  return { socket, transport };
}

describe("createWebSocketTransport", () => {
  it("queues events until the socket opens, then flushes in order", () => {
    const { socket, transport } = openTransport();

    transport.send({ type: "init", spec_version: "0.1.0-draft", context: { route: "/" } });
    transport.send({ type: "user_message", text: "hi", client_msg_id: "c-1" });
    expect(socket.sent).toHaveLength(0);

    socket.dispatch("open");

    expect(socket.sent.map((raw) => (JSON.parse(raw) as { type: string }).type)).toEqual([
      "init",
      "user_message",
    ]);
  });

  it("parses incoming JSON into server events", () => {
    const { socket, transport } = openTransport();
    const received: ServerEvent[] = [];
    transport.subscribe((event) => received.push(event));

    socket.dispatch("message", {
      data: JSON.stringify({ type: "chat_message", msg_id: "m1", text: "hello", done: true }),
    });

    expect(received).toEqual([{ type: "chat_message", msg_id: "m1", text: "hello", done: true }]);
  });

  it("reports malformed frames instead of throwing", () => {
    const { socket, transport } = openTransport();
    const errors: unknown[] = [];
    const withHook = createWebSocketTransport("wss://example/agent", {
      webSocketFactory: () => socket as unknown as WebSocket,
      onTransportError: (error) => errors.push(error),
    });
    withHook.subscribe(() => {
      throw new Error("must not receive");
    });
    void transport;

    socket.dispatch("message", { data: "not json" });

    expect(errors).toHaveLength(1);
  });

  it("close() closes the socket", () => {
    const { socket, transport } = openTransport();

    transport.close();

    expect(socket.closed).toBe(true);
  });
});
