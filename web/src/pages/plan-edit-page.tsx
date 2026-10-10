import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useId, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";

import { api, unwrap } from "@/api/client";
import { planKey, usePlan } from "@/api/queries";
import type { Concern, ConcernAnswer, Plan, PlanCheck, PlanUnit, UnitIndex } from "@/api/types";
import { ConcernLine } from "@/components/concerns";
import { Markdown } from "@/components/markdown";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section, SubSection } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { UnitBody, UnitLine } from "@/components/track-plan";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect } from "@/components/ui/native-select";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { label } from "@/lib/labels";
import { unitPath, trackPath } from "@/lib/paths";
import { usePermissions } from "@/projects/use-permissions";

/**
 * The plan editor: the open revision of a track's plan, built through the
 * same write routes agents use. Researchers set the approach, add, edit and
 * drop units, say what happens to the units already done or in flight,
 * check the draft and submit it for review.
 */
export function PlanEditPage() {
  const { track = "" } = useParams();
  return (
    <ProjectPage>{(project) => <PlanEditor project={project.slug} track={track} />}</ProjectPage>
  );
}

function PlanEditor({ project, track }: { project: string; track: string }) {
  const plan = usePlan(project, track, "draft");
  const { isResearcher } = usePermissions();
  return (
    <>
      <PageHeader
        title="Plan draft"
        description={`Track ${track}`}
        actions={
          <Link to={trackPath(track)} className="text-sm font-medium underline underline-offset-4">
            Back to the track
          </Link>
        }
      />
      <QueryView query={plan}>
        {(draft) =>
          draft === null ? (
            <EmptyState>
              This track has no open plan revision. Start one from the track page.
            </EmptyState>
          ) : draft.state !== "draft" || !isResearcher ? (
            <ReadOnlyDraft draft={draft} />
          ) : (
            <DraftForms project={project} track={track} draft={draft} />
          )
        }
      </QueryView>
    </>
  );
}

function ReadOnlyDraft({ draft }: { draft: Plan }) {
  return (
    <Section
      title={`Revision ${String(draft.revision)}`}
      actions={<StatusChip domain="plan" value={draft.state} />}
    >
      <div className="flex flex-col gap-4">
        {draft.state === "submitted" ? (
          <p className="text-sm text-muted-foreground">
            This revision is waiting for a researcher&apos;s review on the track page.
          </p>
        ) : null}
        {draft.approach.trim() ? (
          <Markdown>{draft.approach}</Markdown>
        ) : (
          <EmptyState>No approach yet.</EmptyState>
        )}
        <ul className="flex flex-col divide-y">
          {draft.units.map((unit) => (
            <li key={unit.key} className="py-3">
              <UnitLine unit={unit} />
              <UnitBody unit={unit} />
            </li>
          ))}
        </ul>
      </div>
    </Section>
  );
}

function Field({
  label: text,
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
        {text}
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

function useRefresh(project: string, track: string) {
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: planKey(project, track) });
}

function DraftForms({ project, track, draft }: { project: string; track: string; draft: Plan }) {
  const [adding, setAdding] = useState(false);
  return (
    <div className="flex flex-col gap-6">
      <ApproachForm project={project} track={track} draft={draft} />
      <Section
        title="Units"
        description="Each unit is queued when the plan is approved."
        actions={
          adding ? null : (
            <Button
              variant="outline"
              onClick={() => {
                setAdding(true);
              }}
            >
              Add a unit
            </Button>
          )
        }
      >
        <div className="flex flex-col gap-4">
          {adding ? (
            <UnitForm
              project={project}
              track={track}
              onDone={() => {
                setAdding(false);
              }}
            />
          ) : null}
          {draft.units.length === 0 && !adding ? <EmptyState>No unit yet.</EmptyState> : null}
          {draft.units.map((unit) => (
            <UnitEntry key={unit.key} project={project} track={track} unit={unit} />
          ))}
        </div>
      </Section>
      <AlignmentSection project={project} track={track} draft={draft} />
      <AnswerSection project={project} track={track} draft={draft} />
      <SubmitSection project={project} track={track} />
    </div>
  );
}

