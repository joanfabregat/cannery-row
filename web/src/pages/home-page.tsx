import { Link } from "react-router";

import { useAttention, useBrief, useConcerns } from "@/api/queries";
import type { Attention } from "@/api/types";
import { ConcernLine } from "@/components/concerns";
import { ImportedBadge } from "@/components/imported-badge";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
import { navItems } from "@/components/shell/nav-items";
import { StatusChip } from "@/components/status-chip";
import { excerpt, formatDateTime, formatWaited } from "@/lib/format";
import { label } from "@/lib/labels";
import { attemptPath, unitPath, reviewPath, writeupPath } from "@/lib/paths";
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
          <BriefCard project={project.slug} />
          {isResearcher ? <ConcernQueue project={project.slug} /> : null}
          {isResearcher ? <ReviewQueue data={data} /> : null}
          {isResearcher ? <WriteupQueue data={data} /> : null}
          <RecentOutcomes data={data} />
          <StalledVerifications data={data} />
          <RunningWork data={data} />
          <RecentFailures data={data} />
        </div>
      )}
    </QueryView>
  );
}

/** The brief's title and goal, or a note that the project has none yet. */
function BriefCard({ project }: { project: string }) {
  const brief = useBrief(project);
  if (brief.isPending || brief.isError) return null;
  const current = brief.data;
  return (
    <Section
      title="Brief"
      description={current ? current.title : "This project has no brief yet."}
      actions={
        <Link to="/brief" className="text-sm font-medium hover:underline">
          {current ? "Read the brief" : "Open the brief"}
        </Link>
      }
    >
      {current ? (
        <p className="text-sm text-muted-foreground">{excerpt(current.goal, 300)}</p>
      ) : null}
    </Section>
  );
}

/** The open concerns about the project's plans: each holds up its track's new work. */
function ConcernQueue({ project }: { project: string }) {
  const concerns = useConcerns(project, "open");
  return (
    <Section
      title="Concerns about plans"
      description="While a concern is open, no new unit of its track starts. Revise the track's plan to answer it, or dismiss it with a reason, on the track page."
    >
      <QueryView query={concerns}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>No concern is open.</EmptyState>
          ) : (
            <ul className="flex flex-col divide-y">
              {page.items.map((concern) => (
                <li key={concern.id} className="flex flex-col gap-1 py-3">
                  <ConcernLine concern={concern} showTrack />
                  <p className="text-sm text-muted-foreground">{excerpt(concern.body, 200)}</p>
                </li>
              ))}
            </ul>
          )
        }
      </QueryView>
    </Section>
  );
}

