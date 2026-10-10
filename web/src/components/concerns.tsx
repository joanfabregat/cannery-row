import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useNavigate } from "react-router";

import { api, unwrap } from "@/api/client";
import { planKey, projectKey, usePlan, useTrackConcerns } from "@/api/queries";
import type { Concern } from "@/api/types";
import { Markdown } from "@/components/markdown";
import { EmptyState, QueryView } from "@/components/query-state";
import { Collapsible, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { NativeSelect } from "@/components/ui/native-select";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { attemptPath, hypothesisPath, planEditorPath, trackPath } from "@/lib/paths";

/**
 * Concerns about a track's plan: anyone working on the track says the plan
 * is wrong, and while a concern is open no new hypothesis of the track
 * starts. A plan revision answers it, or a researcher dismisses it.
 */

const KINDS = ["wrong_assumption", "better_idea", "blocker", "other"] as const;

/** A concern document: the kind as front matter, the argument as its body. */
function concernDocument(kind: string, body: string): string {
  return `---\nkind: ${kind}\n---\n${body.trim()}\n`;
}

/** Who raised a concern, and through what. */
function raisedBy(concern: Concern): string {
  const who =
    concern.raised_by_name ??
    (concern.raised_by_kind === "service" ? "A service account" : "A member");
  const through = label("channel", concern.via_channel);
  return concern.via_client ? `${who} (${through}, ${concern.via_client})` : `${who} (${through})`;
}

/** Where a concern comes from: its hypothesis or attempt, when it names one. */
function Origin({ concern }: { concern: Concern }) {
  if (concern.hypothesis == null) return null;
  const to =
    concern.attempt == null
      ? hypothesisPath(concern.hypothesis)
      : attemptPath(concern.hypothesis, concern.attempt);
  return (
    <>
      {" "}
      from{" "}
      <Link to={to} className="underline underline-offset-4">
        #{concern.hypothesis}
        {concern.attempt == null ? "" : `.${String(concern.attempt)}`}
      </Link>
    </>
  );
}

/** A concern's heading line: its kind, where it comes from, its state. */
export function ConcernLine({
  concern,
  showTrack = false,
}: {
  concern: Concern;
  showTrack?: boolean;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <span className="text-sm">
        <span className="font-medium">{label("concernKind", concern.kind)}</span>
        {showTrack ? (
          <>
            {" "}
            about the plan of{" "}
            <Link to={trackPath(concern.track)} className="underline underline-offset-4">
              {concern.track}
            </Link>
          </>
        ) : null}
        <Origin concern={concern} />
        <span className="text-muted-foreground">
          {" "}
          · {raisedBy(concern)} · {formatDateTime(concern.raised_at)}
        </span>
      </span>
      <StatusChip domain="concern" value={concern.state} />
    </div>
  );
}

function ErrorLine({ error }: { error: unknown }) {
  if (!error) return null;
  return (
    <p role="alert" className="text-sm text-status-danger">
      {describeError(error)}
    </p>
  );
}

export function RaiseConcernDialog({ project, track }: { project: string; track: string }) {
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState<string>("wrong_assumption");
  const [body, setBody] = useState("");
  const ids = { kind: useId(), body: useId(), hint: useId() };
  const queryClient = useQueryClient();
  const raise = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/concerns", {
          params: { path: { slug: project, track_slug: track } },
          body: { document: concernDocument(kind, body) },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setBody("");
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) raise.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline">Raise a concern</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Raise a concern about the plan</DialogTitle>
        <DialogDescription>
          Say what is wrong with the plan itself: an assumption it relies on, a better way to test
          the track&apos;s idea, or something that stops it from being carried out. While the
          concern is open, no new hypothesis of this track starts; work already started continues.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (body.trim()) raise.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.kind} className="text-sm font-medium">
              Kind
            </label>
            <NativeSelect
              id={ids.kind}
              value={kind}
              onChange={(event) => {
                setKind(event.target.value);
              }}
            >
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {label("concernKind", k)}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.body} className="text-sm font-medium">
              Argument
            </label>
            <p id={ids.hint} className="text-xs text-muted-foreground">
              Markdown, at most 16 KiB: what you saw, why it matters to the plan, and what you would
              change.
            </p>
            <Textarea
              id={ids.body}
              aria-describedby={ids.hint}
              value={body}
              className="min-h-32"
              onChange={(event) => {
                setBody(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={raise.error} />
          <DialogFooter>
            <Button type="submit" disabled={raise.isPending || !body.trim()}>
              Raise the concern
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DismissConcernDialog({ project, concern }: { project: string; concern: Concern }) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const id = useId();
  const queryClient = useQueryClient();
  const dismiss = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/concerns/{concern_id}/dismissal", {
          params: { path: { slug: project, concern_id: concern.id } },
          body: { reason: reason.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) dismiss.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button size="sm" variant="outline">
          Dismiss
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Dismiss the concern</DialogTitle>
        <DialogDescription>
          The plan stays as it is and the concern closes. If no other concern is open, the
          track&apos;s hypotheses can start again.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (reason.trim()) dismiss.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={id} className="text-sm font-medium">
              Reason
            </label>
            <Textarea
              id={id}
              value={reason}
              onChange={(event) => {
                setReason(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={dismiss.error} />
          <DialogFooter>
            <Button type="submit" disabled={dismiss.isPending || !reason.trim()}>
              Dismiss the concern
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Open the plan editor on the track's draft, starting one if needed, with the concern listed first. */
function ReviseThePlan({
  project,
  track,
  concern,
}: {
  project: string;
  track: string;
  concern: Concern;
}) {
  const draft = usePlan(project, track, "draft");
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const editor = `${planEditorPath(track)}?answer=${encodeURIComponent(concern.id)}`;
  const start = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/plans", {
          params: { path: { slug: project, track_slug: track } },
        }),
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: planKey(project, track) });
      await navigate(editor);
    },
  });
  const submitted = draft.data?.state === "submitted";
  return (
    <div className="flex flex-col gap-1">
      <Button
        size="sm"
        disabled={draft.isPending || submitted || start.isPending}
        title={submitted ? "A revision is waiting for review; review it first." : undefined}
        onClick={() => {
          if (draft.data?.state === "draft") {
            void navigate(editor);
          } else {
            start.mutate();
          }
        }}
      >
        Revise the plan
      </Button>
      <ErrorLine error={start.error} />
    </div>
  );
}

