import { HistoryIcon } from "lucide-react";

/**
 * Marks a record loaded from a reviewed research history by `cannery import`
 * (docs/import.md): it was not produced by this project's agents or
 * verifiers. An imported unit also shows its id in the source history.
 * Where it comes from (a document location or an artifact URI) is written
 * out with `showSource`, as on detail pages; otherwise it is the tooltip and
 * is read out to screen readers.
 */
export function ImportedBadge({
  origin,
  sourceRef,
  externalId,
  showSource = false,
}: {
  origin: string | undefined;
  sourceRef?: string | null | undefined;
  externalId?: string | null | undefined;
  showSource?: boolean;
}) {
  if (origin !== "imported") return null;
  const visible = showSource && Boolean(sourceRef);
  const badge = (
    <span
      data-slot="imported-badge"
      title={sourceRef ?? undefined}
      className="inline-flex items-center gap-1 rounded-full bg-status-info-bg px-2 py-0.5 text-xs font-medium text-status-info"
    >
      <HistoryIcon className="size-3.5" aria-hidden="true" />
      Imported
      {externalId ? <span className="font-normal">{externalId}</span> : null}
      {sourceRef && !visible ? <span className="sr-only">, from {sourceRef}</span> : null}
    </span>
  );
  if (!visible) return badge;
  return (
    <span className="inline-flex min-w-0 flex-wrap items-center gap-1.5">
      {badge}
      <span className="min-w-0 text-xs break-all text-muted-foreground">
        from <span className="font-mono">{sourceRef}</span>
      </span>
    </span>
  );
}
