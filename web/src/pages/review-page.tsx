import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { projectKey, useHypothesis, useReport, useReviewCase } from "@/api/queries";
import type {
  Discrepancy,
  GateResult,
  Hypothesis,
  HypothesisReview,
  Measurement,
} from "@/api/types";
import { type DecisionChoice, DecisionForm } from "@/components/decision-form";
import { DecisionList } from "@/components/decisions";
import { Assessment } from "@/components/assessment";
import { MeasurementsTable } from "@/components/evidence";
import { ImportedBadge } from "@/components/imported-badge";
import { Markdown } from "@/components/markdown";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading, QueryView } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { asComparisons, judgedBy } from "@/lib/comparisons";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { parseNumber } from "@/lib/navigation";
import { controlText, pendingReview } from "@/lib/outcome";
import { hypothesisPath, parseAttemptRef } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

/**
 * The one-screen review of whatever waits for a researcher on a hypothesis:
 * its draft, its result or its failure. The evidence summary, the
 * evaluator's verdict, checks and comparisons, the decisions and a required
 * reason.
 */
export function ReviewPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  if (number === null) {
    return (
      <>
        <PageHeader title="Review" />
        <EmptyState>This address does not name a hypothesis.</EmptyState>
      </>
    );
  }
  return <ProjectPage>{(project) => <ReviewView project={project} number={number} />}</ProjectPage>;
}

function ReviewView({ project, number }: { project: Project; number: number }) {
  const { isResearcher } = usePermissions();
  const hypothesis = useHypothesis(project.slug, number);
  if (!isResearcher) {
    return (
      <>
        <PageHeader title={`Review #${number}`} />
        <EmptyState>
          Only researchers record decisions.{" "}
          <Link to={hypothesisPath(number)} className="font-medium underline underline-offset-4">
            Read hypothesis #{number}
          </Link>
          .
        </EmptyState>
      </>
    );
  }
  if (hypothesis.isPending) {
    return (
      <>
        <PageHeader title={`Review #${number}`} />
        <Loading />
      </>
    );
  }
  if (hypothesis.isError) {
    return (
      <>
        <PageHeader title={`Review #${number}`} />
        <LoadError error={hypothesis.error} retry={hypothesis.refetch} />
      </>
    );
  }
  const h = hypothesis.data;
  const pending = pendingReview(h);
  if (pending === undefined) {
    return (
      <>
        <PageHeader title={`Review #${number}`} />
        <EmptyState>
          Nothing on #{number} is waiting for a decision.{" "}
          <Link to={hypothesisPath(number)} className="font-medium underline underline-offset-4">
            Go to the hypothesis
          </Link>
          .
        </EmptyState>
      </>
    );
  }
  const title = `${label("reviewKind", pending.kind)}: ${h.ref} ${h.title}`;
  return (
    <>
      <PageHeader
        title={title}
        description={`Track ${h.track} · waiting since ${formatDateTime(pending.opened_at)}`}
        actions={<StatusChip domain="hypothesis" value={h.state} className="text-sm" />}
      />
      <p className="mb-6 text-sm">
        <Link to={hypothesisPath(number)} className="font-medium underline underline-offset-4">
          Open the full hypothesis page
        </Link>
      </p>
      {pending.kind === "draft" ? (
        <DraftReview project={project.slug} hypothesis={h} />
      ) : pending.kind === "result" ? (
        <ResultReview project={project.slug} hypothesis={h} review={pending} />
      ) : (
        <FailureReview project={project.slug} hypothesis={h} review={pending} />
      )}
    </>
  );
}

