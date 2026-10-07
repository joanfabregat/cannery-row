import { render, screen } from "@testing-library/react";

import type { Measurement } from "@/api/types";
import { MeasurementsTable } from "@/components/evidence";
import { judgedBy } from "@/lib/comparisons";

import { ImportedBadge } from "./imported-badge";

describe("imported records", () => {
  it("are badged with their source as the tooltip and their external id", () => {
    render(
      <ImportedBadge origin="imported" sourceRef="hypotheses/H-001.yaml" externalId="H-001" />,
    );
    const badge = screen.getByText("Imported").closest("[data-slot=imported-badge]");
    expect(badge).toHaveAttribute("title", "hypotheses/H-001.yaml");
    expect(badge).toHaveTextContent("H-001");
    expect(badge).toHaveTextContent("from hypotheses/H-001.yaml");
  });

  it("write their source out on detail pages", () => {
    render(<ImportedBadge origin="imported" sourceRef="notebook.md:88@3f9c2ab" showSource />);
    expect(screen.getByText("notebook.md:88@3f9c2ab")).toBeVisible();
    expect(screen.queryByText(/, from/)).not.toBeInTheDocument();
  });

  it("leave live records unbadged", () => {
    const { container } = render(<ImportedBadge origin="live" sourceRef={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("show each measurement's own authority, never as verified", () => {
    const base = { metric: "mrr", split: "dev", unit: "ratio", direction: "higher" };
    const verified: Measurement[] = [
      {
        ...base,
        value: 0.71,
        authority: "imported_artifact",
        source: "gs://history/run/metrics.json#/mrr",
      },
      {
        ...base,
        dimensions: { language: "fr" },
        value: 0.55,
        authority: "imported_transcribed",
        source: "notebook.md:96@3f9c2ab",
      },
    ];
    render(<MeasurementsTable verified={verified} claimed={[]} />);
    expect(screen.getByText("Imported from a run file")).toBeInTheDocument();
    expect(screen.getByText("Imported from a document")).toBeInTheDocument();
    expect(screen.queryByText("Verified")).not.toBeInTheDocument();
    expect(screen.queryByText("Reported by agent")).not.toBeInTheDocument();
    expect(screen.getByText("0.71").closest("td")).toHaveAttribute(
      "title",
      "gs://history/run/metrics.json#/mrr",
    );
  });

  it("name the imported history as the verdict's judge", () => {
    expect(judgedBy({ kind: "import", id: null }, "release-gate@v1")).toBe(
      "Recorded in the imported history (rules version release-gate@v1)",
    );
  });
});
