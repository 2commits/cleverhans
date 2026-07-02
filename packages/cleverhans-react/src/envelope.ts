/**
 * The transport-agnostic envelope (spec §6), JSON encoding — mirrors the
 * serde-tagged shapes emitted by `cleverhans-core`. These types define
 * message shapes only, never actions: `action_id` is a plain string and
 * params/slots are generic maps, so the registry evolves without touching
 * this file. Registry-specific typing comes from cleverhans-codegen output.
 */

/** Spec version this library implements (spec §13). */
export const SPEC_VERSION = "0.1.0-draft";

/** App-defined context snapshot, app → agent (spec §6.2). */
export interface AppContext {
  route: string;
  params?: Record<string, unknown>;
  selected_record_id?: string | null;
  view_type?: string | null;
  extensions?: Record<string, unknown>;
}

/** Messages from the app frontend to the agent (spec §6.2). */
export type ClientEvent =
  | { type: "init"; spec_version: string; context: AppContext }
  | { type: "context_update"; context: AppContext; context_seq: number }
  | { type: "user_message"; text: string; client_msg_id: string }
  | { type: "confirm_action"; proposal_id: string }
  | { type: "reject_action"; proposal_id: string; reason?: string | null };

/** Permission-correct preview of a mutating action (spec §6.4). */
export interface DryRunPreview {
  affected_count: number;
  sample_ids?: string[];
  summary?: string | null;
  extensions?: Record<string, unknown>;
}

/** A validated proposal, ready to render (spec §6.4). */
export interface ActionProposal {
  proposal_id: string;
  action_id: string;
  params: Record<string, unknown>;
  block_type: string;
  slots: Record<string, unknown>;
  preview?: DryRunPreview | null;
  context_seq: number;
  turn_msg_id?: string | null;
}

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

/** Messages from the agent to the app frontend (spec §6.3). */
export type ServerEvent =
  | { type: "chat_message"; msg_id: string; text: string; done: boolean }
  | ({ type: "action_proposal" } & ActionProposal)
  | {
      type: "proposal_state_changed";
      proposal_id: string;
      state: string;
      reason?: string | null;
      result?: unknown;
    }
  | { type: "error"; code: string; message: string; recoverable: boolean };

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
