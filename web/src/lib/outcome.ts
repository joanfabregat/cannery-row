import type {
  Attempt,
  AttemptDetail,
  Decision,
  Hypothesis,
  HypothesisReview,
  Report,
} from "@/api/types";

import { formatDate } from "./format";
import { statusLabel } from "./labels";
import { plainText } from "./plain-text";

/**
 * A hypothesis page answers first, in one sentence, "what was tried, what
 * happened, what was decided and why". The sentence is built from the
 * hypothesis, its latest attempt, that attempt's report and verification
 * verdict, and the decision in force with its reason. The parts are also
 * returned on their own for the summary under the sentence.
 */
export interface OutcomeSummary {
  sentence: string;
  tried: string;
  happened: string | null;
  /** The decision in force, as an action (`promote`, `failed`…), if any. */
  decision: Decision | null;
  decided: string | null;
  why: string | null;
}

/** The decision a later correction has not superseded. */
export function currentDecision(review: HypothesisReview | undefined): Decision | null {
  if (review === undefined) return null;
  const superseded = new Set(review.decisions.map((d) => d.supersedes).filter(Boolean));
  const live = review.decisions.filter((d) => !superseded.has(d.id));
  return live.at(-1) ?? null;
}

export function latestReview(
  hypothesis: Hypothesis,
  kind: "decision" | "failure",
): HypothesisReview | undefined {
  return hypothesis.reviews.filter((r) => r.kind === kind).at(-1);
}

