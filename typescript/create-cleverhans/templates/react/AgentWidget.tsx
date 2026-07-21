// CleverHans chat widget — scaffolded starter. Client component (the
// packages ship "use client"; the transport is inert under SSR).

import { useEffect, useMemo } from "react";

import { AgentSession, createWebSocketTransport } from "@cleverhans/react";
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";

export interface AgentWidgetProps {
  /** Your backend's agent mount, e.g. wss://your-app/agent. Auth rides the
   * transport (cookie / subprotocol token) — never the envelope. */
  url: string;
  /** Where the user is standing right now; re-render with new values on
   * navigation and the agent follows. */
  route: string;
  selectedRecordId: string | null;
}

export function AgentWidget({ url, route, selectedRecordId }: AgentWidgetProps) {
  const session = useMemo(
    () =>
      new AgentSession(createWebSocketTransport(url), {
        context: { route, selected_record_id: selectedRecordId, view_type: null },
      }),
    // The session owns the socket; create it once per URL.
    [url],
  );

  useEffect(() => {
    session.updateContext({ route, selected_record_id: selectedRecordId, view_type: null });
  }, [session, route, selectedRecordId]);

  return <FloatingChat session={session} />;
}
