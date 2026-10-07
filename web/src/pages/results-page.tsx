import { useState } from "react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import {
  useClaimedMetrics,
  useDashboard,
  useDashboardView,
  useImportedMetrics,
} from "@/api/queries";
import type { Series, ViewData } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading, QueryView } from "@/components/query-state";
import { Collapsible, Section } from "@/components/section";
import { navItems } from "@/components/shell/nav-items";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
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
  chartReferences,
  chartRows,
  noReferenceNote,
  referenceTooltipName,
  xText,
} from "@/lib/chart-rows";
import { formatDateTime, formatNumber } from "@/lib/format";
import { humanize, label } from "@/lib/labels";
import type { Project } from "@/projects/project-context";

/**
 * Results: the project's dashboard views, each resolved against verified
 * measurements and drawn with Recharts (the chart library shadcn/ui's charts
 * build on), with the same numbers as a table. The agent's own claims are
 * only shown on request, labelled "Reported by agent".
 */

const item = navItems.find((entry) => entry.to === "/results");

// Categorical hues in fixed order; more series than this fold into the table.
const SERIES_COLORS = [1, 2, 3, 4, 5, 6, 7, 8].map((n) => `var(--series-${n})`);

export function ResultsPage() {
  return (
    <>
      <PageHeader title="Results" description={item?.description} />
      <ProjectPage>{(project) => <Dashboard project={project} />}</ProjectPage>
    </>
  );
}

interface View {
  id: string;
  title: string;
  chart: string;
  metric: string;
  split?: string;
}

function asViews(value: Record<string, unknown>[]): View[] {
  return value.flatMap((v) =>
    typeof v.id === "string" && typeof v.metric === "string"
      ? [
          {
            id: v.id,
            title: typeof v.title === "string" ? v.title : v.id,
            chart: typeof v.chart === "string" ? v.chart : "table",
            metric: v.metric,
            split: typeof v.split === "string" ? v.split : undefined,
          },
        ]
      : [],
  );
}

function Dashboard({ project }: { project: Project }) {
  const dashboard = useDashboard(project.slug);
  return (
    <QueryView
      query={dashboard}
      notFound="This project has no measurements set up yet: an administrator must publish its science configuration first."
    >
      {(data) => {
        const views = asViews(data.views);
        if (views.length === 0) {
          return <EmptyState>No result view is defined for this project yet.</EmptyState>;
        }
        return (
          <div className="flex flex-col gap-6">
            <p className="text-sm text-muted-foreground">
              {data.derived
                ? "Default views, one set per registered metric."
                : `Views of dashboard revision ${String(data.dashboard_revision)}.`}{" "}
              Values are verified by the tester unless labelled otherwise.
            </p>
            {views.map((view) => (
              <ViewCard key={view.id} project={project.slug} view={view} />
            ))}
          </div>
        );
      }}
    </QueryView>
  );
}

function seriesName(series: Series, revisions: number): string {
  const parts = Object.entries(series.group).map(([name, value]) =>
    name === "track" ? groupValue(value) : `${humanize(name)} ${groupValue(value)}`,
  );
  const name = parts.length > 0 ? parts.join(", ") : "All";
  return revisions > 1 ? `${name} (science revision ${series.science_revision})` : name;
}

function ViewCard({ project, view }: { project: string; view: View }) {
  const resolved = useDashboardView(project, view.id);
  const [claims, setClaims] = useState(false);
  const [history, setHistory] = useState(false);
  return (
    <Section
      title={view.title}
      description={`${label("chart", view.chart)} of ${view.metric}${view.split ? ` on the ${view.split} split` : ""}.`}
      actions={<StatusChip domain="authority" value="tester_verified" />}
    >
      <QueryView query={resolved}>
        {(data) => {
          const revisions = new Set(data.series.map((s) => s.science_revision)).size;
          const names = data.series.map((s) => seriesName(s, revisions));
          const points = data.series.reduce((sum, s) => sum + s.points.length, 0);
          return (
            <div className="flex flex-col gap-4">
              {points === 0 ? (
                <EmptyState>No verified measurement yet.</EmptyState>
              ) : view.chart === "table" ? (
                <ViewTable data={data} names={names} />
              ) : (
                <>
                  <ViewChart data={data} names={names} chart={view.chart} title={view.title} />
                  <Collapsible summary="The same numbers as a table">
                    <ViewTable data={data} names={names} />
                  </Collapsible>
                </>
              )}
              <ContextLine data={data} />
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  aria-expanded={claims}
                  onClick={() => {
                    setClaims(!claims);
                  }}
                >
                  {claims
                    ? "Hide values reported by the agent"
                    : "Show values reported by the agent"}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  aria-expanded={history}
                  onClick={() => {
                    setHistory(!history);
                  }}
                >
                  {history ? "Hide the imported history" : "Show the imported history"}
                </Button>
              </div>
              {claims ? (
                <ClaimedValues project={project} metric={view.metric} split={view.split} />
              ) : null}
              {history ? (
                <ImportedValues project={project} metric={view.metric} split={view.split} />
              ) : null}
            </div>
          );
        }}
      </QueryView>
    </Section>
  );
}