function ReviewQueue({ data }: { data: Attention }) {
  const counts = data.pending_counts;
  const total = (counts.decision ?? 0) + (counts.failure ?? 0);
  return (
    <Section
      title="Decisions to take"
      description={
        total === 0
          ? undefined
          : `${counts.decision ?? 0} ${counts.decision === 1 ? "decision" : "decisions"} and ${counts.failure ?? 0} ${counts.failure === 1 ? "failure" : "failures"}, oldest first.`
      }
    >
      {data.pending_reviews.length === 0 ? (
        <EmptyState>Nothing is waiting for a decision.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.pending_reviews.map((r) => (
            <li key={r.case_id} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {label("reviewKind", r.kind)}
                </span>
                {r.kind === "decision" && r.verdict ? (
                  <StatusChip domain="verdict" value={r.verdict} />
                ) : null}
                <ImportedBadge origin={r.origin} />
              </div>
              <Link to={reviewPath(r.unit)} className="font-medium hover:underline">
                {r.unit_ref} {r.title}
              </Link>
              <p className="text-sm text-muted-foreground">
                {r.kind === "decision"
                  ? r.verdict
                    ? `Attempt ${r.attempt_ref ?? ""}: the verification ${VERDICT_WORDS[r.verdict] ?? "finished"}; written up.`
                    : `Attempt ${r.attempt_ref ?? ""}: stopped after a failure; written up.`
                  : `Attempt ${r.attempt_ref ?? ""} failed${r.failure_reason ? `: ${excerpt(r.failure_reason, 120)}` : "."}`}{" "}
                Waiting since {formatDateTime(r.opened_at)}.
              </p>
              {r.decider ? (
                <p className="text-sm text-muted-foreground">
                  The decider {r.decider} decides this one automatically; you can correct its
                  decision once it is recorded.
                </p>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

/** The units waiting for their write-up, before their decision. */
function WriteupQueue({ data }: { data: Attention }) {
  const total = data.pending_writeup_count;
  return (
    <Section
      title="Write-ups to do"
      description={
        total === 0
          ? undefined
          : `${String(total)} ${total === 1 ? "unit waits" : "units wait"} for a write-up before the decision, oldest first. An agent or a researcher writes each one up; a researcher may skip one with a reason.`
      }
    >
      {data.pending_writeups.length === 0 ? (
        <EmptyState>Nothing is waiting for a write-up.</EmptyState>
      ) : (
        <ul className="flex flex-col divide-y">
          {data.pending_writeups.map((w) => (
            <li key={w.job_id} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Link to={writeupPath(w.unit)} className="font-medium hover:underline">
                  {w.unit_ref} {w.title}
                </Link>
                <StatusChip domain="writeup" value={w.job_state} />
              </div>
              <p className="text-sm text-muted-foreground">
                Attempt {w.attempt_ref}:{" "}
                {w.attempt_state === "verified" ? "verified" : "stopped after a failure"}. Waiting
                since {formatDateTime(w.waiting_since)}.
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
            <li key={`${o.unit}-${o.decided_at}`} className="flex flex-col gap-1 py-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Link to={unitPath(o.unit)} className="font-medium hover:underline">
                  {o.unit_ref} {o.title}
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
                to={attemptPath(r.unit, r.attempt_ref.split(".").at(-1) ?? "1")}
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
                  to={attemptPath(f.unit, f.attempt_ref.split(".").at(-1) ?? "1")}
                  className="font-medium hover:underline"
                >
                  {f.attempt_ref} {f.title}
                </Link>
                <span className="flex items-center gap-2">
                  <ImportedBadge origin={f.origin} />
                  <StatusChip domain="unit" value={f.unit_state} />
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

/** Who a stalled verification waits for, in words. */
function waitsFor(s: Attention["stalled_verifications"][number]): string {
  if (s.performer === "runner" && s.verifier) {
    return s.revision
      ? `the verifier ${s.verifier}, rules version ${s.revision}`
      : `the verifier ${s.verifier}`;
  }
  return "an agent or a researcher who did not run it";
}

function StalledVerifications({ data }: { data: Attention }) {
  if (data.stalled_verifications.length === 0) return null;
  return (
    <Section
      title="Waiting for verification"
      description={`${
        data.stalled_verification_count > data.stalled_verifications.length
          ? `${String(data.stalled_verification_count)} results are waiting to be verified; here are the oldest. `
          : "These results are waiting to be verified. "
      }Check that the project's verifier is running with the rules version the project names, or that an agent or a researcher picks them up.`}
    >
      <ul className="flex flex-col divide-y">
        {data.stalled_verifications.map((s) => (
          <li key={s.attempt_ref} className="flex flex-col gap-1 py-3">
            <Link
              to={attemptPath(s.unit, s.attempt_ref.split(".").at(-1) ?? "1")}
              className="font-medium hover:underline"
            >
              {s.attempt_ref} {s.title}
            </Link>
            <p className="text-sm text-muted-foreground">
              {s.unit_ref} has been waiting{" "}
              <time dateTime={s.waiting_since} title={formatDateTime(s.waiting_since)}>
                {formatWaited(s.waiting_since)}
              </time>{" "}
              for {waitsFor(s)}.
            </p>
          </li>
        ))}
      </ul>
    </Section>
  );
}
