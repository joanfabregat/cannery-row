import type { Message } from "@/api/types";
import { excerpt } from "@/lib/format";
import { label } from "@/lib/labels";

/** Who wrote a question, an answer or a steering note, and through what. */
export function writtenBy(message: Message): string {
  const who = message.author_name ?? (message.author_kind === "service" ? "An agent" : "A member");
  const through = label("channel", message.via_channel);
  return message.via_client ? `${who} (${through}, ${message.via_client})` : `${who} (${through})`;
}

/** One line about a message, for the transcript timeline. */
export function messageSummary(message: Message): string {
  return `${writtenBy(message)}: ${excerpt(message.body, 400)}`;
}
