import type { Decision, HypothesisReview } from "@/api/types";
import { ImportedBadge } from "@/components/imported-badge";
import { EmptyState } from "@/components/query-state";
import { StatusChip } from "@/components/status-chip";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { usePeople } from "@/projects/use-people";

interface Entry {
  decision: Decision;
  kind: string;
  superseded: boolean;
}

function entries(reviews: HypothesisReview[]): Entry[] {
  const superseded = new Set(
    reviews.flatMap((r) => r.decisions.map((d) => d.supersedes)).filter(Boolean),
  );
  return reviews
    .flatMap((review) =>
      review.decisions.map((decision) => ({
        decision,
        kind: review.kind,
        superseded: superseded.has(decision.id),
      })),
    )
    .sort((a, b) => b.decision.decided_at.localeCompare(a.decision.decided_at));
}

/** Every human decision on a hypothesis, newest first, each with its reason. */
export function DecisionList({ reviews }: { reviews: HypothesisReview[] }) {
  const person = usePeople();
  const list = entries(reviews);
  if (list.length === 0) {
    return <EmptyState>No decision has been recorded yet.</EmptyState>;
  }
  return (
    <ol className="flex flex-col gap-4">
      {list.map(({ decision, kind, superseded }) => (
        <li key={decision.id} className="flex flex-col gap-1.5 border-l-2 pl-4">
          <div className="flex flex-wrap items-center gap-2">
            <StatusChip domain="decision" value={decision.action} />
            <ImportedBadge origin={decision.origin} sourceRef={decision.source_ref} showSource />
            <span className="text-sm text-muted-foreground">{label("reviewKind", kind)}</span>
            {superseded ? (
              <span className="text-sm text-muted-foreground">(corrected by a later decision)</span>
            ) : null}
          </div>
          <p className="text-sm">
            <span className="sr-only">Reason: </span>“{decision.reason}”
          </p>
          <p className="text-xs text-muted-foreground">
            {person(decision.actor_user_id)} · {formatDateTime(decision.decided_at)} ·{" "}
            {label("channel", decision.via_channel)}
          </p>
        </li>
      ))}
    </ol>
  );
}
