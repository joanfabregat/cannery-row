import { Link } from "react-router";

import { useAttention } from "@/api/queries";
import type { Attention } from "@/api/types";
import { ImportedBadge } from "@/components/imported-badge";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
import { navItems } from "@/components/shell/nav-items";
import { StatusChip } from "@/components/status-chip";
import { excerpt, formatDateTime, formatWaited } from "@/lib/format";
import { label } from "@/lib/labels";
import { attemptPath, hypothesisPath, reviewPath } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

const item = navItems.find((entry) => entry.to === "/");

export function HomePage() {
  return (
    <>
      <PageHeader title="Home" description={item?.description} />
      <ProjectPage>{(project) => <Attention project={project} />}</ProjectPage>
    </>
  );
}

const VERDICT_WORDS: Record<string, string> = {
  pass: "passed every check",
  fail: "did not pass its checks",
  inconclusive: "was inconclusive",
};

function Attention({ project }: { project: Project }) {
  const attention = useAttention(project.slug);
  const { isResearcher } = usePermissions();
  return (
    <QueryView query={attention}>
      {(data) => (
        <div className="flex flex-col gap-6">
          {isResearcher ? <ReviewQueue data={data} /> : null}
          <RecentOutcomes data={data} />
          <StalledEvaluations data={data} />
          <RunningWork data={data} />
          <RecentFailures data={data} />
        </div>
      )}
    </QueryView>
  );
}

function ReviewQueue({ data }: { data: Attention }) {
  const counts = data.pending_counts;
  const total = (counts.draft ?? 0) + (counts.result ?? 0) + (counts.failure ?? 0);
  return (
    <Section
      title="Waiting for your review"
      description={
        total === 0
          ? undefined
          : `${counts.draft ?? 0} ${counts.draft === 1 ? "draft" : "drafts"}, ${counts.result ?? 0} ${counts.result === 1 ? "result" : "results"} and ${counts.failure ?? 0} ${counts.failure === 1 ? "failure" : "failures"}, oldest first.`
      }
    >
      {data.pending_reviews.length === 0 ? (
        <EmptyState>Nothing is waiting for a review.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.pending_reviews.map((r) => (
            <li key={r.case_id} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {label("reviewKind", r.kind)}
                </span>
                {r.kind === "result" && r.verdict ? (
                  <StatusChip domain="verdict" value={r.verdict} />
                ) : null}
                <ImportedBadge origin={r.origin} />
              </div>
              <Link to={reviewPath(r.hypothesis)} className="font-medium hover:underline">
                {r.hypothesis_ref} {r.title}
              </Link>
              <p className="text-sm text-muted-foreground">
                {r.kind === "draft"
                  ? "A new draft to approve, send back or decline."
                  : r.kind === "result"
                    ? `Attempt ${r.attempt_ref ?? ""}: the evaluation ${VERDICT_WORDS[r.verdict ?? ""] ?? "finished"}.`
                    : `Attempt ${r.attempt_ref ?? ""} failed${r.failure_reason ? `: ${excerpt(r.failure_reason, 120)}` : "."}`}{" "}
                Waiting since {formatDateTime(r.opened_at)}.
              </p>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function RecentOutcomes({ data }: { data: Attention }) {
  return (
    <Section title="Recent outcomes" description="The latest results a researcher decided on.">
      {data.recent_outcomes.length === 0 ? (
        <EmptyState>No result has been decided yet.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.recent_outcomes.map((o) => (
            <li key={`${o.hypothesis}-${o.decided_at}`} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Link to={hypothesisPath(o.hypothesis)} className="font-medium hover:underline">
                  {o.hypothesis_ref} {o.title}
                </Link>
                <span className="flex items-center gap-2">
                  <ImportedBadge origin={o.origin} />
                  <StatusChip domain="decision" value={o.action} />
                </span>
              </div>
              <p className="text-sm text-muted-foreground">
                “{excerpt(o.reason, 160)}” · {formatDateTime(o.decided_at)}
              </p>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function RunningWork({ data }: { data: Attention }) {
  return (
    <Section
      title="Running now"
      description={
        data.running_count > data.running.length
          ? `${data.running_count} attempts in progress; the latest ones:`
          : "Attempts in progress."
      }
    >
      {data.running.length === 0 ? (
        <EmptyState>Nothing is running right now.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.running.map((r) => (
            <li
              key={r.attempt_ref}
              className="flex flex-wrap items-center justify-between gap-2 py-3"
            >
              <Link
                to={attemptPath(r.hypothesis, r.attempt_ref.split(".").at(-1) ?? "1")}
                className="font-medium hover:underline"
              >
                {r.attempt_ref} {r.title}
              </Link>
              <div className="flex items-center gap-3 text-sm text-muted-foreground">
                <span>Started {formatDateTime(r.claimed_at)}</span>
                <StatusChip domain="attempt" value={r.state} />
              </div>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function RecentFailures({ data }: { data: Attention }) {
  return (
    <Section
      title="Recent failures"
      description="Attempts that could not produce a result. A failure is not a scientific rejection."
    >
      {data.recent_failures.length === 0 ? (
        <EmptyState>No attempt has failed recently.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.recent_failures.map((f) => (
            <li key={`${f.attempt_ref}-${f.created_at}`} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Link
                  to={attemptPath(f.hypothesis, f.attempt_ref.split(".").at(-1) ?? "1")}
                  className="font-medium hover:underline"
                >
                  {f.attempt_ref} {f.title}
                </Link>
                <span className="flex items-center gap-2">
                  <ImportedBadge origin={f.origin} />
                  <StatusChip domain="hypothesis" value={f.hypothesis_state} />
                </span>
              </div>
              <p className="text-sm text-muted-foreground">
                {label("stage", f.stage)}: {excerpt(f.reason, 160)} · {formatDateTime(f.created_at)}
              </p>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function StalledEvaluations({ data }: { data: Attention }) {
  if (data.stalled_evaluations.length === 0) return null;
  return (
    <Section
      title="Waiting for the evaluator"
      description={`${
        data.stalled_evaluation_count > data.stalled_evaluations.length
          ? `${String(data.stalled_evaluation_count)} results are waiting to be judged; here are the oldest. `
          : "These results are waiting to be judged. "
      }Check that the project's evaluator is running, with the rules version the project names.`}
    >
      <ul className="flex flex-col divide-y">
        {data.stalled_evaluations.map((s) => (
          <li key={s.attempt_ref} className="flex flex-col gap-1 py-3">
            <Link
              to={attemptPath(s.hypothesis, s.attempt_ref.split(".").at(-1) ?? "1")}
              className="font-medium hover:underline"
            >
              {s.attempt_ref} {s.title}
            </Link>
            <p className="text-sm text-muted-foreground">
              {s.hypothesis_ref} has been waiting{" "}
              <time dateTime={s.waiting_since} title={formatDateTime(s.waiting_since)}>
                {formatWaited(s.waiting_since)}
              </time>{" "}
              for evaluator {s.evaluator}, rules version {s.revision}.
            </p>
          </li>
        ))}
      </ul>
    </Section>
  );
}
