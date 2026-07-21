/**
 * Framework-agnostic session state: one instance per envelope stream. Pure
 * stream/lifecycle logic — the escape hatch under `useAgentSession` (spec
 * §10 layer 1 lives on top of this).
 */

import {
  type ActionProposal,
  type AgentTransport,
  type AppContext,
  type ClientEvent,
  type ProposalLifecycleState,
  type ServerEvent,
  SPEC_VERSION,
} from "./envelope";

/** One transcript entry. */
export interface ChatEntry {
  id: string;
  role: "user" | "assistant";
  text: string;
}

/** A proposal as the frontend tracks it. */
export interface ProposalView {
  proposal: ActionProposal;
  /**
   * `"unknown"` = the wire carried a state this library doesn't recognize;
   * treated as terminal, rendered no further (spec §11).
   */
  state: ProposalLifecycleState | "unknown";
  /**
   * The user turn this proposal answered (transcript entry id), or `null`
   * when it arrived before any user turn. Lets UIs keep receipts with the
   * exchange that produced them.
   */
  turnId: string | null;
  /**
   * True between a sent confirm/reject and the server's answering state
   * change — the "agent is doing it" window.
   */
  working: boolean;
  reason?: string;
  result?: unknown;
}

/** Stream-level error surfaced by the agent. */
export interface SessionError {
  code: string;
  message: string;
  recoverable: boolean;
}

/**
 * What the agent is doing right now, from the client's point of view:
 * `"thinking"` between a sent user turn and the first server event,
 * `"streaming"` while an assistant message has unfinished deltas.
 */
export type SessionBusyState = "idle" | "thinking" | "streaming";

/** Immutable snapshot for `useSyncExternalStore`. */
export interface SessionSnapshot {
  transcript: readonly ChatEntry[];
  /** Every proposal seen this session, in arrival order. */
  proposals: readonly ProposalView[];
  /** Proposals awaiting a user decision (`validated`). */
  pending: readonly ProposalView[];
  busy: SessionBusyState;
  lastError: SessionError | null;
}

const KNOWN_STATES: ReadonlySet<string> = new Set([
  "validated",
  "confirmed",
  "executed",
  "rejected",
  "expired",
  "failed",
]);

const EMPTY: SessionSnapshot = {
  transcript: [],
  proposals: [],
  pending: [],
  busy: "idle",
  lastError: null,
};

/** Options for {@link AgentSession}. */
export interface AgentSessionOptions {
  /** Initial context snapshot, sent with `init`. */
  context: AppContext;
  /** Override the announced spec version (testing/negotiation). */
  specVersion?: string;
  /**
   * How long a proposal stays `working` without a server state change
   * before reverting to actionable (ms). Guards against a dropped confirm
   * leaving the card stuck "on it" forever. `null` disables the guard.
   * Default: 10 000.
   */
  workingTimeoutMs?: number | null;
}

const DEFAULT_WORKING_TIMEOUT_MS = 10_000;

/**
 * Drives one envelope stream: sends client events, folds server events into
 * an immutable {@link SessionSnapshot}. Framework-owned even in headless
 * setups; components stay presentational and never see the stream.
 */
