import { NotebookPenIcon, ScaleIcon } from "lucide-react";
import { Link, useParams } from "react-router";

import {
  useAttempt,
  useAttempts,
  useUnit,
  useReport,
  useRevisions,
  useWriteup,
} from "@/api/queries";
import type { Attempt, Unit, Link as LinkOut } from "@/api/types";
import { CommentsSection } from "@/components/comments";
import { DecisionList } from "@/components/decisions";
import { ReportSection, VerificationSection } from "@/components/evidence";
import { Notice } from "@/components/notice";
import { OutcomeCard } from "@/components/outcome-card";
import { ImportedBadge } from "@/components/imported-badge";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { WriteupView } from "@/components/writeup";
import { Button } from "@/components/ui/button";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { parseNumber, useNotice } from "@/lib/navigation";
import { controlText, pendingReview, summarizeOutcome } from "@/lib/outcome";
import { attemptPath, unitPath, reviewPath, trackPath, writeupPath } from "@/lib/paths";
import { ARCHIVED_STATES } from "@/lib/states";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

export function UnitPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  if (number === null) {
    return (
      <>
        <PageHeader title="Unit not found" />
        <EmptyState>This address does not name a unit.</EmptyState>
      </>
    );
  }
  return <ProjectPage>{(project) => <UnitView project={project} number={number} />}</ProjectPage>;
}

const REVIEW_BUTTON: Record<string, string> = {
  decision: "Decide",
  failure: "Review this failure",
};

function UnitView({ project, number }: { project: Project; number: number }) {
  const slug = project.slug;
  const unit = useUnit(slug, number);
  const attempts = useAttempts(slug, number);
  const latest = attempts.data?.items.at(-1) ?? null;
  const attempt = useAttempt(slug, number, latest?.sequence ?? null);
  const report = useReport(slug, number, latest?.sequence ?? null);
  const { isResearcher } = usePermissions();
  const notice = useNotice();

  if (unit.isPending) {
    return (
      <>
        <PageHeader title={`Unit #${number}`} />
        <Loading />
      </>
    );
  }
  if (unit.isError) {
    return (
      <>
        <PageHeader title={`Unit #${number}`} />
        <LoadError
          error={unit.error}
          retry={unit.refetch}
          notFound={`There is no unit #${number} in ${project.title}, or you cannot read it.`}
        />
      </>
    );
  }
  const h = unit.data;
  const pending = pendingReview(h);
  // The sentence waits for the latest attempt and its report, not for errors.
  const detailsReady =
    !attempts.isPending && (latest === null || (!attempt.isPending && !report.isPending));

  return (
    <>
      <PageHeader
        title={h.title}
        description={`${h.ref} · ${ARCHIVED_STATES.includes(h.state) ? "Archived · " : ""}Track ${h.track}`}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <StatusChip domain="unit" value={h.state} className="text-sm" />
            <ImportedBadge
              origin={h.origin}
              sourceRef={h.source_ref}
              externalId={h.external_id}
              showSource
            />
            {isResearcher && h.state === "documenting" ? (
              <Button asChild>
                <Link to={writeupPath(h.number)}>
                  <NotebookPenIcon aria-hidden="true" />
                  Write it up
                </Link>
              </Button>
            ) : null}
            {isResearcher && pending ? (
              <Button asChild>
                <Link to={reviewPath(h.number)}>
                  <ScaleIcon aria-hidden="true" />
                  {REVIEW_BUTTON[pending.kind] ?? "Review"}
                </Link>
              </Button>
            ) : null}
          </div>
        }
      />
      <Notice>{notice}</Notice>
      <div className="flex flex-col gap-6">
        {detailsReady ? (
          <OutcomeCard
            summary={summarizeOutcome(
              h,
              attempt.data ?? null,
              report.data ?? null,
              attempts.data?.items.at(-2) ?? null,
            )}
          />
        ) : (
          <Loading>Loading the outcome…</Loading>
        )}

        <IdeaSection unit={h} />

        <Section
          title="Attempts"
          description="Each attempt is one try of this unit. Failed attempts stay visible."
        >
          {attempts.isPending ? (
            <Loading />
          ) : attempts.isError ? (
            <LoadError error={attempts.error} retry={attempts.refetch} />
          ) : (
            <AttemptList number={h.number} attempts={attempts.data.items} />
          )}
        </Section>

        {latest !== null && report.data ? (
          <>
            <ReportSection report={report.data} />
            <VerificationSection project={slug} report={report.data} />
          </>
        ) : latest !== null && report.isError ? (
          <LoadError error={report.error} retry={report.refetch} />
        ) : null}

        <WriteupSection project={slug} unit={h} />

        <Section
          title="Decisions"
          description="Every decision a researcher recorded, with the reason."
        >
          <DecisionList reviews={h.reviews} />
        </Section>

        <RelatedSection unit={h} />

        <CommentsSection project={slug} target={{ number: h.number }} />

        <UnitDetails project={slug} unit={h} />
      </div>
    </>
  );
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

/** The write-up, once the unit has one to write: none before its first verification or stop. */
function WriteupSection({ project, unit }: { project: string; unit: Unit }) {
  const writeup = useWriteup(
    project,
    unit.number,
    !["queued", "active", "cancelled"].includes(unit.state),
  );
  if (writeup.data === undefined || writeup.data === null) return null;
  return (
    <Section
      title="Write-up"
      actions={
        <Link to={writeupPath(unit.number)} className="text-sm font-medium hover:underline">
          Open the write-up
        </Link>
      }
    >
      <WriteupView writeup={writeup.data} />
    </Section>
  );
}

