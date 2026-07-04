/**
 * The styled block pack (spec §8, §10 layer 3): opt-in components built on
 * the headless primitives. Import `@cleverhans/ui/styles.css` (or restyle
 * the `ch-*` classes) — nothing here forces a design system.
 */

import { Proposal, type BlockProps } from "@cleverhans/react";
import type { ReactNode } from "react";

import { HorseIcon } from "./icon";

const STATUS_LABELS: Record<string, string> = {
  executed: "✓ Done",
  failed: "✗ Failed",
  rejected: "Rejected",
  expired: "Expired",
  unknown: "Unavailable",
};

/**
 * The lifecycle-driven tail of a block: confirm/reject while the decision
 * is open, the working indicator while the agent executes, a receipt line
 * once the proposal is terminal.
 */
function Footer(props: BlockProps): ReactNode {
  const { state, working, reason } = props.view;
  if (working || state === "confirmed") {
    return (
      <div className="ch-block-working" role="status">
        <HorseIcon size={15} />
        <span>On it</span>
        <span className="ch-dots" aria-hidden="true">
          <span />
          <span />
          <span />
        </span>
      </div>
    );
  }
  if (state === "validated") {
    return (
      <div className="ch-block-actions">
        <Proposal.Confirm className="ch-btn ch-btn--confirm" />
        <Proposal.Reject className="ch-btn ch-btn--reject" />
      </div>
    );
  }
  return (
    <p className="ch-block-status" data-cleverhans-status>
      {STATUS_LABELS[state] ?? state}
      {reason ? ` — ${reason}` : ""}
    </p>
  );
}

/**
 * The default block: title, dry-run summary, optional detail slot,
 * confirm/reject. Suits any action whose `confirm` block carries
 * `{ title, detail? }` slots. The summary names what the action touches;
 * `detail` describes the change, so it reads current state → new state.
 */
export function ConfirmBlock(props: BlockProps): ReactNode {
  const detail = props.slots["detail"];
  return (
    <Proposal.Root {...props} className="ch-block ch-block--confirm">
      <Proposal.Title className="ch-block-title" />
      <Proposal.Preview className="ch-block-preview" />
      {detail != null && <p className="ch-block-preview">{String(detail)}</p>}
      <Footer {...props} />
    </Proposal.Root>
  );
}

/**
 * For predicate/bulk actions (spec §4.2): makes the blast radius explicit —
 * affected count plus the sample of record IDs from the permission-correct
 * dry run.
 */
export function BulkPreviewBlock(props: BlockProps): ReactNode {
  return (
    <Proposal.Root {...props} className="ch-block ch-block--bulk">
      <Proposal.Title className="ch-block-title" />
      <Proposal.Preview className="ch-block-preview">
        {(preview) => (
          <>
            <span>
              Affects <strong>{preview.affected_count}</strong>{" "}
              {preview.affected_count === 1 ? "record" : "records"}
              {preview.summary ? ` — ${preview.summary}` : ""}
            </span>
            {preview.sample_ids && preview.sample_ids.length > 0 && (
              <ul className="ch-block-samples">
                {preview.sample_ids.map((id) => (
                  <li key={id}>{id}</li>
                ))}
              </ul>
            )}
          </>
        )}
      </Proposal.Preview>
      <Footer {...props} />
    </Proposal.Root>
  );
}

/** Block types this pack registers out of the box. */
export const DEFAULT_BLOCKS = {
  confirm: ConfirmBlock,
  bulk_preview: BulkPreviewBlock,
} as const;
