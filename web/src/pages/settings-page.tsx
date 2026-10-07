import { useQueryClient } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { useTokens } from "@/api/queries";
import { useMe } from "@/auth/session";
import { PageHeader } from "@/components/page-header";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
import { AdminSections } from "@/components/settings/admin";
import { NewTokenDialog, TokenRows } from "@/components/settings/tokens";
import { navItems } from "@/components/shell/nav-items";
import { isAdmin } from "@/lib/roles";
import { useCurrentProject } from "@/projects/project-context";

const item = navItems.find((entry) => entry.to === "/settings");

/** Everyone manages their tokens; only administrators see the admin sections (hidden, not disabled). */
export function SettingsPage() {
  const { data: me } = useMe();
  const { current } = useCurrentProject();
  return (
    <>
      <PageHeader title="Settings" description={item?.description} />
      <div className="flex flex-col gap-6">
        <PersonalTokens />
        {isAdmin(me) ? (
          <AdminSections current={current} canWrite={me?.scopes.includes("write") === true} />
        ) : null}
      </div>
    </>
  );
}

function PersonalTokens() {
  const tokens = useTokens();
  const queryClient = useQueryClient();
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ["tokens"] });
  };
  return (
    <Section
      title="Access tokens"
      description="Personal tokens let command-line tools and agents act as you. Create, review and revoke them here."
      actions={
        <NewTokenDialog
          trigger="New token"
          title="New personal token"
          description="The token acts as you, within the projects and roles you have."
          create={async (body) => unwrap(await api.POST("/api/tokens", { body }))}
          onCreated={refresh}
        />
      }
    >
      <QueryView query={tokens}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>You have no token yet.</EmptyState>
          ) : (
            <TokenRows tokens={page.items} onRevoked={refresh} />
          )
        }
      </QueryView>
    </Section>
  );
}