/** After a decision: refresh the project's pages and go back to the hypothesis. */
function useAfterDecision(project: string, number: number) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  return {
    done: async (choice: DecisionChoice) => {
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
      await navigate(hypothesisPath(number), {
        state: { notice: `Your decision (${choice.label}) was recorded.` },
      });
    },
    reload: async () => {
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    },
  };
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function DraftReview({ project, hypothesis }: { project: string; hypothesis: Hypothesis }) {
  const after = useAfterDecision(project, hypothesis.number);
  const doc = hypothesis.document as Record<string, unknown>;
  const plan = (typeof doc.plan === "object" && doc.plan !== null ? doc.plan : {}) as Record<
    string,
    unknown
  >;
  const ref = hypothesis.ref;
  const choices: DecisionChoice<components["schemas"]["DraftReviewRequestAction"]>[] = [
    {
      action: "approve",
      label: "Approve",
      variant: "default",
      effect: `${ref} will be queued: an agent can then claim it and try it. Revision ${hypothesis.revision} is the one that will be tried.`,
    },
    {
      action: "request_revision",
      label: "Ask for changes",
      effect: `${ref} stays a draft. It must be revised before it can be reviewed again.`,
    },
    {
      action: "decline",
      label: "Decline",
      variant: "destructive",
      effect: `${ref} will not be tried. It moves to the archive, where it stays readable.`,
    },
  ];
  return (
    <div className="flex flex-col gap-6">
      <Section
        title="The draft"
        description={`You are reviewing revision ${hypothesis.revision}, the current one.`}
      >
        <dl className="grid gap-4 md:grid-cols-2">
          {(
            [
              ["Question", text(doc.question)],
              ["Why try it", text(doc.rationale)],
              ["What changes", text(doc.intervention)],
              ["Compared with", controlText(doc)],
              ["Measured by", text(plan.primary_metric)],
              ["Success looks like", text(plan.success_criteria)],
              ["It is disproved if", text(plan.falsification_criteria)],
            ] as [string, string | null][]
          ).map(([term, value]) =>
            value === null ? null : (
              <Fact key={term} term={term}>
                {value}
              </Fact>
            ),
          )}
        </dl>
        <Collapsible summary="The full draft" className="mt-4">
          <RawJson value={hypothesis.document} label="Draft document" />
        </Collapsible>
      </Section>
      <Section title="Earlier decisions">
        <DecisionList reviews={hypothesis.reviews} />
      </Section>
      <Section title="Your decision">
        <DecisionForm
          subject={ref}
          choices={choices}
          submit={async (action, reason, key) =>
            unwrap(
              await api.POST("/api/projects/{slug}/hypotheses/{number}/draft-review", {
                params: {
                  path: { slug: project, number: hypothesis.number },
                  header: { "idempotency-key": key },
                },
                body: { draft_revision: hypothesis.revision, action, reason },
              }),
            )
          }
          onDone={after.done}
          onStale={after.reload}
        />
      </Section>
    </div>
  );
}

function decideCase(project: string, caseId: string, revision: number) {
  return async (
    action: components["schemas"]["HumanDecisionRequestAction"],
    reason: string,
    key: string,
  ) =>
    unwrap(
      await api.POST("/api/projects/{slug}/review-cases/{case_id}/decisions", {
        params: {
          path: { slug: project, case_id: caseId },
          header: { "idempotency-key": key },
        },
        body: { review_case_id: caseId, evidence_revision: revision, action, reason },
      }),
    );
}

function asList<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function ResultReview({
  project,
  hypothesis,
  review,
}: {
  project: string;
  hypothesis: Hypothesis;
  review: HypothesisReview;
}) {
  const after = useAfterDecision(project, hypothesis.number);
  const reviewCase = useReviewCase(project, review.id);
  const ref = parseAttemptRef(reviewCase.data?.attempt_ref);
  const report = useReport(project, hypothesis.number, ref?.[1] ?? null);
  return (
    <QueryView query={reviewCase}>
      {(found) => {
        const evaluation = (found.evaluation ?? {}) as {
          assessment?: Record<string, unknown>;
          producer?: { kind?: unknown; id?: unknown };
          finished_at?: unknown;
        };
        const assessment = evaluation.assessment ?? {};
        const verdict = text(assessment.verdict) ?? "unknown";
        const subject = found.attempt_ref ?? hypothesis.ref;
        const choices: DecisionChoice<components["schemas"]["HumanDecisionRequestAction"]>[] = [
          ...(verdict === "pass"
            ? [
                {
                  action: "promote" as const,
                  label: "Accept",
                  variant: "default" as const,
                  effect: `${hypothesis.ref} will be marked as accepted. This accepts the research result only: it changes no baseline and starts nothing.`,
                },
              ]
            : []),
          {
            action: "reject",
            label: "Reject",
            variant: "destructive",
            effect: `${hypothesis.ref} will be marked as rejected and moves to the archive. The attempt and its evidence stay readable.`,
          },
          {
            action: "inconclusive",
            label: "Inconclusive",
            effect: `${hypothesis.ref} will be marked as inconclusive and moves to the archive. The attempt and its evidence stay readable.`,
          },
        ];
        const imported = report.data?.origin === "imported";
        return (
          <div className="flex flex-col gap-6">
            <Section
              title="What happened"
              description={
                imported
                  ? `Attempt ${subject}, as recorded in the history this project was imported from: no agent reported it and this project's tester did not measure it.`
                  : `Attempt ${subject}, as reported by the agent and measured by the tester.`
              }
            >
              {report.isPending && ref !== null ? (
                <Loading />
              ) : report.isError ? (
                <LoadError error={report.error} retry={report.refetch} />
              ) : report.data ? (
                <div className="flex flex-col gap-5">
                  {imported ? (
                    <ImportedBadge
                      origin={report.data.origin}
                      sourceRef={report.data.source_ref}
                      showSource
                    />
                  ) : null}
                  {"findings" in report.data.report && text(report.data.report.findings) ? (
                    <Fact term="Findings (reported by the agent)">
                      <Markdown>{report.data.report.findings}</Markdown>
                    </Fact>
                  ) : null}
                  <MeasurementsTable
                    verified={asList<Measurement>(report.data.tester?.measurements)}
                    claimed={asList<Measurement>(report.data.claimed_measurements)}
                  />
                  {asList<Discrepancy>(report.data.tester?.discrepancies).length > 0 ? (
                    <div className="flex flex-col gap-2">
                      <h3 className="font-medium">Disagreements found by the tester</h3>
                      <ul className="flex list-disc flex-col gap-1 pl-5 text-sm">
                        {asList<Discrepancy>(report.data.tester?.discrepancies).map((d, i) => (
                          <li key={i}>{d.description}</li>
                        ))}
                      </ul>
                    </div>
                  ) : null}
                  {"body_markdown" in report.data.report &&
                  text(report.data.report.body_markdown) ? (
                    <Collapsible summary="Full report">
                      <Markdown>{report.data.report.body_markdown}</Markdown>
                    </Collapsible>
                  ) : null}
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">No report was found.</p>
              )}
            </Section>
            <Section
              title="Evaluator verdict"
              description={
                imported
                  ? "The verdict the imported history recorded, under the rules it names; the decision is yours."
                  : "The evaluator registered for this project applies its own rules; the decision is yours."
              }
            >
              <div className="flex flex-col gap-4">
                <Assessment
                  project={project}
                  scienceRevision={report.data?.science_revision ?? null}
                  measurements={asList<Measurement>(report.data?.tester?.measurements)}
                  verdict={verdict}
                  reason={text(assessment.reason)}
                  judged={judgedBy(evaluation.producer, text(assessment.policy_revision))}
                  at={text(evaluation.finished_at) ?? undefined}
                  gates={asList<GateResult>(assessment.gates)}
                  comparisons={asComparisons(assessment.comparisons)}
                  reportLoading={ref !== null && report.isPending}
                />
                {verdict !== "pass" ? (
                  <p className="text-sm text-muted-foreground">
                    Accepting is not possible: only a result the evaluator passed can be accepted.
                  </p>
                ) : null}
              </div>
            </Section>
            <Section title="Your decision">
              <DecisionForm
                subject={subject}
                choices={choices}
                submit={decideCase(project, found.id, found.subject_revision)}
                onDone={after.done}
                onStale={after.reload}
              />
            </Section>
          </div>
        );
      }}
    </QueryView>
  );
}