function ApproachForm({ project, track, draft }: { project: string; track: string; draft: Plan }) {
  const [approach, setApproach] = useState(draft.approach);
  const refresh = useRefresh(project, track);
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.PUT("/api/projects/{slug}/tracks/{track_slug}/plans/draft/approach", {
          params: { path: { slug: project, track_slug: track } },
          body: { approach },
        }),
      ),
    onSuccess: refresh,
  });
  return (
    <Section
      title="Approach"
      description={`Revision ${String(draft.revision)}${draft.based_on != null ? `, based on revision ${String(draft.based_on)}` : ""}.`}
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field
          label="Approach"
          hint="Markdown: how the track tests its idea, the edge cases and risks, and how the units fit together."
        >
          {(id, hintId) => (
            <Textarea
              id={id}
              aria-describedby={hintId}
              value={approach}
              className="min-h-48 font-mono text-sm"
              onChange={(event) => {
                setApproach(event.target.value);
              }}
            />
          )}
        </Field>
        <ErrorLine error={save.error} />
        <div>
          <Button type="submit" disabled={save.isPending || approach === draft.approach}>
            Save the approach
          </Button>
        </div>
      </form>
    </Section>
  );
}

function UnitEntry({ project, track, unit }: { project: string; track: string; unit: PlanUnit }) {
  const [editing, setEditing] = useState(false);
  const refresh = useRefresh(project, track);
  const drop = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.DELETE("/api/projects/{slug}/tracks/{track_slug}/plans/draft/units/{key}", {
          params: { path: { slug: project, track_slug: track, key: unit.key } },
        }),
      ),
    onSuccess: refresh,
  });
  if (editing) {
    return (
      <UnitForm
        project={project}
        track={track}
        unit={unit}
        onDone={() => {
          setEditing(false);
        }}
      />
    );
  }
  return (
    <div className="rounded-md border p-4">
      <UnitLine unit={unit} />
      <UnitBody unit={unit} />
      <div className="mt-3 flex gap-2">
        <Button
          size="sm"
          variant="outline"
          onClick={() => {
            setEditing(true);
          }}
        >
          Edit {unit.key}
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={drop.isPending}
          onClick={() => {
            drop.mutate();
          }}
        >
          Drop {unit.key}
        </Button>
      </div>
      <ErrorLine error={drop.error} />
    </div>
  );
}

const pretty = (value: unknown) =>
  value === undefined || value === null ? "" : JSON.stringify(value, null, 2);

/** A JSON field's text: empty is `fallback`, anything else must parse. */
function parseJson<T>(text: string, fallback: T, name: string): T {
  if (!text.trim()) return fallback;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error(`${name} is not valid JSON.`);
  }
}

const ACCEPTANCE_TEMPLATE = {
  selection_splits: ["validation"],
  confirmation_splits: ["test"],
  primary_metric: "",
  required_slices: [],
  success_criteria: "",
  falsification_criteria: "",
  regression_gates: [],
  compute_budget: { gpu_hours_max: 1 },
};

