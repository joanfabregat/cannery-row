import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link } from "react-router";

import { api, unwrap } from "@/api/client";
import { projectKey, useQuestions, useSteering, useUnitMessages } from "@/api/queries";
import type { Message } from "@/api/types";
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
import { writtenBy } from "@/lib/messages";
import { attemptPath, transcriptPath } from "@/lib/paths";

/**
 * Questions and steering notes: a performer asks a researcher about the
 * unit it works on, and a researcher steers a running attempt unasked. A
 * question concerns one unit and pauses at most its own attempt or job; a
 * concern holds up the whole track.
 */

const CONCERN_KINDS = ["wrong_assumption", "better_idea", "blocker", "other"] as const;

/** Where a question was asked: its attempt, or the job of its attempt. */
function Origin({ message, project }: { message: Message; project?: string }) {
  const to = attemptPath(message.unit, message.attempt, project);
  return (
    <>
      {message.job_phase ? `${label("stage", message.job_phase)} of ` : ""}
      <Link to={to} className="underline underline-offset-4">
        #{message.unit}.{message.attempt}
      </Link>
    </>
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

function useRefresh(project: string) {
  const queryClient = useQueryClient();
  return async () => {
    await queryClient.invalidateQueries({ queryKey: projectKey(project) });
  };
}

function AnswerQuestionDialog({ project, question }: { project: string; question: Message }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState("");
  const id = useId();
  const refresh = useRefresh(project);
  const answer = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/questions/{question_id}/answer", {
          params: { path: { slug: project, question_id: question.id } },
          body: { body: body.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setBody("");
      await refresh();
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) answer.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button size="sm">Answer</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Answer the question</DialogTitle>
        <DialogDescription>
          {question.blocking
            ? "The performer is waiting: once you answer, its lease clock starts again with a fresh lease."
            : "The performer goes on with its stated default until it reads your answer."}
        </DialogDescription>
        <Markdown className="rounded-md border bg-muted/40 p-3 text-sm">{question.body}</Markdown>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (body.trim()) answer.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={id} className="text-sm font-medium">
              Your answer
            </label>
            <Textarea
              id={id}
              value={body}
              className="min-h-28"
              onChange={(event) => {
                setBody(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={answer.error} />
          <DialogFooter>
            <Button type="submit" disabled={answer.isPending || !body.trim()}>
              Send the answer
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function EscalateQuestionDialog({ project, question }: { project: string; question: Message }) {
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState<string>("wrong_assumption");
  const [note, setNote] = useState("");
  const ids = { kind: useId(), note: useId() };
  const refresh = useRefresh(project);
  const escalate = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/questions/{question_id}/escalation", {
          params: { path: { slug: project, question_id: question.id } },
          body: { kind, note: note.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setNote("");
      await refresh();
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) escalate.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button size="sm" variant="outline">
          Raise as a concern
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Raise the question as a concern</DialogTitle>
        <DialogDescription>
          When the question shows that the track&apos;s plan itself is wrong, it becomes a concern
          about the plan of {question.track}, quoting the question. Your note answers the question
          and closes it. While the concern is open, no new unit of the track starts.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (note.trim()) escalate.mutate();
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
              {CONCERN_KINDS.map((k) => (
                <option key={k} value={k}>
                  {label("concernKind", k)}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.note} className="text-sm font-medium">
              Note
            </label>
            <Textarea
              id={ids.note}
              value={note}
              onChange={(event) => {
                setNote(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={escalate.error} />
          <DialogFooter>
            <Button type="submit" disabled={escalate.isPending || !note.trim()}>
              Raise the concern
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** One question, its default and its answer; researchers answer or escalate an open one. */
export function QuestionItem({
  project,
  question,
  isResearcher,
  showUnit = false,
}: {
  project: string;
  question: Message;
  isResearcher: boolean;
  showUnit?: boolean;
}) {
  const state = question.state ?? "open";
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm">
          <span className="font-medium">
            {question.blocking ? "Blocking question" : "Question"}
          </span>{" "}
          from <Origin message={question} />
          {showUnit ? ` (${question.track})` : ""}
          <span className="text-muted-foreground">
            {" "}
            · {writtenBy(question)} · {formatDateTime(question.created_at)}
          </span>
        </span>
        <StatusChip domain="question" value={state} />
      </div>
      <Markdown>{question.body}</Markdown>
      {question.default ? (
        <p className="text-sm text-muted-foreground">
          Proceeding meanwhile on: <span className="text-foreground">{question.default}</span>
        </p>
      ) : null}
      {question.released_at ? (
        <p className="text-sm text-status-attention">
          Unanswered in time: the attempt was released on {formatDateTime(question.released_at)},
          and the unit&apos;s next attempt reads the question and its answer.
        </p>
      ) : null}
      {question.answer ? (
        <div className="rounded-md border-l-4 border-status-success bg-muted/40 p-3">
          <p className="text-xs text-muted-foreground">
            {state === "escalated" ? "Raised as a concern by " : "Answered by "}
            {question.answer.author_name ?? "a researcher"} ·{" "}
            {formatDateTime(question.answer.created_at)}
            {question.answer.acknowledged_at ? " · read by the performer" : ""}
          </p>
          <Markdown className="text-sm">{question.answer.body}</Markdown>
        </div>
      ) : null}
      {state === "escalated" && question.concern ? (
        <p className="text-sm text-muted-foreground">
          The concern is listed on the track&apos;s page.
        </p>
      ) : null}
      {isResearcher && state === "open" ? (
        <div className="flex flex-wrap gap-2">
          <AnswerQuestionDialog project={project} question={question} />
          <EscalateQuestionDialog project={project} question={question} />
        </div>
      ) : null}
    </li>
  );
}

/** Home: the open questions, blocking ones first; each pauses at most its own attempt or job. */
export function QuestionQueue({ project }: { project: string }) {
  const questions = useQuestions(project, "open");
  return (
    <Section
      title="Questions"
      description="Performers ask about the unit they work on. A blocking question stops its attempt or job until you answer it; a non-blocking one goes on with its stated default. Raise a question as a concern when it shows the plan is wrong."
    >
      <QueryView query={questions}>
        {(page) => {
          if (page.items.length === 0) return <EmptyState>No question is waiting.</EmptyState>;
          const sorted = [...page.items].sort(
            (a, b) => Number(b.blocking ?? false) - Number(a.blocking ?? false),
          );
          return (
            <ul aria-label="Open questions" className="flex flex-col divide-y">
              {sorted.map((question) => (
                <QuestionItem
                  key={question.id}
                  project={project}
                  question={question}
                  isResearcher
                  showUnit
                />
              ))}
            </ul>
          );
        }}
      </QueryView>
    </Section>
  );
}

/** A unit's questions, open ones first; shown only once a performer asked one. */
export function UnitQuestions({
  project,
  number,
  isResearcher,
}: {
  project: string;
  number: number;
  isResearcher: boolean;
}) {
  const messages = useUnitMessages(project, number);
  const questions = (messages.data?.items ?? []).filter((m) => m.kind === "question");
  if (questions.length === 0) return null;
  const open = questions.filter((q) => q.state === "open");
  const closed = questions.filter((q) => q.state !== "open");
  return (
    <Section
      title="Questions"
      description="What the unit's performers asked. A question concerns this unit only and pauses at most its own attempt; a concern about the plan holds up the whole track."
    >
      <div className="flex flex-col gap-4">
        {open.length > 0 ? (
          <ul aria-label="Open questions" className="flex flex-col divide-y">
            {open.map((question) => (
              <QuestionItem
                key={question.id}
                project={project}
                question={question}
                isResearcher={isResearcher}
              />
            ))}
          </ul>
        ) : (
          <EmptyState>No question is waiting.</EmptyState>
        )}
        {closed.length > 0 ? (
          <Collapsible summary={`Answered questions (${String(closed.length)})`}>
            <ul className="flex flex-col divide-y">
              {closed.map((question) => (
                <QuestionItem
                  key={question.id}
                  project={project}
                  question={question}
                  isResearcher={isResearcher}
                />
              ))}
            </ul>
          </Collapsible>
        ) : null}
      </div>
    </Section>
  );
}

function PostSteering({
  project,
  number,
  sequence,
}: {
  project: string;
  number: number;
  sequence: number;
}) {
  const [body, setBody] = useState("");
  const ids = { body: useId(), hint: useId() };
  const refresh = useRefresh(project);
  const post = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/units/{number}/attempts/{sequence}/steering", {
          params: { path: { slug: project, number, sequence } },
          body: { body: body.trim() },
        }),
      ),
    onSuccess: async () => {
      setBody("");
      await refresh();
    },
  });
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (body.trim()) post.mutate();
      }}
    >
      <label htmlFor={ids.body} className="text-sm font-medium">
        Steering note
      </label>
      <p id={ids.hint} className="text-xs text-muted-foreground">
        Markdown, at most 16 KiB. The agent reads it at its next heartbeat and acknowledges it.
      </p>
      <Textarea
        id={ids.body}
        aria-describedby={ids.hint}
        value={body}
        onChange={(event) => {
          setBody(event.target.value);
        }}
      />
      <ErrorLine error={post.error} />
      <div>
        <Button type="submit" size="sm" disabled={post.isPending || !body.trim()}>
          Send the note
        </Button>
      </div>
    </form>
  );
}

const STEERABLE = new Set(["claimed", "running", "waiting_on_human"]);

/** An agent-mode attempt's steering notes and its transcript. */
export function AttemptConversation({
  project,
  number,
  sequence,
  state,
  isResearcher,
}: {
  project: string;
  number: number;
  sequence: number;
  state: string;
  isResearcher: boolean;
}) {
  const steering = useSteering(project, number, sequence);
  const running = STEERABLE.has(state);
  return (
    <Section
      title="Steering"
      description={
        state === "waiting_on_human"
          ? "The agent asked a blocking question and waits for its answer; its lease clock is stopped. Answer it on the unit's page or on Home."
          : "Notes researchers post to the running agent, unasked. The agent reads them at its next heartbeat and acknowledges them."
      }
      actions={
        <Link
          to={transcriptPath(number, sequence)}
          className="text-sm font-medium underline underline-offset-4"
        >
          Open the transcript
        </Link>
      }
    >
      <div className="flex flex-col gap-4">
        <QueryView query={steering}>
          {(data) =>
            data.items.length === 0 ? (
              <EmptyState>No steering note was posted.</EmptyState>
            ) : (
              <ul aria-label="Steering notes" className="flex flex-col gap-2">
                {data.items.map((note) => (
                  <li
                    key={note.id}
                    className={
                      note.acknowledged_at
                        ? "flex flex-col gap-1 rounded-md border p-3"
                        : "flex flex-col gap-1 rounded-md border-2 border-status-attention p-3"
                    }
                  >
                    <p className="text-xs text-muted-foreground">
                      {writtenBy(note)} · {formatDateTime(note.created_at)} ·{" "}
                      {note.acknowledged_at ? (
                        `acknowledged ${formatDateTime(note.acknowledged_at)}`
                      ) : (
                        <span className="font-medium text-status-attention">
                          not acknowledged yet
                        </span>
                      )}
                    </p>
                    <Markdown className="text-sm">{note.body}</Markdown>
                  </li>
                ))}
              </ul>
            )
          }
        </QueryView>
        {isResearcher && running ? (
          <PostSteering project={project} number={number} sequence={sequence} />
        ) : null}
      </div>
    </Section>
  );
}
