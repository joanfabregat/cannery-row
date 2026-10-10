/**
 * Plain language: internal names never reach the screen raw. Each state or
 * code the API returns maps to a short label, a tone (the chip's colors) and an
 * icon, so a status is always readable by its word and its icon, not only its
 * color.
 */

export type Tone = "neutral" | "info" | "attention" | "success" | "danger";

export type StatusIcon =
  | "draft"
  | "waiting"
  | "progress"
  | "review"
  | "success"
  | "failure"
  | "inconclusive"
  | "stopped"
  | "archived"
  | "verified"
  | "claimed"
  | "paused"
  | "imported";

export interface StatusMeta {
  label: string;
  tone: Tone;
  icon: StatusIcon;
}

const s = (label: string, tone: Tone, icon: StatusIcon): StatusMeta => ({ label, tone, icon });

const hypothesisState = {
  queued: s("Waiting to start", "neutral", "waiting"),
  active: s("In progress", "info", "progress"),
  documenting: s("Being written up", "info", "progress"),
  deciding: s("Needs a decision", "attention", "review"),
  promoted: s("Accepted", "success", "success"),
  rejected: s("Rejected", "danger", "failure"),
  inconclusive: s("Inconclusive", "neutral", "inconclusive"),
  failed: s("Failed", "danger", "failure"),
  cancelled: s("Cancelled", "neutral", "stopped"),
};

const attemptState = {
  claimed: s("Started", "info", "progress"),
  running: s("Running", "info", "progress"),
  verifying: s("Verifying", "info", "progress"),
  verified: s("Verified", "success", "verified"),
  failed: s("Failed", "danger", "failure"),
  cancelled: s("Cancelled", "neutral", "stopped"),
  // An imported run with no decision of its own (docs/import.md).
  unreviewed: s("Not reviewed", "neutral", "inconclusive"),
};

const trackState = {
  planning: s("Planning", "attention", "draft"),
  active: s("Active", "success", "progress"),
  paused: s("Paused", "attention", "paused"),
  archived: s("Archived", "neutral", "archived"),
};

/** Where a metric value comes from. */
const authority = {
  agent_claim: s("Reported by agent", "neutral", "claimed"),
  tester_verified: s("Verified", "success", "verified"),
  // An imported history's values: never measured by this project's verifier.
  imported_artifact: s("Imported from a run file", "info", "imported"),
  imported_transcribed: s("Imported from a document", "attention", "imported"),
};

const verdict = {
  pass: s("Passed", "success", "success"),
  fail: s("Did not pass", "danger", "failure"),
  inconclusive: s("Inconclusive", "neutral", "inconclusive"),
  unknown: s("Not measured", "neutral", "inconclusive"),
};

/** A compared value against its reference, in the metric's own direction. */
const standing = {
  better: s("Better", "success", "success"),
  worse: s("Worse", "danger", "failure"),
  same: s("Same", "neutral", "inconclusive"),
  unknown: s("Can't tell", "neutral", "inconclusive"),
};

/** A track plan revision, from its draft to the researcher's review. */
const planState = {
  draft: s("Draft", "neutral", "draft"),
  submitted: s("Waiting for review", "attention", "review"),
  approved: s("Approved", "success", "success"),
  sent_back: s("Sent back", "attention", "draft"),
  declined: s("Declined", "neutral", "stopped"),
};

const reviewState = {
  pending: s("Waiting for a decision", "attention", "review"),
  resolved: s("Decided", "success", "success"),
};

const jobState = {
  pending: s("Waiting", "neutral", "waiting"),
  claimed: s("In progress", "info", "progress"),
  completed: s("Done", "success", "success"),
  failed: s("Failed", "danger", "failure"),
  skipped: s("Skipped", "neutral", "stopped"),
};

/** A hypothesis's write-up, from its document job. */
const writeup = {
  pending: s("To write", "attention", "waiting"),
  claimed: s("Being written", "info", "progress"),
  written: s("Written", "success", "success"),
  skipped: s("Skipped", "neutral", "stopped"),
};

/** The effect of a decision, as a past-tense status: "Accepted", "Closed as failed". */
const decision = {
  approve: s("Approved", "success", "success"),
  decline: s("Declined", "neutral", "stopped"),
  promote: s("Accepted", "success", "success"),
  reject: s("Rejected", "danger", "failure"),
  inconclusive: s("Inconclusive", "neutral", "inconclusive"),
  retry: s("Tried again", "info", "progress"),
  stop: s("Stopped", "neutral", "stopped"),
  failed: s("Closed as failed", "danger", "failure"),
};

/** A concern about a track's plan: while open, it holds up the track's new work. */
const concern = {
  open: s("Open", "attention", "review"),
  answered: s("Answered by the plan", "success", "success"),
  dismissed: s("Dismissed", "neutral", "stopped"),
};

const token = {
  active: s("Active", "success", "success"),
  expired: s("Expired", "neutral", "stopped"),
  revoked: s("Revoked", "neutral", "stopped"),
};

