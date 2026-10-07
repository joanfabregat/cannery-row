import { screen, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";

import { attempt, hypothesis, hypothesisApi, report } from "@/test/fixtures";
import { renderApp, signedIn } from "@/test/render";

const imported = {
  origin: "imported",
  source_ref: "hypotheses/H-001.yaml#/attempts/0",
} as const;

function importedAttempt(
  retrospective: Schemas["ImportedReportDocument"] | Schemas["EmptyReportDocument"],
) {
  const a = attempt({ ...imported, imported: { label: "seed-1" } });
  const r = report({ ...imported, report: retrospective, claimed_measurements: [] });
  return hypothesisApi(hypothesis({ ...imported, external_id: "H-001" }), {
    attempts: [a],
    reports: { 1: r },
  });
}

describe("an imported attempt's report", () => {
  it("is shown as the history's retrospective report, rendered and sanitised", async () => {
    signedIn(
      {},
      importedAttempt({
        kind: "retrospective",
        author: "A research agent; reviewed by Ana",
        written_at: "2026-09-28",
        body_markdown:
          "## What was tried\n\nBM25 with **k1 at 0.9**.\n\n<script>alert(1)</script>\n\n[a link](javascript:alert(1))",
        origin: "imported",
        source_ref: "reports/H-001/seed-1.md",
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Retrospective report" });
    expect(within(section).getByText("A research agent; reviewed by Ana")).toBeInTheDocument();
    const day = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" });
    expect(section).toHaveTextContent(`Written${day.format(new Date("2026-09-28T00:00:00Z"))}`);
    expect(within(section).getByText("Imported")).toBeInTheDocument();
    expect(within(section).getByText("reports/H-001/seed-1.md")).toBeInTheDocument();
    expect(within(section).getByRole("heading", { name: "What was tried" })).toBeInTheDocument();
    expect(within(section).getByText("k1 at 0.9").tagName).toBe("STRONG");
    expect(section.querySelector("script")).toBeNull();
    expect(within(section).queryByRole("link", { name: "a link" })).not.toBeInTheDocument();
    expect(screen.queryByText(/no agent wrote a report/)).not.toBeInTheDocument();
  });

  it("shows an explicit midnight instant as an instant, not as a bare date", async () => {
    const instant = "2026-09-28T00:00:00Z";
    signedIn(
      {},
      importedAttempt({
        kind: "retrospective",
        author: "Ana",
        written_at: instant,
        body_markdown: "Done.",
        origin: "imported",
        source_ref: "reports/H-001/seed-1.md",
      }),
    );
    renderApp("/hypotheses/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Retrospective report" });
    const at = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
    expect(section).toHaveTextContent(`Written${at.format(new Date(instant))}`);
  });

  it("says no agent wrote one when the history has none", async () => {
    signedIn({}, importedAttempt({}));
    renderApp("/hypotheses/12/attempts/1");
    expect(await screen.findByText(/no agent wrote a report for it/)).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Retrospective report" })).not.toBeInTheDocument();
  });
});
