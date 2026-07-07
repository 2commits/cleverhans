/**
 * Layer 2 (spec §10): headless block primitives, Radix-style. Behavior and
 * slot access, zero styling — compose them inside a block component:
 *
 * ```tsx
 * function ConfirmBlock(props: BlockProps<ConfirmSlots>) {
 *   return (
 *     <Proposal.Root {...props}>
 *       <Proposal.Title />
 *       <Proposal.Preview />
 *       <Proposal.Confirm>Do it</Proposal.Confirm>
 *       <Proposal.Reject>Cancel</Proposal.Reject>
 *     </Proposal.Root>
 *   );
 * }
 * ```
 */

import { createContext, useContext } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";

import type { DryRunPreview } from "./envelope";
import type { BlockProps } from "./router";

const BlockContext = createContext<BlockProps | null>(null);

/** Escape hatch: the enclosing block's props, for custom subcomponents. */
export function useBlock(): BlockProps {
  const block = useContext(BlockContext);
  if (block === null) {
    throw new Error("cleverhans: this primitive must be inside <Proposal.Root>");
  }
  return block;
}

function Root(
  props: BlockProps & { children?: ReactNode; className?: string },
): ReactNode {
  const { children, className, ...block } = props;
  return (
    <BlockContext.Provider value={block}>
      <div
        className={className}
        data-cleverhans-proposal={block.proposal.proposal_id}
        data-action-id={block.proposal.action_id}
        data-state={block.view.state}
        data-working={block.view.working ? "true" : undefined}
      >
        {children}
      </div>
    </BlockContext.Provider>
  );
}

/** Renders `slots.title` (or children override). */
function Title(props: { children?: ReactNode; className?: string }): ReactNode {
  const { slots } = useBlock();
  return (
    <h3 className={props.className} data-cleverhans-title>
      {props.children ?? String(slots["title"] ?? "")}
    </h3>
  );
}

/**
 * Renders the dry-run preview: children as a render prop when given, the
 * preview summary otherwise. Nothing renders for non-mutating actions.
 */
function Preview(props: {
  children?: (preview: DryRunPreview) => ReactNode;
  className?: string;
}): ReactNode {
  const { proposal } = useBlock();
  const preview = proposal.preview;
  if (!preview) {
    return null;
  }
  return (
    <div
      className={props.className}
      data-cleverhans-preview
      data-affected-count={preview.affected_count}
    >
      {props.children ? props.children(preview) : (preview.summary ?? "")}
    </div>
  );
}

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement>;

/**
 * The confirmation gate — the user is the executor. Disabled once the
 * proposal leaves `validated`; a stale click can never re-execute
 * (confirm-time revalidation backs this up server-side, spec §7.3).
 */
function Confirm(props: ButtonProps): ReactNode {
  const { confirm, view } = useBlock();
  const { children, disabled, onClick, ...rest } = props;
  return (
    <button
      type="button"
      data-cleverhans-confirm
      disabled={disabled ?? (view.state !== "validated" || view.working)}
      onClick={(e) => {
        onClick?.(e);
        confirm();
      }}
      {...rest}
    >
      {children ?? "Confirm"}
    </button>
  );
}

/** Declines the proposal; `reason` flows back to the model as context. */
function Reject(props: ButtonProps & { reason?: string }): ReactNode {
  const { reject, view } = useBlock();
  const { children, disabled, onClick, reason, ...rest } = props;
  return (
    <button
      type="button"
      data-cleverhans-reject
      disabled={disabled ?? (view.state !== "validated" || view.working)}
      onClick={(e) => {
        onClick?.(e);
        reject(reason);
      }}
      {...rest}
    >
      {children ?? "Reject"}
    </button>
  );
}

/** The headless primitive set. */
export const Proposal = { Root, Title, Preview, Confirm, Reject };
