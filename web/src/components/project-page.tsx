import type { ReactNode } from "react";

import { EmptyState } from "@/components/query-state";
import { type Project, useCurrentProject } from "@/projects/project-context";

/**
 * A page about the chosen project; without one, it says how to get one. A
 * link naming a project the user cannot open shows that, never the current
 * project's record with the same number.
 */
export function ProjectPage({ children }: { children: (project: Project) => ReactNode }) {
  const { current, unreadable } = useCurrentProject();
  if (unreadable !== null) {
    return <EmptyState>You cannot open project {unreadable}, or it does not exist.</EmptyState>;
  }
  if (current === null) {
    return (
      <EmptyState>
        You do not belong to a project yet. An administrator can add you to one.
      </EmptyState>
    );
  }
  return <>{children(current)}</>;
}
