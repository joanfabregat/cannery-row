import type { Schemas } from "@/api/client";

import { json, USER_ID } from "./render";

/** Typed builders for API responses, with plain defaults and per-test overrides. */

export const RESEARCHER_ID = "00000000-0000-4000-8000-0000000000aa";
const T0 = "2026-03-01T10:00:00Z";

let counter = 0;
function uuid(): string {
  counter += 1;
  return `00000000-0000-4000-9000-${counter.toString().padStart(12, "0")}`;
}

export function page<T>(items: T[], next_before: unknown = null) {
  return { items, next_before };
}

export function summary(
  overrides: Partial<Schemas["HypothesisSummary"]> = {},
): Schemas["HypothesisSummary"] {
  const number = overrides.number ?? 12;
  return {
    number,
    ref: `#${number}`,
    title: "Shorter prompts",
    track: "tokenizer",
    mode: "agent",
    state: "promoted",
    revision: 1,
    approved_revision: 1,
    created_by: { kind: "service", id: "codex" },
    created_at: T0,
    updated_at: T0,
    approved_at: T0,
    origin: "live",
    source_ref: null,
    external_id: null,
    imported: null,
    ...overrides,
  };
}

export function decision(overrides: Partial<Schemas["DecisionOut"]> = {}): Schemas["DecisionOut"] {
  return {
    id: uuid(),
    action: "promote",
    subject_revision: 1,
    reason: "The gain holds on every split",
    actor_user_id: RESEARCHER_ID,
    via_channel: "ui",
    via_client: null,
    decided_at: "2026-03-05T10:00:00Z",
    supersedes: null,
    origin: "live",
    source_ref: null,
    ...overrides,
  };
}

export type HypothesisReview = Schemas["cannery_row__hypotheses__routes__ReviewCaseOut"];

export function review(overrides: Partial<HypothesisReview> = {}): HypothesisReview {
  return {
    id: uuid(),
    kind: "result",
    subject_revision: 1,
    state: "resolved",
    opened_at: "2026-03-04T10:00:00Z",
    resolved_at: null,
    origin: "live",
    source_ref: null,
    decisions: [],
    ...overrides,
  };
}

export function hypothesis(
  overrides: Partial<Schemas["HypothesisOut"]> = {},
): Schemas["HypothesisOut"] {
  const base = summary(overrides);
  return {
    ...base,
    id: uuid(),
    project: "sardines",
    document: {
      schema_version: "0.2",
      track: base.track,
      title: base.title,
      question: "Do shorter prompts keep quality?",
      rationale: "Long prompts cost tokens.",
      intervention: "Cut the system prompt in half",
      plan: {
        selection_splits: ["dev"],
        confirmation_splits: ["test"],
        primary_metric: "accuracy",
        required_slices: [],
        regression_gates: [],
        compute_budget: { runs_max: 1 },
        success_criteria: "Accuracy holds",
        falsification_criteria: "Accuracy drops",
      },
    },
    science_revision: 1,
    relations: [],
    backlinks: [],
    reviews: [],
    ...overrides,
  };
}

export function attempt(
  overrides: Partial<Schemas["AttemptDetail"]> = {},
): Schemas["AttemptDetail"] {
  const number = overrides.number ?? 12;
  const sequence = overrides.sequence ?? 1;
  return {
    id: uuid(),
    ref: `#${number}.${sequence}`,
    number,
    sequence,
    state: "promoted",
    track: "tokenizer",
    mode: "agent",
    workflow: null,
    hypothesis_revision: 1,
    science_revision: 1,
    producer: null,
    claimed_by: { kind: "service", id: "codex" },
    via_channel: "api",
    via_client: null,
    predecessor_id: null,
    lease_generation: 1,
    lease_expires_at: null,
    claimed_at: "2026-03-02T10:00:00Z",
    started_at: "2026-03-02T10:00:00Z",
    submitted_at: "2026-03-03T10:00:00Z",
    finished_at: "2026-03-04T10:00:00Z",
    origin: "live",
    source_ref: null,
    imported: null,
    artifacts: [],
    failures: [],
    claimed_sheet: null,
    ...overrides,
  };
}