function FailureReview({
  project,
  hypothesis,
  review,
}: {
  project: string;
  hypothesis: Hypothesis;
  review: HypothesisReview;
}) {
  const after = useAfterDecision(project, hypothesis.number);
  const reviewCase = useReviewCase(project, review.id);
  return (
    <QueryView query={reviewCase}>
      {(found) => {
        const failure = found.failure;
        const subject = found.attempt_ref ?? hypothesis.ref;
        const agentSide = failure?.stage === "agent";
        const choices: DecisionChoice<components["schemas"]["HumanDecisionRequestAction"]>[] = [
          {
            action: "retry",
            label: "Try again",
            variant: "default",
            effect: agentSide
              ? `${hypothesis.ref} goes back to the queue: an agent can claim it again, which starts a new attempt.`
              : `The ${label("stage", failure?.stage ?? "tester").toLowerCase()} of attempt ${subject} will run again on the same submission.`,
          },
          {
            action: "close_failed",
            label: "Close as failed",
            variant: "destructive",
            effect: `${hypothesis.ref} will be closed as failed and moves to the archive. This is not a scientific rejection; the failed attempt stays readable.`,
          },
        ];
        return (
          <div className="flex flex-col gap-6">
            <Section
              title="What went wrong"
              description={`Attempt ${subject} could not produce a result.`}
            >
              {failure ? (
                <dl className="grid gap-4 md:grid-cols-2">
                  <Fact term="Where">{label("stage", failure.stage)}</Fact>
                  <Fact term="When">{formatDateTime(failure.created_at)}</Fact>
                  <Fact term="Reason">{failure.reason}</Fact>
                  <Fact term="Code">{failure.code}</Fact>
                </dl>
              ) : (
                <p className="text-sm text-muted-foreground">No failure detail was recorded.</p>
              )}
              {failure &&
              (Object.keys(failure.details).length > 0 || failure.log_refs.length > 0) ? (
                <Collapsible summary="Technical details" className="mt-4">
                  <RawJson
                    value={{ details: failure.details, logs: failure.log_refs }}
                    label="Failure details"
                  />
                </Collapsible>
              ) : null}
            </Section>
            <Section title="Your decision">
              <DecisionForm
                subject={subject}
                choices={choices}
                submit={decideCase(project, found.id, found.subject_revision)}
                onDone={after.done}
                onStale={after.reload}
              />
            </Section>
          </div>
        );
      }}
    </QueryView>
  );
}