function UnitForm({
  project,
  track,
  unit,
  onDone,
}: {
  project: string;
  track: string;
  unit?: PlanUnit;
  onDone: () => void;
}) {
  const [key, setKey] = useState(unit?.key ?? "");
  const [title, setTitle] = useState(unit?.title ?? "");
  const [question, setQuestion] = useState(unit?.question ?? "");
  const [intervention, setIntervention] = useState(unit?.intervention ?? "");
  const [acceptance, setAcceptance] = useState(pretty(unit?.acceptance ?? ACCEPTANCE_TEMPLATE));
  const [control, setControl] = useState(pretty(unit?.control));
  const [parameters, setParameters] = useState(pretty(unit?.parameters));
  const [relations, setRelations] = useState(pretty(unit?.relations ?? []));
  const [context, setContext] = useState(pretty(unit?.context ?? []));
  const [brief, setBrief] = useState(unit?.brief ?? "");
  const refresh = useRefresh(project, track);
  const save = useMutation({
    mutationFn: async () => {
      const body = {
        title,
        question,
        intervention,
        acceptance: parseJson<Record<string, unknown>>(acceptance, {}, "Acceptance"),
        control: parseJson<Record<string, unknown> | null>(control, null, "Control"),
        parameters: parseJson<Record<string, unknown> | null>(parameters, null, "Parameters"),
        relations: parseJson<Plan["units"][number]["relations"]>(relations, [], "Relations"),
        context: parseJson<Plan["units"][number]["context"]>(context, [], "Context"),
        brief,
      };
      if (unit) {
        return unwrap(
          await api.PUT("/api/projects/{slug}/tracks/{track_slug}/plans/draft/units/{key}", {
            params: { path: { slug: project, track_slug: track, key: unit.key } },
            body,
          }),
        );
      }
      return unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/plans/draft/units", {
          params: { path: { slug: project, track_slug: track } },
          body: {
            ...body,
            key,
            control: body.control ?? undefined,
            parameters: body.parameters ?? undefined,
          },
        }),
      );
    },
    onSuccess: async () => {
      await refresh();
      onDone();
    },
  });
  const text = (
    name: string,
    value: string,
    set: (value: string) => void,
    hint?: string,
    long = false,
  ) => (
    <Field label={name} hint={hint}>
      {(id, hintId) =>
        long ? (
          <Textarea
            id={id}
            aria-describedby={hintId}
            value={value}
            className="min-h-24 font-mono text-sm"
            onChange={(event) => {
              set(event.target.value);
            }}
          />
        ) : (
          <Input
            id={id}
            aria-describedby={hintId}
            value={value}
            onChange={(event) => {
              set(event.target.value);
            }}
          />
        )
      }
    </Field>
  );
  return (
    <SubSection title={unit ? `Edit ${unit.key}` : "New unit"}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        {unit
          ? null
          : text(
              "Key",
              key,
              setKey,
              "A short name other units use to refer to this one: lowercase letters, digits and hyphens.",
            )}
        {text("Title", title, setTitle)}
        {text("Question", question, setQuestion, undefined, true)}
        {text("Intervention", intervention, setIntervention, undefined, true)}
        {text(
          "Acceptance",
          acceptance,
          setAcceptance,
          "JSON: the splits, the primary metric, the success and falsification criteria, the regression gates and the compute budget.",
          true,
        )}
        {text("Control", control, setControl, "JSON, optional.", true)}
        {text(
          "Parameters",
          parameters,
          setParameters,
          "JSON, optional: the project's own fields.",
          true,
        )}
        {text(
          "Relations",
          relations,
          setRelations,
          'JSON list, for example [{"kind": "derived_from", "unit": "baseline"}] or {"kind": "related_to", "unit": 12}.',
          true,
        )}
        {text(
          "Context",
          context,
          setContext,
          'JSON list of what the performer should read first: {"kind": "unit", "unit": 12}, {"kind": "writeup", "unit": 12, "attempt": 1} or {"kind": "artifact", "artifact": "<id>"}, each with an optional "note".',
          true,
        )}
        {text(
          "Brief",
          brief,
          setBrief,
          "Markdown: what the performer of this unit should know.",
          true,
        )}
        <ErrorLine error={save.error} />
        <div className="flex gap-2">
          <Button type="submit" disabled={save.isPending}>
            {unit ? "Save the unit" : "Add the unit"}
          </Button>
          <Button type="button" variant="outline" onClick={onDone}>
            Cancel
          </Button>
        </div>
      </form>
    </SubSection>
  );
}