/** The review a researcher still has to decide, if any. */
export function pendingReview(hypothesis: Hypothesis): HypothesisReview | undefined {
  return hypothesis.reviews.filter((r) => r.state === "pending").at(-1);
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

/**
 * What the hypothesis compares against, in words ("base-camp, revision
 * r3"), or null when it names no control: the control is optional, and the
 * policy decides what it means.
 */
export function controlText(document: unknown): string | null {
  const control =
    typeof document === "object" && document !== null
      ? (document as Record<string, unknown>).control
      : undefined;
  if (typeof control !== "object" || control === null) return null;
  const { id, revision } = control as Record<string, unknown>;
  const name = text(id);
  if (name === null) return null;
  const pinned = text(revision);
  return pinned === null ? name : `${name}, revision ${pinned}`;
}

/** A reason in quotes, ending with its own punctuation. */
export function quoteReason(reason: string): string {
  const trimmed = reason.trim();
  return `“${/[.!?…]$/.test(trimmed) ? trimmed : `${trimmed}.`}”`;
}

const VERDICT_PHRASES: Record<string, string> = {
  pass: "passed every check",
  fail: "did not pass its checks",
  inconclusive: "was inconclusive",
};

const DECISION_PHRASES: Record<string, string> = {
  promote: "accepted it",
  reject: "rejected it",
  inconclusive: "marked it inconclusive",
};

const DECIDED_WORDS: Record<string, string> = {
  promote: "Accepted",
  reject: "Rejected",
  inconclusive: "Marked inconclusive",
  retry: "Tried again",
  stop: "Stopped",
  failed: "Closed as failed",
};

function lastDecision(review: HypothesisReview | undefined): Decision | null {
  return review?.decisions.at(-1) ?? null;
}

/**
 * The newest decision still in force across these kinds of review: what put
 * a queued or active hypothesis back where it is (a decision to try again
 * after a failure). Its approval is its plan's.
 */
function newestLiveDecision(
  hypothesis: Hypothesis,
  kinds: HypothesisReview["kind"][],
): Decision | null {
  const decisions = hypothesis.reviews
    .filter((r) => kinds.includes(r.kind))
    .flatMap((r) => r.decisions);
  const superseded = new Set(decisions.map((d) => d.supersedes).filter(Boolean));
  const live = decisions.filter((d) => !superseded.has(d.id));
  return live.reduce<Decision | null>(
    (newest, d) =>
      newest === null || Date.parse(d.decided_at) >= Date.parse(newest.decided_at) ? d : newest,
    null,
  );
}

function decisionFor(hypothesis: Hypothesis): Decision | null {
  switch (hypothesis.state) {
    case "promoted":
    case "rejected":
    case "inconclusive":
      return currentDecision(latestReview(hypothesis, "decision"));
    case "failed":
      return (
        currentDecision(latestReview(hypothesis, "decision")) ??
        lastDecision(latestReview(hypothesis, "failure"))
      );
    case "queued":
    case "active":
      return newestLiveDecision(hypothesis, ["failure"]);
    default:
      return null;
  }
}

/**
 * The failure a researcher decided to try again after: the latest attempt's
 * own (a failed attempt waiting for a new one, or a stage run again on the
 * same attempt), else the attempt before it.
 */
function failedAttempt(attempt: AttemptDetail | null, previous: Attempt | null): string {
  const failure = attempt?.failures.at(-1);
  if (attempt !== null && failure !== undefined) {
    return `attempt ${attempt.ref} failed (${failure.reason.trim().replace(/[.\s]+$/, "")})`;
  }
  if (previous !== null && previous.state === "failed") return `attempt ${previous.ref} failed`;
  return "an earlier attempt failed";
}

/**
 * `previous` is the attempt before `attempt`, if any: after a failed attempt
 * is tried again, the new attempt follows it.
 */
export function summarizeOutcome(
  hypothesis: Hypothesis,
  attempt: AttemptDetail | null,
  report: Report | null,
  previous: Attempt | null = null,
): OutcomeSummary {
  const title = `“${hypothesis.title}”`;
  const document = hypothesis.document;
  const agentReport = report !== null && "what_was_tried" in report.report ? report.report : null;
  // Shown inline as plain text: the report's Markdown syntax would read as noise.
  const tried = plainText(
    text(agentReport?.what_was_tried) ??
      text("intervention" in document ? document.intervention : undefined) ??
      text(document.question) ??
      hypothesis.title,
  );

  const verification = report?.verification ?? null;
  const verdict = verification?.verdict ?? null;
  const verdictPhrase = verdict === null ? null : (VERDICT_PHRASES[verdict] ?? "was assessed");
  const failure = attempt?.state === "failed" ? (attempt.failures.at(-1) ?? null) : null;

  let happened: string | null = null;
  if (verdictPhrase !== null) {
    const reason = text(verification?.reason);
    happened = `The verification ${verdictPhrase}${reason ? `: ${reason}` : "."}`;
  } else if (failure !== null && attempt !== null) {
    happened = `Attempt ${attempt.ref} failed: ${failure.reason}`;
  } else if (attempt !== null) {
    happened = `Attempt ${attempt.ref}: ${statusLabel("attempt", attempt.state).toLowerCase()}.`;
  }

  const decision = decisionFor(hypothesis);
  const why = decision === null ? null : decision.reason;
  const decided =
    decision === null
      ? null
      : `${DECIDED_WORDS[decision.action] ?? "Decided"} by a researcher on ${formatDate(decision.decided_at)}`;
  const because = decision === null ? "" : ` because ${quoteReason(decision.reason)}`;

  let sentence: string;
  switch (hypothesis.state) {
    case "queued":
      sentence =
        decision?.action === "retry"
          ? `${title} is waiting for an agent to try it again: ${failedAttempt(attempt, previous)}, and a researcher decided to try again${because}`
          : `${title} is planned and waiting for an agent to try it.`;
      break;
    case "active": {
      const now =
        attempt === null
          ? ""
          : ` (attempt ${attempt.ref}: ${statusLabel("attempt", attempt.state).toLowerCase()})`;
      if (pendingReview(hypothesis)?.kind === "failure" && attempt !== null) {
        // A failure waits for a researcher: try again, or stop and write it up.
        sentence = `We tried ${title}, but attempt ${attempt.ref} failed${
          failure ? ` (${failure.reason.replace(/[.\s]+$/, "")})` : ""
        }; a researcher has to decide whether to try again or stop.`;
      } else {
        sentence =
          decision?.action === "retry"
            ? `${title} is being tried again now${now}: ${failedAttempt(attempt, previous)}, and a researcher decided to try again${because}`
            : `${title} is being tried now${now}.`;
      }
      break;
    }
    case "documenting":
      sentence =
        verdictPhrase === null
          ? `We tried ${title}, but the work could not produce a result; it is being written up before its decision.`
          : `We tried ${title}: the verification ${verdictPhrase}, and it is being written up before its decision.`;
      break;
    case "deciding":
      sentence = `We tried ${title}${
        verdictPhrase === null
          ? ", but the work could not produce a result"
          : `: the verification ${verdictPhrase}`
      }; it is written up and waiting for a researcher's decision.`;
      break;
    case "promoted":
    case "rejected":
    case "inconclusive": {
      const phrase =
        decision === null
          ? `it was ${statusLabel("hypothesis", hypothesis.state).toLowerCase()}.`
          : `a researcher ${DECISION_PHRASES[decision.action] ?? "decided"}${because}`;
      sentence = `We tried ${title}${
        verdictPhrase === null ? ", and " : `: the verification ${verdictPhrase}, and `
      }${phrase}`;
      break;
    }
    case "failed":
      sentence = `We tried ${title}, but the work could not produce a result${
        decision === null ? "." : `; a researcher closed it as failed${because}`
      }`;
      break;
    case "cancelled":
      sentence = `${title} was cancelled before it produced a result.`;
      break;
    default:
      sentence = `${title}: ${statusLabel("hypothesis", hypothesis.state).toLowerCase()}.`;
  }

  return { sentence, tried, happened, decision, decided, why };
}
