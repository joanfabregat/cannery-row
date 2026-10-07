import type { OutcomeSummary } from "@/lib/outcome";

import { Fact } from "./section";

/**
 * The first thing on a hypothesis page: one sentence saying what was tried,
 * what happened, what was decided and why, then the same four answers apart.
 */
export function OutcomeCard({ summary }: { summary: OutcomeSummary }) {
  return (
    <section
      aria-label="Outcome"
      className="rounded-lg border border-l-4 border-l-primary bg-card p-5 md:p-6"
    >
      <p data-testid="outcome-sentence" className="text-lg leading-relaxed font-medium">
        {summary.sentence}
      </p>
      <dl className="mt-5 grid gap-4 md:grid-cols-2">
        <Fact term="What was tried">{summary.tried}</Fact>
        <Fact term="What happened">{summary.happened ?? "Nothing yet."}</Fact>
        <Fact term="What was decided">{summary.decided ?? "No decision yet."}</Fact>
        <Fact term="Why">{summary.why ? `“${summary.why}”` : "—"}</Fact>
      </dl>
    </section>
  );
}
