/**
 * The transport-agnostic envelope (spec §6), JSON encoding. The message
 * shapes are generated from the envelope's JSON Schemas
 * (`spec/schemas/client-event.json`, `spec/schemas/server-event.json`) into
 * `./generated/envelope.ts`; to change a shape, change the schema and run
 * `pnpm codegen`. These types define message shapes only, never actions:
 * `action_id` is a plain string and params/slots are generic maps, so the
 * registry evolves without touching the envelope. Registry-specific typing
 * comes from cleverhans-codegen output.
 *
 * Only what a schema cannot express is written here: the spec version this
 * library speaks, the lifecycle states a frontend observes, and the
 * transport seam.
 */

import type { ClientEvent, Context, ServerEvent } from "./generated/envelope";

export type {
  ActionProposal,
  ChatMessage,
  ClientEvent,
  ConfirmAction,
  ContextUpdate,
  DryRunPreview,
  ErrorMessage,
  Init,
  ProposalStateChanged,
  RejectAction,
  ServerEvent,
  UserMessage,
} from "./generated/envelope";

/** Spec version this library implements (spec §13). */
export const SPEC_VERSION = "0.1.0-draft";

/**
 * App-defined context snapshot, app → agent (spec §6.2): the schema's
 * `Context`, named `AppContext` here so it does not read as React context.
 */
export type AppContext = Context;

/**
 * Lifecycle states a frontend can observe (spec §7); `proposed`/`invalid`
 * never leave the agent. The wire carries states as strings — an
 * unrecognized value MUST be treated as terminal (spec §11), which
 * `AgentSession` does by mapping it to `"unknown"`.
 */
export type ProposalLifecycleState =
  | "validated"
  | "confirmed"
  | "executed"
  | "rejected"
  | "expired"
  | "failed";

/**
 * What the app plugs in to carry the envelope: any authenticated
 * bidirectional stream (gRPC-web, WebSocket, SSE+POST…). The transport owns
 * authentication — the envelope never carries credentials (spec §10).
 */
export interface AgentTransport {
  send(event: ClientEvent): void;
  /** Returns an unsubscribe function. */
  subscribe(onEvent: (event: ServerEvent) => void): () => void;
}
