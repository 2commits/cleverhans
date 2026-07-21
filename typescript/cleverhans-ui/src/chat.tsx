/**
 * The batteries-included chat window: a continuous flow, not bubbles. Each
 * turn renders as a section — the ask as an eyebrow header, the answer as
 * page-like text, and every proposal that turn produced inline through its
 * full lifecycle. Receipts never leave the flow, so a session of many
 * actions reads as a complete audit trail top to bottom. Wire it to a
 * backend with any `AgentTransport` (e.g. `createWebSocketTransport`) and a
 * session:
 *
 * ```tsx
 * const transport = createWebSocketTransport("wss://app.example/agent");
 * const session = new AgentSession(transport, { context: currentContext() });
 * <AgentChat session={session} />
 * ```
 */

import {
  AgentProvider,
  BlockRouter,
  useAgentSession,
  type AgentSession,
  type BlockComponents,
  type ChatEntry,
} from "@cleverhans/react";
import { useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";

import { DEFAULT_BLOCKS } from "./blocks";
import { HorseIcon } from "./icon";

/** Props for {@link AgentChat}. */
export interface AgentChatProps {
  /**
   * The session to drive. Optional when an `<AgentProvider>` already wraps
   * this component.
   */
  session?: AgentSession;
  /**
   * Block components, merged over the styled defaults (`confirm`,
   * `bulk_preview`). Keys are registered block types; unregistered types
   * still render nothing (fail closed).
   */
  components?: BlockComponents;
  placeholder?: string;
  className?: string;
}

/** One conversational turn: the user's ask and every reply it produced. */
interface Exchange {
  id: string;
  /** `null` when the agent spoke unprompted (greetings, follow-ups). */
  ask: string | null;
  replies: ChatEntry[];
}

function groupExchanges(transcript: readonly ChatEntry[]): Exchange[] {
  const exchanges: Exchange[] = [];
  for (const entry of transcript) {
    if (entry.role === "user") {
      exchanges.push({ id: entry.id, ask: entry.text, replies: [] });
      continue;
    }
    const last = exchanges[exchanges.length - 1];
    if (last) {
      last.replies.push(entry);
    } else {
      exchanges.push({ id: entry.id, ask: null, replies: [entry] });
    }
  }
  return exchanges;
}

function ThinkingDots(): ReactNode {
  return (
    <span className="ch-dots" aria-label="Thinking">
      <span />
      <span />
      <span />
    </span>
  );
}

function Composer(props: { placeholder: string }): ReactNode {
  const { sendMessage } = useAgentSession();
  const [draft, setDraft] = useState("");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if (text.length === 0) {
      return;
    }
    sendMessage(text);
    setDraft("");
  };

  return (
    <form className="ch-composer" onSubmit={submit}>
      <input
        className="ch-composer-input"
        value={draft}
        placeholder={props.placeholder}
        aria-label="Message"
        onChange={(event) => setDraft(event.target.value)}
      />
      <button className="ch-btn ch-btn--confirm" type="submit">
        Send
      </button>
    </form>
  );
}

/**
 * Every proposal a turn produced, rendered through its full lifecycle —
 * pending cards, the working indicator, and terminal receipts all stay
 * inline with the turn, forming the audit trail.
 */
function TurnProposals(props: {
  turnId: string | null;
  components: BlockComponents;
}): ReactNode {
  const { snapshot } = useAgentSession();
  const views = snapshot.proposals.filter((view) => view.turnId === props.turnId);
  return (
    <>
      {views.map((view) => (
        <BlockRouter
          key={view.proposal.proposal_id}
          proposalId={view.proposal.proposal_id}
          components={props.components}
        />
      ))}
    </>
  );
}

