import { Link, useParams } from "react-router";

import { useHypotheses, useTrack, useTrackHistory } from "@/api/queries";
import type { Track } from "@/api/types";
import { TrackExecution } from "@/components/execution";
import { Markdown } from "@/components/markdown";
import { Notice } from "@/components/notice";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading, QueryView } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { EditTrackDialog, TransitionDialog } from "@/components/track-forms";
import { TrackPlan } from "@/components/track-plan";
import { formatDateTime } from "@/lib/format";
import { humanize, label } from "@/lib/labels";
import { useNotice } from "@/lib/navigation";
import { hypothesisPath } from "@/lib/paths";
import { TRACK_TRANSITIONS } from "@/lib/states";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

export function TrackPage() {
  const { track = "" } = useParams();
  return <ProjectPage>{(project) => <TrackView project={project} slug={track} />}</ProjectPage>;
}

function TrackView({ project, slug }: { project: Project; slug: string }) {
  const track = useTrack(project.slug, slug);
  const { isResearcher } = usePermissions();
  const notice = useNotice();
  if (track.isPending) {
    return (
      <>
        <PageHeader title="Track" />
        <Loading />
      </>
    );
  }
  if (track.isError) {
    return (
      <>
        <PageHeader title="Track" />
        <LoadError
          error={track.error}
          retry={track.refetch}
          notFound={`There is no track “${slug}” in ${project.title}.`}
        />
      </>
    );
  }
  const t = track.data;
  return (
    <>
      <PageHeader
        title={t.title}
        description={`Track ${t.slug}`}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <StatusChip domain="track" value={t.state} className="text-sm" />
            {isResearcher ? (
              <>
                {t.state !== "archived" ? (
                  <EditTrackDialog project={project.slug} track={t} />
                ) : null}
                {(TRACK_TRANSITIONS[t.state] ?? []).map((to) => (
                  <TransitionDialog key={to} project={project.slug} track={t} to={to} />
                ))}
              </>
            ) : null}
          </div>
        }
      />
      <Notice>{notice}</Notice>
      <div className="flex flex-col gap-6">
        <Section title="About">
          <div className="flex flex-col gap-4">
            {t.description ? (
              <Markdown>{t.description}</Markdown>
            ) : (
              <p className="text-sm text-muted-foreground">No description.</p>
            )}
            <dl className="grid gap-4 md:grid-cols-2">
              <Fact term="Status">{TRACK_STATE_WORDS[t.state] ?? humanize(t.state)}</Fact>
              <Fact term="Tested with">
                {t.producer
                  ? `The ${String(t.producer.name)} producer (revision ${String(t.producer.revision)})`
                  : "The project's default producer"}
              </Fact>
            </dl>
          </div>
        </Section>
        <TrackPlan
          project={project.slug}
          track={t.slug}
          archived={t.state === "archived"}
          isResearcher={isResearcher}
        />
        <TrackExecution track={t} />
        <TrackHypotheses project={project.slug} track={t.slug} />
        <TrackDetails project={project.slug} track={t} />
      </div>
    </>
  );
}

const TRACK_STATE_WORDS: Record<string, string> = {
  planning: "Planning: nothing in it can start until a researcher approves its first plan.",
  active: "Active: it accepts drafts, and its queued hypotheses can start.",
  paused: "Paused: drafts can be written and reviewed, but its queued hypotheses do not start.",
  archived: "Archived: read-only; it accepts no new draft.",
};

function TrackHypotheses({ project, track }: { project: string; track: string }) {
  const hypotheses = useHypotheses(project, { track, archived: null, limit: 20 });
  return (
    <Section
      title="Hypotheses in this track"
      actions={
        <Link
          to={`/hypotheses?track=${encodeURIComponent(track)}&archived=1`}
          className="text-sm font-medium underline underline-offset-4"
        >
          See all in the list
        </Link>
      }
    >
      <QueryView query={hypotheses}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>No hypothesis in this track yet.</EmptyState>
          ) : (
            <ul className="flex flex-col divide-y">
              {page.items.map((h) => (
                <li
                  key={h.number}
                  className="flex flex-wrap items-center justify-between gap-2 py-2.5"
                >
                  <Link to={hypothesisPath(h.number)} className="font-medium hover:underline">
                    {h.ref} {h.title}
                  </Link>
                  <StatusChip domain="hypothesis" value={h.state} />
                </li>
              ))}
            </ul>
          )
        }
      </QueryView>
    </Section>
  );
}

function TrackDetails({ project, track }: { project: string; track: Track }) {
  const history = useTrackHistory(project, track.slug);
  return (
    <Collapsible summary="Details">
      <div className="flex flex-col gap-5">
        <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <Fact term="Revision">{track.revision}</Fact>
          <Fact term="Created">{formatDateTime(track.created_at)}</Fact>
          <Fact term="Last change">{formatDateTime(track.updated_at)}</Fact>
        </dl>
        <div className="flex flex-col gap-2">
          <h3 className="text-sm font-medium">History</h3>
          <QueryView query={history}>
            {(page) => (
              <ol className="flex flex-col gap-2 text-sm">
                {page.items.map((event) => (
                  <li key={event.seq}>
                    {formatDateTime(event.occurred_at)} ·{" "}
                    {humanize(event.action.replace(/^track\./, ""))}
                    {event.reason ? ` · “${event.reason}”` : ""} ·{" "}
                    {label("channel", event.via_channel)}
                  </li>
                ))}
              </ol>
            )}
          </QueryView>
        </div>
        {track.producer ? <RawJson value={track.producer} label="Producer binding" /> : null}
        {track.workflow ? <RawJson value={track.workflow} label="Workflow binding" /> : null}
      </div>
    </Collapsible>
  );
}
