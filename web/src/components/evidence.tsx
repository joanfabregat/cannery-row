import { DownloadIcon } from "lucide-react";

import type { Artifact, Discrepancy, GateResult, Measurement, Report } from "@/api/types";
import { Assessment } from "@/components/assessment";
import { ImportedBadge } from "@/components/imported-badge";
import { Markdown } from "@/components/markdown";
import { EmptyState } from "@/components/query-state";
import { Collapsible, Fact, Section, SubSection } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { asComparisons, judgedBy } from "@/lib/comparisons";
import { formatDateTime, formatDelta, formatHistoryDate, formatNumber } from "@/lib/format";
import { humanize, label } from "@/lib/labels";

/**
 * An attempt's report and its assessment (the test and the evaluation), shared by the
 * hypothesis page (its latest attempt) and the attempt page. Verified
 * measurements come first; the agent's own numbers are labelled "Reported by
 * agent" and never mixed with them.
 */

function asList<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function sliceLabel(dimensions: Record<string, string> | undefined): string {
  const entries = Object.entries(dimensions ?? {});
  return entries.length === 0
    ? "Overall"
    : entries.map(([name, value]) => `${humanize(name)}: ${value}`).join(", ");
}

function measurementKey(m: {
  metric?: string;
  split?: string;
  dimensions?: Record<string, string>;
}) {
  const dims = Object.entries(m.dimensions ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}=${v}`)
    .join("&");
  return `${m.metric ?? ""}|${m.split ?? ""}|${dims}`;
}

function valueText(m: Measurement | undefined): string {
  if (m === undefined) return "—";
  if (m.value === undefined) return m.missing_reason ? `Missing: ${m.missing_reason}` : "Missing";
  const interval = m.uncertainty
    ? ` (${formatNumber(m.uncertainty.lower)} to ${formatNumber(m.uncertainty.upper)})`
    : "";
  return `${formatNumber(m.value)}${interval}`;
}

interface Row {
  key: string;
  metric: string;
  split: string;
  slice: string;
  unit: string;
  verified: Measurement | undefined;
  claimed: Measurement | undefined;
}

function compareRows(verified: Measurement[], claimed: Measurement[]): Row[] {
  const rows = new Map<string, Row>();
  const add = (m: Measurement, side: "verified" | "claimed") => {
    const key = measurementKey(m);
    const row = rows.get(key) ?? {
      key,
      metric: m.metric,
      split: m.split,
      slice: sliceLabel(m.dimensions),
      unit: m.unit,
      verified: undefined,
      claimed: undefined,
    };
    row[side] = m;
    rows.set(key, row);
  };
  verified.forEach((m) => {
    add(m, "verified");
  });
  claimed.forEach((m) => {
    add(m, "claimed");
  });
  return [...rows.values()];
}

export function MeasurementsTable({
  verified,
  claimed,
}: {
  verified: Measurement[];
  claimed: Measurement[];
}) {
  const rows = compareRows(verified, claimed);
  if (rows.length === 0) return <EmptyState>No measurement was recorded.</EmptyState>;
  // Without a control named, the tester reports none: no empty column.
  const controls = rows.some((row) => typeof row.verified?.control_value === "number");
  // An imported history's values carry their own authority, never the tester's.
  const imported = verified.some((m) => m.authority !== "tester_verified");
  const claims = !imported || claimed.length > 0;
  return (
    <Table>
      <TableCaption>
        {imported
          ? "Imported values come from the history this project was imported from: they were not measured by this project's tester. Each says whether it was read from a run file or copied from a document."
          : "Verified values were measured by the tester. Values reported by the agent are its own claims and are never used to evaluate the attempt."}
      </TableCaption>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead scope="col">Measurement</TableHead>
          <TableHead scope="col">
            {imported ? (
              "Imported value"
            ) : (
              <StatusChip domain="authority" value="tester_verified" />
            )}
          </TableHead>
          {controls ? (
            <>
              <TableHead scope="col">Control</TableHead>
              <TableHead scope="col">Difference</TableHead>
            </>
          ) : null}
          {claims ? (
            <TableHead scope="col">
              <StatusChip domain="authority" value="agent_claim" />
            </TableHead>
          ) : null}
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => {
          const verifiedValue = row.verified?.value;
          const control = row.verified?.control_value;
          const delta =
            verifiedValue !== undefined && control !== undefined ? verifiedValue - control : null;
          return (
            <TableRow key={row.key}>
              <TableHead scope="row" className="h-auto py-2 font-normal text-foreground">
                <span className="font-medium">{row.metric}</span>
                <span className="block text-xs text-muted-foreground">
                  {row.split} · {row.slice}
                  {row.unit ? ` · ${row.unit}` : ""}
                </span>
              </TableHead>
              <TableCell className="font-medium" title={row.verified?.source}>
                <span>{valueText(row.verified)}</span>
                {row.verified && row.verified.authority !== "tester_verified" ? (
                  <StatusChip domain="authority" value={row.verified.authority} className="ml-2" />
                ) : null}
              </TableCell>
              {controls ? (
                <>
                  <TableCell>{formatNumber(control)}</TableCell>
                  <TableCell>{formatDelta(delta)}</TableCell>
                </>
              ) : null}
              {claims ? (
                <TableCell className="text-muted-foreground">{valueText(row.claimed)}</TableCell>
              ) : null}
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

function Discrepancies({ items }: { items: Discrepancy[] }) {
  if (items.length === 0) {
    return <p className="text-sm">The tester found no disagreement with the agent's claims.</p>;
  }
  return (
    <ul className="flex flex-col gap-2">
      {items.map((d, index) => (
        <li key={index} className="rounded-md border border-status-attention/40 p-3 text-sm">
          <p>{d.description}</p>
          {d.metric ? (
            <p className="mt-1 text-xs text-muted-foreground">
              {d.metric}
              {d.split ? ` · ${d.split}` : ""} · {sliceLabel(d.dimensions)} · reported by agent{" "}
              {formatNumber(d.claimed_value)}, verified {formatNumber(d.verified_value)}
            </p>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

/** The tester's record: what it measured when it re-ran the result. */
function TestRecord({ report }: { report: Report }) {
  const verified = asList<Measurement>(report.tester?.measurements);
  const claimed = asList<Measurement>(report.claimed_measurements);
  const imported = report.origin === "imported";
  return (
    <div className="flex flex-col gap-5">
      {report.tester === null && !imported ? (
        <p className="text-sm text-muted-foreground">
          The tester has not published its measurements yet.
        </p>
      ) : null}
      <MeasurementsTable verified={verified} claimed={claimed} />
      {report.tester && !imported ? (
        <div className="flex flex-col gap-2">
          <h4 className="font-medium">Disagreements</h4>
          <Discrepancies items={asList<Discrepancy>(report.tester.discrepancies)} />
        </div>
      ) : null}
      {report.tester?.observations ? (
        <div className="flex flex-col gap-2">
          <h4 className="font-medium">{imported ? "Notes" : "Tester observations"}</h4>
          <Markdown>{report.tester.observations}</Markdown>
        </div>
      ) : null}
    </div>
  );
}

/** The evaluator's record: its verdict under the project's rules. */
function EvaluationRecord({ project, report }: { project: string; report: Report }) {
  const evaluation = report.evaluation;
  if (evaluation === null) {
    return <p className="text-sm text-muted-foreground">The evaluation has not finished yet.</p>;
  }
  return (
    <Assessment
      project={project}
      scienceRevision={report.science_revision}
      measurements={asList<Measurement>(report.tester?.measurements)}
      verdict={evaluation.verdict ?? "unknown"}
      reason={evaluation.reason}
      judged={judgedBy(evaluation.producer, evaluation.policy_revision)}
      at={evaluation.published_at}
      gates={asList<GateResult>(evaluation.gates)}
      comparisons={asComparisons(evaluation.comparisons)}
      headingLevel="h4"
    />
  );
}

/**
 * The test and the evaluation under one heading, each still its own record
 * in its own frame: the tester's measurements first, then the evaluator's
 * verdict on them.
 */
export function AssessmentSection({ project, report }: { project: string; report: Report }) {
  const imported = report.origin === "imported";
  return (
    <Section
      title="Assessment"
      description="Two separate records: the test, then the evaluation. Neither is the final decision: a researcher decides."
    >
      <div className="flex flex-col gap-4">
        <SubSection
          title="Test"
          description={
            imported
              ? "The values the imported history recorded for this attempt."
              : "What the tester measured when it re-ran the result, compared with what the agent reported."
          }
        >
          <TestRecord report={report} />
        </SubSection>
        <SubSection
          title="Evaluation"
          description="The evaluator registered for this project applies its own rules to the tested values."
        >
          <EvaluationRecord project={project} report={report} />
        </SubSection>
      </div>
    </Section>
  );
}

const REPORT_FIELDS: [string, string][] = [
  ["what_was_tried", "What was tried"],
  ["findings", "Findings"],
  ["observations", "Observations"],
  ["limitations", "Limitations"],
  ["next_question", "Next question"],
];

/** An imported attempt's own report, as the history wrote it (docs/import.md). */
interface ImportedReport {
  kind: string;
  author: string;
  writtenAt: string;
  body: string;
  sourceRef: string | null;
}

function importedReport(fields: Record<string, unknown>): ImportedReport | null {
  const { kind, author, written_at, body_markdown, source_ref } = fields;
  if (
    typeof kind !== "string" ||
    typeof author !== "string" ||
    typeof written_at !== "string" ||
    typeof body_markdown !== "string"
  ) {
    return null;
  }
  return {
    kind,
    author,
    writtenAt: written_at,
    body: body_markdown,
    sourceRef: typeof source_ref === "string" ? source_ref : null,
  };
}

function RetrospectiveReport({ report }: { report: ImportedReport }) {
  return (
    <Section
      title={report.kind === "retrospective" ? "Retrospective report" : "Imported report"}
      description="Written after the fact from the imported history's sources, not by an agent of this project."
      actions={<ImportedBadge origin="imported" sourceRef={report.sourceRef} showSource />}
    >
      <div className="flex flex-col gap-4">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Fact term="Author">{report.author}</Fact>
          <Fact term="Written">{formatHistoryDate(report.writtenAt)}</Fact>
        </dl>
        <Markdown>{report.body}</Markdown>
      </div>
    </Section>
  );
}

export function ReportSection({ report }: { report: Report }) {
  if (report.origin === "imported") {
    const retrospective = importedReport(report.report);
    if (retrospective) return <RetrospectiveReport report={retrospective} />;
    return (
      <Section title="Report">
        <p className="text-sm text-muted-foreground">
          This attempt was imported from a research history: no agent wrote a report for it.
          {report.source_ref ? ` It is recorded at ${report.source_ref}.` : ""}
        </p>
      </Section>
    );
  }
  const fields = report.report as Record<string, unknown>;
  const body = typeof fields.body_markdown === "string" ? fields.body_markdown : "";
  return (
    <Section
      title="Report"
      description={`Written by the agent for attempt ${report.attempt_ref}, submitted ${formatDateTime(report.submitted_at)}.`}
    >
      <div className="flex flex-col gap-4">
        {report.status === "failed" ? (
          <p className="text-sm font-medium text-status-danger">
            The agent reported that this attempt failed.
          </p>
        ) : null}
        <dl className="grid gap-4 md:grid-cols-2">
          {REPORT_FIELDS.map(([key, name]) => {
            const value = fields[key];
            return typeof value === "string" && value.trim() ? (
              <Fact key={key} term={name}>
                <Markdown>{value}</Markdown>
              </Fact>
            ) : null;
          })}
        </dl>
        {body ? (
          <Collapsible summary="Full report">
            <Markdown>{body}</Markdown>
          </Collapsible>
        ) : null}
      </div>
    </Section>
  );
}

export function ArtifactList({ project, artifacts }: { project: string; artifacts: Artifact[] }) {
  if (artifacts.length === 0) return <p className="text-sm text-muted-foreground">No file.</p>;
  return (
    <ul className="flex flex-col gap-2">
      {artifacts.map((artifact) => {
        const name = artifact.storage.key.split("/").at(-1) ?? artifact.id;
        return (
          <li key={artifact.id} className="flex flex-wrap items-center gap-2 text-sm">
            <a
              href={`/api/projects/${encodeURIComponent(project)}/artifacts/${artifact.id}`}
              download
              className="inline-flex items-center gap-1.5 font-medium underline underline-offset-4 hover:no-underline"
            >
              <DownloadIcon className="size-4" aria-hidden="true" />
              {name}
            </a>
            <span className="text-muted-foreground">
              {label("artifactRole", artifact.role)} · {formatNumber(artifact.size_bytes)} bytes
            </span>
          </li>
        );
      })}
    </ul>
  );
}
