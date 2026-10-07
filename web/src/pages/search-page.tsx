import { useId, useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";

import { type SearchFilters, useSearch } from "@/api/queries";
import type { SearchHit } from "@/api/types";
import { Snippet } from "@/components/markdown";
import { PageHeader } from "@/components/page-header";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { navItems } from "@/components/shell/nav-items";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import { formatDateTime } from "@/lib/format";
import { label, statusLabel } from "@/lib/labels";
import { attemptPath, hypothesisPath, parseAttemptRef, trackPath } from "@/lib/paths";
import { refTarget } from "@/lib/refs";
import { useCurrentProject } from "@/projects/project-context";

const item = navItems.find((entry) => entry.to === "/search");

/** The facets offered as filters, with how each value is named. */
const FACETS: { name: keyof Omit<SearchFilters, "q" | "before">; title: string }[] = [
  { name: "kind", title: label("searchFacet", "kind") },
  { name: "project", title: label("searchFacet", "project") },
  { name: "track", title: label("searchFacet", "track") },
  { name: "hypothesis_state", title: label("searchFacet", "hypothesis_state") },
  { name: "verdict", title: label("searchFacet", "verdict") },
  { name: "decision", title: label("searchFacet", "decision") },
];

function facetValueLabel(facet: string, value: string): string {
  switch (facet) {
    case "kind":
      return label("searchKind", value);
    case "hypothesis_state":
      return statusLabel("hypothesis", value);
    case "verdict":
      return statusLabel("verdict", value);
    case "decision":
      return statusLabel("decision", value);
    default:
      return value;
  }
}

/** The page a hit is about: an attempt's records open the attempt. */
function hitPath(hit: SearchHit, currentProject: string | null): string {
  const project = hit.project === currentProject ? null : hit.project;
  if (hit.kind === "track" || hit.hypothesis === null) {
    return hit.track ? trackPath(hit.track, project) : "/tracks";
  }
  const attempt = parseAttemptRef(hit.attempt_ref);
  return attempt === null
    ? hypothesisPath(hit.hypothesis, project)
    : attemptPath(attempt[0], attempt[1], project);
}

export function SearchPage() {
  const [params] = useSearchParams();
  const query = params.get("q")?.trim() ?? "";
  const target = refTarget(query);
  if (target !== null) return <Navigate to={target} replace />;
  return (
    <>
      <PageHeader title="Search" description={item?.description} />
      {query ? (
        <Results key={query} query={query} />
      ) : (
        <EmptyState>
          Type in the search bar above to find hypotheses, reports, decisions and comments. Type #12
          to go straight to hypothesis 12.
        </EmptyState>
      )}
    </>
  );
}

function Results({ query }: { query: string }) {
  const [params, setParams] = useSearchParams();
  const { current } = useCurrentProject();
  const chosen = (name: string) => params.getAll(name);
  const filters: SearchFilters = { q: query };
  for (const { name } of FACETS) {
    const values = chosen(name);
    if (values.length > 0) filters[name] = values;
  }
  const [cursors, setCursors] = useState<(string | undefined)[]>([]);
  const [before, setBefore] = useState<string | undefined>(undefined);
  const [filterKey, setFilterKey] = useState(params.toString());
  if (filterKey !== params.toString()) {
    setFilterKey(params.toString());
    setCursors([]);
    setBefore(undefined);
  }
  const search = useSearch({ ...filters, before }, true);

  const toggle = (name: string, value: string) => {
    const next = new URLSearchParams(params);
    const values = next.getAll(name);
    next.delete(name);
    const updated = values.includes(value) ? values.filter((v) => v !== value) : [...values, value];
    updated.forEach((v) => {
      next.append(name, v);
    });
    setParams(next, { replace: true });
  };

  if (search.isPending) return <Loading>Searching…</Loading>;
  if (search.isError) return <LoadError error={search.error} retry={search.refetch} />;
  const data = search.data;
  return (
    <div className="grid gap-6 lg:grid-cols-[16rem_1fr]">
      <aside aria-label="Filters" className="flex flex-col gap-5">
        {FACETS.map(({ name, title }) => {
          const counts = data.facets[name] ?? {};
          const values = Object.entries(counts).sort((a, b) => b[1] - a[1]);
          const selected = chosen(name);
          // Keep a selected value visible even when it no longer matches anything.
          for (const value of selected) if (!(value in counts)) values.push([value, 0]);
          if (values.length === 0) return null;
          if (name === "project" && values.length < 2 && selected.length === 0) return null;
          return (
            <FacetGroup
              key={name}
              title={title}
              values={values.map(([value, count]) => ({
                value,
                label: facetValueLabel(name, value),
                count,
                checked: selected.includes(value),
              }))}
              onToggle={(value) => {
                toggle(name, value);
              }}
            />
          );
        })}
      </aside>
      <div className="flex flex-col gap-4">
        <p role="status" className="text-sm text-muted-foreground">
          {data.total === 1 ? "1 result" : `${data.total} results`} for “{query}”
        </p>
        {data.items.length === 0 ? (
          <EmptyState>Nothing matches “{query}”. Try other words, or remove a filter.</EmptyState>
        ) : (
          <ol className="flex flex-col gap-3">
            {data.items.map((hit) => (
              <li key={`${hit.kind}-${hit.source_id}`} className="rounded-lg border bg-card p-4">
                <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {label("searchKind", hit.kind)}
                  {hit.project !== current?.slug ? ` · ${hit.project}` : ""}
                  {hit.track ? ` · ${hit.track}` : ""}
                </p>
                <Link
                  to={hitPath(hit, current?.slug ?? null)}
                  className="mt-1 block font-medium hover:underline"
                >
                  {hit.attempt_ref ?? hit.ref ?? ""} {hit.title}
                </Link>
                <p className="mt-1 text-sm text-muted-foreground">
                  <Snippet text={hit.snippet} />
                </p>
                <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                  {hit.hypothesis_state ? (
                    <StatusChip domain="hypothesis" value={hit.hypothesis_state} />
                  ) : null}
                  {hit.verdict ? <StatusChip domain="verdict" value={hit.verdict} /> : null}
                  {hit.decision ? <StatusChip domain="decision" value={hit.decision} /> : null}
                  <span>{formatDateTime(hit.occurred_at)}</span>
                </div>
              </li>
            ))}
          </ol>
        )}
        {cursors.length > 0 || data.next_before !== null ? (
          <nav aria-label="Pages" className="flex justify-between gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={cursors.length === 0 || search.isPlaceholderData}
              onClick={() => {
                setBefore(cursors.at(-1));
                setCursors(cursors.slice(0, -1));
              }}
            >
              Previous results
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={data.next_before === null || search.isPlaceholderData}
              onClick={() => {
                if (data.next_before === null) return;
                setCursors([...cursors, before]);
                setBefore(data.next_before);
              }}
            >
              More results
            </Button>
          </nav>
        ) : null}
      </div>
    </div>
  );
}

function FacetGroup({
  title,
  values,
  onToggle,
}: {
  title: string;
  values: { value: string; label: string; count: number; checked: boolean }[];
  onToggle: (value: string) => void;
}) {
  const id = useId();
  return (
    <fieldset className="flex flex-col gap-1.5">
      <legend className="mb-1 text-sm font-medium">{title}</legend>
      {values.map((v) => (
        <div key={v.value} className="flex items-center gap-2 text-sm">
          <input
            id={`${id}-${v.value}`}
            type="checkbox"
            className="size-4 accent-primary"
            checked={v.checked}
            onChange={() => {
              onToggle(v.value);
            }}
          />
          <label htmlFor={`${id}-${v.value}`} className="flex-1">
            {v.label}
          </label>
          <span className="text-xs text-muted-foreground">{v.count}</span>
        </div>
      ))}
    </fieldset>
  );
}
