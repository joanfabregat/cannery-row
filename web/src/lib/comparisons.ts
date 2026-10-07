import type { Schemas } from "@/api/client";

import { formatDelta } from "./format";
import { resolveLink } from "./links";
import { refTarget } from "./refs";

/**
 * What an evaluator verdict compared, as its record reports it
 * (`assessment.comparisons`): a value per metric, split and slice, and the
 * reference it was held against. Cannery Row does not judge a comparison;
 * the page only says which side the metric's direction favours.
 */

export type Reference = Schemas["Reference"];

export interface Comparison {
  metric: string;
  split: string;
  dimensions: Record<string, string>;
  value: number;
  source: string;
  reference: Reference;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asReference(value: unknown): Reference | null {
  if (!isRecord(value) || typeof value.value !== "number" || typeof value.label !== "string") {
    return null;
  }
  return {
    value: value.value,
    label: value.label,
    kind: typeof value.kind === "string" ? value.kind : "other",
    ref: typeof value.ref === "string" && value.ref.trim() ? value.ref : null,
  };
}

/** The comparisons of an evaluator record, skipping any entry it cannot read. */
export function asComparisons(value: unknown): Comparison[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry: unknown) => {
    if (!isRecord(entry)) return [];
    const reference = asReference(entry.reference);
    if (
      reference === null ||
      typeof entry.metric !== "string" ||
      typeof entry.split !== "string" ||
      typeof entry.value !== "number"
    ) {
      return [];
    }
    // A slice with a value that is not text is not one we can name: skip the entry.
    const raw = entry.dimensions ?? {};
    if (!isRecord(raw)) return [];
    const pairs = Object.entries(raw);
    if (!pairs.every(([, v]) => typeof v === "string")) return [];
    const dimensions = Object.fromEntries(pairs) as Record<string, string>;
    return [
      {
        metric: entry.metric,
        split: entry.split,
        dimensions,
        value: entry.value,
        source: typeof entry.source === "string" ? entry.source : "evaluator",
        reference,
      },
    ];
  });
}

export type Direction = "higher" | "lower";

/** A registered direction (`higher`, `lower`, or the older `higher_is_better`…). */
export function parseDirection(value: unknown): Direction | null {
  if (typeof value !== "string") return null;
  if (value.startsWith("higher")) return "higher";
  if (value.startsWith("lower")) return "lower";
  return null;
}

/**
 * Each metric's direction, keyed by metric: from the catalog's registry
 * entries (`{key, direction}`) first, else from the measurements' own
 * `direction`.
 */
export function directionsOf(
  catalog: Record<string, unknown>[] | undefined,
  measurements: { metric?: unknown; direction?: unknown }[] = [],
): Record<string, Direction> {
  const directions: Record<string, Direction> = {};
  for (const m of measurements) {
    const direction = parseDirection(m.direction);
    if (typeof m.metric === "string" && direction !== null) directions[m.metric] = direction;
  }
  for (const entry of catalog ?? []) {
    const direction = parseDirection(entry.direction);
    if (typeof entry.key === "string" && direction !== null) directions[entry.key] = direction;
  }
  return directions;
}

/**
 * The value less the reference, as shown: a difference that is only
 * floating-point noise (`0.3 - (0.1 + 0.2)`) or that the page's number
 * format shows as zero is zero, so "Same" is never shown next to a
 * non-zero difference, nor "Better" next to a zero one.
 */
export function difference(value: number, reference: number): number {
  const raw = value - reference;
  const scale = Math.max(Math.abs(value), Math.abs(reference));
  if (Math.abs(raw) <= scale * 1e-9) return 0;
  return /^[+−-]?0$/.test(formatDelta(raw)) ? 0 : raw;
}

export type Standing = "better" | "worse" | "same" | "unknown";

/** Whether the value beats the reference, given the metric's direction. */
export function standing(value: number, reference: number, direction: Direction | null): Standing {
  const delta = difference(value, reference);
  if (delta === 0) return "same";
  if (direction === null) return "unknown";
  return delta > 0 === (direction === "higher") ? "better" : "worse";
}

/**
 * Who judged, under which rules: "Judged by stock-evaluator (rules version
 * p2)". The rules are the evaluator's, not Cannery Row's.
 */
export function judgedBy(
  producer: { kind?: unknown; id?: unknown } | null | undefined,
  policyRevision: string | null | undefined,
): string | null {
  const rules = policyRevision ? ` (rules version ${policyRevision})` : "";
  if (producer?.kind === "import") {
    return `Recorded in the imported history${rules}`;
  }
  if (producer?.kind === "builtin") {
    return `Judged by Cannery Row's former built-in checks${rules}`;
  }
  if (typeof producer?.id === "string" && producer.id) return `Judged by ${producer.id}${rules}`;
  return policyRevision ? `Judged under rules version ${policyRevision}` : null;
}

export type ReferenceTarget = { kind: "internal"; to: string } | { kind: "external"; href: string };

const PROJECT_PREFIX = /^\s*([a-z0-9][a-z0-9-]{0,62})#/;

/**
 * Where a reference's `ref` leads: a hypothesis (`#12`) or attempt (`#12.3`)
 * of this project, or an http(s) address opened in a new tab. A ref naming
 * another project (`other#12`) is not followed: the comparison's evaluator
 * reports on this project only. A baseline id, a DOI or anything else stays
 * plain text (null).
 */
export function referenceTarget(
  ref: string | null | undefined,
  project: string,
): ReferenceTarget | null {
  if (!ref) return null;
  const prefix = PROJECT_PREFIX.exec(ref);
  if (prefix !== null && prefix[1] !== project) return null;
  const internal = refTarget(prefix === null ? ref : ref.replace(PROJECT_PREFIX, "#"));
  if (internal !== null) return { kind: "internal", to: internal };
  if (!/^https?:\/\//i.test(ref.trim())) return null;
  const link = resolveLink(ref.trim());
  return link?.kind === "external" ? link : null;
}