function IdeaSection({ unit }: { unit: Unit }) {
  const doc = unit.document as Record<string, unknown>;
  const plan = (typeof doc.plan === "object" && doc.plan !== null ? doc.plan : {}) as Record<
    string,
    unknown
  >;
  const facts: [string, string | null][] = [
    ["Question", text(doc.question)],
    ["Why try it", text(doc.rationale)],
    ["What changes", text(doc.intervention)],
    ["Compared with", controlText(doc)],
    ["Success looks like", text(plan.success_criteria)],
    ["It is disproved if", text(plan.falsification_criteria)],
  ];
  return (
    <Section title="The idea">
      <dl className="grid gap-4 md:grid-cols-2">
        {facts.map(([term, value]) =>
          value === null ? null : (
            <Fact key={term} term={term}>
              {value}
            </Fact>
          ),
        )}
      </dl>
    </Section>
  );
}

function AttemptList({ number, attempts }: { number: number; attempts: Attempt[] }) {
  if (attempts.length === 0) {
    return <EmptyState>Not tried yet.</EmptyState>;
  }
  return (
    <ul className="flex flex-col divide-y">
      {[...attempts].reverse().map((a) => (
        <li key={a.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
          <Link to={attemptPath(number, a.sequence)} className="font-medium hover:underline">
            Attempt {a.ref}
          </Link>
          <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
            <span>Started {formatDateTime(a.claimed_at)}</span>
            {a.finished_at ? <span>Finished {formatDateTime(a.finished_at)}</span> : null}
            <StatusChip domain="attempt" value={a.state} />
          </div>
        </li>
      ))}
    </ul>
  );
}

function LinkItem({ link }: { link: LinkOut }) {
  return (
    <li className="flex flex-wrap items-center gap-2 text-sm">
      <span className="text-muted-foreground">{label("relation", link.kind)}</span>
      <Link to={unitPath(link.number, link.project)} className="font-medium hover:underline">
        {link.ref} {link.title}
      </Link>
      <StatusChip domain="unit" value={link.state} />
    </li>
  );
}

function RelatedSection({ unit }: { unit: Unit }) {
  if (unit.relations.length === 0 && unit.backlinks.length === 0) return null;
  return (
    <Section title="Related units">
      <div className="flex flex-col gap-4">
        {unit.relations.length > 0 ? (
          <ul className="flex flex-col gap-2">
            {unit.relations.map((link) => (
              <LinkItem key={`${link.kind}-${link.ref}`} link={link} />
            ))}
          </ul>
        ) : null}
        {unit.backlinks.length > 0 ? (
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">Mentioned by</h3>
            <ul className="flex flex-col gap-2">
              {unit.backlinks.map((link) => (
                <li key={link.ref} className="text-sm">
                  <Link
                    to={unitPath(link.number, link.project)}
                    className="font-medium hover:underline"
                  >
                    {link.ref} {link.title}
                  </Link>
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>
    </Section>
  );
}

function UnitDetails({ project, unit }: { project: string; unit: Unit }) {
  return (
    <Collapsible summary="Details">
      <div className="flex flex-col gap-5">
        <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <Fact term="Revision">{unit.revision}</Fact>
          <Fact term="Approved revision">{unit.approved_revision ?? "—"}</Fact>
          <Fact term="Science revision">{unit.science_revision}</Fact>
          <Fact term="Created">{formatDateTime(unit.created_at)}</Fact>
          <Fact term="Created by">
            {unit.origin === "imported"
              ? "Imported"
              : unit.created_by.kind === "service"
                ? "An agent"
                : "A person"}
          </Fact>
          <Fact term="Approved">{formatDateTime(unit.approved_at)}</Fact>
          <Fact term="Last change">{formatDateTime(unit.updated_at)}</Fact>
          <Fact term="Track">
            <Link to={trackPath(unit.track)} className="underline underline-offset-4">
              {unit.track}
            </Link>
          </Fact>
          <Fact term="Identifier">
            <code className="text-xs break-all">{unit.id}</code>
          </Fact>
        </dl>
        <RevisionHistory project={project} number={unit.number} />
        <RawJson value={unit.document} label="Unit document" />
        {unit.imported ? (
          <RawJson value={unit.imported} label="As stated in the imported history" />
        ) : null}
      </div>
    </Collapsible>
  );
}

function RevisionHistory({ project, number }: { project: string; number: number }) {
  const revisions = useRevisions(project, number);
  if (revisions.isPending) return <Loading />;
  if (revisions.isError) return <LoadError error={revisions.error} retry={revisions.refetch} />;
  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-sm font-medium">Revisions</h3>
      <ol className="flex flex-col gap-1 text-sm">
        {revisions.data.items.map((r) => (
          <li key={r.revision}>
            Revision {r.revision} · {formatDateTime(r.created_at)} ·{" "}
            {r.origin === "imported"
              ? "imported"
              : r.author.kind === "service"
                ? "an agent"
                : "a person"}{" "}
            · {label("channel", r.via_channel)}
          </li>
        ))}
      </ol>
    </div>
  );
}