function ChatInner(props: {
  components: BlockComponents;
  placeholder: string;
  className?: string | undefined;
}): ReactNode {
  const { snapshot } = useAgentSession();
  const scrollRef = useRef<HTMLDivElement>(null);
  // "actions" strips the prose and shows only proposal cards under their
  // originating asks — the audit trail at a glance.
  const [filter, setFilter] = useState<"all" | "actions">("all");

  const exchanges = groupExchanges(snapshot.transcript);
  const empty = exchanges.length === 0 && snapshot.proposals.length === 0;
  const orphanProposals = snapshot.proposals.some((view) => view.turnId === null);
  const actionsOnly = filter === "actions" && snapshot.proposals.length > 0;
  const turnsWithProposals = new Set(
    snapshot.proposals.map((view) => view.turnId).filter((id) => id !== null),
  );

  // Follow the conversation: new turns, deltas, and lifecycle changes keep
  // the flow pinned to the latest activity.
  useEffect(() => {
    const scroll = scrollRef.current;
    if (scroll) {
      scroll.scrollTop = scroll.scrollHeight;
    }
  }, [snapshot]);

  const lastEntry = snapshot.transcript[snapshot.transcript.length - 1];
  const streamingId =
    snapshot.busy === "streaming" && lastEntry?.role === "assistant" ? lastEntry.id : null;

  return (
    <div className={props.className ?? "ch-chat"} data-cleverhans-chat>
      {snapshot.proposals.length > 0 && (
        <div className="ch-filter">
          <button
            type="button"
            className="ch-filter-btn"
            aria-pressed={!actionsOnly}
            onClick={() => setFilter("all")}
          >
            All
          </button>
          <button
            type="button"
            className="ch-filter-btn"
            aria-pressed={actionsOnly}
            onClick={() => setFilter("actions")}
          >
            Actions ({snapshot.proposals.length})
          </button>
        </div>
      )}
      <div className="ch-stage-scroll" ref={scrollRef} role="log" aria-live="polite">
        {empty ? (
          <div className="ch-stage ch-stage--empty">
            <HorseIcon size={40} />
            <p>Ask about this page — I’ll propose, you decide.</p>
          </div>
        ) : (
          <div className="ch-flow">
            {orphanProposals && (
              <section className="ch-flow-turn">
                <TurnProposals turnId={null} components={props.components} />
              </section>
            )}
            {exchanges.map((exchange, index) => {
              if (actionsOnly && !turnsWithProposals.has(exchange.id)) {
                return null;
              }
              return (
                <section className="ch-flow-turn" key={exchange.id}>
                  {exchange.ask !== null && (
                    <div className="ch-stage-ask">
                      <span className="ch-stage-ask-label">You</span>
                      {exchange.ask}
                    </div>
                  )}
                  {!actionsOnly &&
                    exchange.replies.map((reply) => (
                      <p key={reply.id} className="ch-stage-text">
                        {reply.text}
                        {reply.id === streamingId && (
                          <span className="ch-caret" aria-hidden="true" />
                        )}
                      </p>
                    ))}
                  {!actionsOnly &&
                    index === exchanges.length - 1 &&
                    snapshot.busy === "thinking" && <ThinkingDots />}
                  <TurnProposals turnId={exchange.id} components={props.components} />
                </section>
              );
            })}
          </div>
        )}
      </div>
      {snapshot.lastError && (
        <div className="ch-error" role="alert">
          {snapshot.lastError.message}
        </div>
      )}
      <Composer placeholder={props.placeholder} />
    </div>
  );
}

/**
 * Drop-in chat window. Everything rendered inside stays within the
 * propose-only contract: proposals arrive as blocks, the confirm button is
 * the only path to execution, and it runs server-side under the user's own
 * credentials.
 */
export function AgentChat(props: AgentChatProps): ReactNode {
  const inner = (
    <ChatInner
      components={{ ...DEFAULT_BLOCKS, ...props.components }}
      placeholder={props.placeholder ?? "Ask the assistant…"}
      className={props.className}
    />
  );
  if (props.session) {
    return <AgentProvider session={props.session}>{inner}</AgentProvider>;
  }
  return inner;
}
