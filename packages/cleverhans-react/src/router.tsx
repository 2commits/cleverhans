/**
 * The block-type router — framework-owned even in a headless setup
 * (spec §8, §10): "proposal arrived → mount the registered component for its
 * `block_type`, pass slots". App components are presentational and never see
 * the stream.
 */

import type { ComponentType, ReactNode } from "react";

import type { ActionProposal } from "./envelope";
import type { ProposalView } from "./session";
import { useAgentProposal, useAgentSession } from "./context";

/** What every block component receives. */
export interface BlockProps<S = Record<string, unknown>> {
  proposal: ActionProposal;
  slots: S;
  view: ProposalView;
  confirm: () => void;
  reject: (reason?: string) => void;
}

/**
 * One component per registered block type, keyed by the codegen'd
 * `BlockSlotsMap` for full type safety:
 *
 * ```ts
 * const components: BlockComponents<BlockSlotsMap> = { confirm: ConfirmBlock };
 * ```
 */
export type BlockComponents<M extends Record<string, unknown> = Record<string, unknown>> = {
  [K in keyof M]?: ComponentType<BlockProps<M[K]>>;
};

/**
 * Renders one proposal through its registered block component. An
 * unregistered `block_type` renders nothing — fail closed (spec §8): such a
 * proposal should have been `invalid` server-side, and the frontend refuses
 * to invent UI for it.
 */
export function BlockRouter<M extends Record<string, unknown>>(props: {
  proposalId: string;
  components: BlockComponents<M>;
}): ReactNode {
  const { view, confirm, reject } = useAgentProposal(props.proposalId);
  if (!view) {
    return null;
  }
  const Component = props.components[view.proposal.block_type as keyof M] as
    | ComponentType<BlockProps>
    | undefined;
  if (!Component) {
    return null;
  }
  return (
    <Component
      proposal={view.proposal}
      slots={view.proposal.slots}
      view={view}
      confirm={confirm}
      reject={reject}
    />
  );
}

/** Convenience: routes every pending proposal, in arrival order. */
export function PendingProposals<M extends Record<string, unknown>>(props: {
  components: BlockComponents<M>;
}): ReactNode {
  const { snapshot } = useAgentSession();
  return (
    <>
      {snapshot.pending.map((view) => (
        <BlockRouter
          key={view.proposal.proposal_id}
          proposalId={view.proposal.proposal_id}
          components={props.components}
        />
      ))}
    </>
  );
}
