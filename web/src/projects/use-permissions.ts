import { useMe } from "@/auth/session";
import { type Permissions, permissionsFor } from "@/lib/roles";

import { useCurrentProject } from "./project-context";

/** The signed-in user's permissions in the current project (from `/api/me`). */
export function usePermissions(): Permissions {
  const { data: me } = useMe();
  const { current } = useCurrentProject();
  return permissionsFor(me, current?.slug ?? null);
}
