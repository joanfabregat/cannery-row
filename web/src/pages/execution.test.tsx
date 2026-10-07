import { screen, within } from "@testing-library/react";

import {
  artifact,
  attempt,
  hypothesis,
  hypothesisApi,
  job,
  page,
  report,
  track,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

function trackApi(t: ReturnType<typeof track>) {
  return {
    "GET /api/projects/sardines/tracks/tokenizer": () => json(t),
    "GET /api/projects/sardines/tracks/tokenizer/history": () => json(page([])),
    "GET /api/projects/sardines/hypotheses": () => json(page([])),
  };
}

describe("a track's execution mode", () => {
  it("says an agent-mode track's hypotheses are run by outside agents, with no workflow", async () => {
    signedIn({}, trackApi(track()));
    renderApp("/tracks/tokenizer");
    const execution = await screen.findByRole("region", { name: "Execution" });
    expect(within(execution).getByText("Agent")).toBeInTheDocument();
    expect(
      within(execution).getByText(/An outside agent claims each hypothesis/),
    ).toBeInTheDocument();
    expect(within(execution).queryByRole("heading", { name: "Workflow" })).not.toBeInTheDocument();
  });

  it("lists a workflow track's pinned experiment steps in order", async () => {
    const workflow = {
      steps: [
        { name: "prepare-data", revision: 2 },
        { name: "fixture-experiment", revision: 1 },
      ],
    };
    signedIn({}, trackApi(track({ mode: "workflow", workflow })));
    renderApp("/tracks/tokenizer");
    const execution = await screen.findByRole("region", { name: "Execution" });
    expect(execution).toHaveTextContent("Workflow: A Cannery Row runner claims each hypothesis");
    expect(within(execution).getByRole("heading", { name: "Workflow" })).toBeInTheDocument();
    const steps = within(execution).getAllByRole("listitem");
    expect(steps.map((s) => s.textContent)).toEqual([
      "prepare-data (revision 2)",
      "fixture-experiment (revision 1)",
    ]);
  });
});

const EXPERIMENT_LOG = "projects/p1/attempts/a1/step_log/fixture-experiment.log";

/** A runner-driven attempt with a log of its experiment, a test run and an evaluation run. */
function workflowAttempt() {
  const experimentLog = artifact(EXPERIMENT_LOG);
  const a = attempt({
    mode: "workflow",
    workflow: { steps: [{ name: "fixture-experiment", revision: 1 }] },
    via_client: "token:local-runner",
    artifacts: [
      experimentLog,
      artifact("projects/p1/attempts/a1/candidate/c.json", { role: "candidate" }),
    ],
  });
  const tester = job();
  const evaluator = job({
    stage: "evaluator",
    tester: "stock-evaluator",
    via_client: "token:eval-runner",
    steps: [{ name: "stock-policy", revision: "p2" }],
    outputs: [],
  });
  const evaluatorLog = artifact(`${evaluator.output_prefix}stock-policy/step_log/stock-policy.log`);
  return {
    a,
    experimentLog,
    tester,
    evaluator: { ...evaluator, outputs: [evaluatorLog] },
    evaluatorLog,
  };
}

describe("how an attempt ran", () => {
  it("names the runner of each stage and links each step's logs", async () => {
    const { a, experimentLog, tester, evaluator, evaluatorLog } = workflowAttempt();
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [a],
        reports: { 1: report() },
        jobs: { 1: [tester, evaluator] },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const ran = await screen.findByRole("region", { name: "How it ran" });

    const experiment = within(ran).getByRole("region", { name: "Experiment run" });
    expect(experiment).toHaveTextContent(/Mode\s*Workflow/);
    expect(within(experiment).getByRole("heading", { name: "Workflow" })).toBeInTheDocument();
    expect(experiment).toHaveTextContent("The runner holding token local-runner");
    expect(within(experiment).getByText("fixture-experiment (revision 1)")).toBeInTheDocument();
    expect(
      within(experiment).getByRole("link", { name: "fixture-experiment.log" }),
    ).toHaveAttribute("href", `/api/projects/sardines/artifacts/${experimentLog.id}`);
    // Only logs: the candidate is a file, listed under Files.
    expect(within(experiment).queryByRole("link", { name: "c.json" })).not.toBeInTheDocument();

    const test = await within(ran).findByRole("region", { name: "Test runs" });
    expect(test).toHaveTextContent("Run 1");
    expect(test).toHaveTextContent("The runner holding token gke-runner");
    expect(test).toHaveTextContent("Step overlap-producer");
    expect(test).toHaveTextContent("Step fixture-scorer");
    const [first] = tester.outputs;
    expect(within(test).getByRole("link", { name: "overlap-producer.log" })).toHaveAttribute(
      "href",
      `/api/projects/sardines/artifacts/${first?.id ?? ""}`,
    );
    expect(within(test).getByRole("link", { name: "fixture-scorer.log" })).toBeInTheDocument();
    // A step's other outputs are not logs.
    expect(within(test).queryByRole("link", { name: "run.json" })).not.toBeInTheDocument();

    const evaluation = within(ran).getByRole("region", { name: "Evaluation runs" });
    expect(evaluation).toHaveTextContent("The runner holding token eval-runner");
    expect(within(evaluation).getByRole("link", { name: "stock-policy.log" })).toHaveAttribute(
      "href",
      `/api/projects/sardines/artifacts/${evaluatorLog.id}`,
    );
    expect(screen.getByRole("region", { name: "Summary" })).toHaveTextContent(
      "A workflow runner (token local-runner)",
    );
  });

  it("shows a client label that names no token plainly, never as a token", async () => {
    const { a } = workflowAttempt();
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [{ ...a, via_client: "cli" }],
        reports: { 1: report() },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const ran = await screen.findByRole("region", { name: "How it ran" });
    const experiment = within(ran).getByRole("region", { name: "Experiment run" });
    expect(experiment).toHaveTextContent("The runner cli");
    expect(experiment).not.toHaveTextContent("token cli");
    const summary = screen.getByRole("region", { name: "Summary" });
    expect(summary).toHaveTextContent("A workflow runner (cli)");
    expect(summary).not.toHaveTextContent("token cli");
  });

  it("says an agent ran the experiment, and that no run of the next stages has started", async () => {
    signedIn({}, hypothesisApi(hypothesis(), { attempts: [attempt()], reports: { 1: report() } }));
    renderApp("/hypotheses/12/attempts/1");
    const ran = await screen.findByRole("region", { name: "How it ran" });
    const experiment = within(ran).getByRole("region", { name: "Experiment run" });
    expect(within(experiment).getByText("Agent")).toBeInTheDocument();
    expect(experiment).toHaveTextContent("An agent through API");
    expect(experiment).toHaveTextContent("Cannery Row keeps no logs of an outside agent's run.");
    expect(await within(ran).findByText("No test run yet.")).toBeInTheDocument();
    expect(within(ran).getByText("No evaluation run yet.")).toBeInTheDocument();
  });

  it("tells a viewer there are logs without offering to download them", async () => {
    const { a, tester, evaluator } = workflowAttempt();
    signedIn(
      { projects: [project("sardines", "Sardines", "viewer")] },
      hypothesisApi(hypothesis(), {
        attempts: [a],
        reports: { 1: report() },
        jobs: { 1: [tester, evaluator] },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const ran = await screen.findByRole("region", { name: "How it ran" });
    const experiment = within(ran).getByRole("region", { name: "Experiment run" });
    expect(experiment).toHaveTextContent("One log: members can download them.");
    expect(within(ran).queryAllByRole("link")).toHaveLength(0);
  });

  it("shows a failure's step, whether it was queued again, and the logs of the failing run", async () => {
    const { a, experimentLog } = workflowAttempt();
    const failed = {
      ...a,
      state: "failed",
      failures: [
        {
          stage: "agent",
          code: "step_failed",
          reason: "step fixture-experiment exited with code 4",
          details: { step: "fixture-experiment" },
          created_at: "2026-03-02T11:00:00Z",
          requeued: true,
          log_refs: [
            {
              key: EXPERIMENT_LOG,
              size_bytes: experimentLog.size_bytes,
              sha256: experimentLog.sha256,
            },
          ],
        },
      ],
    };
    signedIn({}, hypothesisApi(hypothesis(), { attempts: [failed] }));
    renderApp("/hypotheses/12/attempts/1");
    const wrong = await screen.findByRole("region", { name: "What went wrong" });
    expect(
      within(wrong).getByText(
        "Experiment, step fixture-experiment: step fixture-experiment exited with code 4",
      ),
    ).toBeInTheDocument();
    expect(wrong).toHaveTextContent("the hypothesis was queued again automatically");
    expect(within(wrong).getByText("Logs of the failing run")).toBeInTheDocument();
    expect(within(wrong).getByRole("link", { name: "fixture-experiment.log" })).toHaveAttribute(
      "href",
      `/api/projects/sardines/artifacts/${experimentLog.id}`,
    );
  });
});

describe("the assessment", () => {
  it("groups the test and the evaluation under one heading, as separate records", async () => {
    signedIn({}, hypothesisApi(hypothesis(), { attempts: [attempt()], reports: { 1: report() } }));
    renderApp("/hypotheses/12/attempts/1");
    const assessment = await screen.findByRole("region", { name: "Assessment" });
    expect(screen.getByRole("heading", { level: 2, name: "Assessment" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 2, name: "Evidence" })).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { level: 2, name: "Evaluator verdict" }),
    ).not.toBeInTheDocument();

    const test = within(assessment).getByRole("region", { name: "Test" });
    const evaluation = within(assessment).getByRole("region", { name: "Evaluation" });
    expect(within(assessment).getByRole("heading", { level: 3, name: "Test" })).toBeInTheDocument();
    expect(
      within(assessment).getByRole("heading", { level: 3, name: "Evaluation" }),
    ).toBeInTheDocument();
    // Two records, neither inside the other, the test first.
    expect(test.contains(evaluation)).toBe(false);
    expect(evaluation.contains(test)).toBe(false);
    expect(
      test.compareDocumentPosition(evaluation) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // The tester's measurements stay in the test; the verdict stays in the evaluation.
    expect(within(test).getByText("0.91")).toBeInTheDocument();
    expect(within(test).queryByText("Passed")).not.toBeInTheDocument();
    expect(within(evaluation).getByText("Passed")).toBeInTheDocument();
    expect(within(evaluation).getByText("Accuracy held within the margin")).toBeInTheDocument();
    expect(within(evaluation).queryByText("0.91")).not.toBeInTheDocument();
  });

  it("says when the evaluation has not finished, while the test is shown", async () => {
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [attempt({ state: "evaluating" })],
        reports: { 1: report({ evaluation: null }) },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const assessment = await screen.findByRole("region", { name: "Assessment" });
    expect(within(assessment).getByRole("region", { name: "Test" })).toHaveTextContent("0.91");
    expect(within(assessment).getByRole("region", { name: "Evaluation" })).toHaveTextContent(
      "The evaluation has not finished yet.",
    );
  });
});
