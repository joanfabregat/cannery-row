import type { Me } from "@/auth/session";

export type ProjectRole = "viewer" | "member" | "researcher";

const rank: Record<ProjectRole, number> = { viewer: 1, member: 2, researcher: 3 };

function isProjectRole(value: string): value is ProjectRole {
  return Object.hasOwn(rank, value);
}

/** Whether a project role grants at least `needed`. */
export function hasRole(role: string | null | undefined, needed: ProjectRole): boolean {
  return role != null && isProjectRole(role) && rank[role] >= rank[needed];
}

/** Installation administrators: projects, memberships, service accounts. */
export function isAdmin(me: Me | null | undefined): boolean {
  return me?.user?.is_admin === true;
}

/**
 * What the signed-in user may do in one project, mirroring the backend's
 * checks so that actions a role cannot perform are hidden, not disabled. The
 * backend still enforces every one of them.
 *
 * An administrator who is not a member reads every project but acts in none:
 * decisions, comments and track management need a membership role.
 */
export interface Permissions {
  role: ProjectRole | null;
  isAdmin: boolean;
  /** Comment, and edit one's own comments. */
  canComment: boolean;
  /** Download every artifact, not only a report's images. */
  canDownloadArtifacts: boolean;
  /** Write and review plans, record result and failure decisions, manage tracks. */
  isResearcher: boolean;
}

export function projectRole(me: Me | null | undefined, project: string | null): ProjectRole | null {
  if (!me || project === null) return null;
  const role = me.memberships?.find((m) => m.project === project)?.role;
  return role != null && isProjectRole(role) ? role : null;
}

export function permissionsFor(me: Me | null | undefined, project: string | null): Permissions {
  const role = projectRole(me, project);
  const admin = isAdmin(me);
  const writes = me?.scopes.includes("write") === true;
  return {
    role,
    isAdmin: admin,
    canComment: writes && hasRole(role, "member"),
    canDownloadArtifacts: admin || hasRole(role, "member"),
    isResearcher: writes && hasRole(role, "researcher"),
  };
}
