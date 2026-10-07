import { useQuery } from "@tanstack/react-query";
import { createContext, useContext } from "react";

import { api, type Schemas, unwrap } from "@/api/client";

export type Project = Schemas["ProjectOut"];

/** Where the chosen project is remembered: per user, so people sharing a browser do not collide. */
export function projectStorageKey(userId: string): string {
  return `cannery-row.project.${userId}`;
}

const PAGE_SIZE = 200;
/** A bound on the pages read, so a misbehaving cursor cannot loop forever. */
export const MAX_PROJECT_PAGES = 10;

/** Every project the user can read (all of them for administrators), across pages. */
export async function fetchProjects(): Promise<Project[]> {
  const projects: Project[] = [];
  let before: string | undefined;
  for (let page = 0; page < MAX_PROJECT_PAGES; page++) {
    const result = unwrap(
      await api.GET("/api/projects", { params: { query: { limit: PAGE_SIZE, before } } }),
    );
    projects.push(...result.items);
    if (!result.next_before) break;
    before = result.next_before;
  }
  return projects;
}

export function useProjects({ enabled = true }: { enabled?: boolean } = {}) {
  return useQuery({ queryKey: ["projects"], enabled, queryFn: fetchProjects });
}

export interface ProjectState {
  projects: Project[];
  current: Project | null;
  /** The project a `?project=` link names when the user cannot open it, or it does not exist. */
  unreadable: string | null;
  select: (slug: string) => void;
}

export const ProjectContext = createContext<ProjectState | null>(null);

export function useCurrentProject(): ProjectState {
  const state = useContext(ProjectContext);
  if (state === null) throw new Error("useCurrentProject needs a ProjectProvider");
  return state;
}