export function report(overrides: Partial<Schemas["ReportOut"]> = {}): Schemas["ReportOut"] {
  return {
    id: uuid(),
    attempt_ref: "#12.1",
    hypothesis: 12,
    hypothesis_title: "Shorter prompts",
    track: "tokenizer",
    attempt_state: "promoted",
    science_revision: 1,
    status: "accepted",
    origin: "live",
    source_ref: null,
    report: nativeReport(),
    claimed_measurements: [
      {
        metric: "accuracy",
        value: 0.95,
        authority: "agent_claim",
        unit: "ratio",
        direction: "higher",
        split: "test",
      },
    ],
    submitted_at: "2026-03-03T10:00:00Z",
    author: { kind: "service", id: "codex" },
    tester: {
      status: "accepted",
      observations: null,
      discrepancies: [],
      measurements: [
        {
          metric: "accuracy",
          value: 0.91,
          authority: "tester_verified",
          unit: "ratio",
          direction: "higher",
          split: "test",
          control_value: 0.9,
        },
      ],
      published_at: "2026-03-03T12:00:00Z",
    },
    evaluation: {
      status: "accepted",
      verdict: "pass",
      reason: "Accuracy held within the margin",
      policy_revision: "1",
      gates: [],
      comparisons: [],
      producer: { kind: "service", id: "judge" },
      published_at: "2026-03-04T10:00:00Z",
    },
    decisions: [],
    assets: [],
    ...overrides,
  };
}

export type ReviewCase = Schemas["cannery_row__reviews__routes__ReviewCaseOut"];

export function reviewCase(overrides: Partial<ReviewCase> = {}): ReviewCase {
  return {
    id: uuid(),
    kind: "result",
    state: "pending",
    subject_revision: 3,
    hypothesis: 12,
    hypothesis_ref: "#12",
    hypothesis_state: "awaiting_human_review",
    attempt_ref: "#12.1",
    attempt_state: "promoted",
    opened_at: "2026-03-04T10:00:00Z",
    resolved_at: null,
    origin: "live",
    source_ref: null,
    failure: null,
    evaluation: evaluation(),
    decisions: [],
    ...overrides,
  };
}

export function comment(overrides: Partial<Schemas["CommentOut"]> = {}): Schemas["CommentOut"] {
  return {
    id: uuid(),
    hypothesis: 12,
    hypothesis_ref: "#12",
    attempt_ref: null,
    author_user_id: USER_ID,
    body_markdown: "Looks good",
    revision: 1,
    created_at: "2026-03-05T11:00:00Z",
    edited_at: null,
    ...overrides,
  };
}

export function track(overrides: Partial<Schemas["TrackOut"]> = {}): Schemas["TrackOut"] {
  return {
    id: uuid(),
    slug: "tokenizer",
    title: "Tokenizer",
    description: "Cheaper tokens",
    producer: null,
    mode: "agent",
    workflow: null,
    state: "active",
    revision: 1,
    created_at: T0,
    updated_at: T0,
    ...overrides,
  };
}

export function token(overrides: Partial<Schemas["TokenOut"]> = {}): Schemas["TokenOut"] {
  return {
    id: uuid(),
    kind: "personal",
    name: "laptop CLI",
    display_prefix: "cr_abc",
    scopes: ["read"],
    created_at: T0,
    expires_at: "2099-01-01T00:00:00Z",
    last_used_at: null,
    revoked_at: null,
    ...overrides,
  };
}

export function member(overrides: Partial<Schemas["MemberOut"]> = {}): Schemas["MemberOut"] {
  return {
    user_id: RESEARCHER_ID,
    email: "grace@example.com",
    display_name: "Grace Hopper",
    role: "researcher",
    granted_at: T0,
    ...overrides,
  };
}

