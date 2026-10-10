import { createColumnHelper } from "@tanstack/react-table";
import { useId, useState } from "react";
import { Link, useSearchParams } from "react-router";

import { useUnits, useTracks } from "@/api/queries";
import type { UnitSummary } from "@/api/types";
import { DataTable } from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { navItems } from "@/components/shell/nav-items";
import { ImportedBadge } from "@/components/imported-badge";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import { NativeSelect } from "@/components/ui/native-select";
import { formatDate } from "@/lib/format";
import { statusLabel } from "@/lib/labels";
import { unitPath, trackPath } from "@/lib/paths";
import { UNIT_STATES, isUnitState } from "@/lib/states";
import type { Project } from "@/projects/project-context";

const item = navItems.find((entry) => entry.to === "/units");

const column = createColumnHelper<UnitSummary>();

const columns = [
  column.accessor("title", {
    header: "Unit",
    cell: (info) => (
      <Link
        to={unitPath(info.row.original.number)}
        className="flex flex-col font-medium hover:underline"
      >
        <span className="text-xs font-normal text-muted-foreground">{info.row.original.ref}</span>{" "}
        <span>{info.getValue()}</span>
      </Link>
    ),
  }),
  column.accessor("track", {
    header: "Track",
    cell: (info) => (
      <Link to={trackPath(info.getValue())} className="hover:underline">
        {info.getValue()}
      </Link>
    ),
  }),
  column.accessor("state", {
    header: "Status",
    cell: (info) => (
      <span className="flex flex-wrap items-center gap-1.5">
        <StatusChip domain="unit" value={info.getValue()} />
        <ImportedBadge
          origin={info.row.original.origin}
          sourceRef={info.row.original.source_ref}
          externalId={info.row.original.external_id}
        />
      </span>
    ),
  }),
  column.accessor("updated_at", {
    header: "Last change",
    cell: (info) => <span className="whitespace-nowrap">{formatDate(info.getValue())}</span>,
  }),
];

export function UnitsPage() {
  return (
    <>
      <PageHeader title="Units" description={item?.description} />
      <ProjectPage>{(project) => <UnitList key={project.slug} project={project} />}</ProjectPage>
    </>
  );
}

function UnitList({ project }: { project: Project }) {
  const [params, setParams] = useSearchParams();
  const track = params.get("track") ?? undefined;
  const rawState = params.get("state");
  const state = isUnitState(rawState) ? rawState : undefined;
  const showArchived = params.get("archived") === "1";
  // Cursors of the pages before this one, to go back.
  const [cursors, setCursors] = useState<(number | undefined)[]>([]);
  const [before, setBefore] = useState<number | undefined>(undefined);
  const [filterKey, setFilterKey] = useState(params.toString());
  if (filterKey !== params.toString()) {
    // New filters start from the first page.
    setFilterKey(params.toString());
    setCursors([]);
    setBefore(undefined);
  }

  const units = useUnits(project.slug, {
    track,
    state,
    archived: showArchived ? null : false,
    before,
  });
  const tracks = useTracks(project.slug);

  const update = (name: string, value: string | null) => {
    const next = new URLSearchParams(params);
    if (value === null || value === "") next.delete(name);
    else next.set(name, value);
    setParams(next, { replace: true });
  };

  const trackId = useId();
  const stateId = useId();
  const archivedId = useId();

  return (
    <div className="flex flex-col gap-4">
      <form
        aria-label="Filter units"
        className="flex flex-wrap items-end gap-4"
        onSubmit={(event) => {
          event.preventDefault();
        }}
      >
        <div className="flex min-w-48 flex-col gap-1.5">
          <label htmlFor={trackId} className="text-sm font-medium">
            Track
          </label>
          <NativeSelect
            id={trackId}
            value={track ?? ""}
            onChange={(event) => {
              update("track", event.target.value);
            }}
          >
            <option value="">All tracks</option>
            {(tracks.data?.items ?? []).map((t) => (
              <option key={t.slug} value={t.slug}>
                {t.title}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex min-w-48 flex-col gap-1.5">
          <label htmlFor={stateId} className="text-sm font-medium">
            Status
          </label>
          <NativeSelect
            id={stateId}
            value={state ?? ""}
            onChange={(event) => {
              update("state", event.target.value);
            }}
          >
            <option value="">Any status</option>
            {UNIT_STATES.map((value) => (
              <option key={value} value={value}>
                {statusLabel("unit", value)}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex items-center gap-2 pb-2">
          <input
            id={archivedId}
            type="checkbox"
            className="size-4 accent-primary"
            checked={showArchived}
            disabled={state !== undefined}
            onChange={(event) => {
              update("archived", event.target.checked ? "1" : null);
            }}
          />
          <label htmlFor={archivedId} className="text-sm">
            Show archived (rejected, inconclusive, failed, cancelled)
          </label>
        </div>
      </form>

      {units.isPending ? (
        <Loading />
      ) : units.isError ? (
        <LoadError error={units.error} retry={units.refetch} />
      ) : units.data.items.length === 0 ? (
        <EmptyState>
          {track || state || !showArchived
            ? "No unit matches these filters."
            : "No unit yet. Units appear here once a researcher approves a plan."}
          {!showArchived && state === undefined ? (
            <> Archived units are hidden; tick “Show archived” to see them.</>
          ) : null}
        </EmptyState>
      ) : (
        <>
          <div className="rounded-lg border bg-card">
            <DataTable
              data={units.data.items}
              columns={columns}
              caption="Units, newest first"
              rowKey={(row) => String(row.number)}
            />
          </div>
          <nav aria-label="Pages" className="flex items-center justify-between gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={cursors.length === 0 || units.isPlaceholderData}
              onClick={() => {
                setBefore(cursors.at(-1));
                setCursors(cursors.slice(0, -1));
              }}
            >
              Newer
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={units.data.next_before === null || units.isPlaceholderData}
              onClick={() => {
                const next = units.data.next_before;
                if (next === null) return;
                setCursors([...cursors, before]);
                setBefore(next);
              }}
            >
              Older
            </Button>
          </nav>
        </>
      )}
    </div>
  );
}
