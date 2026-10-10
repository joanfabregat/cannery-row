import type { ViewData } from "@/api/types";
import {
  artifact,
  attempt,
  decision,
  hypothesis,
  dashboardView,
  metric,
  nativeReport,
  job,
  report,
  review,
  token,
  verification,
} from "@/test/fixtures";

import {
  chartReferences,
  chartRows,
  noReferenceNote,
  referenceTooltipName,
  xText,
} from "./chart-rows";
import {
  asComparisons,
  difference,
  directionsOf,
  judgedBy,
  referenceTarget,
  standing,
} from "./comparisons";
import { jobLogsByStep, runnerLabel, runnerName, stepText, workflowSteps } from "./execution";
import { formatDelta, formatWaited } from "./format";
import { controlText, currentDecision, quoteReason, summarizeOutcome } from "./outcome";
import { hypothesisPath, parseAttemptRef } from "./paths";
import { plainText } from "./plain-text";
import { refTarget } from "./refs";
import { tokenNameProblem, tokenStatus } from "./tokens";

describe("the outcome sentence", () => {
  it("names what was tried, what happened, the decision and its reason", () => {
    const h = hypothesis({
      state: "rejected",
      reviews: [review({ decisions: [decision({ action: "reject", reason: "Too slow!" })] })],
    });
    const summary = summarizeOutcome(
      h,
      attempt(),
      report({ verification: verification({ verdict: "fail", reason: null }) }),
    );
    expect(summary.sentence).toBe(
      "We tried “Shorter prompts”: the verification did not pass its checks, and a researcher rejected it because “Too slow!”",
    );
    expect(summary.tried).toBe("Halved the system prompt");
    expect(summary.happened).toBe("The verification did not pass its checks.");
    expect(summary.why).toBe("Too slow!");
  });

  it("follows a correction, not the decision it replaced", () => {
    const first = decision({ action: "promote", reason: "Looked fine" });
    const corrected = decision({
      action: "inconclusive",
      reason: "Leak in the test split",
      supersedes: first.id,
    });
    const r = review({ decisions: [first, corrected] });
    expect(currentDecision(r)?.reason).toBe("Leak in the test split");
    const summary = summarizeOutcome(
      hypothesis({ state: "inconclusive", reviews: [r] }),
      null,
      null,
    );
    expect(summary.sentence).toBe(
      "We tried “Shorter prompts”, and a researcher marked it inconclusive because “Leak in the test split.”",
    );
  });

  it("describes the queue and work in progress", () => {
    const queued = summarizeOutcome(hypothesis({ state: "queued" }), null, null);
    expect(queued.sentence).toBe(
      "“Shorter prompts” is planned and waiting for an agent to try it.",
    );
    expect(queued.decision).toBeNull();
    expect(
      summarizeOutcome(hypothesis({ state: "active" }), attempt({ state: "running" }), null)
        .sentence,
    ).toMatch(/^“Shorter prompts” is being tried now \(attempt #12\.1: /);
  });

  it("quotes a reason with its own punctuation", () => {
    expect(quoteReason("Fine")).toBe("“Fine.”");
    expect(quoteReason(" Fine? ")).toBe("“Fine?”");
  });
});

describe("references", () => {
  it("leads #N, slug#N and #N.M to their page", () => {
    expect(refTarget("#12")).toBe("/hypotheses/12");
    expect(refTarget(" sardines#12 ")).toBe("/hypotheses/12?project=sardines");
    expect(refTarget("#12.3")).toBe("/hypotheses/12/attempts/3");
    expect(refTarget("see #12")).toBeNull();
    expect(refTarget("#0")).toBeNull();
    expect(refTarget("Sardines#1")).toBeNull();
  });

  it("builds and reads addresses", () => {
    expect(hypothesisPath(4)).toBe("/hypotheses/4");
    expect(parseAttemptRef("sardines#12.3")).toEqual([12, 3]);
    expect(parseAttemptRef("sardines#12")).toBeNull();
  });
});

describe("token rules", () => {
  it("match the backend's name rules", () => {
    expect(tokenNameProblem("")).toBe("Give the token a name.");
    expect(tokenNameProblem("   ")).toBe("Give the token a name.");
    expect(tokenNameProblem("a".repeat(101))).toMatch(/At most 100/);
    expect(tokenNameProblem("a".repeat(100))).toBeNull();
    expect(tokenNameProblem("ci;job")).toMatch(/“;”/);
    expect(tokenNameProblem("tab\there")).not.toBeNull();
    expect(tokenNameProblem("laptop CLI (é)")).toBeNull();
  });

  it("tell active, expired and revoked tokens apart", () => {
    const now = Date.parse("2026-06-01T00:00:00Z");
    expect(tokenStatus(token(), now)).toBe("active");
    expect(tokenStatus(token({ expires_at: "2026-05-01T00:00:00Z" }), now)).toBe("expired");
    expect(tokenStatus(token({ revoked_at: "2026-04-01T00:00:00Z" }), now)).toBe("revoked");
  });
});

describe("hypotheses without a control", () => {
  const withControl = hypothesis({
    document: {
      ...hypothesis().document,
      control: { kind: "baseline", id: "base-camp", revision: "r3" },
    },
  });

  it("name the control only when there is one", () => {
    expect(controlText(withControl.document)).toBe("base-camp, revision r3");
    expect(controlText(hypothesis().document)).toBeNull();
    expect(controlText({ control: {} })).toBeNull();
    expect(controlText(null)).toBeNull();
  });

  it("never mention a missing control in the outcome", () => {
    for (const state of ["queued", "documenting", "deciding", "promoted"]) {
      const summary = summarizeOutcome(hypothesis({ state }), attempt(), report());
      expect(summary.sentence).not.toMatch(/undefined|null|control/i);
      expect(summary.tried).not.toMatch(/undefined|null/);
    }
  });
});

describe("verification comparisons", () => {
  it("read the comparisons of a record, skipping malformed entries", () => {
    expect(
      asComparisons([
        {
          metric: "ndcg",
          split: "dev",
          dimensions: { language: "fr" },
          value: 0.42,
          source: "tester",
          reference: { value: 0.4, label: "Base camp", kind: "baseline", ref: "base-camp" },
        },
        { metric: "ndcg", split: "dev", value: 0.4 },
        "junk",
      ]),
    ).toEqual([
      {
        metric: "ndcg",
        split: "dev",
        dimensions: { language: "fr" },
        value: 0.42,
        source: "tester",
        reference: { value: 0.4, label: "Base camp", kind: "baseline", ref: "base-camp" },
      },
    ]);
    expect(asComparisons(undefined)).toEqual([]);
  });

  it("skip an entry whose slice holds a value that is not text", () => {
    const entry = {
      metric: "ndcg",
      split: "dev",
      value: 0.42,
      source: "tester",
      reference: { value: 0.4, label: "Base camp", kind: "baseline" },
    };
    expect(asComparisons([{ ...entry, dimensions: { language: "fr", shard: 3 } }])).toEqual([]);
    expect(asComparisons([{ ...entry, dimensions: { language: null } }])).toEqual([]);
    expect(asComparisons([{ ...entry, dimensions: "fr" }])).toEqual([]);
    expect(asComparisons([{ ...entry, dimensions: { language: "fr" } }])).toHaveLength(1);
    expect(asComparisons([entry])[0]?.dimensions).toEqual({});
  });

  it("name the verifier that judged and its rules version", () => {
    expect(judgedBy({ kind: "service", id: "stock-verifier" }, "p2")).toBe(
      "Judged by stock-verifier (rules version p2)",
    );
    expect(judgedBy({ kind: "builtin", id: "builtin-evaluator" }, "1")).toBe(
      "Judged by Cannery Row's former built-in checks (rules version 1)",
    );
    expect(judgedBy(undefined, "p2")).toBe("Judged under rules version p2");
    expect(judgedBy(undefined, null)).toBeNull();
  });

  it("call better or worse in the metric's own direction", () => {
    expect(standing(0.42, 0.4, "higher")).toBe("better");
    expect(standing(0.38, 0.4, "higher")).toBe("worse");
    expect(standing(120, 150, "lower")).toBe("better");
    expect(standing(180, 150, "lower")).toBe("worse");
    expect(standing(0.4, 0.4, "higher")).toBe("same");
    expect(standing(0.42, 0.4, null)).toBe("unknown");
  });

  it("call a difference shown as zero the same, never better or worse", () => {
    // 0.1 + 0.2 is 0.30000000000000004: a floating-point difference, not a gain.
    const noise = 0.1 + 0.2;
    expect(noise - 0.3).not.toBe(0);
    expect(difference(noise, 0.3)).toBe(0);
    expect(formatDelta(difference(noise, 0.3))).toBe("0");
    expect(standing(noise, 0.3, "higher")).toBe("same");
    expect(standing(0.3, noise, "lower")).toBe("same");
    expect(standing(1e12 + 1e-4, 1e12, "higher")).toBe("same");
    // A real difference, however small against the values, is kept.
    expect(standing(0.40001, 0.4, "higher")).toBe("better");
    expect(formatDelta(difference(0.40001, 0.4))).toMatch(/^\+0\.0000/);
    for (const [value, reference] of [
      [noise, 0.3],
      [0.42, 0.4],
      [0.7, 0.1 + 0.6],
    ] as const) {
      const shown = formatDelta(difference(value, reference));
      expect(standing(value, reference, "higher") === "same").toBe(shown === "0");
    }
  });

  it("take each metric's direction from the catalog, else from the measurements", () => {
    expect(
      directionsOf(
        [{ key: "latency", direction: "lower" }, { key: "ndcg" }],
        [
          { metric: "ndcg", direction: "higher_is_better" },
          { metric: "latency", direction: "higher" },
        ],
      ),
    ).toEqual({ ndcg: "higher", latency: "lower" });
  });

  it("link hypothesis and attempt refs in the app and http(s) addresses outside it", () => {
    expect(referenceTarget("#42", "sardines")).toEqual({ kind: "internal", to: "/hypotheses/42" });
    expect(referenceTarget("#12.3", "sardines")).toEqual({
      kind: "internal",
      to: "/hypotheses/12/attempts/3",
    });
    expect(referenceTarget("https://arxiv.org/abs/2401.1", "sardines")).toEqual({
      kind: "external",
      href: "https://arxiv.org/abs/2401.1",
    });
    expect(referenceTarget("base-camp", "sardines")).toBeNull();
    expect(referenceTarget("10.1145/3397271", "sardines")).toBeNull();
    expect(referenceTarget("javascript:alert(1)", "sardines")).toBeNull();
    expect(referenceTarget("//evil.example", "sardines")).toBeNull();
    expect(referenceTarget(null, "sardines")).toBeNull();
  });

  it("link a project-qualified ref only within the current project", () => {
    expect(referenceTarget("sardines#42", "sardines")).toEqual({
      kind: "internal",
      to: "/hypotheses/42",
    });
    expect(referenceTarget("sardines#12.3", "sardines")).toEqual({
      kind: "internal",
      to: "/hypotheses/12/attempts/3",
    });
    expect(referenceTarget("anchovies#42", "sardines")).toBeNull();
    expect(referenceTarget("anchovies#12.3", "sardines")).toBeNull();
  });
});

describe("waiting times", () => {
  it("count minutes, then hours, then days", () => {
    const now = Date.parse("2026-03-03T12:00:00Z");
    expect(formatWaited("2026-03-03T11:15:00Z", now)).toBe("45 minutes");
    expect(formatWaited("2026-03-03T10:00:00Z", now)).toBe("2 hours");
    expect(formatWaited("2026-02-28T12:00:00Z", now)).toBe("3 days");
  });
});

describe("the outcome sentence after a failure was tried again", () => {
  const retried = review({
    kind: "failure",
    decisions: [
      decision({
        action: "retry",
        reason: "The runner was flaky",
        decided_at: "2026-03-06T10:00:00Z",
      }),
    ],
  });
  const failure = {
    stage: "agent",
    code: "released",
    reason: "Out of memory.",
    details: {},
    created_at: "2026-03-05T10:00:00Z",
    requeued: false,
  };

  it("says a queued hypothesis waits to be tried again, and why", () => {
    const summary = summarizeOutcome(
      hypothesis({ state: "queued", reviews: [retried] }),
      attempt({ state: "failed", failures: [failure] }),
      null,
    );
    expect(summary.sentence).toBe(
      "“Shorter prompts” is waiting for an agent to try it again: attempt #12.1 failed (Out of memory), and a researcher decided to try again because “The runner was flaky.”",
    );
    expect(summary.decision?.action).toBe("retry");
    expect(summary.decided).toMatch(/^Tried again by a researcher on /);
    expect(summary.why).toBe("The runner was flaky");
  });

  it("says an active hypothesis is being tried again after the failed attempt", () => {
    const summary = summarizeOutcome(
      hypothesis({ state: "active", reviews: [retried] }),
      attempt({ sequence: 2, state: "running" }),
      null,
      attempt({ sequence: 1, state: "failed" }),
    );
    expect(summary.sentence).toBe(
      "“Shorter prompts” is being tried again now (attempt #12.2: running): attempt #12.1 failed, and a researcher decided to try again because “The runner was flaky.”",
    );
  });

  it("names the failure when the verification is run again on the same attempt", () => {
    const summary = summarizeOutcome(
      hypothesis({ state: "active", reviews: [retried] }),
      attempt({
        state: "verifying",
        failures: [{ ...failure, stage: "verify", reason: "Timeout" }],
      }),
      null,
    );
    expect(summary.sentence).toMatch(
      /^“Shorter prompts” is being tried again now \(attempt #12\.1: .+\): attempt #12\.1 failed \(Timeout\), and a researcher decided to try again because/,
    );
  });
});

describe("plain text from Markdown", () => {
  it("drops the syntax and keeps the words", () => {
    expect(plainText("Halved the **system** prompt")).toBe("Halved the system prompt");
    expect(
      plainText("# Tried\n\n- a [link](https://x.example) and `code`\n- _emphasis_ and snake_case"),
    ).toBe("Tried a link and code emphasis and snake_case");
    expect(plainText("![a chart](x.png) <b>bold</b> 1 < 2 > 0")).toBe("a chart bold 1 < 2 > 0");
  });

  it("is what the outcome shows as what was tried", () => {
    const summary = summarizeOutcome(
      hypothesis({ state: "promoted" }),
      attempt(),
      report({ report: nativeReport({ what_was_tried: "Cut the *system* prompt to **half**" }) }),
    );
    expect(summary.tried).toBe("Cut the system prompt to half");
  });
});

describe("chart rows", () => {
  const point = (
    value: number,
    reference: number | null,
    label: string | null = null,
    x = "2026-03-04T10:00:00Z",
  ) => ({
    x,
    value,
    count: 1,
    attempt_refs: ["#12.1"],
    control_value: reference,
    reference_label: reference === null ? null : label,
    uncertainty: null,
    sample_count: 500,
    missing_reasons: [],
  });
  const data: ViewData = {
    view: dashboardView({ baseline: "control" }),
    dashboard_revision: null,
    metric: metric(),
    aggregation: "mean",
    series: [
      { science_revision: 1, group: { track: "a" }, points: [point(0.91, 0.9)] },
      { science_revision: 1, group: { track: "b" }, points: [point(0.8, 0.7)] },
    ],
    context: {
      science_revisions: [1],
      split: "test",
      authority: "tester_verified",
      rows: 2,
      measured: 2,
      sample_count: 1000,
      failed_attempts: 0,
      controls: [],
    },
    truncated: false,
    warnings: [],
  } satisfies ViewData;

  it("keeps each series' reference apart, with its own legend label", () => {
    const rows = chartRows(data, ["a", "b"]);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ a: 0.91, "a reference": 0.9, b: 0.8, "b reference": 0.7 });
    const references = chartReferences(rows, ["a", "b"], ["red", "blue"]);
    expect(references).toEqual([
      {
        key: "a reference",
        labelKey: "a reference label",
        name: "a reference",
        label: "a reference",
        color: "red",
      },
      {
        key: "b reference",
        labelKey: "b reference label",
        name: "b reference",
        label: "b reference",
        color: "blue",
      },
    ]);
  });

  it("calls a single series' reference by its label", () => {
    const single = {
      ...data,
      series: [
        {
          science_revision: 1,
          group: { track: "a" },
          points: [point(0.91, 0.9, "best promoted (#42)")],
        },
      ],
    };
    const rows = chartRows(single, ["a"]);
    const [reference] = chartReferences(rows, ["a"], ["red"]);
    expect(reference).toEqual({
      key: "a reference",
      labelKey: "a reference label",
      name: "Reference",
      label: "Reference: best promoted (#42)",
      color: "var(--chart-baseline)",
    });
    if (reference === undefined) throw new Error("no reference");
    expect(referenceTooltipName(reference, rows[0])).toBe("Reference: best promoted (#42)");
  });

  describe("the step reference line", () => {
    // A line chart: one row per measurement, in the order of the series' points.
    const stepped = {
      ...data,
      aggregation: null,
      series: [
        {
          science_revision: 1,
          group: { track: "a" },
          points: [
            point(0.8, null, null, "2026-03-01T10:00:00Z"),
            point(0.85, 0.8, "Base camp", "2026-03-05T10:00:00Z"),
            point(0.9, 0.8, "Base camp", "2026-03-06T10:00:00Z"),
            point(0.92, 0.9, "best promoted (#42)", "2026-03-07T10:00:00Z"),
          ],
        },
      ],
    };

    it("holds each point's own reference and changes where the reference changes", () => {
      const rows = chartRows(stepped, ["a"]);
      expect(rows.map((row) => row["a reference"] ?? null)).toEqual([null, 0.8, 0.8, 0.9]);
      expect(rows.map((row) => row["a reference label"] ?? null)).toEqual([
        null,
        "Base camp",
        "Base camp",
        "best promoted (#42)",
      ]);
      const [reference] = chartReferences(rows, ["a"], ["red"]);
      // Two references over time: the legend says Reference, each tooltip its own.
      expect(reference?.label).toBe("Reference");
      if (reference === undefined) throw new Error("no reference");
      expect(referenceTooltipName(reference, rows[1])).toBe("Reference: Base camp");
      expect(referenceTooltipName(reference, rows[3])).toBe("Reference: best promoted (#42)");
      expect(referenceTooltipName(reference, rows[0])).toBe("Reference");
    });

    /** Each row's value in a column: "—" where the row has no such column at all. */
    const column = (rows: ReturnType<typeof chartRows>, key: string) =>
      rows.map((row) => (Object.hasOwn(row, key) ? row[key] : "—"));

    it("breaks the line where the series' own point has no reference", () => {
      const gap = {
        ...stepped,
        series: [
          {
            science_revision: 1,
            group: { track: "a" },
            points: [
              point(0.85, 0.8, "Base camp", "2026-03-05T10:00:00Z"),
              point(0.86, null, null, "2026-03-06T10:00:00Z"),
              point(0.92, 0.9, "best promoted (#42)", "2026-03-07T10:00:00Z"),
            ],
          },
        ],
      };
      const rows = chartRows(gap, ["a"]);
      // An explicit null, not a missing value: the line does not carry 0.8 over it.
      expect(column(rows, "a reference")).toEqual([0.8, null, 0.9]);
      expect(column(rows, "a reference label")).toEqual(["Base camp", null, "best promoted (#42)"]);
    });

    it("holds each series' step across the other series' rows, and breaks at its own gaps", () => {
      const interleaved = {
        ...stepped,
        series: [
          {
            science_revision: 1,
            group: { track: "a" },
            points: [
              point(0.8, 0.8, "Base camp", "2026-03-01T10:00:00Z"),
              point(0.81, 0.8, "Base camp", "2026-03-03T10:00:00Z"),
              point(0.9, 0.9, "best promoted (#42)", "2026-03-05T10:00:00Z"),
            ],
          },
          {
            science_revision: 1,
            group: { track: "b" },
            points: [
              point(0.7, 0.7, "Paper", "2026-03-02T10:00:00Z"),
              point(0.72, null, null, "2026-03-04T10:00:00Z"),
              point(0.75, 0.75, "Paper v2", "2026-03-06T10:00:00Z"),
            ],
          },
        ],
      };
      const rows = chartRows(interleaved, ["a", "b"]);
      // The rows follow time, so the two series interleave: a, b, a, b, a, b.
      expect(column(rows, "a")).toEqual([0.8, "—", 0.81, "—", 0.9, "—"]);
      expect(column(rows, "b")).toEqual(["—", 0.7, "—", 0.72, "—", 0.75]);
      // a: its step holds over b's rows between its points, and stops after its last one.
      expect(column(rows, "a reference")).toEqual([0.8, 0.8, 0.8, 0.8, 0.9, "—"]);
      expect(column(rows, "a reference label")).toEqual([
        "Base camp",
        "Base camp",
        "Base camp",
        "Base camp",
        "best promoted (#42)",
        "—",
      ]);
      // b: nothing before its first point, a break at its own gap, not bridged over a's row.
      expect(column(rows, "b reference")).toEqual(["—", 0.7, 0.7, null, "—", 0.75]);
      const references = chartReferences(rows, ["a", "b"], ["red", "blue"]);
      expect(references.map((r) => r.label)).toEqual(["a reference", "b reference"]);
    });

    it("draws no line for a series without any reference", () => {
      const none = {
        ...stepped,
        series: [
          {
            science_revision: 1,
            group: { track: "a" },
            points: [point(0.8, null, null, "2026-03-01T10:00:00Z")],
          },
        ],
      };
      expect(chartReferences(chartRows(none, ["a"]), ["a"], ["red"])).toEqual([]);
    });

    it("notes the results verified before references were reported", () => {
      expect(noReferenceNote(stepped)).toMatch(/^No reference for results verified before .+\.$/);
      expect(noReferenceNote(stepped)).toContain(xText("2026-03-05T10:00:00Z"));
      const series = stepped.series[0];
      if (series === undefined) throw new Error("fixture series missing");
      const all: ViewData = {
        ...stepped,
        series: [{ ...series, points: series.points.slice(1) }],
      };
      expect(noReferenceNote(all)).toBeNull();
      const none: ViewData = {
        ...stepped,
        series: [{ ...series, points: series.points.slice(0, 1) }],
      };
      expect(noReferenceNote(none)).toMatch(/^No reference for these results/);
      expect(noReferenceNote({ ...stepped, view: dashboardView() })).toBeNull();
    });

    it("counts the results without a reference when they are not all older", () => {
      const gap = {
        ...stepped,
        series: [
          {
            science_revision: 1,
            group: { track: "a" },
            points: [
              point(0.85, 0.8, "Base camp", "2026-03-05T10:00:00Z"),
              point(0.9, null, null, "2026-03-06T10:00:00Z"),
            ],
          },
        ],
      };
      expect(noReferenceNote(gap)).toBe(
        "1 of 2 results have no reference: their verification reported none for this slice.",
      );
    });
  });
});

describe("how a stage ran", () => {
  it("reads a workflow's steps in order, ignoring malformed entries", () => {
    expect(
      workflowSteps({
        steps: [
          { name: "prepare", revision: 2 },
          { revision: 1 },
          "x",
          { name: "run", revision: "r3" },
        ],
      }),
    ).toEqual([
      { name: "prepare", revision: "2" },
      { name: "run", revision: "r3" },
    ]);
    expect(workflowSteps(null)).toEqual([]);
    expect(workflowSteps({ steps: "nope" })).toEqual([]);
    expect(stepText({ name: "run", revision: 3 })).toBe("run (revision 3)");
  });

  it("names a runner by the token it claimed with", () => {
    expect(runnerName("token:gke-runner")).toBe("gke-runner");
    expect(runnerName("token:local;agent/1.0")).toBe("local");
    expect(runnerName("cli")).toBe("cli");
    expect(runnerName(null)).toBeNull();
    expect(runnerName("token:")).toBeNull();
  });

  it("tells a token's name from another client label", () => {
    expect(runnerLabel("token:gke-runner")).toEqual({ name: "gke-runner", token: true });
    expect(runnerLabel("cli")).toEqual({ name: "cli", token: false });
    expect(runnerLabel("token:")).toBeNull();
    expect(runnerLabel(null)).toBeNull();
  });

  it("groups a run's logs by step, in the run's step order, leaving other outputs out", () => {
    const run = job({
      outputs: [],
    });
    const scorer = artifact(`${run.output_prefix}fixture-scorer/step_log/s.log`);
    const setup = artifact(`${run.output_prefix}overlap-producer/setup_log/setup.log`, {
      role: "setup_log",
    });
    const output = artifact(`${run.output_prefix}overlap-producer/run/run.json`, { role: "run" });
    const groups = jobLogsByStep({ ...run, outputs: [scorer, output, setup] });
    expect(groups.map((g) => [g.step, g.logs.map((l) => l.id)])).toEqual([
      ["overlap-producer", [setup.id]],
      ["fixture-scorer", [scorer.id]],
    ]);
  });
});
