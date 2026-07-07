/**
 * Local playground for the FloatingChat widget against the demo server.
 * Start the backend first:
 *
 *   ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- serve
 *
 * then `pnpm --filter @cleverhans/playground dev`.
 *
 * A tiny two-view "app": a document list and a document detail view. The
 * agent context follows navigation (route + selected record), so the same
 * utterance targets whichever document you're standing on — and single-doc
 * actions fail validation from the list view, where nothing is selected.
 *
 * The doc list mirrors the demo server's seed and folds executed proposal
 * results back in, so renames/publishes/deletes show up on the page.
 * Restart the server to reset both sides.
 */

import { useEffect, useMemo, useState, useSyncExternalStore } from "react";

import {
  AgentSession,
  createWebSocketTransport,
  type ProposalView,
} from "@cleverhans/react";
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";
import "./app.css";

interface Doc {
  id: string;
  title: string;
  status: "draft" | "published" | "archived";
}

/** Mirror of the demo server's seeded store. */
const SEED: Doc[] = [
  { id: "doc-1", title: "Q3 Planning", status: "draft" },
  { id: "doc-2", title: "Launch Checklist", status: "draft" },
  { id: "doc-3", title: "Retro Notes", status: "published" },
];

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/**
 * Folds executed proposal results over the seed so the page reflects what
 * the agent actually did. Pure derivation — replaying the full proposal
 * list every render keeps it idempotent.
 */
function applyResults(seed: Doc[], proposals: readonly ProposalView[]): Doc[] {
  let docs = seed;
  for (const view of proposals) {
    if (view.state !== "executed" || typeof view.result !== "object" || view.result === null) {
      continue;
    }
    const result = view.result as Record<string, unknown>;
    switch (view.proposal.action_id) {
      case "document.rename": {
        const id = str(result["id"]);
        const title = str(result["title"]);
        if (id !== null && title !== null) {
          docs = docs.map((doc) => (doc.id === id ? { ...doc, title } : doc));
        }
        break;
      }
      case "document.publish":
      case "document.archive": {
        const id = str(result["id"]);
        const status = str(result["status"])?.toLowerCase();
        if (id !== null && (status === "published" || status === "archived")) {
          docs = docs.map((doc) => (doc.id === id ? { ...doc, status } : doc));
        }
        break;
      }
      case "documents.deleteByStatus": {
        const deleted = result["deleted"];
        if (Array.isArray(deleted)) {
          docs = docs.filter((doc) => !deleted.includes(doc.id));
        }
        break;
      }
    }
  }
  return docs;
}

function DocList(props: { docs: Doc[]; onOpen: (id: string) => void }) {
  return (
    <>
      <p className="pg-hint">
        No document selected — try “delete all drafts” (bulk actions work anywhere), or notice
        how “rename this” has nothing to target. Open a document to give the agent context.
      </p>
      <ul className="pg-list">
        {props.docs.map((doc) => (
          <li key={doc.id}>
            <button type="button" className="pg-card" onClick={() => props.onOpen(doc.id)}>
              <span className="pg-card-title">{doc.title}</span>
              <span className="pg-meta">
                <span className={`pg-status pg-status--${doc.status}`}>{doc.status}</span>
                <span className="pg-id">{doc.id}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
      {props.docs.length === 0 && <p className="pg-hint">All documents deleted. Restart the demo server to reseed.</p>}
    </>
  );
}

function DocDetail(props: { doc: Doc | undefined; id: string; onBack: () => void }) {
  const { doc } = props;
  return (
    <>
      <button type="button" className="pg-back" onClick={props.onBack}>
        ← All documents
      </button>
      {doc === undefined ? (
        <p className="pg-hint">
          Document <code>{props.id}</code> no longer exists (deleted?). Head back to the list.
        </p>
      ) : (
        <article className="pg-detail">
          <h2>{doc.title}</h2>
          <p className="pg-meta">
            <span className={`pg-status pg-status--${doc.status}`}>{doc.status}</span>
            <span className="pg-id">{doc.id}</span>
          </p>
          <p className="pg-hint">
            The agent sees this document as selected. Try “rename this to …”, “publish this”,
            “archive this”. Navigating away expires pending proposals — propose something, then
            hit back and watch the card expire.
          </p>
        </article>
      )}
    </>
  );
}

export default function App() {
  const session = useMemo(() => {
    const transport = createWebSocketTransport("ws://127.0.0.1:8787/agent");
    return new AgentSession(transport, {
      context: { route: "/documents", selected_record_id: null, view_type: "list" },
    });
  }, []);

  const snapshot = useSyncExternalStore(session.subscribe, session.getSnapshot);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const docs = applyResults(SEED, snapshot.proposals);
  const selected = docs.find((doc) => doc.id === selectedId);

  // Context follows navigation: the agent always knows where the user is.
  useEffect(() => {
    session.updateContext(
      selectedId === null
        ? { route: "/documents", selected_record_id: null, view_type: "list" }
        : { route: `/documents/${selectedId}`, selected_record_id: selectedId, view_type: "detail" },
    );
  }, [session, selectedId]);

  return (
    <main className="pg">
      <header className="pg-header">
        <h1>CleverHans playground</h1>
        <span className="pg-route">{selectedId === null ? "/documents" : `/documents/${selectedId}`}</span>
      </header>
      {selectedId === null ? (
        <DocList docs={docs} onOpen={setSelectedId} />
      ) : (
        <DocDetail doc={selected} id={selectedId} onBack={() => setSelectedId(null)} />
      )}
      <FloatingChat session={session} />
    </main>
  );
}