function AlignmentSection({
  project,
  track,
  draft,
}: {
  project: string;
  track: string;
  draft: Plan;
}) {
  const decided = new Map(draft.alignments.map((a) => [a.number, a]));
  const units: UnitIndex[] = [
    ...draft.needs_alignment,
    ...draft.alignments
      .filter((a) => !draft.needs_alignment.some((u) => u.number === a.number))
      .map((a) => ({
        number: a.number,
        title: a.title,
        state: a.state,
        key: null,
        obsolete: false,
      })),
  ];
  if (units.length === 0) return null;
  return (
    <Section
      title="Units already done or in flight"
      description="Say for each whether the plan keeps it, makes it obsolete (an attempt in flight is cancelled) or redoes it as a new unit."
    >
      <div className="flex flex-col gap-4">
        {units.map((unit) => (
          <AlignmentForm
            key={unit.number}
            project={project}
            track={track}
            unit={unit}
            decision={decided.get(unit.number)?.decision ?? ""}
            reason={decided.get(unit.number)?.reason ?? ""}
          />
        ))}
      </div>
    </Section>
  );
}

function AlignmentForm({
  project,
  track,
  unit,
  decision: initialDecision,
  reason: initialReason,
}: {
  project: string;
  track: string;
  unit: UnitIndex;
  decision: string;
  reason: string;
}) {
  const [decision, setDecision] = useState(initialDecision);
  const [reason, setReason] = useState(initialReason);
  const refresh = useRefresh(project, track);
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.PUT("/api/projects/{slug}/tracks/{track_slug}/plans/draft/alignments/{number}", {
          params: { path: { slug: project, track_slug: track, number: unit.number } },
          body: { decision, reason },
        }),
      ),
    onSuccess: refresh,
  });
  return (
    <form
      className="flex flex-col gap-2 rounded-md border p-4"
      onSubmit={(event) => {
        event.preventDefault();
        save.mutate();
      }}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Link to={unitPath(unit.number)} className="font-medium hover:underline">
          #{unit.number} {unit.title}
        </Link>
        <StatusChip domain="unit" value={unit.state} />
      </div>
      <div className="grid gap-3 sm:grid-cols-[12rem_1fr]">
        <Field label={`Decision for #${String(unit.number)}`}>
          {(id) => (
            <NativeSelect
              id={id}
              value={decision}
              onChange={(event) => {
                setDecision(event.target.value);
              }}
            >
              <option value="" disabled>
                Choose…
              </option>
              {(["keep", "obsolete", "redo"] as const).map((value) => (
                <option key={value} value={value}>
                  {label("alignment", value)}
                </option>
              ))}
            </NativeSelect>
          )}
        </Field>
        <Field label={`Reason for #${String(unit.number)}`}>
          {(id) => (
            <Input
              id={id}
              value={reason}
              onChange={(event) => {
                setReason(event.target.value);
              }}
            />
          )}
        </Field>
      </div>
      <ErrorLine error={save.error} />
      <div>
        <Button type="submit" size="sm" disabled={save.isPending || !decision || !reason.trim()}>
          Save #{unit.number}
        </Button>
      </div>
    </form>
  );
}

/**
 * The open concerns about the track's plan and how this revision answers
 * each. The concern the track page sent the researcher here for comes first.
 */
function AnswerSection({ project, track, draft }: { project: string; track: string; draft: Plan }) {
  const [params] = useSearchParams();
  const first = params.get("answer");
  const entries: { id: string; concern: Concern | null; answer: ConcernAnswer | null }[] = [
    ...draft.needs_answer.map((concern) => ({ id: concern.id, concern, answer: null })),
    ...draft.answers.map((answer) => ({ id: answer.concern, concern: null, answer })),
  ].sort((a, b) => Number(b.id === first) - Number(a.id === first));
  if (entries.length === 0) return null;
  return (
    <Section
      title="Concerns this revision answers"
      description="Every open concern about the plan needs an answer before the draft can be submitted: say how this revision answers it. Approving the revision closes the concerns it answers."
    >
      <div className="flex flex-col gap-4">
        {entries.map(({ id, concern, answer }) => (
          <AnswerForm
            key={id}
            project={project}
            track={track}
            id={id}
            concern={concern}
            answer={answer}
          />
        ))}
      </div>
    </Section>
  );
}