function OpenConcern({
  project,
  track,
  concern,
  isResearcher,
}: {
  project: string;
  track: string;
  concern: Concern;
  isResearcher: boolean;
}) {
  return (
    <li className="flex flex-col gap-2 py-3">
      <ConcernLine concern={concern} />
      <Markdown>{concern.body}</Markdown>
      {isResearcher ? (
        <div className="flex flex-wrap gap-2">
          <ReviseThePlan project={project} track={track} concern={concern} />
          <DismissConcernDialog project={project} concern={concern} />
        </div>
      ) : null}
    </li>
  );
}

function ClosedConcern({ concern }: { concern: Concern }) {
  return (
    <li className="flex flex-col gap-1 py-3">
      <ConcernLine concern={concern} />
      <Markdown>{concern.body}</Markdown>
      <p className="text-sm text-muted-foreground">
        {concern.state === "answered"
          ? `Answered by revision ${String(concern.answered_by_revision ?? "")} of the plan.`
          : `Dismissed by ${concern.dismissed_by_name ?? "a researcher"}: “${concern.dismissal_reason ?? ""}”`}
      </p>
    </li>
  );
}

export function TrackConcerns({
  project,
  track,
  archived,
  canRaise,
  isResearcher,
}: {
  project: string;
  track: string;
  archived: boolean;
  canRaise: boolean;
  isResearcher: boolean;
}) {
  const concerns = useTrackConcerns(project, track);
  return (
    <Section
      title="Concerns"
      description="A concern says the plan itself is wrong. While one is open, no new hypothesis of this track starts; work already started continues. A plan revision answers it, or a researcher dismisses it."
      actions={
        canRaise && !archived ? <RaiseConcernDialog project={project} track={track} /> : null
      }
    >
      <QueryView query={concerns}>
        {(page) => {
          const open = page.items.filter((c) => c.state === "open");
          const closed = page.items.filter((c) => c.state !== "open");
          return (
            <div className="flex flex-col gap-4">
              {open.length === 0 ? (
                <EmptyState>No concern is open.</EmptyState>
              ) : (
                <ul aria-label="Open concerns" className="flex flex-col divide-y">
                  {open.map((concern) => (
                    <OpenConcern
                      key={concern.id}
                      project={project}
                      track={track}
                      concern={concern}
                      isResearcher={isResearcher}
                    />
                  ))}
                </ul>
              )}
              {closed.length > 0 ? (
                <Collapsible summary={`Closed concerns (${String(closed.length)})`}>
                  <ul className="flex flex-col divide-y">
                    {closed.map((concern) => (
                      <ClosedConcern key={concern.id} concern={concern} />
                    ))}
                  </ul>
                </Collapsible>
              ) : null}
            </div>
          );
        }}
      </QueryView>
    </Section>
  );
}
