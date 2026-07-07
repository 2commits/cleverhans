/**
 * WebSocket + JSON binding of the envelope (spec §11 welcomes non-gRPC
 * bindings; browsers cannot open native gRPC bidirectional streams). The
 * JSON shapes are exactly the serde encoding of `cleverhans-core`'s envelope.
 *
 * Authentication is the app's concern (cookie, subprotocol token, ticket in
 * the URL) — the envelope itself never carries credentials (spec §10).
 */

import type { AgentTransport, ClientEvent, ServerEvent } from "./envelope";

/** Options for {@link createWebSocketTransport}. */
export interface WebSocketTransportOptions {
  /** WebSocket subprotocols, e.g. a bearer-token subprotocol. */
  protocols?: string | string[];
  /** Injectable constructor for tests / non-browser runtimes. */
  webSocketFactory?: (url: string, protocols?: string | string[]) => WebSocket;
  /** Called for messages that fail to parse and socket-level errors. */
  onTransportError?: (error: unknown) => void;
}

/** An {@link AgentTransport} that can be shut down. */
export interface ClosableTransport extends AgentTransport {
  close(): void;
}

/**
 * Opens a WebSocket carrying one envelope session. Client events sent before
 * the socket opens are queued and flushed on open, so `AgentSession`'s
 * init-first guarantee survives the handshake.
 */
export function createWebSocketTransport(
  url: string,
  options: WebSocketTransportOptions = {},
): ClosableTransport {
  const factory =
    options.webSocketFactory ?? ((u, p) => new WebSocket(u, p));
  const socket = factory(url, options.protocols);
  const handlers = new Set<(event: ServerEvent) => void>();
  const queue: ClientEvent[] = [];
  let open = false;

  socket.addEventListener("open", () => {
    open = true;
    for (const event of queue.splice(0)) {
      socket.send(JSON.stringify(event));
    }
  });
  socket.addEventListener("message", (message: MessageEvent) => {
    let event: ServerEvent;
    try {
      event = JSON.parse(String(message.data)) as ServerEvent;
    } catch (error) {
      options.onTransportError?.(error);
      return;
    }
    for (const handler of handlers) {
      handler(event);
    }
  });
  socket.addEventListener("error", (error) => {
    options.onTransportError?.(error);
  });

  return {
    send(event) {
      if (open) {
        socket.send(JSON.stringify(event));
      } else {
        queue.push(event);
      }
    },
    subscribe(onEvent) {
      handlers.add(onEvent);
      return () => handlers.delete(onEvent);
    },
    close() {
      socket.close();
    },
  };
}
