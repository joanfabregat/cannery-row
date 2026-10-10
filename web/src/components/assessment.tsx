import { ExternalLinkIcon } from "lucide-react";
import { Link } from "react-router";

import { useMetricCatalog } from "@/api/queries";
import type { GateResult } from "@/api/types";
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
import {
  type Comparison,
  type Direction,
  difference,
  directionsOf,
  referenceTarget,
  standing,
} from "@/lib/comparisons";
import { formatDateTime, formatDelta, formatNumber } from "@/lib/format";
import { humanize, label } from "@/lib/labels";

/**
 * A verification's verdict: its reason, the checks the policy ran and
 * what it compared. The rules belong to the policy the science revision
 * registers; the page names it and its rules version, and never judges a
 * comparison itself beyond saying which side the metric's direction favours.
 */

function sliceLabel(dimensions: Record<string, string>): string {
  const entries = Object.entries(dimensions);
  return entries.length === 0
    ? "Overall"
    : entries.map(([name, value]) => `${humanize(name)}: ${value}`).join(", ");
}

export function GatesTable({ gates }: { gates: GateResult[] }) {
  if (gates.length === 0) return null;
  return (
    <Table>
      <TableCaption className="sr-only">The policy's checks</TableCaption>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead scope="col">Check</TableHead>
          <TableHead scope="col">Result</TableHead>
          <TableHead scope="col">Values used</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {gates.map((gate) => (
          <TableRow key={gate.id}>
            <TableHead scope="row" className="h-auto py-2 font-medium text-foreground">
              {humanize(gate.id)}
            </TableHead>
            <TableCell>
              <StatusChip domain="verdict" value={gate.result} />
            </TableCell>
            <TableCell className="text-muted-foreground">{gate.detail ?? "—"}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function ReferenceRef({ value, project }: { value: string; project: string }) {
  const target = referenceTarget(value, project);
  const className = "font-medium underline underline-offset-4 hover:no-underline";
  if (target === null) return <span>{value}</span>;
  if (target.kind === "internal") {
    return (
      <Link to={target.to} className={className}>
        {value}
      </Link>
    );
  }
  return (
    <a
      href={target.href}
      rel="noopener noreferrer nofollow"
      target="_blank"
      className={`inline-flex items-center gap-1 ${className}`}
    >
      {value}
      <ExternalLinkIcon className="size-3" aria-hidden="true" />
      <span className="sr-only">(opens in a new tab)</span>
    </a>
  );
}

export function ComparisonsTable({
  project,
  comparisons,
  directions,
  directionsLoading = false,
}: {
  project: string;
  comparisons: Comparison[];
  directions: Record<string, Direction>;
  /** While the metrics' directions load, "Better?" waits rather than saying "Can't tell". */
  directionsLoading?: boolean;
}) {
  if (comparisons.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        The verification did not say what it compared this result with.
      </p>
    );
  }
  return (
    <Table>
      <TableCaption>
        “Better” follows each metric's own direction: for some, higher is better; for others, lower
        is.
      </TableCaption>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead scope="col">What was measured</TableHead>
          <TableHead scope="col">This result</TableHead>
          <TableHead scope="col">Compared with</TableHead>
          <TableHead scope="col">Difference</TableHead>
          <TableHead scope="col">Better?</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {comparisons.map((c) => {
          const ref = c.reference.ref;
          const call = standing(c.value, c.reference.value, directions[c.metric] ?? null);
          return (
            <TableRow key={`${c.metric}|${c.split}|${JSON.stringify(c.dimensions)}`}>
              <TableHead scope="row" className="h-auto py-2 font-normal text-foreground">
                <span className="font-medium">{c.metric}</span>{" "}
                <span className="block text-xs text-muted-foreground">
                  Data set: {c.split} · {sliceLabel(c.dimensions)}
                </span>
              </TableHead>
              <TableCell>
                <span className="font-medium">{formatNumber(c.value)}</span>
                <span className="block text-xs text-muted-foreground">
                  {label("comparisonSource", c.source)}
                </span>
              </TableCell>
              <TableCell className="whitespace-normal">
                <span className="font-medium">{formatNumber(c.reference.value)}</span>
                <span className="block text-xs text-muted-foreground">
                  {c.reference.label} · {label("referenceKind", c.reference.kind)}
                </span>
                {ref && ref !== c.reference.label ? (
                  <span className="block text-xs">
                    <ReferenceRef value={ref} project={project} />
                  </span>
                ) : null}
              </TableCell>
              <TableCell>{formatDelta(difference(c.value, c.reference.value))}</TableCell>
              <TableCell>
                {directionsLoading && call !== "same" ? (
                  <span role="status" className="text-sm text-muted-foreground">
                    Checking…
                  </span>
                ) : (
                  <StatusChip domain="standing" value={call} />
                )}
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

export interface AssessmentProps {
  project: string;
  /** The science revision the attempt pinned: its catalog gives each metric's direction. */
  scienceRevision: number | null;
  /** The verified measurements, whose own direction is used when the catalog lacks one. */
  measurements: { metric?: unknown; direction?: unknown }[];
  verdict: string;
  reason: string | null;
  judged: string | null;
  /** When the verdict was published. */
  at?: string | undefined;
  gates: GateResult[];
  comparisons: Comparison[];
  /** True while the attempt's report (its science revision and measurements) loads. */
  reportLoading?: boolean;
  /** The level of its own headings, below the section that holds it. */
  headingLevel?: "h3" | "h4";
}

export function Assessment({
  project,
  scienceRevision,
  measurements,
  verdict,
  reason,
  judged,
  at,
  gates,
  comparisons,
  reportLoading = false,
  headingLevel: Heading = "h3",
}: AssessmentProps) {
  const catalog = useMetricCatalog(project, comparisons.length > 0 ? scienceRevision : null);
  const directions = directionsOf(catalog.data?.metrics, measurements);
  // `isLoading` is a first fetch in flight; a failed catalog falls back on the measurements.
  const directionsLoading = reportLoading || catalog.isLoading;
  const byline = [judged, at ? formatDateTime(at) : null].filter(Boolean).join(" · ");
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <StatusChip domain="verdict" value={verdict} className="text-sm" />
        {byline ? <span className="text-sm text-muted-foreground">{byline}</span> : null}
      </div>
      {reason ? <p className="text-sm">{reason}</p> : null}
      {gates.length > 0 ? (
        <div className="flex flex-col gap-2">
          <Heading className="font-medium">The policy's checks</Heading>
          <GatesTable gates={gates} />
        </div>
      ) : null}
      <div className="flex flex-col gap-2">
        <Heading className="font-medium">What it compared</Heading>
        <ComparisonsTable
          project={project}
          comparisons={comparisons}
          directions={directions}
          directionsLoading={directionsLoading}
        />
      </div>
    </div>
  );
}