const account = {
  active: s("Active", "success", "success"),
  disabled: s("Disabled", "neutral", "stopped"),
};

export const statusDomains = {
  hypothesis: hypothesisState,
  attempt: attemptState,
  track: trackState,
  plan: planState,
  authority,
  verdict,
  standing,
  review: reviewState,
  job: jobState,
  writeup,
  decision,
  concern,
  token,
  account,
} satisfies Record<string, Record<string, StatusMeta>>;

export type StatusDomain = keyof typeof statusDomains;

/** Labels that are not statuses: roles, decisions, review kinds, channels. */
export const labels = {
  role: {
    viewer: "Viewer",
    member: "Member",
    researcher: "Researcher",
    admin: "Administrator",
  },
  reviewKind: {
    decision: "Decision",
    failure: "Failure review",
    plan: "Plan review",
  },
  decision: {
    approve: "Approve",
    decline: "Decline",
    promote: "Accept",
    reject: "Reject",
    inconclusive: "Inconclusive",
    retry: "Try again",
    stop: "Stop",
    failed: "Close as failed",
  },
  channel: {
    ui: "Web app",
    api: "API",
    mcp: "Agent (MCP)",
    cli: "Command line",
    system: "Cannery Row",
  },
  /** Who produced a piece of evidence, or where a failure happened. */
  stage: {
    agent: "Experiment",
    verify: "Verification",
  },
  searchKind: {
    track: "Track",
    hypothesis: "Hypothesis",
    attempt: "Attempt",
    report: "Report",
    verification: "Verification report",
    decision_reason: "Decision reason",
    comment: "Comment",
  },
  searchFacet: {
    kind: "Type",
    project: "Project",
    track: "Track",
    hypothesis_state: "Hypothesis status",
    attempt_state: "Attempt status",
    verdict: "Verification verdict",
    decision: "Decision",
    actor: "Person or agent",
  },
  scope: {
    read: "Read",
    write: "Write",
  },
  serviceKind: {
    agent: "Agent",
    experimenter: "Experimenter (workflow runner)",
    verifier: "Verifier (verify runner)",
    decider: "Decider (decide runner)",
  },
  /** What a concern says about a track's plan. */
  concernKind: {
    wrong_assumption: "Wrong assumption",
    better_idea: "Better idea",
    blocker: "Blocker",
    other: "Other",
  },
  /** What a re-plan does with a unit already done or in flight. */
  alignment: {
    keep: "Keep",
    obsolete: "Obsolete",
    redo: "Redo",
  },
  relation: {
    derived_from: "Derived from",
    supersedes: "Supersedes",
    related_to: "Related to",
  },
  direction: {
    higher: "Higher is better",
    lower: "Lower is better",
  },
  /** What a verification compared a value against. */
  referenceKind: {
    paper: "Paper",
    benchmark: "Benchmark",
    promoted_attempt: "Best promoted result",
    baseline: "Baseline",
    manual: "Set by hand",
    other: "Other",
  },
  /** Where a compared value came from. */
  comparisonSource: {
    tester: "Measured by the verifier",
    evaluator: "Computed by the policy",
  },
  artifactRole: {
    report_asset: "Report image",
    candidate: "Candidate",
    log: "Log",
    step_log: "Step log",
    setup_log: "Setup log",
    validator_log: "Validator log",
  },
  /** Who runs a track's experiments. */
  trackMode: {
    agent: "Agent",
    workflow: "Workflow",
  },
  /** Who performs a verify run. */
  performer: {
    runner: "A verify runner",
    agent: "An agent or a researcher",
  },
  /** Why a verify run exists. */
  jobOrigin: {
    submission: "First run",
    auto_retry: "Automatic rerun",
    human_retry: "Rerun a researcher asked for",
  },
  chart: {
    line: "Line chart",
    scatter: "Scatter chart",
    bar: "Bar chart",
    table: "Table",
  },
} satisfies Record<string, Record<string, string>>;

export type LabelGroup = keyof typeof labels;

/** `some_internal_name` → "Some internal name": the fallback for a value we do not know yet. */
export function humanize(value: string): string {
  const words = value.replace(/[_-]+/g, " ").trim().toLowerCase();
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : "Unknown";
}

function lookup<T>(table: Record<string, T>, value: string): T | undefined {
  return Object.hasOwn(table, value) ? table[value] : undefined;
}

export function statusMeta(domain: StatusDomain, value: string): StatusMeta {
  return (
    lookup<StatusMeta>(statusDomains[domain], value) ?? {
      label: humanize(value),
      tone: "neutral",
      icon: "inconclusive",
    }
  );
}

export function statusLabel(domain: StatusDomain, value: string): string {
  return statusMeta(domain, value).label;
}

export function label(group: LabelGroup, value: string): string {
  return lookup<string>(labels[group], value) ?? humanize(value);
}
