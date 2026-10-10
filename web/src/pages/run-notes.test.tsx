import { screen, within } from "@testing-library/react";

import { attempt, hypothesis, hypothesisApi, report } from "@/test/fixtures";
import { renderApp, signedIn } from "@/test/render";

function runAttempt(body: string) {
  return hypothesisApi(hypothesis(), {
    attempts: [attempt()],
    reports: { 1: report({ report: { body_markdown: body } }) },
  });
}

describe("a run's notes", () => {
  it("are shown as the agent wrote them, rendered", async () => {
    signedIn({}, runAttempt("## Setup\n\nTwo **seeds**."));
    renderApp("/hypotheses/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Run notes" });
    expect(within(section).getByRole("heading", { name: "Setup" })).toBeInTheDocument();
    expect(within(section).getByText("seeds").tagName).toBe("STRONG");
    expect(screen.queryByRole("region", { name: "Report" })).not.toBeInTheDocument();
  });

  it("say so when the run wrote none", async () => {
    signedIn({}, runAttempt(""));
    renderApp("/hypotheses/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Run notes" });
    expect(within(section).getByText("The agent wrote no run notes.")).toBeInTheDocument();
  });
});
