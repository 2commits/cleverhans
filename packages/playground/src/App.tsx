/**
 * Local playground for the FloatingChat widget against the demo server.
 * Start the backend first:
 *
 *   ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- serve
 *
 * then `pnpm --filter @cleverhans/playground dev`.
 */

import { useMemo } from "react";

import { AgentSession, createWebSocketTransport } from "@cleverhans/react";
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";

export default function App() {
  const session = useMemo(() => {
    const transport = createWebSocketTransport("ws://127.0.0.1:8787/agent");
    return new AgentSession(transport, {
      context: {
        route: "/documents/doc-1",
        selected_record_id: "doc-1",
        view_type: "detail",
      },
    });
  }, []);

  return (
    <main style={{ height: "100vh", padding: 24, fontFamily: "system-ui" }}>
      <h1>CleverHans playground</h1>
      <p>
        Pretend app. Context: viewing <strong>doc-1 “Q3 Planning”</strong>. Try
        “rename this to Q3 Roadmap”, “publish this”, “delete all drafts”. Drag
        the bubble anywhere; click it to open.
      </p>
      <FloatingChat session={session} />
    </main>
  );
}
