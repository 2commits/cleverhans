export {
  SPEC_VERSION,
  type ActionProposal,
  type AgentTransport,
  type AppContext,
  type ChatMessage,
  type ClientEvent,
  type ConfirmAction,
  type ContextUpdate,
  type DryRunPreview,
  type ErrorMessage,
  type Init,
  type ProposalLifecycleState,
  type ProposalStateChanged,
  type RejectAction,
  type ServerEvent,
  type UserMessage,
} from "./envelope";
export {
  AgentSession,
  type AgentSessionOptions,
  type ChatEntry,
  type ProposalView,
  type SessionBusyState,
  type SessionError,
  type SessionSnapshot,
} from "./session";
export {
  AgentProvider,
  useAgentSession,
  useAgentProposal,
  type AgentProposalHandle,
  type AgentSessionHandle,
} from "./context";
export {
  BlockRouter,
  PendingProposals,
  type BlockComponents,
  type BlockProps,
} from "./router";
export { Proposal, useBlock } from "./primitives";
export {
  SUBPROTOCOL,
  createWebSocketTransport,
  type ClosableTransport,
  type WebSocketTransportOptions,
} from "./websocket";
