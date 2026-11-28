/**
 * WebSocket + JSON binding of the envelope (spec §11 welcomes non-gRPC
 * bindings; browsers cannot open native gRPC bidirectional streams). The
 * JSON shapes are exactly the serde encoding of `cleverhans-core`'s envelope.
 *
 * Authentication is the app's concern (cookie, a token offered as an extra
 * subprotocol, ticket in the URL) — the envelope itself never carries
 * credentials (spec §10). A browser cannot set headers on a WebSocket, so a
 * token often rides in `protocols`: the transport then offers
 * {@link SUBPROTOCOL} first and your values after it, the server selects
 * `cleverhans.v0.1` (the Rust `cleverhans-ws` adapter does), and your
 * principal extractor reads the token from the upgrade request's
 * `Sec-WebSocket-Protocol` header. Without `protocols` the transport offers
 * no subprotocol at all, because a browser fails the connection when it
 * offers one and the server selects none.
 *
 * The server ends a session after an `error` with `recoverable: false`
 * (`init_required`, `unsupported_spec_version`): it sends that error, then a
 * close frame with code 1008 and the error's `code` as the reason.
 */

import type { AgentTransport, ClientEvent, ServerEvent } from "./envelope";

/**
 * The WebSocket subprotocol name of the binding (the wire version). Offered
 * first whenever the caller passes `protocols`; a server selects it.
 */
export const SUBPROTOCOL = "cleverhans.v0.1";

/** Options for {@link createWebSocketTransport}. */
export interface WebSocketTransportOptions {
  /**
   * Extra WebSocket subprotocols to offer, e.g. a bearer token for the
   * server's principal extractor. When set, the transport offers
   * {@link SUBPROTOCOL} first and these values after it; when unset (or
   * empty) it offers none.
   */
  protocols?: string | string[];
  /** Injectable constructor for tests / non-browser runtimes. */
  webSocketFactory?: (url: string, protocols?: string | string[]) => WebSocket;
  /** Called for messages that fail to parse and socket-level errors. */
  onTransportError?: (error: unknown) => void;
}

/**
 * The subprotocols the transport offers: none without caller values (unset
 * or an empty list), otherwise the binding's name first, then the caller's
 * values in order.
 */
function offeredProtocols(protocols: string | string[] | undefined): string[] | undefined {
  const values = protocols === undefined ? [] : typeof protocols === "string" ? [protocols] : protocols;
  if (values.length === 0) {
    return undefined;
  }
  return [SUBPROTOCOL, ...values.filter((protocol) => protocol !== SUBPROTOCOL)];
}

/** An {@link AgentTransport} that can be shut down. */
export interface ClosableTransport extends AgentTransport {
  close(): void;
}

/**
 * Opens a WebSocket carrying one envelope session. Client events sent before
 * the socket opens are queued and flushed on open, so `AgentSession`'s
 * init-first guarantee survives the handshake.
 *
 * Under SSR (no `WebSocket` global and no injected factory) it returns an
 * inert transport instead of throwing, so a session constructed during a
 * server render is safe — the client render constructs the live one.
 */
export function createWebSocketTransport(
  url: string,
  options: WebSocketTransportOptions = {},
): ClosableTransport {
  if (options.webSocketFactory === undefined && typeof WebSocket === "undefined") {
    return {
      send() {},
      subscribe() {
        return () => {};
      },
      close() {},
    };
  }
  const factory =
    options.webSocketFactory ?? ((u, p) => new WebSocket(u, p));
  const socket = factory(url, offeredProtocols(options.protocols));
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
