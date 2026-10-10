import { Link, useParams } from "react-router";

import { useAttempt, useTranscript, useUnitMessages } from "@/api/queries";
import type { Message, TranscriptEvent } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { messageSummary } from "@/lib/messages";
import { parseNumber } from "@/lib/navigation";
import { attemptPath } from "@/lib/paths";
import type { Project } from "@/projects/project-context";

/**
 * An agent's transcript as a timeline: what it said and did, the tools it
 * called (collapsed), and the questions, answers and steering notes of its
 * attempt, highlighted. A running attempt's transcript refreshes on its own.
 */

export function TranscriptPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  const sequence = parseNumber(params.sequence);
  if (number === null || sequence === null) {
    return (
      <>
        <PageHeader title="Transcript not found" />
        <EmptyState>This address does not name an attempt.</EmptyState>
      </>
    );
  }
  return (
    <ProjectPage>
      {(project) => <TranscriptView project={project} number={number} sequence={sequence} />}
    </ProjectPage>
  );
}

/** A transcript event, as the schema defines it. */
interface Event {
  ts?: string;
  kind?: string;
  content?: unknown;
  tool?: string;
  call_id?: string;
  message?: string;
  model?: string;
}

type Entry =
  | { type: "event"; at: string; index: number; event: Event }
  | { type: "message"; at: string; message: Message };

const CONVERSATION = new Set(["question", "answer", "steer"]);
const TOOLS = new Set(["tool_call", "tool_result"]);

function asEvent(value: TranscriptEvent["event"]): Event {
  return typeof value === "object" && value !== null ? value : {};
}

function text(content: unknown): string {
  return typeof content === "string" ? content : JSON.stringify(content, null, 2);
}

/**
 * The transcript's events with the attempt's messages the agent did not
 * record itself, in time order.
 */
function timeline(events: TranscriptEvent[], messages: Message[]): Entry[] {
  const recorded = new Set<string>();
  const entries: Entry[] = events.map((e) => {
    const event = asEvent(e.event);
    if (event.message) recorded.add(event.message);
    return { type: "event", at: event.ts ?? "", index: e.index, event };
  });
  for (const message of messages) {
    if (recorded.has(message.id)) continue;
    entries.push({ type: "message", at: message.created_at, message });
    if (message.answer && !recorded.has(message.answer.id)) {
      entries.push({
        type: "message",
        at: message.answer.created_at,
        message: {
          ...message,
          id: message.answer.id,
          kind: "answer",
          body: message.answer.body,
          author_kind: "user",
          author_name: message.answer.author_name,
          via_channel: "ui",
          via_client: null,
          created_at: message.answer.created_at,
          answer: null,
        },
      });
    }
  }
  // Stable: events keep their order among themselves.
  return entries
    .map((entry, position) => ({ entry, position }))
    .sort((a, b) => {
      const at = Date.parse(a.entry.at) - Date.parse(b.entry.at);
      return Number.isNaN(at) || at === 0 ? a.position - b.position : at;
    })
    .map(({ entry }) => entry);
}

function EventItem({ index, event }: { index: number; event: Event }) {
  const kind = event.kind ?? "note";
  const heading = (
    <span className="text-xs text-muted-foreground">
      <span className="font-medium text-foreground">{label("transcriptKind", kind)}</span>
      {event.tool ? ` · ${event.tool}` : ""}
      {event.model ? ` · ${event.model}` : ""} · {formatDateTime(event.ts)} · #{index}
    </span>
  );
  if (TOOLS.has(kind)) {
    return (
      <li data-kind={kind} className="rounded-md border p-3">
        <details>
          <summary className="cursor-pointer">{heading}</summary>
          <pre className="mt-2 max-h-96 overflow-auto rounded-md bg-muted p-3 font-mono text-xs whitespace-pre-wrap">
            {text(event.content)}
          </pre>
        </details>
      </li>
    );
  }
  const highlighted = CONVERSATION.has(kind);
  return (
    <li
      data-kind={kind}
      className={
        highlighted
          ? "flex flex-col gap-1 rounded-md border-2 border-status-attention bg-status-attention-bg/40 p-3"
          : "flex flex-col gap-1 rounded-md border p-3"
      }
    >
      {heading}
      <p className="text-sm whitespace-pre-wrap">{text(event.content)}</p>
    </li>
  );
}

function MessageItem({ message }: { message: Message }) {
  return (
    <li
      data-kind={message.kind}
      className="flex flex-col gap-1 rounded-md border-2 border-status-attention bg-status-attention-bg/40 p-3"
    >
      <span className="text-xs text-muted-foreground">
        <span className="font-medium text-foreground">{label("transcriptKind", message.kind)}</span>{" "}
        · {formatDateTime(message.created_at)} · not in the transcript
      </span>
      <p className="text-sm whitespace-pre-wrap">{messageSummary(message)}</p>
    </li>
  );
}

function TranscriptView({
  project,
  number,
  sequence,
}: {
  project: Project;
  number: number;
  sequence: number;
}) {
  const slug = project.slug;
  const attempt = useAttempt(slug, number, sequence);
  const transcript = useTranscript(slug, number, sequence);
  const messages = useUnitMessages(slug, number);
  const title = `Transcript of attempt #${number}.${sequence}`;
  const header = (
    <PageHeader
      title={title}
      description="What the agent recorded as it worked. The agent redacts what it appends; tool calls are collapsed."
      actions={
        attempt.data ? (
          <StatusChip domain="attempt" value={attempt.data.state} className="text-sm" />
        ) : null
      }
    />
  );
  if (transcript.isPending) {
    return (
      <>
        {header}
        <Loading />
      </>
    );
  }
  if (transcript.isError) {
    return (
      <>
        {header}
        <LoadError
          error={transcript.error}
          retry={transcript.refetch}
          notFound={`There is no attempt #${number}.${sequence} in ${project.title}.`}
        />
      </>
    );
  }
  const feed = transcript.data;
  const own = (messages.data?.items ?? []).filter(
    (m) => m.attempt === sequence && m.kind !== "answer",
  );
  const entries = timeline(feed.events, own);
  return (
    <>
      {header}
      <div className="flex flex-col gap-6">
        <p className="text-sm">
          <Link
            to={attemptPath(number, sequence)}
            className="font-medium underline underline-offset-4"
          >
            Back to attempt #{number}.{sequence}
          </Link>
        </p>
        <Section
          title="Timeline"
          description={
            feed.sealed
              ? `Sealed when the attempt was submitted: ${String(feed.events.length)} events.`
              : `Live: ${String(feed.events.length)} events so far, refreshed every few seconds.`
          }
        >
          {entries.length === 0 ? (
            <EmptyState>The agent has not appended anything yet.</EmptyState>
          ) : (
            <ol aria-label="Transcript" className="flex flex-col gap-2">
              {entries.map((entry) =>
                entry.type === "event" ? (
                  <EventItem
                    key={`e${String(entry.index)}`}
                    index={entry.index}
                    event={entry.event}
                  />
                ) : (
                  <MessageItem key={`m${entry.message.id}`} message={entry.message} />
                ),
              )}
            </ol>
          )}
        </Section>
      </div>
    </>
  );
}