export function attention(
  overrides: Partial<Schemas["AttentionOut"]> = {},
): Schemas["AttentionOut"] {
  return {
    pending_counts: { result: 0, failure: 0 },
    pending_reviews: [],
    running_count: 0,
    running: [],
    recent_outcomes: [],
    recent_failures: [],
    stalled_evaluation_count: 0,
    stalled_evaluations: [],
    ...overrides,
  };
}

export function searchHit(overrides: Partial<Schemas["SearchHit"]> = {}): Schemas["SearchHit"] {
  return {
    kind: "hypothesis",
    source_id: uuid(),
    project: "sardines",
    ref: "sardines#12",
    hypothesis: 12,
    attempt_ref: null,
    title: "Shorter prompts",
    snippet: "Cut the \u0001tokenizer\u0002 prompt",
    track: "tokenizer",
    hypothesis_state: "promoted",
    attempt_state: null,
    verdict: null,
    decision: null,
    actor: null,
    occurred_at: T0,
    origin: "live",
    score: 1,
    ...overrides,
  };
}

/** A verified upload; `key` is its storage key, whose last part is its file name. */
export function artifact(
  key: string,
  overrides: Partial<Schemas["ArtifactOut"]> = {},
): Schemas["ArtifactOut"] {
  return {
    id: uuid(),
    role: "step_log",
    storage: { backend: "s3", bucket: "cannery", key },
    size_bytes: 120,
    sha256: "a".repeat(64),
    media_type: "text/plain",
    verified_at: T0,
    origin: "live",
    ...overrides,
  };
}

const ATTEMPT_PREFIX = "projects/p1/attempts/a1";

/** A finished test run of two steps, claimed by a runner, with a log per step. */
export function job(overrides: Partial<Schemas["JobOut"]> = {}): Schemas["JobOut"] {
  const id = overrides.id ?? uuid();
  const stage = overrides.stage ?? "tester";
  const prefix = `${ATTEMPT_PREFIX}/${stage === "tester" ? "test" : "eval"}-runs/${id}/`;
  return {
    id,
    attempt_id: uuid(),
    stage,
    run_number: 1,
    origin: "submission",
    previous_run_id: null,
    state: "completed",
    science_revision: 1,
    tester: "cannery-runner",
    track: "tokenizer",
    steps: [
      { name: "overlap-producer", revision: 1 },
      { name: "fixture-scorer", revision: "1" },
    ],
    parameters: stage === "tester" ? null : {},
    output_prefix: prefix,
    created_at: "2026-03-03T10:00:00Z",
    claimed_at: "2026-03-03T10:01:00Z",
    claimed_by: uuid(),
    via_client: "token:gke-runner",
    deadline: "2026-03-03T11:01:00Z",
    finished_at: "2026-03-03T10:20:00Z",
    lease_generation: 1,
    lease_expires_at: null,
    error_step: null,
    error_code: null,
    error_reason: null,
    logs: [],
    evidence: null,
    outputs: [
      artifact(`${prefix}overlap-producer/step_log/overlap-producer.log`),
      artifact(`${prefix}overlap-producer/run/run.json`, { role: "run" }),
      artifact(`${prefix}fixture-scorer/step_log/fixture-scorer.log`),
    ],
    ...overrides,
  };
}

type Handler = (request: Request) => Response | Promise<Response>;

