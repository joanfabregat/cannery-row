import { useMembers } from "@/api/queries";
import { useMe } from "@/auth/session";

import { useCurrentProject } from "./project-context";

/**
 * Names for the people behind user ids (comment authors, decision makers),
 * from the project's member list, which every member can read. The signed-in
 * user is "You"; someone no longer a member is "A former member".
 */
export function usePeople(): (userId: string | null | undefined) => string {
  const { data: me } = useMe();
  const { current } = useCurrentProject();
  const members = useMembers(current?.slug ?? "", current !== null);
  const names = new Map(
    (members.data?.items ?? []).map((m) => [m.user_id, m.display_name ?? m.email ?? "A member"]),
  );
  return (userId) => {
    if (!userId) return "An agent";
    if (userId === me?.user?.id) return "You";
    return names.get(userId) ?? (members.isPending ? "…" : "A former member");
  };
}
