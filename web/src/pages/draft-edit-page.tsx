import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";

import { api, unwrap } from "@/api/client";
import { projectKey, useHypothesis } from "@/api/queries";
import type { components } from "@/api/schema";
import type { Hypothesis } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { parseNumber } from "@/lib/navigation";
import { hypothesisPath } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

/**
 * Researchers revise a draft's wording. Each save is a new revision, which
 * the draft review then covers; the plan's structured parts (splits,
 * metric, budget) keep the values of the current revision.
 */
export function DraftEditPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  if (number === null) {
    return (
      <>
        <PageHeader title="Edit draft" />
        <EmptyState>This address does not name a hypothesis.</EmptyState>
      </>
    );
  }
  return <ProjectPage>{(project) => <EditView project={project} number={number} />}</ProjectPage>;
}

function EditView({ project, number }: { project: Project; number: number }) {
  const { isResearcher } = usePermissions();
  const hypothesis = useHypothesis(project.slug, number);
  return (
    <>
      <PageHeader title={`Edit draft #${number}`} />
      {!isResearcher ? (
        <EmptyState>Only researchers edit drafts.</EmptyState>
      ) : (
        <QueryView query={hypothesis}>
          {(h) =>
            h.state !== "draft" ? (
              <EmptyState>
                #{number} is no longer a draft, so it cannot be edited.{" "}
                <Link to={hypothesisPath(number)} className="font-medium underline">
                  Go to the hypothesis
                </Link>
                .
              </EmptyState>
            ) : "plan" in h.document ? (
              <DraftForm project={project.slug} hypothesis={{ ...h, document: h.document }} />
            ) : (
              <EmptyState>This imported hypothesis has no editable draft plan.</EmptyState>
            )
          }
        </QueryView>
      )}
    </>
  );
}

type Doc = Record<string, unknown>;

const FIELDS: { path: [string] | [string, string]; label: string; long: boolean }[] = [
  { path: ["title"], label: "Title", long: false },
  { path: ["question"], label: "Question", long: true },
  { path: ["rationale"], label: "Why try it", long: true },
  { path: ["intervention"], label: "What changes", long: true },
  { path: ["plan", "success_criteria"], label: "Success looks like", long: true },
  { path: ["plan", "falsification_criteria"], label: "It is disproved if", long: true },
];

function read(doc: Doc, path: string[]): string {
  let value: unknown = doc;
  for (const key of path) {
    value = typeof value === "object" && value !== null ? (value as Doc)[key] : undefined;
  }
  return typeof value === "string" ? value : "";
}

function valuesOf(doc: Doc): string[] {
  return FIELDS.map((f) => read(doc, f.path));
}

/**
 * The form edits the revision it started from (`base`). When a refetch
 * brings a newer revision, untouched fields move to it; edits in progress
 * are kept, with a notice that saving them will be refused as stale until
 * the latest revision is loaded.
 */
type EditableHypothesis = Omit<Hypothesis, "document"> & {
  document: components["schemas"]["HypothesisCreateRequest"];
};

function DraftForm({ project, hypothesis }: { project: string; hypothesis: EditableHypothesis }) {
  const [base, setBase] = useState(hypothesis);
  const [values, setValues] = useState<string[]>(() => valuesOf(base.document));
  const dirty = FIELDS.some((f, i) => values[i] !== read(base.document, f.path));
  const newer = hypothesis.revision !== base.revision;
  if (newer && !dirty) {
    setBase(hypothesis);
    setValues(valuesOf(hypothesis.document));
  }
  const doc = base.document;
  const baseId = useId();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const save = useMutation({
    mutationFn: async () => {
      const [title, question, rationale, intervention, success, falsification] = values.map(
        (value) => value.trim(),
      );
      const next: EditableHypothesis["document"] = {
        ...doc,
        title: title ?? "",
        question: question ?? "",
        rationale: rationale ?? "",
        intervention: intervention ?? "",
        plan: {
          ...doc.plan,
          success_criteria: success ?? "",
          falsification_criteria: falsification ?? "",
        },
      };
      return unwrap(
        await api.PUT("/api/projects/{slug}/hypotheses/{number}", {
          params: { path: { slug: project, number: hypothesis.number } },
          body: { expected_revision: base.revision, document: next },
        }),
      );
    },
    onSuccess: async (saved) => {
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
      await navigate(hypothesisPath(hypothesis.number), {
        state: { notice: `Revision ${saved.revision} of the draft was saved.` },
      });
    },
  });
  const complete = values.every((v) => v.trim());
  return (
    <Section
      title={`Revision ${String(base.revision)}`}
      description="Saving creates a new revision; a researcher then reviews that one."
    >
      {newer ? (
        <div
          role="status"
          className="mb-4 flex flex-wrap items-center justify-between gap-3 rounded-md border border-status-attention/40 bg-status-attention-bg px-4 py-3 text-sm"
        >
          <p>This draft changed since you started editing; saving will fail until you reload.</p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => {
              setBase(hypothesis);
              setValues(valuesOf(hypothesis.document));
              save.reset();
            }}
          >
            Discard my edits and load revision {hypothesis.revision}
          </Button>
        </div>
      ) : null}
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (complete) save.mutate();
        }}
      >
        {FIELDS.map((field, index) => {
          const id = `${baseId}-${String(index)}`;
          const change = (value: string) => {
            setValues(values.map((v, i) => (i === index ? value : v)));
          };
          return (
            <div key={field.label} className="flex flex-col gap-1.5">
              <label htmlFor={id} className="text-sm font-medium">
                {field.label}
              </label>
              {field.long ? (
                <Textarea
                  id={id}
                  required
                  value={values[index] ?? ""}
                  onChange={(event) => {
                    change(event.target.value);
                  }}
                />
              ) : (
                <Input
                  id={id}
                  required
                  value={values[index] ?? ""}
                  onChange={(event) => {
                    change(event.target.value);
                  }}
                />
              )}
            </div>
          );
        })}
        {save.isError ? (
          <p role="alert" className="text-sm text-status-danger">
            {describeError(save.error)}
          </p>
        ) : null}
        <div className="flex gap-2">
          <Button type="submit" disabled={!complete || save.isPending}>
            Save a new revision
          </Button>
          <Button asChild variant="outline">
            <Link to={hypothesisPath(hypothesis.number)}>Cancel</Link>
          </Button>
        </div>
      </form>
    </Section>
  );
}
