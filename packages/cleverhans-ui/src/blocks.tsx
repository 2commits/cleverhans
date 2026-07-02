/**
 * The styled block pack (spec §8, §10 layer 3): opt-in components built on
 * the headless primitives. Import `@cleverhans/ui/styles.css` (or restyle
 * the `ch-*` classes) — nothing here forces a design system.
 */

import { Proposal, type BlockProps } from "@cleverhans/react";
import type { ReactNode } from "react";

function statusLine(state: string, reason?: string): ReactNode {
  if (state === "validated") {
    return null;
  }
  return (
    <p className="ch-block-status" data-cleverhans-status>
      {state}
      {reason ? ` — ${reason}` : ""}
    </p>
  );
}

function Actions(): ReactNode {
  return (
    <div className="ch-block-actions">
      <Proposal.Confirm className="ch-btn ch-btn--confirm" />
      <Proposal.Reject className="ch-btn ch-btn--reject" />
    </div>
  );
}

/**
 * The default block: title, optional detail slot, dry-run summary,
 * confirm/reject. Suits any action whose `confirm` block carries
 * `{ title, detail? }` slots.
 */
export function ConfirmBlock(props: BlockProps): ReactNode {
  const detail = props.slots["detail"];
  return (
    <Proposal.Root {...props} className="ch-block ch-block--confirm">
      <Proposal.Title className="ch-block-title" />
      {detail != null && <p className="ch-block-preview">{String(detail)}</p>}
      <Proposal.Preview className="ch-block-preview" />
      {statusLine(props.view.state, props.view.reason)}
      <Actions />
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
      {statusLine(props.view.state, props.view.reason)}
      <Actions />
    </Proposal.Root>
  );
}

/** Block types this pack registers out of the box. */
export const DEFAULT_BLOCKS = {
  confirm: ConfirmBlock,
  bulk_preview: BulkPreviewBlock,
} as const;
