/**
 * Layer 1 (spec §10): React bindings over {@link AgentSession}. Pure
 * stream/lifecycle logic — no markup, no styling.
 */

import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";

import type { AppContext } from "./envelope";
import type { AgentSession, ProposalView, SessionSnapshot } from "./session";

const SessionContext = createContext<AgentSession | null>(null);

/** Mounts a session for the tree below. The app owns the session lifetime. */
export function AgentProvider(props: {
  session: AgentSession;
  children?: ReactNode;
}): ReactNode {
  return (
    <SessionContext.Provider value={props.session}>{props.children}</SessionContext.Provider>
  );
}

function useSession(): AgentSession {
  const session = useContext(SessionContext);
  if (session === null) {
    throw new Error("cleverhans: no <AgentProvider> above this component");
  }
  return session;
}

/** Everything `useAgentSession` returns. */
export interface AgentSessionHandle {
  snapshot: SessionSnapshot;
  sendMessage: (text: string) => void;
  updateContext: (context: AppContext) => void;
  confirm: (proposalId: string) => void;
  reject: (proposalId: string, reason?: string) => void;
}

/** Live session state plus the client-event actions. */
export function useAgentSession(): AgentSessionHandle {
  const session = useSession();
  const snapshot = useSyncExternalStore(session.subscribe, session.getSnapshot);
  return useMemo(
    () => ({
      snapshot,
      sendMessage: (text) => session.sendMessage(text),
      updateContext: (context) => session.updateContext(context),
      confirm: (proposalId) => session.confirm(proposalId),
      reject: (proposalId, reason) => session.reject(proposalId, reason),
    }),
    [session, snapshot],
  );
}

/** One proposal's view plus its confirm/reject actions. */
export interface AgentProposalHandle {
  view: ProposalView | undefined;
  confirm: () => void;
  reject: (reason?: string) => void;
}

/**
 * The spec §10 hook: lifecycle of a single proposal. Confirm/reject are the
 * only writes a frontend can perform on a proposal — naming is not firing.
 */
export function useAgentProposal(proposalId: string): AgentProposalHandle {
  const session = useSession();
  const snapshot = useSyncExternalStore(session.subscribe, session.getSnapshot);
  const view = snapshot.proposals.find((p) => p.proposal.proposal_id === proposalId);
  const confirm = useCallback(() => session.confirm(proposalId), [session, proposalId]);
  const reject = useCallback(
    (reason?: string) => session.reject(proposalId, reason),
    [session, proposalId],
  );
  return useMemo(() => ({ view, confirm, reject }), [view, confirm, reject]);
}
