import { screen, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";

import {
  attempt,
  unit,
  metric,
  unitApi,
  promotedUnit,
  report,
  review,
  reviewCase,
  verification,
  verificationDocument,
  verifiedMeasurement,
} from "@/test/fixtures";
import { json, renderApp, signedIn } from "@/test/render";

const CASE_ID = "00000000-0000-4000-8000-00000000beef";

const COMPARISONS: Schemas["RequestEvidenceEnvelopeComparison"][] = [
  {
    metric: "ndcg",
    split: "dev",
    dimensions: {},
    value: 0.42,
    source: "tester",
    reference: { value: 0.4, label: "Base camp r3", kind: "baseline", ref: "base-camp" },
  },
  {
    metric: "ndcg",
    split: "dev",
    dimensions: { language: "fr" },
    value: 0.38,
    source: "evaluator",
    reference: { value: 0.4, label: "best promoted", kind: "promoted_attempt", ref: "#42" },
  },
  {
    metric: "latency_ms",
    split: "test",
    dimensions: {},
    value: 120,
    source: "evaluator",
    reference: {
      value: 150,
      label: "BEIR paper",
      kind: "paper",
      ref: "https://arxiv.org/abs/2104.08663",
    },
  },
  {
    metric: "recall",
    split: "test",
    dimensions: {},
    value: 0.5,
    source: "tester",
    reference: { value: 0.5, label: "Target", kind: "manual", ref: "javascript:alert(1)" },
  },
  {
    metric: "coverage",
    split: "test",
    dimensions: {},
    value: 0.7,
    source: "evaluator",
    reference: { value: 0.6, label: "Benchmark v2", kind: "benchmark" },
  },
  {
    metric: "mrr",
    split: "dev",
    dimensions: {},
    // 0.1 + 0.2 against 0.3: floating-point noise, shown as no difference.
    value: 0.1 + 0.2,
    source: "tester",
    reference: {
      value: 0.3,
      label: "Anchovies best",
      kind: "promoted_attempt",
      ref: "anchovies#7",
    },
  },
];

const GATES: Schemas["VerificationGate"][] = [
  { id: "primary-beats-control", result: "pass", detail: "ndcg on dev: 0.42 - 0.4 = 0.02 > 0" },
];

/**
 * The catalog says ndcg and mrr are higher-is-better and latency
 * lower-is-better; recall and coverage have none.
 */
const CATALOG: Schemas["CatalogOut"] = {
  science_revision: 1,
  metrics: [
    metric({ key: "ndcg", direction: "higher" }),
    metric({ key: "mrr", direction: "higher" }),
    metric({ key: "latency_ms", unit: "ms", direction: "lower" }),
  ],
  baselines: [],
  group_by: [],
};
const catalog = () => json(CATALOG);

function row(table: HTMLElement, name: RegExp): HTMLElement {
  const header = within(table).getByRole("rowheader", { name });
  const tr = header.closest("tr");
  if (tr === null) throw new Error("no row");
  return tr;
}

/** Checks the comparisons table the review screen and the unit page share. */
function expectComparisons(scope: HTMLElement) {
  const table = within(scope).getByRole("table", { name: /“Better” follows/ });
  expect(within(table).getAllByRole("row")).toHaveLength(COMPARISONS.length + 1);

  const overall = row(table, /^ndcg Data set: dev · Overall$/);
  expect(within(overall).getByText("Measured by the verifier")).toBeInTheDocument();
  expect(within(overall).getByText("Base camp r3 · Baseline")).toBeInTheDocument();
  expect(within(overall).getByText("base-camp")).not.toHaveAttribute("href");
  expect(within(overall).getByText("+0.02")).toBeInTheDocument();
  expect(within(overall).getByText("Better")).toBeInTheDocument();

  const french = row(table, /^ndcg Data set: dev · Language: fr$/);
  expect(within(french).getByText("Computed by the policy")).toBeInTheDocument();
  expect(within(french).getByText("best promoted · Best promoted result")).toBeInTheDocument();
  expect(within(french).getByRole("link", { name: "#42" })).toHaveAttribute("href", "/units/42");
  expect(within(french).getByText("Worse")).toBeInTheDocument();

  // Lower is better: 120 ms against 150 ms is an improvement.
  const latency = row(table, /^latency_ms Data set: test · Overall$/);
  expect(within(latency).getByText("−30")).toBeInTheDocument();
  expect(within(latency).getByText("Better")).toBeInTheDocument();
  const paper = within(latency).getByRole("link", { name: /arxiv\.org/ });
  expect(paper).toHaveAttribute("href", "https://arxiv.org/abs/2104.08663");
  expect(paper).toHaveAttribute("target", "_blank");
  expect(paper.getAttribute("rel")).toMatch(/noopener/);
  expect(paper.getAttribute("rel")).toMatch(/noreferrer/);

  // An unsafe address is shown as text, never as a link.
  const manual = row(table, /^recall Data set: test · Overall$/);
  expect(within(manual).getByText("Target · Set by hand")).toBeInTheDocument();
  expect(within(manual).queryByRole("link")).not.toBeInTheDocument();
  expect(within(manual).getByText("javascript:alert(1)")).toBeInTheDocument();
  expect(within(manual).getByText("Same")).toBeInTheDocument();

  // No registered direction: the page does not guess.
  const coverage = row(table, /^coverage Data set: test · Overall$/);
  expect(within(coverage).getByText("Benchmark v2 · Benchmark")).toBeInTheDocument();
  expect(within(coverage).getByText("Can't tell")).toBeInTheDocument();

  // Rounding: a difference shown as 0 is never called better. Another
  // project's ref is shown as text, never followed.
  const other = row(table, /^mrr Data set: dev · Overall$/);
  expect(within(other).getByText("0")).toBeInTheDocument();
  expect(within(other).getByText("Same")).toBeInTheDocument();
  expect(within(other).queryByText("Better")).not.toBeInTheDocument();
  expect(within(other).getByText("anchovies#7")).toBeInTheDocument();
  expect(within(other).queryByRole("link")).not.toBeInTheDocument();
}

describe("the verification's comparisons on the review screen", () => {
  function awaitingResult() {
    const h = unit({
      state: "deciding",
      reviews: [review({ id: CASE_ID, kind: "decision", state: "pending", subject_revision: 3 })],
    });
    return {
      ...unitApi(h, {
        attempts: [attempt({ state: "verified" })],
        reports: {
          1: report({
            verification: verification({ producer: { kind: "service", id: "stock-verifier" } }),
          }),
        },
      }),
      "GET /api/projects/sardines/metrics": catalog,
      [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
        json(
          reviewCase({
            id: CASE_ID,
            verification: verificationDocument({
              policy_revision: "p2",
              verdict: "pass",
              reason: "Pass: all 1 gates pass.",
              gates: GATES,
              comparisons: COMPARISONS,
            }),
          }),
        ),
    };
  }

  it("names the verifier and its rules version, lists its checks and what it compared", async () => {
    signedIn({}, awaitingResult());
    renderApp("/units/12/review");
    const verdict = await screen.findByRole("region", { name: "Verification verdict" });
    expect(
      await within(verdict).findByText(/^Judged by stock-verifier \(rules version p2\)/),
    ).toBeInTheDocument();
    expect(verdict).not.toHaveTextContent(/policy_revision/);
    expect(
      within(verdict).getByRole("heading", { name: "The policy's checks" }),
    ).toBeInTheDocument();
    expect(
      within(verdict).getByRole("rowheader", { name: "Primary beats control" }),
    ).toBeInTheDocument();
    expect(within(verdict).getByRole("heading", { name: "What it compared" })).toBeInTheDocument();
    // The directions come from the catalog, once it has loaded.
    expect(await within(verdict).findAllByText("Better")).toHaveLength(2);
    expectComparisons(verdict);
  });
});

describe("the verification's comparisons on the unit page", () => {
  it("shows the same table under the verdict, with the directions of the attempt's science revision", async () => {
    const { h, attempts } = promotedUnit();
    const withComparisons = report({
      science_revision: 3,
      verification: verification({
        reason: "Pass: all 1 gates pass.",
        policy_revision: "p2",
        gates: GATES,
        comparisons: COMPARISONS,
        producer: { kind: "service", id: "stock-verifier" },
      }),
    });
    const { requests } = signedIn(
      {},
      {
        ...unitApi(h, { attempts, reports: { 1: withComparisons } }),
        "GET /api/projects/sardines/metrics": () => json({ ...CATALOG, science_revision: 3 }),
      },
    );
    renderApp("/units/12");
    const verdict = await screen.findByRole("region", { name: "Verdict" });
    expect(
      within(verdict).getByText(/^Judged by stock-verifier \(rules version p2\)/),
    ).toBeInTheDocument();
    expect(await within(verdict).findAllByText("Better")).toHaveLength(2);
    expectComparisons(verdict);
    const catalogCalls = requests
      .map((r) => new URL(r.url))
      .filter((url) => url.pathname === "/api/projects/sardines/metrics");
    expect(catalogCalls.length).toBeGreaterThan(0);
    for (const url of catalogCalls) {
      expect(url.searchParams.get("science_revision")).toBe("3");
    }
  });

  it("waits for the metrics' directions instead of saying it can't tell", async () => {
    const { h, attempts } = promotedUnit();
    const withComparisons = report({
      verification: verification({
        reason: null,
        policy_revision: "p2",
        comparisons: COMPARISONS,
        producer: { kind: "service", id: "stock-verifier" },
      }),
    });
    let release: () => void = () => undefined;
    const answered = new Promise<void>((resolve) => {
      release = resolve;
    });
    signedIn(
      {},
      {
        ...unitApi(h, { attempts, reports: { 1: withComparisons } }),
        "GET /api/projects/sardines/metrics": async () => {
          await answered;
          return catalog();
        },
      },
    );
    renderApp("/units/12");
    const verdict = await screen.findByRole("region", { name: "Verdict" });
    const table = within(verdict).getByRole("table", { name: /“Better” follows/ });
    // Equal values need no direction; every other row waits.
    expect(within(table).getAllByText("Checking…")).toHaveLength(COMPARISONS.length - 2);
    expect(within(table).getAllByText("Same")).toHaveLength(2);
    expect(within(table).queryByText("Can't tell")).not.toBeInTheDocument();
    expect(within(table).queryByText("Better")).not.toBeInTheDocument();
    release();
    expect(await within(table).findAllByText("Better")).toHaveLength(2);
    expect(within(table).queryByText("Checking…")).not.toBeInTheDocument();
    expect(within(table).getByText("Can't tell")).toBeInTheDocument();
  });

  it("falls back on the verified directions and says when nothing was compared", async () => {
    const { h, attempts, reports } = promotedUnit();
    signedIn({}, unitApi(h, { attempts, reports }));
    renderApp("/units/12");
    const verdict = await screen.findByRole("region", { name: "Verdict" });
    expect(within(verdict).getByText(/^Judged by judge \(rules version 1\)/)).toBeInTheDocument();
    expect(
      within(verdict).getByText("The verification did not say what it compared this result with."),
    ).toBeInTheDocument();
    expect(
      within(verdict).queryByRole("heading", { name: "The policy's checks" }),
    ).not.toBeInTheDocument();
  });
});

describe("a unit without a control", () => {
  const noControl = report({
    verification: verification({ measurements: [verifiedMeasurement()] }),
  });

  it("shows no empty control on the unit page", async () => {
    const { h, attempts } = promotedUnit();
    signedIn({}, unitApi(h, { attempts, reports: { 1: noControl } }));
    renderApp("/units/12");
    const sentence = await screen.findByTestId("outcome-sentence");
    expect(sentence).not.toHaveTextContent(/undefined|null|control/i);
    const idea = screen.getByRole("region", { name: "The idea" });
    expect(within(idea).queryByText("Compared with")).not.toBeInTheDocument();
    const evidence = screen.getByRole("region", { name: "Measurements" });
    expect(
      within(evidence).queryByRole("columnheader", { name: "Control" }),
    ).not.toBeInTheDocument();
    expect(within(evidence).getByText("0.91")).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent(/undefined/);
  });

  it("names the control when the unit has one", async () => {
    const { h, attempts, reports } = promotedUnit();
    const named: Schemas["UnitOut"] = {
      ...h,
      document: { ...h.document, control: { kind: "baseline", id: "base-camp", revision: "r3" } },
    };
    signedIn({}, unitApi(named, { attempts, reports }));
    renderApp("/units/12");
    const idea = await screen.findByRole("region", { name: "The idea" });
    expect(within(idea).getByText("Compared with")).toBeInTheDocument();
    expect(within(idea).getByText("base-camp, revision r3")).toBeInTheDocument();
    const evidence = await screen.findByRole("region", { name: "Measurements" });
    expect(within(evidence).getByRole("columnheader", { name: "Control" })).toBeInTheDocument();
  });
});
