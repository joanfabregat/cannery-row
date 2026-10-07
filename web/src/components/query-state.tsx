import type { ReactNode } from "react";

import { Button } from "@/components/ui/button";
import { isNotFound } from "@/lib/errors";
import { cn } from "@/lib/utils";

/** A calm "loading" line, announced to screen readers. */
export function Loading({ children = "Loading…" }: { children?: ReactNode }) {
  return (
    <p role="status" className="py-6 text-sm text-muted-foreground">
      {children}
    </p>
  );
}

/**
 * A query that failed: not found says so; anything else is the shell's "not
 * answering" message with a way to try again.
 */
export function LoadError({
  error,
  retry,
  notFound = "This could not be found. It may not exist, or you may not have access to it.",
}: {
  error: unknown;
  retry: () => unknown;
  notFound?: string;
}) {
  if (isNotFound(error)) {
    return (
      <p role="alert" className="py-6 text-sm text-muted-foreground">
        {notFound}
      </p>
    );
  }
  return (
    <div role="alert" className="flex flex-wrap items-center gap-3 py-6 text-sm">
      <p className="text-muted-foreground">Cannery Row is not answering right now.</p>
      <Button
        variant="outline"
        size="sm"
        onClick={() => {
          void retry();
        }}
      >
        Try again
      </Button>
    </div>
  );
}

/** Nothing to show, said in plain words. */
export function EmptyState({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "rounded-lg border border-dashed bg-card p-6 text-sm text-muted-foreground",
        className,
      )}
    >
      {children}
    </div>
  );
}

interface QueryLike<T> {
  data: T | undefined;
  error: unknown;
  isPending: boolean;
  isError: boolean;
  refetch: () => unknown;
}

/** Loading and error states for a query; `children` renders its data. */
export function QueryView<T>({
  query,
  children,
  loading,
  notFound,
}: {
  query: QueryLike<T>;
  children: (data: T) => ReactNode;
  loading?: ReactNode;
  notFound?: string;
}) {
  if (query.isPending) return <Loading>{loading}</Loading>;
  if (query.isError || query.data === undefined) {
    return <LoadError error={query.error} retry={query.refetch} notFound={notFound} />;
  }
  return <>{children(query.data)}</>;
}
