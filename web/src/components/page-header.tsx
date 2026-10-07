import type { ReactNode } from "react";

import { useDocumentTitle } from "@/lib/use-document-title";

/** The page's title (also the document title) and one plain sentence about it. */
export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string | undefined;
  actions?: ReactNode;
}) {
  useDocumentTitle(title);
  return (
    <div className="mb-8 flex flex-wrap items-start justify-between gap-4">
      <div className="max-w-2xl">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description ? <p className="mt-2 text-muted-foreground">{description}</p> : null}
      </div>
      {actions}
    </div>
  );
}
