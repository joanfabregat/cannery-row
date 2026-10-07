import { PencilLineIcon, ScaleIcon } from "lucide-react";
import { Link, useParams } from "react-router";

import { useAttempt, useAttempts, useHypothesis, useReport, useRevisions } from "@/api/queries";
import type { Attempt, Hypothesis, Link as LinkOut } from "@/api/types";
import { CommentsSection } from "@/components/comments";
import { DecisionList } from "@/components/decisions";
import { AssessmentSection, ReportSection } from "@/components/evidence";
import { Notice } from "@/components/notice";
import { OutcomeCard } from "@/components/outcome-card";
import { ImportedBadge } from "@/components/imported-badge";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { parseNumber, useNotice } from "@/lib/navigation";
import { controlText, pendingReview, summarizeOutcome } from "@/lib/outcome";
import { attemptPath, hypothesisPath, reviewPath, trackPath } from "@/lib/paths";
import { ARCHIVED_STATES } from "@/lib/states";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

export function HypothesisPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  if (number === null) {
    return (
      <>
        <PageHeader title="Hypothesis not found" />
        <EmptyState>This address does not name a hypothesis.</EmptyState>
      </>
    );
  }
  return (
    <ProjectPage>{(project) => <HypothesisView project={project} number={number} />}</ProjectPage>
  );
}

const REVIEW_BUTTON: Record<string, string> = {
  draft: "Review this draft",
  result: "Review this result",
  failure: "Review this failure",
};

function HypothesisView({ project, number }: { project: Project; number: number }) {
  const slug = project.slug;
  const hypothesis = useHypothesis(slug, number);
  const attempts = useAttempts(slug, number);
  const latest = attempts.data?.items.at(-1) ?? null;
  const attempt = useAttempt(slug, number, latest?.sequence ?? null);
  const report = useReport(slug, number, latest?.sequence ?? null);
  const { isResearcher } = usePermissions();
  const notice = useNotice();

  if (hypothesis.isPending) {
    return (
      <>
        <PageHeader title={`Hypothesis #${number}`} />
        <Loading />
      </>
    );
  }
  if (hypothesis.isError) {
    return (
      <>
        <PageHeader title={`Hypothesis #${number}`} />
        <LoadError
          error={hypothesis.error}
          retry={hypothesis.refetch}
          notFound={`There is no hypothesis #${number} in ${project.title}, or you cannot read it.`}
        />
      </>
    );
  }
  const h = hypothesis.data;
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
            <StatusChip domain="hypothesis" value={h.state} className="text-sm" />
            <ImportedBadge
              origin={h.origin}
              sourceRef={h.source_ref}
              externalId={h.external_id}
              showSource
            />
            {isResearcher && pending ? (
              <Button asChild>
                <Link to={reviewPath(h.number)}>
                  <ScaleIcon aria-hidden="true" />
                  {REVIEW_BUTTON[pending.kind] ?? "Review"}
                </Link>
              </Button>
            ) : null}
            {isResearcher && h.state === "draft" ? (
              <Button asChild variant="outline">
                <Link to={`/hypotheses/${h.number}/edit`}>
                  <PencilLineIcon aria-hidden="true" />
                  Edit draft
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

        <IdeaSection hypothesis={h} />

        <Section
          title="Attempts"
          description="Each attempt is one try of this hypothesis. Failed attempts stay visible."
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
            <AssessmentSection project={slug} report={report.data} />
          </>
        ) : latest !== null && report.isError ? (
          <LoadError error={report.error} retry={report.refetch} />
        ) : null}

        <Section
          title="Decisions"
          description="Every decision a researcher recorded, with the reason."
        >
          <DecisionList reviews={h.reviews} />
        </Section>

        <RelatedSection hypothesis={h} />

        <CommentsSection project={slug} target={{ number: h.number }} />

        <HypothesisDetails project={slug} hypothesis={h} />
      </div>
    </>
  );
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function IdeaSection({ hypothesis }: { hypothesis: Hypothesis }) {
  const doc = hypothesis.document as Record<string, unknown>;
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
      <Link to={hypothesisPath(link.number, link.project)} className="font-medium hover:underline">
        {link.ref} {link.title}
      </Link>
      <StatusChip domain="hypothesis" value={link.state} />
    </li>
  );
}

function RelatedSection({ hypothesis }: { hypothesis: Hypothesis }) {
  if (hypothesis.relations.length === 0 && hypothesis.backlinks.length === 0) return null;
  return (
    <Section title="Related hypotheses">
      <div className="flex flex-col gap-4">
        {hypothesis.relations.length > 0 ? (
          <ul className="flex flex-col gap-2">
            {hypothesis.relations.map((link) => (
              <LinkItem key={`${link.kind}-${link.ref}`} link={link} />
            ))}
          </ul>
        ) : null}
        {hypothesis.backlinks.length > 0 ? (
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">Mentioned by</h3>
            <ul className="flex flex-col gap-2">
              {hypothesis.backlinks.map((link) => (
                <li key={link.ref} className="text-sm">
                  <Link
                    to={hypothesisPath(link.number, link.project)}
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

function HypothesisDetails({ project, hypothesis }: { project: string; hypothesis: Hypothesis }) {
  return (
    <Collapsible summary="Details">
      <div className="flex flex-col gap-5">
        <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <Fact term="Draft revision">{hypothesis.revision}</Fact>
          <Fact term="Approved revision">{hypothesis.approved_revision ?? "—"}</Fact>
          <Fact term="Science revision">{hypothesis.science_revision}</Fact>
          <Fact term="Created">{formatDateTime(hypothesis.created_at)}</Fact>
          <Fact term="Created by">
            {hypothesis.origin === "imported"
              ? "Imported"
              : hypothesis.created_by.kind === "service"
                ? "An agent"
                : "A person"}
          </Fact>
          <Fact term="Approved">{formatDateTime(hypothesis.approved_at)}</Fact>
          <Fact term="Last change">{formatDateTime(hypothesis.updated_at)}</Fact>
          <Fact term="Track">
            <Link to={trackPath(hypothesis.track)} className="underline underline-offset-4">
              {hypothesis.track}
            </Link>
          </Fact>
          <Fact term="Identifier">
            <code className="text-xs break-all">{hypothesis.id}</code>
          </Fact>
        </dl>
        <RevisionHistory project={project} number={hypothesis.number} />
        <RawJson value={hypothesis.document} label="Hypothesis document" />
        {hypothesis.imported ? (
          <RawJson value={hypothesis.imported} label="As stated in the imported history" />
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
      <h3 className="text-sm font-medium">Draft revisions</h3>
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
