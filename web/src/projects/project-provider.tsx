import { type ReactNode, useEffect, useState } from "react";
import { useSearchParams } from "react-router";

import { type Project, ProjectContext, projectStorageKey } from "./project-context";

function readSaved(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/**
 * The project the pages show; remembered per user in this browser, the first
 * one by default. A remembered project the user can no longer open is
 * forgotten. Give it `key={userId}` so another user starts afresh.
 *
 * A link to another project's record (`?project=slug`, from search or a
 * mention) opens that project when it is followed: choosing a project in the
 * switcher afterwards wins, and drops the link from the address. A link to a
 * project the user cannot open is reported (`unreadable`), so a page never
 * shows the current project's record with the same number instead.
 */
export function ProjectProvider({
  userId,
  projects,
  children,
}: {
  userId: string;
  projects: Project[];
  children: ReactNode;
}) {
  const storageKey = projectStorageKey(userId);
  const [slug, setSlug] = useState<string | null>(() => readSaved(storageKey));
  const [params, setParams] = useSearchParams();
  const linked = params.get("project");
  const fromLink = linked === null ? undefined : projects.find((p) => p.slug === linked);
  // The link last applied: it is applied when it changes, not on every render.
  const [applied, setApplied] = useState<string | null>(null);
  const fresh = linked !== applied;
  if (fresh) {
    setApplied(linked);
    if (fromLink !== undefined) setSlug(fromLink.slug);
  }
  // Until the state above settles, this render already shows the new link's project.
  const effective = fresh && fromLink !== undefined ? fromLink.slug : slug;
  const chosen = projects.find((p) => p.slug === effective);
  const current = chosen ?? projects[0] ?? null;
  const stale = effective !== null && chosen === undefined;
  const unreadable = linked !== null && fromLink === undefined ? linked : null;
  const chosenSlug = chosen?.slug ?? null;

  useEffect(() => {
    try {
      if (chosenSlug !== null) localStorage.setItem(storageKey, chosenSlug);
      else if (stale) localStorage.removeItem(storageKey);
    } catch {
      // Not remembered, still selected.
    }
  }, [chosenSlug, stale, storageKey]);

  const select = (next: string) => {
    setSlug(next);
    if (params.has("project")) {
      // The link was followed; the choice made now is what the page shows.
      setParams(
        (prev) => {
          const without = new URLSearchParams(prev);
          without.delete("project");
          return without;
        },
        { replace: true },
      );
    }
  };

  return (
    <ProjectContext value={{ projects, current, unreadable, select }}>{children}</ProjectContext>
  );
}
