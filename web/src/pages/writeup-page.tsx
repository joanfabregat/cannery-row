import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";

import { api, unwrap } from "@/api/client";
import { projectKey, useUnit, useWriteup } from "@/api/queries";
import type { Unit, Writeup } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { WriteupView } from "@/components/writeup";
import { writeupTemplate } from "@/lib/documents";
import { describeError } from "@/lib/errors";
import { parseNumber } from "@/lib/navigation";
import { unitPath } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

/**
 * A unit's write-up. While it waits, a researcher writes it up here (the
 * document job is claimed and completed in one action) or skips it with a
 * reason; once written or skipped, the page shows it.
 */
export function WriteupPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  if (number === null) {
    return (
      <>
        <PageHeader title="Write-up" />
        <EmptyState>This address does not name a unit.</EmptyState>
      </>
    );
  }
  return (
    <ProjectPage>{(project) => <WriteupScreen project={project} number={number} />}</ProjectPage>
  );
}

function WriteupScreen({ project, number }: { project: Project; number: number }) {
  const unit = useUnit(project.slug, number);
  const writeup = useWriteup(project.slug, number);
  const { isResearcher } = usePermissions();
  if (unit.isPending || writeup.isPending) {
    return (
      <>
        <PageHeader title={`Write-up #${number}`} />
        <Loading />
      </>
    );
  }
  if (unit.isError || writeup.isError) {
    const failed = unit.isError ? unit : writeup;
    return (
      <>
        <PageHeader title={`Write-up #${number}`} />
        <LoadError error={failed.error} retry={failed.refetch} />
      </>
    );
  }
  const h = unit.data;
  const w = writeup.data;
  const open = w !== null && (w.status === "pending" || w.status === "claimed");
  return (
    <>
      <PageHeader
        title={`Write-up: ${h.ref} ${h.title}`}
        description={`Track ${h.track}`}
        actions={<StatusChip domain="unit" value={h.state} className="text-sm" />}
      />
      <p className="mb-6 text-sm">
        <Link to={unitPath(number)} className="font-medium underline underline-offset-4">
          Open the full unit page
        </Link>
      </p>
      <div className="flex flex-col gap-6">
        <Section title="Write-up">
          {w === null ? (
            <EmptyState>#{number} has not been written up.</EmptyState>
          ) : (
            <WriteupView writeup={w} />
          )}
        </Section>
        {open && isResearcher ? (
          <>
            <WriteItUp project={project.slug} unit={h} writeup={w} />
            <SkipIt project={project.slug} unit={h} />
          </>
        ) : null}
      </div>
    </>
  );
}

function useAfter(project: string, number: number) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  return async (notice: string) => {
    await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    await navigate(unitPath(number), { state: { notice } });
  };
}

function WriteItUp({ project, unit, writeup }: { project: string; unit: Unit; writeup: Writeup }) {
  const id = useId();
  const hintId = useId();
  const [document, setDocument] = useState(() => writeupTemplate(writeup));
  const after = useAfter(project, unit.number);
  const write = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/units/{number}/writeup", {
          params: { path: { slug: project, number: unit.number } },
          body: { document },
        }),
      ),
    onSuccess: async () => {
      await after(`${unit.ref} is written up and waits for its decision.`);
    },
  });
  return (
    <Section
      title="Write it up"
      description="Markdown with YAML front matter: a one-sentence summary, the attempts it covers and the verification report it cites, as filled in below. The write-up cannot be edited once it is recorded."
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          write.mutate();
        }}
      >
        <label htmlFor={id} className="font-medium">
          Write-up
        </label>
        <p id={hintId} className="text-sm text-muted-foreground">
          Say what was done across every attempt, what happened and what the verification found.
        </p>
        <Textarea
          id={id}
          aria-describedby={hintId}
          required
          value={document}
          onChange={(event) => {
            setDocument(event.target.value);
          }}
          className="min-h-72 font-mono text-sm"
        />
        {write.isError ? (
          <p role="alert" className="text-sm text-status-danger">
            {describeError(write.error)}
          </p>
        ) : null}
        <div>
          <Button type="submit" disabled={write.isPending || document.trim() === ""}>
            {write.isPending ? "Recording…" : "Write it up"}
          </Button>
        </div>
      </form>
    </Section>
  );
}

function SkipIt({ project, unit }: { project: string; unit: Unit }) {
  const id = useId();
  const [reason, setReason] = useState("");
  const after = useAfter(project, unit.number);
  const skip = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/units/{number}/writeup/skip", {
          params: { path: { slug: project, number: unit.number } },
          body: { reason: reason.trim() },
        }),
      ),
    onSuccess: async () => {
      await after(`The write-up of ${unit.ref} was skipped; it waits for its decision.`);
    },
  });
  return (
    <Section
      title="Skip the write-up"
      description={`${unit.ref} then waits for its decision without one, and the decision shows "No write-up" with your reason.`}
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          skip.mutate();
        }}
      >
        <label htmlFor={id} className="font-medium">
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
        {skip.isError ? (
          <p role="alert" className="text-sm text-status-danger">
            {describeError(skip.error)}
          </p>
        ) : null}
        <div>
          <Button type="submit" variant="outline" disabled={skip.isPending || reason.trim() === ""}>
            {skip.isPending ? "Recording…" : "Skip the write-up"}
          </Button>
        </div>
      </form>
    </Section>
  );
}
