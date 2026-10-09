import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router";

import { api, unwrap } from "@/api/client";
import { planKey, projectKey, usePlan, usePlanRevisions } from "@/api/queries";
import type { Plan, PlanRevision, PlanUnit } from "@/api/types";
import { type DecisionChoice, DecisionForm } from "@/components/decision-form";
import { Markdown } from "@/components/markdown";
import { EmptyState, QueryView } from "@/components/query-state";
import { Collapsible, Fact, Section, SubSection } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import { describeError } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { hypothesisPath, planEditorPath } from "@/lib/paths";

/**
 * A track's plan: the approach and the units it defines, the open revision
 * and its review, and the revision history. Researchers author and approve
 * plans; agents draft them through the same routes.
 */

function writtenBy(plan: Plan | PlanRevision): string {
  const who = plan.created_by_name ?? "A researcher";
  if (!("via_channel" in plan)) return who;
  const through = label("channel", plan.via_channel);
  return plan.via_client ? `${who} (${through}, ${plan.via_client})` : `${who} (${through})`;
}

/** A unit's line: its key, its hypothesis once approved and its state. */
export function UnitLine({ unit }: { unit: PlanUnit }) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <span className="font-medium">
        {unit.number != null ? (
          <Link to={hypothesisPath(unit.number)} className="hover:underline">
            #{unit.number} {unit.title}
          </Link>
        ) : (
          unit.title
        )}{" "}
        <span className="font-mono text-xs text-muted-foreground">{unit.key}</span>
        {unit.redo_of != null ? (
          <span className="text-sm text-muted-foreground"> · redoes #{unit.redo_of}</span>
        ) : null}
      </span>
      {unit.state ? (
        <StatusChip domain="hypothesis" value={unit.state} />
      ) : (
        <span className="text-sm text-muted-foreground">New</span>
      )}
    </div>
  );
}

export function UnitBody({ unit }: { unit: PlanUnit }) {
  return (
    <div className="mt-2 flex flex-col gap-2 text-sm">
      <p>{unit.question}</p>
      {unit.brief.trim() ? (
        <Collapsible summary="Unit brief">
          <Markdown>{unit.brief}</Markdown>
        </Collapsible>
      ) : null}
    </div>
  );
}

function PlanUnits({ plan }: { plan: Plan }) {
  if (plan.units.length === 0) {
    return <EmptyState>This revision lists no unit waiting to start.</EmptyState>;
  }
  return (
    <ul className="flex flex-col divide-y">
      {plan.units.map((unit) => (
        <li key={unit.key} className="py-3">
          <UnitLine unit={unit} />
          <UnitBody unit={unit} />
        </li>
      ))}
    </ul>
  );
}

function Alignments({ plan }: { plan: Plan }) {
  if (plan.alignments.length === 0) return null;
  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-sm font-medium">Units already done or in flight</h3>
      <ul className="flex flex-col gap-1 text-sm">
        {plan.alignments.map((a) => (
          <li key={a.number}>
            <Link to={hypothesisPath(a.number)} className="font-medium hover:underline">
              #{a.number} {a.title}
            </Link>
            : {label("alignment", a.decision)} · “{a.reason}”
          </li>
        ))}
      </ul>
    </div>
  );
}

const REVIEW_CHOICES: DecisionChoice<"approve" | "send_back" | "decline">[] = [
  {
    action: "approve",
    label: "Approve",
    effect:
      "The plan's new units become queued hypotheses, changed ones get a new revision, dropped ones are cancelled and the alignment entries apply.",
    variant: "default",
  },
  {
    action: "send_back",
    label: "Send back",
    effect: "The author starts another revision from this one.",
  },
  {
    action: "decline",
    label: "Decline",
    effect: "The revision is closed and changes nothing.",
    variant: "destructive",
  },
];

function PlanReview({ project, track, plan }: { project: string; track: string; plan: Plan }) {
  const queryClient = useQueryClient();
  return (
    <SubSection
      title={`Review revision ${String(plan.revision)}`}
      description={`Submitted by ${writtenBy(plan)}${plan.submitted_at ? ` on ${formatDateTime(plan.submitted_at)}` : ""}.`}
    >
      <DecisionForm
        subject={`revision ${String(plan.revision)} of the plan`}
        choices={REVIEW_CHOICES}
        submit={async (action, reason) =>
          unwrap(
            await api.POST("/api/projects/{slug}/tracks/{track_slug}/plans/{revision}/review", {
              params: {
                path: { slug: project, track_slug: track, revision: plan.revision },
              },
              body: { action, reason },
            }),
          )
        }
        onDone={async () => {
          await queryClient.invalidateQueries({ queryKey: projectKey(project) });
        }}
        onStale={async () => {
          await queryClient.invalidateQueries({ queryKey: planKey(project, track) });
        }}
      />
    </SubSection>
  );
}

