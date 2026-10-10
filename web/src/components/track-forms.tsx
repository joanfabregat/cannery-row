import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useId, useState } from "react";
import { useNavigate } from "react-router";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { projectKey } from "@/api/queries";
import type { Track } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { trackPath } from "@/lib/paths";

/** Track management for researchers: create, edit, and change a track's state with a reason. */

const SLUG = /^[a-z0-9][a-z0-9-]{0,62}$/;

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: (id: string, hintId: string | undefined) => ReactNode;
}) {
  const id = useId();
  const hintId = useId();
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      {hint ? (
        <p id={hintId} className="text-xs text-muted-foreground">
          {hint}
        </p>
      ) : null}
      {children(id, hint ? hintId : undefined)}
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

export function NewTrackDialog({ project }: { project: string }) {
  const [open, setOpen] = useState(false);
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const create = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks", {
          params: { path: { slug: project } },
          body: {
            slug,
            title: title.trim(),
            ...(description.trim() ? { description: description.trim() } : {}),
          },
        }),
      ),
    onSuccess: async (track) => {
      setOpen(false);
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
      await navigate(trackPath(track.slug), { state: { notice: "The track was created." } });
    },
  });
  const slugValid = SLUG.test(slug);
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) create.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button>New track</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>New track</DialogTitle>
        <DialogDescription>
          A track is one line of research; every unit belongs to one track. It starts active and
          uses the project's default producer.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (slugValid && title.trim()) create.mutate();
          }}
        >
          <Field label="Title">
            {(id) => (
              <Input
                id={id}
                value={title}
                required
                onChange={(event) => {
                  setTitle(event.target.value);
                }}
              />
            )}
          </Field>
          <Field
            label="Short name"
            hint="Lowercase letters, digits and dashes, used in addresses (for example dense-hybrid). It cannot change later."
          >
            {(id, hintId) => (
              <Input
                id={id}
                value={slug}
                required
                aria-describedby={hintId}
                aria-invalid={slug !== "" && !slugValid}
                onChange={(event) => {
                  setSlug(event.target.value);
                }}
              />
            )}
          </Field>
          <Field label="Description" hint="Markdown: the approach this track explores.">
            {(id, hintId) => (
              <Textarea
                id={id}
                aria-describedby={hintId}
                value={description}
                onChange={(event) => {
                  setDescription(event.target.value);
                }}
              />
            )}
          </Field>
          <ErrorLine error={create.error} />
          <DialogFooter>
            <Button type="submit" disabled={!slugValid || !title.trim() || create.isPending}>
              Create track
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function EditTrackDialog({ project, track }: { project: string; track: Track }) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(track.title);
  const [description, setDescription] = useState(track.description);
  const queryClient = useQueryClient();
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.PATCH("/api/projects/{slug}/tracks/{track_slug}", {
          params: { path: { slug: project, track_slug: track.slug } },
          body: {
            expected_revision: track.revision,
            title: title.trim(),
            description,
          },
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
        if (next) {
          setTitle(track.title);
          setDescription(track.description);
          save.reset();
        }
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline">Edit</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Edit {track.title}</DialogTitle>
        <DialogDescription>Change the track's title and description.</DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (title.trim()) save.mutate();
          }}
        >
          <Field label="Title">
            {(id) => (
              <Input
                id={id}
                value={title}
                required
                onChange={(event) => {
                  setTitle(event.target.value);
                }}
              />
            )}
          </Field>
          <Field label="Description" hint="Markdown: the approach this track explores.">
            {(id, hintId) => (
              <Textarea
                id={id}
                aria-describedby={hintId}
                value={description}
                onChange={(event) => {
                  setDescription(event.target.value);
                }}
                className="min-h-32"
              />
            )}
          </Field>
          <ErrorLine error={save.error} />
          <DialogFooter>
            <Button type="submit" disabled={!title.trim() || save.isPending}>
              Save
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

const TRANSITIONS: Record<string, { label: string; effect: string }> = {
  paused: {
    label: "Pause",
    effect:
      "Agents stop starting its queued units. Its plan can still be revised and reviewed, and attempts in progress finish normally.",
  },
  active: {
    label: "Resume",
    effect: "Agents can start its queued units again.",
  },
  archived: {
    label: "Archive",
    effect:
      "The track becomes read-only and takes no new plan. It needs no queued, active or awaiting-review unit. A researcher can reactivate it later.",
  },
};

export function TransitionDialog({
  project,
  track,
  to,
}: {
  project: string;
  track: Track;
  to: components["schemas"]["TrackTransitionRequestToState"];
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const queryClient = useQueryClient();
  const copy = TRANSITIONS[to] ?? { label: to, effect: "" };
  const buttonLabel = track.state === "archived" && to === "active" ? "Reactivate" : copy.label;
  const change = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/transitions", {
          params: { path: { slug: project, track_slug: track.slug } },
          body: { to_state: to, expected_revision: track.revision, reason: reason.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setReason("");
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) change.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button variant={to === "archived" ? "destructive" : "outline"}>{buttonLabel}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>
          {buttonLabel} {track.title}?
        </DialogTitle>
        <DialogDescription>{copy.effect}</DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (reason.trim()) change.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={id} className="text-sm font-medium">
              Reason <span className="font-normal text-muted-foreground">(required)</span>
            </label>
            <Textarea
              id={id}
              required
              value={reason}
              onChange={(event) => {
                setReason(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={change.error} />
          <DialogFooter>
            <Button
              type="submit"
              variant={to === "archived" ? "destructive" : "default"}
              disabled={!reason.trim() || change.isPending}
            >
              {buttonLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