function AnswerForm({
  project,
  track,
  id,
  concern,
  answer,
}: {
  project: string;
  track: string;
  id: string;
  concern: Concern | null;
  answer: ConcernAnswer | null;
}) {
  const [how, setHow] = useState(answer?.how ?? "");
  const refresh = useRefresh(project, track);
  const path = { slug: project, track_slug: track, concern_id: id };
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.PUT("/api/projects/{slug}/tracks/{track_slug}/plans/draft/answers/{concern_id}", {
          params: { path },
          body: { how },
        }),
      ),
    onSuccess: refresh,
  });
  const drop = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.DELETE(
          "/api/projects/{slug}/tracks/{track_slug}/plans/draft/answers/{concern_id}",
          { params: { path } },
        ),
      ),
    onSuccess: refresh,
  });
  const kind = concern?.kind ?? answer?.kind ?? "other";
  return (
    <form
      className="flex flex-col gap-2 rounded-md border p-4"
      onSubmit={(event) => {
        event.preventDefault();
        save.mutate();
      }}
    >
      {concern ? (
        <>
          <ConcernLine concern={concern} />
          <Markdown>{concern.body}</Markdown>
        </>
      ) : (
        <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
          <span className="font-medium">{label("concernKind", kind)}</span>
          {answer ? <StatusChip domain="concern" value={answer.state} /> : null}
        </div>
      )}
      <Field
        label={`How this revision answers the ${label("concernKind", kind).toLowerCase()} concern`}
      >
        {(fieldId) => (
          <Textarea
            id={fieldId}
            value={how}
            onChange={(event) => {
              setHow(event.target.value);
            }}
          />
        )}
      </Field>
      <ErrorLine error={save.error ?? drop.error} />
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={save.isPending || !how.trim()}>
          Save the answer
        </Button>
        {answer ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={drop.isPending}
            onClick={() => {
              drop.mutate();
            }}
          >
            Remove the answer
          </Button>
        ) : null}
      </div>
    </form>
  );
}

function SubmitSection({ project, track }: { project: string; track: string }) {
  const [check, setCheck] = useState<PlanCheck | null>(null);
  const refresh = useRefresh(project, track);
  const run = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks/{track_slug}/plans/draft/check", {
          params: { path: { slug: project, track_slug: track } },
        }),
      ),
    onSuccess: (result) => {
      setCheck(result);
    },
  });
  const submit = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/tracks/{track_slug}/plans/draft/submission", {
          params: { path: { slug: project, track_slug: track } },
        }),
      ),
    onSuccess: refresh,
  });
  return (
    <Section
      title="Check and submit"
      description="A submitted plan waits for a researcher's review on the track page."
    >
      <div className="flex flex-col gap-3">
        {check ? (
          check.ready ? (
            <p className="text-sm">Nothing blocks this revision.</p>
          ) : (
            <ul aria-label="Problems" className="flex list-disc flex-col gap-1 pl-5 text-sm">
              {check.problems.map((problem) => (
                <li key={`${problem.code} ${problem.path}`}>{problem.message}</li>
              ))}
            </ul>
          )
        ) : null}
        <ErrorLine error={run.error ?? submit.error} />
        <div className="flex gap-2">
          <Button
            variant="outline"
            disabled={run.isPending}
            onClick={() => {
              run.mutate();
            }}
          >
            Check the plan
          </Button>
          <Button
            disabled={submit.isPending}
            onClick={() => {
              submit.mutate();
            }}
          >
            Submit for review
          </Button>
        </div>
      </div>
    </Section>
  );
}
