import type { ViewData } from "@/api/types";

import { formatDate, formatNumber } from "./format";

export interface ChartRow {
  x: string;
  [column: string]: number | string | null;
}

/**
 * The column of a series' reference values: the reference each point's own
 * verification reported, so each series has its own.
 */
export function referenceColumn(series: string): string {
  return `${series} reference`;
}

/** The column holding the label of the reference at each point. */
export function referenceLabelColumn(series: string): string {
  return `${series} reference label`;
}

const DATE = /^\d{4}-\d{2}-\d{2}T/;

/** How an x value is shown: dates as dates, numbers formatted, anything else as text. */
export function xText(x: unknown): string {
  if (typeof x === "string" && DATE.test(x)) return formatDate(x);
  if (typeof x === "number") return formatNumber(x);
  if (x === null || x === undefined) return "—";
  return typeof x === "string" ? x : JSON.stringify(x);
}

/** An x value's place on a time or number axis; NaN for anything else. */
function position(x: unknown): number {
  if (typeof x === "number") return x;
  return typeof x === "string" && DATE.test(x) ? Date.parse(x) : NaN;
}

/**
 * One row per x value for Recharts, in x order when every x is a date or a
 * number (so the series interleave on the axis), with a column per series
 * and, per series, one for the reference value (`referenceColumn`) and one
 * for its label (`referenceLabelColumn`), so two series with different
 * references are never drawn against one merged line.
 *
 * The reference line is drawn without joining gaps, from these columns:
 * - a row holding a point of the series carries that point's own reference,
 *   or explicit nulls when its verdict reported none, so the line breaks
 *   there instead of carrying the previous reference over it;
 * - a row holding only other series' points, between two points of the
 *   series, carries the reference of the series' point before it, so the
 *   step holds across other series' rows until the series' next point;
 * - rows before the series' first point or after its last carry nothing.
 */
export function chartRows(data: ViewData, names: string[]): ChartRow[] {
  const rows = new Map<string, ChartRow>();
  const positions = new Map<ChartRow, number>();
  const seriesNames = data.series.map((_, index) => names[index] ?? `Series ${String(index + 1)}`);
  data.series.forEach((series, index) => {
    const name = seriesNames[index] ?? "";
    series.points.forEach((point, pointIndex) => {
      const x = xText(point.x);
      // Line charts plot each measurement, so two on one day stay apart.
      const key =
        data.aggregation === null ? `${x}\u0000${String(index)}\u0000${String(pointIndex)}` : x;
      const row = rows.get(key) ?? { x };
      if (!positions.has(row)) positions.set(row, position(point.x));
      row[name] = point.value;
      const reference = point.control_value;
      row[referenceColumn(name)] = reference;
      row[referenceLabelColumn(name)] = reference === null ? null : point.reference_label;
      rows.set(key, row);
    });
  });
  let list = [...rows.values()];
  if (list.every((row) => Number.isFinite(positions.get(row)))) {
    // A stable sort: measurements at one instant keep their order.
    list = list.sort((a, b) => (positions.get(a) ?? 0) - (positions.get(b) ?? 0));
  }
  for (const name of seriesNames) {
    const key = referenceColumn(name);
    const labelKey = referenceLabelColumn(name);
    let held: ChartRow | null = null;
    let between: ChartRow[] = [];
    for (const row of list) {
      if (!Object.hasOwn(row, name)) {
        if (held !== null) between.push(row);
        continue;
      }
      if (held !== null) {
        for (const other of between) {
          other[key] = held[key] ?? null;
          other[labelKey] = held[labelKey] ?? null;
        }
      }
      between = [];
      held = typeof row[key] === "number" ? row : null;
    }
  }
  return list;
}

export interface ChartReference {
  /** The row column holding the reference values. */
  key: string;
  /** The row column holding each point's reference label. */
  labelKey: string;
  /** "Reference" for a single series shown, else "<series> reference". */
  name: string;
  /** Its legend label: "<name>: <label>" when the series kept one reference, else the name. */
  label: string;
  color: string | undefined;
}

/** A reference's name in the tooltip of one row: "Reference: best promoted (#42)". */
export function referenceTooltipName(reference: ChartReference, row: unknown): string {
  const label =
    typeof row === "object" && row !== null
      ? (row as Record<string, unknown>)[reference.labelKey]
      : undefined;
  return typeof label === "string" && label ? `${reference.name}: ${label}` : reference.name;
}

/**
 * The reference lines (or bars) to draw: one per shown series that has
 * reference values, in the series' own hue, or in the neutral baseline hue
 * when only one series is shown.
 */
export function chartReferences(
  rows: ChartRow[],
  shown: string[],
  colors: readonly string[],
): ChartReference[] {
  const single = shown.length === 1;
  return shown.flatMap((name, index) => {
    const key = referenceColumn(name);
    const labelKey = referenceLabelColumn(name);
    const referenced = rows.filter((row) => typeof row[key] === "number");
    if (referenced.length === 0) return [];
    const labels = new Set(referenced.map((row) => row[labelKey] ?? null));
    const [only] = labels;
    const referenceName = single ? "Reference" : `${name} reference`;
    return [
      {
        key,
        labelKey,
        name: referenceName,
        label:
          labels.size === 1 && typeof only === "string" && only
            ? `${referenceName}: ${only}`
            : referenceName,
        color: single ? "var(--chart-baseline)" : colors[index],
      },
    ];
  });
}

/**
 * The note under a chart whose view overlays a reference but some measured
 * points have none. Points verified before verifications reported
 * comparisons carry none, and nothing is backfilled: "No reference for
 * results verified before 4 Mar 2026." when every such point comes before
 * the first one with a reference (on a date axis), else a count.
 */
export function noReferenceNote(data: ViewData): string | null {
  if (typeof data.view.baseline !== "string" || !data.view.baseline) return null;
  const points = data.series.flatMap((s) => s.points).filter((p) => p.value !== null);
  const missing = points.filter((p) => p.control_value === null);
  if (missing.length === 0) return null;
  const referenced = points.filter((p) => p.control_value !== null);
  if (referenced.length === 0) {
    return "No reference for these results: they were verified before the verification reported what it compared, or it reported nothing for them.";
  }
  const time = (x: unknown) => (typeof x === "string" && DATE.test(x) ? Date.parse(x) : NaN);
  const firstReferenced = Math.min(...referenced.map((p) => time(p.x)));
  if (Number.isFinite(firstReferenced) && missing.every((p) => time(p.x) < firstReferenced)) {
    const first = referenced.find((p) => time(p.x) === firstReferenced);
    return `No reference for results verified before ${xText(first?.x)}.`;
  }
  return `${String(missing.length)} of ${String(points.length)} results have no reference: their verification reported none for this slice.`;
}