export class AgentSession {
  #transport: AgentTransport;
  #unsubscribe: () => void;
  #listeners = new Set<() => void>();
  #snapshot: SessionSnapshot = EMPTY;
  #order: string[] = [];
  #views = new Map<string, ProposalView>();
  #transcript: ChatEntry[] = [];
  #lastError: SessionError | null = null;
  #contextSeq = 0;
  #clientMsgCounter = 0;
  /** True from a sent user turn until the next server event. */
  #awaitingReply = false;
  /** Assistant msg_ids with deltas seen but no `done: true` yet. */
  #openStreams = new Set<string>();
  #workingTimeoutMs: number | null;
  #workingTimers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(transport: AgentTransport, options: AgentSessionOptions) {
    this.#workingTimeoutMs = options.workingTimeoutMs ?? DEFAULT_WORKING_TIMEOUT_MS;
    this.#transport = transport;
    this.#unsubscribe = transport.subscribe((event) => {
      this.#onServerEvent(event);
    });
    this.#send({
      type: "init",
      spec_version: options.specVersion ?? SPEC_VERSION,
      context: options.context,
    });
  }

  /** Sends a user chat turn. */
  sendMessage(text: string): void {
    this.#clientMsgCounter += 1;
    const id = `c-${this.#clientMsgCounter}`;
    this.#transcript = [...this.#transcript, { id, role: "user", text }];
    this.#awaitingReply = true;
    this.#send({ type: "user_message", text, client_msg_id: id });
    this.#publish();
  }

  /** Replaces the context snapshot; the sequence number is managed here. */
  updateContext(context: AppContext): void {
    this.#contextSeq += 1;
    this.#send({ type: "context_update", context, context_seq: this.#contextSeq });
  }

  /** Confirms a proposal — the user is the executor (spec §7.3). */
  confirm(proposalId: string): void {
    this.#markWorking(proposalId);
    this.#send({ type: "confirm_action", proposal_id: proposalId });
    this.#publish();
  }

  /** Declines a proposal; the reason flows back to the model. */
  reject(proposalId: string, reason?: string): void {
    this.#markWorking(proposalId);
    this.#send({ type: "reject_action", proposal_id: proposalId, reason: reason ?? null });
    this.#publish();
  }

  #markWorking(proposalId: string): void {
    const view = this.#views.get(proposalId);
    if (!view || view.state !== "validated") {
      return;
    }
    this.#views.set(proposalId, { ...view, working: true });
    if (this.#workingTimeoutMs === null) {
      return;
    }
    this.#clearWorkingTimer(proposalId);
    this.#workingTimers.set(
      proposalId,
      setTimeout(() => {
        this.#workingTimers.delete(proposalId);
        const stuck = this.#views.get(proposalId);
        // No state change arrived: hand the decision back to the user.
        if (stuck?.working) {
          this.#views.set(proposalId, { ...stuck, working: false });
          this.#publish();
        }
      }, this.#workingTimeoutMs),
    );
  }

  #clearWorkingTimer(proposalId: string): void {
    const timer = this.#workingTimers.get(proposalId);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.#workingTimers.delete(proposalId);
    }
  }

  /** Store contract for `useSyncExternalStore`. */
  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  /** Store contract for `useSyncExternalStore`. */
  getSnapshot = (): SessionSnapshot => this.#snapshot;

  /** Stops listening to the transport. */
  dispose(): void {
    this.#unsubscribe();
    this.#listeners.clear();
    for (const timer of this.#workingTimers.values()) {
      clearTimeout(timer);
    }
    this.#workingTimers.clear();
  }

  #send(event: ClientEvent): void {
    this.#transport.send(event);
  }

  #onServerEvent(event: ServerEvent): void {
    // Any server event means the agent has started responding.
    this.#awaitingReply = false;
    switch (event.type) {
      case "chat_message": {
        if (event.done) {
          this.#openStreams.delete(event.msg_id);
        } else {
          this.#openStreams.add(event.msg_id);
        }
        // Streaming: `done: false` events carry fragments sharing a msg_id;
        // the closing `done: true` carries the authoritative full text and
        // replaces whatever accumulated (clients ignoring deltas stay
        // correct — the final message is complete on its own).
        const index = this.#transcript.findIndex((entry) => entry.id === event.msg_id);
        if (index === -1) {
          this.#transcript = [
            ...this.#transcript,
            { id: event.msg_id, role: "assistant", text: event.text },
          ];
        } else {
          const existing = this.#transcript[index] as ChatEntry;
          const text = event.done ? event.text : existing.text + event.text;
          const next = [...this.#transcript];
          next[index] = { ...existing, text };
          this.#transcript = next;
        }
        break;
      }
      case "action_proposal": {
        const { type: _tag, ...proposal } = event;
        if (!this.#views.has(proposal.proposal_id)) {
          this.#order.push(proposal.proposal_id);
        }
        // Anchor the proposal to the user turn it answers.
        const turnId =
          [...this.#transcript].reverse().find((entry) => entry.role === "user")?.id ?? null;
        this.#views.set(proposal.proposal_id, {
          proposal,
          state: "validated",
          turnId,
          working: false,
        });
        break;
      }
      case "proposal_state_changed": {
        this.#clearWorkingTimer(event.proposal_id);
        const view = this.#views.get(event.proposal_id);
        if (!view) {
          break;
        }
        // Unrecognized state: fail closed, terminal (spec §11).
        const state = KNOWN_STATES.has(event.state)
          ? (event.state as ProposalLifecycleState)
          : "unknown";
        const next: ProposalView = { ...view, state, working: false };
        if (event.reason != null) {
          next.reason = event.reason;
        }
        if (event.result !== undefined) {
          next.result = event.result;
        }
        this.#views.set(event.proposal_id, next);
        break;
      }
      case "error": {
        // A stream error aborts any in-flight assistant message.
        this.#openStreams.clear();
        this.#lastError = {
          code: event.code,
          message: event.message,
          recoverable: event.recoverable,
        };
        break;
      }
    }
    this.#publish();
  }

  #publish(): void {
    const proposals = this.#order
      .map((id) => this.#views.get(id))
      .filter((view): view is ProposalView => view !== undefined);
    this.#snapshot = {
      transcript: this.#transcript,
      proposals,
      pending: proposals.filter((view) => view.state === "validated"),
      busy:
        this.#openStreams.size > 0 ? "streaming" : this.#awaitingReply ? "thinking" : "idle",
      lastError: this.#lastError,
    };
    for (const listener of this.#listeners) {
      listener();
    }
  }
}