/** The requests a hypothesis page makes, answered from these records. */
export function hypothesisApi(
  h: Schemas["HypothesisOut"],
  {
    attempts = [],
    reports = {},
    comments = [],
    members = [member()],
    jobs = {},
    project = "sardines",
  }: {
    attempts?: Schemas["AttemptDetail"][];
    reports?: Record<number, Schemas["ReportOut"]>;
    comments?: Schemas["CommentOut"][];
    members?: Schemas["MemberOut"][];
    /** Each attempt's test and evaluation runs, by sequence. */
    jobs?: Record<number, Schemas["JobOut"][]>;
    project?: string;
  } = {},
): Record<string, Handler> {
  const base = `/api/projects/${project}/hypotheses/${h.number}`;
  const handlers: Record<string, Handler> = {
    [`GET ${base}`]: () => json(h),
    [`GET ${base}/attempts`]: () => json(page(attempts)),
    [`GET ${base}/comments`]: () => json(page(comments)),
    [`GET ${base}/revisions`]: () => json(page([])),
    [`GET /api/projects/${project}/members`]: () => json(page(members)),
  };
  for (const a of attempts) {
    handlers[`GET ${base}/attempts/${a.sequence}`] = () => json(a);
    handlers[`GET ${base}/attempts/${a.sequence}/jobs`] = () => json(page(jobs[a.sequence] ?? []));
    handlers[`GET ${base}/attempts/${a.sequence}/comments`] = () => json(page([]));
    const r = reports[a.sequence];
    handlers[`GET ${base}/attempts/${a.sequence}/report`] = () =>
      r
        ? json(r)
        : json({ error: { code: "not_found", message: "no report", details: null } }, 404);
  }
  return handlers;
}

/** A hypothesis a researcher accepted, with its attempt and report. */
export function promotedHypothesis() {
  const h = hypothesis({
    state: "promoted",
    reviews: [review({ kind: "result", decisions: [decision()] })],
  });
  return { h, attempts: [attempt()], reports: { 1: report() } };
}

export function nativeReport(
  overrides: Partial<Schemas["RequestEvidenceEnvelopeReport"]> = {},
): Schemas["RequestEvidenceEnvelopeReport"] {
  return {
    what_was_tried: "Halved the system prompt",
    configuration: "Shorter system prompt",
    observations: "Accuracy remained stable",
    findings: "Accuracy held at **91%**.",
    limitations: "One data set",
    next_question: "Does it hold on the next data set?",
    elapsed_seconds: 1,
    body_markdown: "Details are recorded in the findings.",
    ...overrides,
  };
}
export function metric(
  overrides: Partial<Schemas["RequestScienceRevisionMetric"]> = {},
): Schemas["RequestScienceRevisionMetric"] {
  return {
    key: "accuracy",
    unit: "ratio",
    direction: "higher",
    aggregation: "mean",
    dimensions: [],
    splits: ["test"],
    required_slices: [],
    ...overrides,
  };
}
export function dashboardView(
  overrides: Partial<Schemas["RequestDashboardViewsView"]> = {},
): Schemas["RequestDashboardViewsView"] {
  return {
    id: "accuracy",
    title: "Accuracy",
    chart: "line",
    metric: "accuracy",
    split: "test",
    ...overrides,
  };
}
export function evaluation(
  overrides: Partial<Schemas["EvidenceEnvelopeRequest"]> = {},
): Schemas["EvidenceEnvelopeRequest"] {
  return {
    schema_version: "0.2",
    attempt_id: "00000000-0000-4000-8000-000000000012",
    stage: "evaluator",
    status: "completed",
    producer: { kind: "service", id: "judge" },
    started_at: "2026-03-04T09:00:00Z",
    finished_at: "2026-03-04T10:00:00Z",
    provenance: { source_revision: "1", science_revision: "1" },
    assessment: assessment(),
    ...overrides,
  };
}

export function assessment(
  overrides: Partial<Schemas["RequestEvidenceEnvelopeAssessment"]> = {},
): Schemas["RequestEvidenceEnvelopeAssessment"] {
  return {
    policy_revision: "1",
    verdict: "pass",
    reason: "Accuracy held",
    gates: [{ id: "accuracy_gate", result: "pass" }],
    evidence: [{ ref: "verified_evidence", sha256: "0".repeat(64) }],
    ...overrides,
  };
}