function OpenRevision({
  project,
  track,
  open,
  isResearcher,
}: {
  project: string;
  track: string;
  open: Plan;
  isResearcher: boolean;
}) {
  return (
    <SubSection
      title={`Revision ${String(open.revision)}`}
      description={`Started by ${writtenBy(open)} on ${formatDateTime(open.created_at)}.`}
    >
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <StatusChip domain="plan" value={open.state} />
          {open.state === "draft" ? (
            <Link
              to={planEditorPath(track)}
              className="text-sm font-medium underline underline-offset-4"
            >
              {isResearcher ? "Edit the draft" : "Read the draft"}
            </Link>
          ) : null}
        </div>
        {open.state === "submitted" ? (
          <>
            {open.approach.trim() ? <Markdown>{open.approach}</Markdown> : null}
            <PlanUnits plan={open} />
            <Alignments plan={open} />
            {isResearcher ? <PlanReview project={project} track={track} plan={open} /> : null}
          </>
        ) : null}
      </div>
    </SubSection>
  );
}

function StartRevision({
  project,
  track,
  first,
}: {
  project: string;
  track: string;
  first: boolean;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const start = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/plans", {
          params: { path: { slug: project, track_slug: track } },
        }),
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: planKey(project, track) });
      await navigate(planEditorPath(track));
    },
  });
  return (
    <div className="flex flex-col items-end gap-1">
      <Button
        variant={first ? "default" : "outline"}
        disabled={start.isPending}
        onClick={() => {
          start.mutate();
        }}
      >
        {first ? "Write the plan" : "Start a revision"}
      </Button>
      {start.error ? (
        <p role="alert" className="text-sm text-status-danger">
          {describeError(start.error)}
        </p>
      ) : null}
    </div>
  );
}

function RevisionHistory({ project, track }: { project: string; track: string }) {
  const revisions = usePlanRevisions(project, track);
  return (
    <Collapsible summary="Plan revisions">
      <QueryView query={revisions}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>No revision yet.</EmptyState>
          ) : (
            <ol className="flex flex-col gap-2 text-sm">
              {page.items.map((r) => (
                <li key={r.revision} className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">Revision {r.revision}</span>
                  <StatusChip domain="plan" value={r.state} />
                  <span className="text-muted-foreground">
                    {writtenBy(r)} · {formatDateTime(r.created_at)} · {r.units}{" "}
                    {r.units === 1 ? "unit" : "units"}
                    {r.reviewed_by_name ? ` · reviewed by ${r.reviewed_by_name}` : ""}
                    {r.review_reason ? `: “${r.review_reason}”` : ""}
                  </span>
                </li>
              ))}
            </ol>
          )
        }
      </QueryView>
    </Collapsible>
  );
}

export function TrackPlan({
  project,
  track,
  archived,
  isResearcher,
}: {
  project: string;
  track: string;
  archived: boolean;
  isResearcher: boolean;
}) {
  const current = usePlan(project, track, "current");
  const open = usePlan(project, track, "draft");
  const approved = current.data?.state === "approved" ? current.data : null;
  const canStart = isResearcher && !archived && open.data === null && !open.isPending;
  return (
    <Section
      title="Plan"
      description={
        current.isPending
          ? undefined
          : approved
            ? `Revision ${String(approved.revision)}, approved${approved.reviewed_by_name ? ` by ${approved.reviewed_by_name}` : ""}${approved.reviewed_at ? ` on ${formatDateTime(approved.reviewed_at)}` : ""}.`
            : "No approved plan yet: nothing in this track can start until a researcher approves one."
      }
      actions={
        canStart ? <StartRevision project={project} track={track} first={!approved} /> : null
      }
    >
      <QueryView query={current}>
        {() => (
          <div className="flex flex-col gap-5">
            {approved ? (
              <>
                <div className="flex flex-col gap-2">
                  <h3 className="text-sm font-medium">Approach</h3>
                  <Markdown>{approved.approach}</Markdown>
                </div>
                <div className="flex flex-col gap-2">
                  <h3 className="text-sm font-medium">Units</h3>
                  <PlanUnits plan={approved} />
                </div>
                <Alignments plan={approved} />
                <dl className="grid gap-4 sm:grid-cols-3">
                  <Fact term="Written by">{writtenBy(approved)}</Fact>
                  <Fact term="Review reason">{approved.review_reason ?? "None"}</Fact>
                  <Fact term="Markdown">
                    <a href={approved.markdown_ref} className="underline underline-offset-4">
                      plan.md
                    </a>
                  </Fact>
                </dl>
              </>
            ) : null}
            {open.data ? (
              <OpenRevision
                project={project}
                track={track}
                open={open.data}
                isResearcher={isResearcher}
              />
            ) : null}
            <RevisionHistory project={project} track={track} />
          </div>
        )}
      </QueryView>
    </Section>
  );
}
