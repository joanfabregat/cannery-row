import type { Writeup } from "@/api/types";
import { Markdown } from "@/components/markdown";
import { StatusChip } from "@/components/status-chip";
import { formatDateTime } from "@/lib/format";
import { usePeople } from "@/projects/use-people";

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

/**
 * A unit's write-up: its one-sentence summary and its body, who wrote
 * it and when; "No write-up: <reason>" when a researcher skipped it; or where
 * it stands while it is still to write.
 */
export function WriteupView({ writeup }: { writeup: Writeup }) {
  const person = usePeople();
  if (writeup.status === "skipped") {
    return (
      <p className="text-sm">No write-up: {writeup.skip_reason ?? "a researcher skipped it."}</p>
    );
  }
  const written = writeup.writeup;
  if (writeup.status !== "written" || written === null) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        <StatusChip domain="writeup" value={writeup.status} />
        <span>
          {writeup.status === "claimed"
            ? `Being written by ${
                writeup.claimed_by_user ? person(writeup.claimed_by_user) : "an agent"
              }.`
            : "Waiting for an agent or a researcher to write it up."}
        </span>
      </div>
    );
  }
  const summary = text(written.front_matter.summary);
  return (
    <div className="flex flex-col gap-3">
      {summary ? <p className="font-medium">{summary}</p> : null}
      <Markdown>{written.body_markdown}</Markdown>
      <p className="text-xs text-muted-foreground">
        Written by{" "}
        {written.written_by_user
          ? person(written.written_by_user)
          : written.written_by_service
            ? "an agent"
            : "the imported history"}{" "}
        · {formatDateTime(written.created_at)}
      </p>
    </div>
  );
}
