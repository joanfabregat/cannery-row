import { Link } from "react-router";

import { useTracks } from "@/api/queries";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { navItems } from "@/components/shell/nav-items";
import { StatusChip } from "@/components/status-chip";
import { NewTrackDialog } from "@/components/track-forms";
import { excerpt } from "@/lib/format";
import { trackPath } from "@/lib/paths";
import { type Project, useCurrentProject } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

const item = navItems.find((entry) => entry.to === "/tracks");

export function TracksPage() {
  const { isResearcher } = usePermissions();
  const { current } = useCurrentProject();
  return (
    <>
      <PageHeader
        title="Tracks"
        description={item?.description}
        actions={
          isResearcher && current !== null ? <NewTrackDialog project={current.slug} /> : null
        }
      />
      <ProjectPage>{(project) => <TrackList project={project} />}</ProjectPage>
    </>
  );
}

function TrackList({ project }: { project: Project }) {
  const tracks = useTracks(project.slug);
  return (
    <QueryView query={tracks}>
      {(page) =>
        page.items.length === 0 ? (
          <EmptyState>No track yet. A researcher can create the first one.</EmptyState>
        ) : (
          <ul className="grid gap-4 md:grid-cols-2">
            {page.items.map((track) => (
              <li key={track.slug} className="flex flex-col gap-2 rounded-lg border bg-card p-5">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <Link
                    to={trackPath(track.slug)}
                    className="text-base font-semibold hover:underline"
                  >
                    {track.title}
                  </Link>
                  <StatusChip domain="track" value={track.state} />
                </div>
                {track.description ? (
                  <p className="text-sm text-muted-foreground">{excerpt(track.description, 200)}</p>
                ) : null}
                <p className="text-xs text-muted-foreground">
                  {track.producer
                    ? `Tested with ${String(track.producer.name)}`
                    : "Tested with the project's default producer"}
                </p>
              </li>
            ))}
          </ul>
        )
      }
    </QueryView>
  );
}