function ViewChart({
  data,
  names,
  chart,
  title,
}: {
  data: ViewData;
  names: string[];
  chart: string;
  title: string;
}) {
  const rows = chartRows(data, names);
  const shown = names.slice(0, SERIES_COLORS.length);
  const references = chartReferences(rows, shown, SERIES_COLORS);
  const unit = typeof data.metric.unit === "string" ? data.metric.unit : "";
  const common = (
    <>
      <CartesianGrid stroke="var(--chart-grid)" vertical={false} />
      <XAxis dataKey="x" tick={{ fill: "var(--muted-foreground)", fontSize: 12 }} />
      <YAxis
        tick={{ fill: "var(--muted-foreground)", fontSize: 12 }}
        tickFormatter={(value: number) => formatNumber(value)}
        label={
          unit
            ? { value: unit, angle: -90, position: "insideLeft", fill: "var(--muted-foreground)" }
            : undefined
        }
      />
      <Tooltip
        formatter={(value, name, item) => {
          const shownValue = typeof value === "number" ? formatNumber(value) : String(value);
          const reference = references.find((r) => r.key === item.dataKey);
          // A reference names the one its point's verdict reported.
          return reference === undefined
            ? [shownValue, name]
            : [shownValue, referenceTooltipName(reference, item.payload)];
        }}
        contentStyle={{
          background: "var(--popover)",
          border: "1px solid var(--border)",
          color: "var(--popover-foreground)",
          borderRadius: 8,
        }}
      />
      {shown.length > 1 || references.length > 0 ? <Legend /> : null}
    </>
  );
  return (
    <figure aria-label={title} className="h-72 w-full">
      <ResponsiveContainer width="100%" height="100%">
        {chart === "bar" ? (
          <BarChart data={rows}>
            {common}
            {shown.map((name, index) => (
              <Bar
                key={name}
                dataKey={name}
                fill={SERIES_COLORS[index]}
                radius={[4, 4, 0, 0]}
                maxBarSize={48}
              />
            ))}
            {references.map((reference) => (
              <Bar
                key={reference.key}
                dataKey={reference.key}
                name={reference.label}
                fill={reference.color}
                fillOpacity={shown.length === 1 ? 1 : 0.35}
                maxBarSize={48}
              />
            ))}
          </BarChart>
        ) : (
          <LineChart data={rows}>
            {common}
            {shown.map((name, index) => (
              <Line
                key={name}
                dataKey={name}
                stroke={SERIES_COLORS[index]}
                strokeWidth={2}
                dot={{ r: 4 }}
                connectNulls
                // A scatter view shows the points only.
                strokeOpacity={chart === "scatter" ? 0 : 1}
                isAnimationActive={false}
              />
            ))}
            {references.map((reference) => (
              <Line
                key={reference.key}
                dataKey={reference.key}
                name={reference.label}
                // A step: the reference holds until a verdict reports another. It
                // breaks where the series' own point has none (see chartRows).
                type="stepAfter"
                stroke={reference.color}
                strokeDasharray="6 4"
                strokeWidth={2}
                dot={false}
                connectNulls={false}
                isAnimationActive={false}
              />
            ))}
          </LineChart>
        )}
      </ResponsiveContainer>
      {names.length > shown.length ? (
        <figcaption className="text-xs text-muted-foreground">
          {names.length - shown.length} more series are in the table only.
        </figcaption>
      ) : null}
    </figure>
  );
}

function ViewTable({ data, names }: { data: ViewData; names: string[] }) {
  return (
    <Table>
      <TableCaption className="sr-only">Verified values</TableCaption>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead scope="col">Series</TableHead>
          <TableHead scope="col">At</TableHead>
          <TableHead scope="col">Verified value</TableHead>
          <TableHead scope="col">Reference</TableHead>
          <TableHead scope="col">Measurements</TableHead>
          <TableHead scope="col">Attempts</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {data.series.flatMap((series, index) =>
          series.points.map((point, pointIndex) => (
            <TableRow key={`${String(index)}-${String(pointIndex)}`}>
              <TableCell>{names[index]}</TableCell>
              <TableCell>{xText(point.x)}</TableCell>
              <TableCell className="font-medium">
                {point.value === null
                  ? point.missing_reasons.length > 0
                    ? `Missing: ${point.missing_reasons.join("; ")}`
                    : "—"
                  : formatNumber(point.value)}
              </TableCell>
              <TableCell>
                {formatNumber(point.control_value)}
                {point.control_value !== null && point.reference_label ? (
                  <span className="block text-xs text-muted-foreground">
                    {point.reference_label}
                  </span>
                ) : null}
              </TableCell>
              <TableCell>{point.count}</TableCell>
              <TableCell>{point.attempt_refs.join(", ")}</TableCell>
            </TableRow>
          )),
        )}
      </TableBody>
    </Table>
  );
}

