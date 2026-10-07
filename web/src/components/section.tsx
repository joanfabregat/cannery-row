import { ChevronRightIcon } from "lucide-react";
import { type ReactNode, useId } from "react";

import { cn } from "@/lib/utils";

/** A titled block of a page; its heading names the region for screen readers. */
export function Section({
  title,
  description,
  actions,
  children,
  className,
}: {
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  className?: string;
}) {
  const id = useId();
  return (
    <section aria-labelledby={id} className={cn("rounded-lg border bg-card p-5 md:p-6", className)}>
      <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id={id} className="text-lg font-semibold tracking-tight">
            {title}
          </h2>
          {description ? <p className="mt-1 text-sm text-muted-foreground">{description}</p> : null}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

/**
 * A titled part of a section, set apart in its own frame: one record among
 * several a section groups, never merged with its neighbours.
 */
export function SubSection({
  title,
  description,
  children,
  className,
}: {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  className?: string;
}) {
  const id = useId();
  return (
    <section aria-labelledby={id} className={cn("rounded-md border p-4", className)}>
      <div className="mb-3">
        <h3 id={id} className="font-semibold">
          {title}
        </h3>
        {description ? <p className="mt-1 text-sm text-muted-foreground">{description}</p> : null}
      </div>
      {children}
    </section>
  );
}

/**
 * Depth on demand: technical detail (revisions, hashes, manifests, raw
 * envelopes) folded away, a native disclosure the keyboard and screen
 * readers already know.
 */
export function Collapsible({
  summary,
  children,
  className,
}: {
  summary: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <details className={cn("group rounded-lg border bg-card", className)}>
      <summary className="flex cursor-pointer list-none items-center gap-2 px-5 py-3 font-medium [&::-webkit-details-marker]:hidden">
        <ChevronRightIcon
          className="size-4 shrink-0 transition-transform group-open:rotate-90"
          aria-hidden="true"
        />
        {summary}
      </summary>
      <div className="border-t px-5 py-4">{children}</div>
    </details>
  );
}

/** A term and its value, for the summary lists of the detail pages. */
export function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs font-medium tracking-wide text-muted-foreground uppercase">{term}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** Raw JSON, for the Details sections only. */
export function RawJson({ value, label }: { value: unknown; label: string }) {
  return (
    <pre
      aria-label={label}
      className="max-h-96 overflow-auto rounded-md bg-muted p-3 font-mono text-xs leading-relaxed"
    >
      {JSON.stringify(value, null, 2)}
    </pre>
  );
}
