import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useId, useState } from "react";

import { api, unwrap } from "@/api/client";
import { projectKey, useBrief, useBriefRevisions } from "@/api/queries";
import type { Brief, BriefRevision } from "@/api/types";
import { Markdown } from "@/components/markdown";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { Fact, Section } from "@/components/section";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

/**
 * The project's brief: the context every claim and job is handed, and that
 * each attempt records. Researchers write it; each save is a new revision.
 */
export function BriefPage() {
  return (
    <>
      <PageHeader
        title="Brief"
        description="What the project is for and what everyone working on it should know: the domain, the constraints, the resources and the conventions."
      />
      <ProjectPage>{(project) => <BriefView project={project} />}</ProjectPage>
    </>
  );
}

function BriefView({ project }: { project: Project }) {
  const brief = useBrief(project.slug);
  const { isResearcher } = usePermissions();
  const [editing, setEditing] = useState(false);
  return (
    <QueryView query={brief}>
      {(current) => (
        <div className="flex flex-col gap-6">
          {editing ? (
            <BriefForm
              project={project.slug}
              current={current}
              onDone={() => {
                setEditing(false);
              }}
            />
          ) : current === null ? (
            <EmptyState>
              This project has no brief yet.{" "}
              {isResearcher
                ? "Write one so agents know what the project is for."
                : "A researcher writes it."}
              {isResearcher ? (
                <span className="mt-3 block">
                  <Button
                    onClick={() => {
                      setEditing(true);
                    }}
                  >
                    Write the brief
                  </Button>
                </span>
              ) : null}
            </EmptyState>
          ) : (
            <CurrentBrief
              brief={current}
              actions={
                isResearcher ? (
                  <Button
                    variant="outline"
                    onClick={() => {
                      setEditing(true);
                    }}
                  >
                    Revise
                  </Button>
                ) : null
              }
            />
          )}
          {current === null ? null : <RevisionList project={project.slug} />}
        </div>
      )}
    </QueryView>
  );
}

const CHANNELS: Record<string, string> = {
  ui: "the web app",
  api: "the API",
  mcp: "MCP",
  cli: "the command line",
};

function writtenBy(entry: Brief | BriefRevision): string {
  const who = entry.created_by_name ?? "A researcher";
  const through = CHANNELS[entry.via_channel] ?? entry.via_channel;
  return entry.via_client
    ? `${who}, through ${through} with ${entry.via_client}`
    : `${who}, through ${through}`;
}

function CurrentBrief({ brief, actions }: { brief: Brief; actions: ReactNode }) {
  return (
    <Section title={brief.title} description={brief.goal} actions={actions}>
      <dl className="mb-5 grid gap-4 sm:grid-cols-3">
        <Fact term="Revision">{brief.revision}</Fact>
        <Fact term="Written by">{writtenBy(brief)}</Fact>
        <Fact term="Written">{formatDateTime(brief.created_at)}</Fact>
      </dl>
      {brief.body.trim() ? (
        <Markdown>{brief.body}</Markdown>
      ) : (
        <EmptyState>The brief has no body.</EmptyState>
      )}
    </Section>
  );
}

function RevisionList({ project }: { project: string }) {
  const revisions = useBriefRevisions(project);
  return (
    <Section title="Revisions" description="Every attempt records the revision it ran under.">
      <QueryView query={revisions}>
        {(page) => (
          <ul className="flex flex-col divide-y">
            {page.items.map((r) => (
              <li key={r.revision} className="flex flex-col gap-1 py-3">
                <span className="font-medium">
                  Revision {r.revision}: {r.title}
                </span>
                <span className="text-sm text-muted-foreground">
                  {writtenBy(r)} · {formatDateTime(r.created_at)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </QueryView>
    </Section>
  );
}

/**
 * The document the API takes: YAML front matter, then the Markdown body. JSON
 * strings are valid YAML double-quoted scalars, so any title or goal survives.
 */
function briefDocument(title: string, goal: string, body: string): string {
  return `---\ntitle: ${JSON.stringify(title)}\ngoal: ${JSON.stringify(goal)}\n---\n${body}`;
}

function BriefForm({
  project,
  current,
  onDone,
}: {
  project: string;
  current: Brief | null;
  onDone: () => void;
}) {
  const ids = {
    title: useId(),
    goal: useId(),
    goalHint: useId(),
    body: useId(),
    bodyHint: useId(),
  };
  const [title, setTitle] = useState(current?.title ?? "");
  const [goal, setGoal] = useState(current?.goal ?? "");
  const [body, setBody] = useState(current?.body ?? "");
  const queryClient = useQueryClient();
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/brief", {
          params: { path: { slug: project } },
          body: {
            document: briefDocument(title.trim(), goal.trim(), body),
            expected_revision: current?.revision ?? 0,
          },
        }),
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: [...projectKey(project), "brief"] });
      onDone();
    },
  });
  const valid = title.trim().length > 0 && goal.trim().length > 0;
  return (
    <Section title={current === null ? "Write the brief" : "Revise the brief"}>
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (valid) save.mutate();
        }}
      >
        <div className="flex flex-col gap-1.5">
          <label htmlFor={ids.title} className="text-sm font-medium">
            Title
          </label>
          <Input
            id={ids.title}
            value={title}
            required
            onChange={(event) => {
              setTitle(event.target.value);
            }}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <label htmlFor={ids.goal} className="text-sm font-medium">
            Goal
          </label>
          <p id={ids.goalHint} className="text-xs text-muted-foreground">
            One paragraph: what the project is for.
          </p>
          <Textarea
            id={ids.goal}
            value={goal}
            required
            aria-describedby={ids.goalHint}
            onChange={(event) => {
              setGoal(event.target.value);
            }}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <label htmlFor={ids.body} className="text-sm font-medium">
            Body
          </label>
          <p id={ids.bodyHint} className="text-xs text-muted-foreground">
            Markdown: the domain, the constraints, the resources and the conventions.
          </p>
          <Textarea
            id={ids.body}
            value={body}
            aria-describedby={ids.bodyHint}
            className="min-h-64 font-mono text-sm"
            onChange={(event) => {
              setBody(event.target.value);
            }}
          />
        </div>
        {save.error ? (
          <p role="alert" className="text-sm text-status-danger">
            {describeError(save.error)}
          </p>
        ) : null}
        <div className="flex gap-2">
          <Button type="submit" disabled={!valid || save.isPending}>
            Save
          </Button>
          <Button type="button" variant="outline" onClick={onDone}>
            Cancel
          </Button>
        </div>
      </form>
    </Section>
  );
}
