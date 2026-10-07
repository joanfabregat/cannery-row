import type { ReactNode } from "react";
import { Navigate, useLocation } from "react-router";

import { AppShell } from "@/components/shell/app-shell";
import { Button } from "@/components/ui/button";
import { ProjectProvider } from "@/projects/project-provider";
import { useProjects } from "@/projects/project-context";

import { useMe } from "./session";

function FullPageMessage({ children }: { children: ReactNode }) {
  return (
    <div
      role="status"
      className="flex min-h-dvh flex-col items-center justify-center gap-4 px-4 text-center text-muted-foreground"
    >
      {children}
    </div>
  );
}

/** Signed in: the shell with the user's projects. Not signed in: the sign-in page. */
export function RequireAuth() {
  const location = useLocation();
  const me = useMe();
  const projects = useProjects({ enabled: Boolean(me.data) });

  if (me.isPending) return <FullPageMessage>Loading…</FullPageMessage>;
  if (me.isError) return <NotAnswering retry={me.refetch} />;
  if (me.data === null) {
    const returnTo = `${location.pathname}${location.search}`;
    return <Navigate to={`/sign-in?return_to=${encodeURIComponent(returnTo)}`} replace />;
  }
  if (projects.isPending) return <FullPageMessage>Loading…</FullPageMessage>;
  // Not "you have no project": the list could not be read.
  if (projects.isError) return <NotAnswering retry={projects.refetch} />;
  const userId = me.data.user?.id ?? "";
  return (
    <ProjectProvider key={userId} userId={userId} projects={projects.data}>
      <AppShell me={me.data} />
    </ProjectProvider>
  );
}

function NotAnswering({ retry }: { retry: () => Promise<unknown> }) {
  return (
    <FullPageMessage>
      <p>Cannery Row is not answering right now.</p>
      <Button
        variant="outline"
        onClick={() => {
          void retry();
        }}
      >
        Try again
      </Button>
    </FullPageMessage>
  );
}
