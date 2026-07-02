/**
 * The batteries-included chat window: transcript, inline proposal blocks,
 * composer. Wire it to a backend with any `AgentTransport` (e.g.
 * `createWebSocketTransport`) and a session:
 *
 * ```tsx
 * const transport = createWebSocketTransport("wss://app.example/agent");
 * const session = new AgentSession(transport, { context: currentContext() });
 * <AgentChat session={session} />
 * ```
 */

import {
  AgentProvider,
  PendingProposals,
  useAgentSession,
  type AgentSession,
  type BlockComponents,
} from "@cleverhans/react";
import { useState } from "react";
import type { FormEvent, ReactNode } from "react";

import { DEFAULT_BLOCKS } from "./blocks";

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

function ChatInner(props: {
  components: BlockComponents;
  placeholder: string;
  className?: string | undefined;
}): ReactNode {
  const { snapshot } = useAgentSession();

  return (
    <div className={props.className ?? "ch-chat"} data-cleverhans-chat>
      <div className="ch-chat-log" role="log" aria-live="polite">
        {snapshot.transcript.map((entry) => (
          <div key={entry.id} className={`ch-msg ch-msg--${entry.role}`}>
            {entry.text}
          </div>
        ))}
        <PendingProposals components={props.components} />
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
