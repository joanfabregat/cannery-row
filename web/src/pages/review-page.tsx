import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { projectKey, useHypothesis, useReport, useReviewCase, useWriteup } from "@/api/queries";
import type {
  Discrepancy,
  GateResult,
  Hypothesis,
  HypothesisReview,
  Measurement,
} from "@/api/types";
import { type DecisionChoice, DecisionForm } from "@/components/decision-form";
import { Assessment } from "@/components/assessment";
import { MeasurementsTable } from "@/components/evidence";
import { ImportedBadge } from "@/components/imported-badge";
import { Markdown } from "@/components/markdown";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading, QueryView } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { WriteupView } from "@/components/writeup";
import { type ContentRef, decisionDocument, type Outcome } from "@/lib/documents";
import { asComparisons, judgedBy } from "@/lib/comparisons";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { parseNumber } from "@/lib/navigation";
import { pendingReview } from "@/lib/outcome";
import { hypothesisPath, parseAttemptRef } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

/**
 * The one-screen review of whatever waits for a researcher on a hypothesis:
 * its decision, after the write-up, or its failure. The write-up, the evidence, the
 * verification's verdict, checks and comparisons, the decisions and a required
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
      {pending.kind === "decision" ? (
        <DecisionReview project={project.slug} hypothesis={h} review={pending} />
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

/** A failure case is decided with an action, the failure revision and a reason. */
function decideFailure(project: string, caseId: string, revision: number) {
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

/** A decision case is decided with a decision document citing what it decided on. */
function decideOutcome(
  project: string,
  caseId: string,
  verification: ContentRef,
  writeup: ContentRef,
) {
  return async (outcome: Outcome, reason: string, key: string) =>
    unwrap(
      await api.POST("/api/projects/{slug}/review-cases/{case_id}/decisions", {
        params: {
          path: { slug: project, case_id: caseId },
          header: { "idempotency-key": key },
        },
        body: {
          review_case_id: caseId,
          document: decisionDocument(outcome, verification, writeup, reason),
        },
      }),
    );
}

function asList<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function DecisionReview({
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
  const writeup = useWriteup(project, hypothesis.number);
  return (
    <QueryView query={reviewCase}>
      {(found) => {
        // The verification report the case is about: its front matter holds the verdict.
        const verified = found.verification?.front_matter ?? {};
        const published = report.data?.verification ?? null;
        const verdict = text(verified.verdict) ?? "unknown";
        const subject = found.attempt_ref ?? hypothesis.ref;
        const w = writeup.data ?? null;
        // What the decision cites: the verification report and the write-up, null when there is none.
        const citedVerification = w?.inputs?.verification ?? null;
        const citedWriteup =
          w?.status === "written" && w.writeup
            ? { ref: w.writeup.id, sha256: w.writeup.sha256 }
            : null;
        const stopped = found.verification === null;
        const choices: DecisionChoice<Outcome>[] = stopped
          ? [
              {
                action: "failed",
                label: "Close as failed",
                variant: "destructive",
                effect: `${hypothesis.ref} will be closed as failed and moves to the archive. This is not a scientific rejection; the failed attempts stay readable.`,
              },
            ]
          : [
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
              title="Write-up"
              description="What was done across every attempt, written up before the decision."
            >
              {writeup.isPending ? (
                <Loading />
              ) : writeup.isError ? (
                <LoadError error={writeup.error} retry={writeup.refetch} />
              ) : w === null ? (
                <p className="text-sm text-muted-foreground">No write-up was found.</p>
              ) : (
                <WriteupView writeup={w} />
              )}
            </Section>
            <Section
              title="What happened"
              description={
                imported
                  ? `Attempt ${subject}, as recorded in the history this project was imported from: no agent reported it and this project's verifier did not measure it.`
                  : `Attempt ${subject}, as reported by the agent and measured by the verifier.`
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
                    verified={asList<Measurement>(report.data.verification?.measurements)}
                    claimed={asList<Measurement>(report.data.claimed_measurements)}
                  />
                  {asList<Discrepancy>(report.data.verification?.discrepancies).length > 0 ? (
                    <div className="flex flex-col gap-2">
                      <h3 className="font-medium">Disagreements found by the verifier</h3>
                      <ul className="flex list-disc flex-col gap-1 pl-5 text-sm">
                        {asList<Discrepancy>(report.data.verification?.discrepancies).map(
                          (d, i) => (
                            <li key={i}>{d.description}</li>
                          ),
                        )}
                      </ul>
                    </div>
                  ) : null}
                  {"body_markdown" in report.data.report &&
                  text(report.data.report.body_markdown) ? (
                    <Collapsible
                      summary={"what_was_tried" in report.data.report ? "Full report" : "Run notes"}
                    >
                      <Markdown>{report.data.report.body_markdown}</Markdown>
                    </Collapsible>
                  ) : null}
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">No report was found.</p>
              )}
            </Section>
            {stopped ? (
              <Section title="Verification verdict">
                <p className="text-sm text-muted-foreground">
                  {hypothesis.ref} was stopped after a failure: no verification report was
                  published, and the only decision is to close it as failed.
                </p>
              </Section>
            ) : (
              <Section
                title="Verification verdict"
                description={
                  imported
                    ? "The verdict the imported history recorded, under the rules it names; the decision is yours."
                    : "The verifier applies the policy this project registers; the decision is yours."
                }
              >
                <div className="flex flex-col gap-4">
                  <Assessment
                    project={project}
                    scienceRevision={report.data?.science_revision ?? null}
                    measurements={asList<Measurement>(verified.measurements)}
                    verdict={verdict}
                    reason={text(verified.reason)}
                    judged={judgedBy(published?.producer, text(verified.policy_revision))}
                    at={published?.published_at}
                    gates={asList<GateResult>(verified.gates)}
                    comparisons={asComparisons(verified.comparisons)}
                    reportLoading={ref !== null && report.isPending}
                  />
                  {verdict !== "pass" ? (
                    <p className="text-sm text-muted-foreground">
                      Accepting is not possible: only a result the verification passed can be
                      accepted.
                    </p>
                  ) : null}
                </div>
              </Section>
            )}
            <Section title="Your decision">
              <DecisionForm
                subject={subject}
                choices={choices}
                submit={decideOutcome(project, found.id, citedVerification, citedWriteup)}
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
              : `The ${label("stage", failure?.stage ?? "verify").toLowerCase()} of attempt ${subject} will run again on the same submission.`,
          },
          {
            action: "stop",
            label: "Stop",
            variant: "destructive",
            effect: `${hypothesis.ref} stops here: it is written up, then closed as failed. This is not a scientific rejection; the failed attempt stays readable.`,
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
                submit={decideFailure(project, found.id, found.subject_revision)}
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
