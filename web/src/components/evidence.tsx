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
 * An attempt's report and its verification, shared by the
 * unit page (its latest attempt) and the attempt page. Verified
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
  // Without a control named, the verifier reports none: no empty column.
  const controls = rows.some((row) => typeof row.verified?.control_value === "number");
  // An imported history's values carry their own authority, never the verifier's.
  const imported = verified.some((m) => m.authority !== "tester_verified");
  const claims = !imported || claimed.length > 0;
  return (
    <Table>
      <TableCaption>
        {imported
          ? "Imported values come from the history this project was imported from: they were not measured by this project's verifier. Each says whether it was read from a run file or copied from a document."
          : "Verified values were measured by the verifier. Values reported by the agent are its own claims and are never used to judge the attempt."}
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
    return <p className="text-sm">The verifier found no disagreement with the agent's claims.</p>;
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

/** The verifier's measurements, set against what the agent reported, and its findings. */
function Measured({ report }: { report: Report }) {
  const verification = report.verification;
  const verified = asList<Measurement>(verification?.measurements);
  const claimed = asList<Measurement>(report.claimed_measurements);
  const imported = report.origin === "imported";
  return (
    <div className="flex flex-col gap-5">
      <MeasurementsTable verified={verified} claimed={claimed} />
      {verification && !imported ? (
        <div className="flex flex-col gap-2">
          <h4 className="font-medium">Disagreements</h4>
          <Discrepancies items={asList<Discrepancy>(verification.discrepancies)} />
        </div>
      ) : null}
      {verification?.body_markdown?.trim() ? (
        <div className="flex flex-col gap-2">
          <h4 className="font-medium">{imported ? "Notes" : "Verifier notes"}</h4>
          <Markdown>{verification.body_markdown}</Markdown>
        </div>
      ) : null}
    </div>
  );
}

/** The verdict the verification reached under the project's policy. */
function Verdict({ project, report }: { project: string; report: Report }) {
  const verification = report.verification;
  if (verification?.verdict == null) {
    return (
      <p className="text-sm text-muted-foreground">
        {report.origin === "imported"
          ? "The imported history recorded no verdict for this attempt."
          : "The verification has not reached a verdict."}
      </p>
    );
  }
  return (
    <Assessment
      project={project}
      scienceRevision={report.science_revision}
      measurements={asList<Measurement>(verification.measurements)}
      verdict={verification.verdict}
      reason={verification.reason}
      judged={judgedBy(verification.producer, verification.policy_revision)}
      at={verification.published_at}
      gates={asList<GateResult>(verification.gates)}
      comparisons={asComparisons(verification.comparisons)}
      headingLevel="h4"
    />
  );
}

/**
 * The verification report: one record of what the verify job measured when
 * it re-ran the result, and its verdict under the project's policy.
 */
export function VerificationSection({ project, report }: { project: string; report: Report }) {
  const imported = report.origin === "imported";
  if (report.verification === null && !imported) {
    return (
      <Section title="Verification">
        <p className="text-sm text-muted-foreground">
          The verification has not published its report yet.
        </p>
      </Section>
    );
  }
  return (
    <Section
      title="Verification"
      description="One report: what was measured when the result was re-run, and the verdict under the project's policy. It is not the final decision: a researcher decides."
    >
      <div className="flex flex-col gap-4">
        <SubSection
          title="Measurements"
          description={
            imported
              ? "The values the imported history recorded for this attempt."
              : "What the verifier measured when it re-ran the result, compared with what the agent reported."
          }
        >
          <Measured report={report} />
        </SubSection>
        <SubSection
          title="Verdict"
          description="The verifier applies the project's policy to the verified values."
        >
          <Verdict project={project} report={report} />
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
  if (!("what_was_tried" in fields)) {
    // A run document: the claims are in the results, the notes are its body.
    return (
      <Section
        title="Run notes"
        description={`Written by the agent for attempt ${report.attempt_ref}, submitted ${formatDateTime(report.submitted_at)}.`}
      >
        {body.trim() ? (
          <Markdown>{body}</Markdown>
        ) : (
          <p className="text-sm text-muted-foreground">The agent wrote no run notes.</p>
        )}
      </Section>
    );
  }
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