function ContextLine({ data }: { data: ViewData }) {
  const context = data.context;
  const revisions = context.science_revisions;
  const noReference = noReferenceNote(data);
  return (
    <div className="flex flex-col gap-1 text-xs text-muted-foreground">
      {noReference ? <p>{noReference}</p> : null}
      <p>
        {context.measured} of {context.rows} measurements have a value
        {context.split ? ` · split ${context.split}` : ""}
        {revisions.length > 0
          ? ` · science ${revisions.length === 1 ? "revision" : "revisions"} ${revisions.join(", ")}`
          : ""}
        {context.failed_attempts > 0
          ? ` · ${context.failed_attempts} failed ${context.failed_attempts === 1 ? "attempt" : "attempts"} not shown`
          : ""}
      </p>
      {data.truncated ? <p>Only the first measurements are shown: there are too many.</p> : null}
      {data.warnings.map((warning) => (
        <p key={warning}>Note: {warning}</p>
      ))}
    </div>
  );
}

function ClaimedValues({
  project,
  metric,
  split,
}: {
  project: string;
  metric: string;
  split: string | undefined;
}) {
  const claims = useClaimedMetrics(project, metric, split, true);
  return (
    <QueryView query={claims}>
      {(page) =>
        page.items.length === 0 ? (
          <EmptyState>The agent reported no value for this metric.</EmptyState>
        ) : (
          <Table>
            <TableCaption>
              These are the agents' own claims; they are never used to evaluate an attempt.
            </TableCaption>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead scope="col">Attempt</TableHead>
                <TableHead scope="col">Track</TableHead>
                <TableHead scope="col">Slice</TableHead>
                <TableHead scope="col">
                  <StatusChip domain="authority" value="agent_claim" />
                </TableHead>
                <TableHead scope="col">Recorded</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {page.items.map((point) => (
                <TableRow key={point.id}>
                  <TableCell>
                    {point.attempt_ref} {point.hypothesis_title}
                  </TableCell>
                  <TableCell>{point.track}</TableCell>
                  <TableCell>
                    {Object.entries(point.dimensions)
                      .map(([k, v]) => `${humanize(k)}: ${v}`)
                      .join(", ") || "Overall"}
                  </TableCell>
                  <TableCell>
                    {point.value === null
                      ? `Missing${point.missing_reason ? `: ${point.missing_reason}` : ""}`
                      : formatNumber(point.value)}
                  </TableCell>
                  <TableCell>{formatDateTime(point.recorded_at)}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )
      }
    </QueryView>
  );
}

/**
 * The values of a research history imported with `cannery import`: never
 * measured by this project's tester, each labelled with where it was read
 * (a run file or a document) and dated when it was recorded in the history.
 */
function ImportedValues({
  project,
  metric,
  split,
}: {
  project: string;
  metric: string;
  split: string | undefined;
}) {
  const imported = useImportedMetrics(project, metric, split, true);
  if (imported.isPending) return <Loading />;
  if (imported.isError) return <LoadError error={imported.error} retry={imported.refetch} />;
  const points = imported.data.pages.flatMap((page) => page.items);
  if (points.length === 0) return <EmptyState>No imported value for this metric.</EmptyState>;
  return (
    <div className="flex flex-col gap-3">
      <Table>
        <TableCaption>
          Values of the history this project was imported from; they were not measured by this
          project's tester.
        </TableCaption>
        <TableHeader>
          <TableRow className="hover:bg-transparent">
            <TableHead scope="col">Attempt</TableHead>
            <TableHead scope="col">Track</TableHead>
            <TableHead scope="col">Slice</TableHead>
            <TableHead scope="col">Imported value</TableHead>
            <TableHead scope="col">Recorded</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {points.map((point) => (
            <TableRow key={point.id}>
              <TableCell>
                {point.attempt_ref} {point.hypothesis_title}
              </TableCell>
              <TableCell>{point.track}</TableCell>
              <TableCell>
                {Object.entries(point.dimensions)
                  .map(([k, v]) => `${humanize(k)}: ${v}`)
                  .join(", ") || "Overall"}
              </TableCell>
              <TableCell title={point.source_ref ?? undefined}>
                {point.value === null
                  ? `Missing${point.missing_reason ? `: ${point.missing_reason}` : ""}`
                  : formatNumber(point.value)}
                <StatusChip domain="authority" value={point.authority} className="ml-2" />
              </TableCell>
              <TableCell>{formatDateTime(point.recorded_at)}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {imported.hasNextPage ? (
        <div>
          <Button
            variant="outline"
            disabled={imported.isFetchingNextPage}
            onClick={() => void imported.fetchNextPage()}
          >
            {imported.isFetchingNextPage ? "Loading…" : "Show more imported values"}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function groupValue(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}
