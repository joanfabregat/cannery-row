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
  verification,
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

/**
 * A runner-driven attempt with a log of its experiment, a verify run that
 * failed and the rerun a researcher's agent performed.
 */
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
  const verifier = job();
  const rerun = job({
    run_number: 2,
    origin: "human_retry",
    performer: "agent",
    verifier: null,
    via_client: "mcp",
    outputs: [],
  });
  const rerunLog = artifact(`${rerun.output_prefix}fixture-scorer/step_log/scorer-rerun.log`);
  return {
    a,
    experimentLog,
    verifier,
    rerun: { ...rerun, outputs: [rerunLog] },
    rerunLog,
  };
}

describe("how an attempt ran", () => {
  it("names who performed each verify run and links each step's logs", async () => {
    const { a, experimentLog, verifier, rerun, rerunLog } = workflowAttempt();
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [a],
        reports: { 1: report() },
        jobs: { 1: [verifier, rerun] },
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

    const verify = await within(ran).findByRole("region", { name: "Verify runs" });
    const first = within(verify).getByRole("listitem", { name: "Run 1" });
    const second = within(verify).getByRole("listitem", { name: "Run 2" });
    expect(first).toHaveTextContent("A verify runner");
    expect(first).toHaveTextContent(/Verifier\s*cannery-runner/);
    expect(first).toHaveTextContent("The runner holding token gke-runner");
    expect(first).toHaveTextContent("Step overlap-producer");
    expect(first).toHaveTextContent("Step fixture-scorer");
    const [log] = verifier.outputs;
    expect(within(verify).getByRole("link", { name: "overlap-producer.log" })).toHaveAttribute(
      "href",
      `/api/projects/sardines/artifacts/${log?.id ?? ""}`,
    );
    expect(within(verify).getByRole("link", { name: "fixture-scorer.log" })).toBeInTheDocument();
    // A step's other outputs are not logs.
    expect(within(verify).queryByRole("link", { name: "run.json" })).not.toBeInTheDocument();

    expect(second).toHaveTextContent("Rerun a researcher asked for");
    expect(second).toHaveTextContent("An agent or a researcher");
    expect(second).not.toHaveTextContent("Verifier");
    expect(second).toHaveTextContent(/Run by\s*An agent/);
    expect(within(verify).getByRole("link", { name: "scorer-rerun.log" })).toHaveAttribute(
      "href",
      `/api/projects/sardines/artifacts/${rerunLog.id}`,
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

  it("says an agent ran the experiment, and that no verify run has started", async () => {
    signedIn({}, hypothesisApi(hypothesis(), { attempts: [attempt()], reports: { 1: report() } }));
    renderApp("/hypotheses/12/attempts/1");
    const ran = await screen.findByRole("region", { name: "How it ran" });
    const experiment = within(ran).getByRole("region", { name: "Experiment run" });
    expect(within(experiment).getByText("Agent")).toBeInTheDocument();
    expect(experiment).toHaveTextContent("An agent through API");
    expect(experiment).toHaveTextContent("Cannery Row keeps no logs of an outside agent's run.");
    expect(await within(ran).findByText("No verify run yet.")).toBeInTheDocument();
  });

  it("tells a viewer there are logs without offering to download them", async () => {
    const { a, verifier, rerun } = workflowAttempt();
    signedIn(
      { projects: [project("sardines", "Sardines", "viewer")] },
      hypothesisApi(hypothesis(), {
        attempts: [a],
        reports: { 1: report() },
        jobs: { 1: [verifier, rerun] },
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

describe("the verification", () => {
  it("shows one report: the measurements, then the verdict", async () => {
    signedIn({}, hypothesisApi(hypothesis(), { attempts: [attempt()], reports: { 1: report() } }));
    renderApp("/hypotheses/12/attempts/1");
    const verification = await screen.findByRole("region", { name: "Verification" });
    expect(screen.getByRole("heading", { level: 2, name: "Verification" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 2, name: "Assessment" })).not.toBeInTheDocument();

    const measured = within(verification).getByRole("region", { name: "Measurements" });
    const verdict = within(verification).getByRole("region", { name: "Verdict" });
    expect(
      measured.compareDocumentPosition(verdict) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // The verified values stay with the measurements; the verdict stays on its own.
    expect(within(measured).getByText("0.91")).toBeInTheDocument();
    expect(within(measured).queryByText("Passed")).not.toBeInTheDocument();
    expect(within(verdict).getByText("Passed")).toBeInTheDocument();
    expect(within(verdict).getByText("Accuracy held within the margin")).toBeInTheDocument();
    expect(within(verdict).queryByText("0.91")).not.toBeInTheDocument();
  });

  it("shows the verifier's notes from the report body", async () => {
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [attempt()],
        reports: {
          1: report({ verification: verification({ body_markdown: "Re-ran on **two** seeds." }) }),
        },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const measured = await screen.findByRole("region", { name: "Measurements" });
    expect(within(measured).getByRole("heading", { name: "Verifier notes" })).toBeInTheDocument();
    expect(within(measured).getByText("two")).toBeInTheDocument();
  });

  it("says when the verification has not published its report yet", async () => {
    signedIn(
      {},
      hypothesisApi(hypothesis(), {
        attempts: [attempt({ state: "verifying" })],
        reports: { 1: report({ verification: null }) },
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Verification" });
    expect(section).toHaveTextContent("The verification has not published its report yet.");
  });
});
