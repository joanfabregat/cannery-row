import type { Writeup } from "@/api/types";

/**
 * The Markdown documents a researcher writes in the app: a write-up and a
 * decision. Each states its front matter as YAML whose values are JSON, then
 * its body.
 */

export type Outcome = "promote" | "reject" | "inconclusive" | "failed";

/** A document cited by id and SHA-256, or null when there is none. */
export type ContentRef = { ref: string; sha256: string } | null;

function cite(value: ContentRef): string {
  return value === null ? "null" : JSON.stringify({ ref: value.ref, sha256: value.sha256 });
}

/** A write-up to start from: the front matter the hypothesis's inputs require, and headings. */
export function writeupTemplate(writeup: Writeup): string {
  const attempts = writeup.inputs?.attempts ?? [];
  return [
    "---",
    'summary: ""',
    `attempts: [${attempts.join(", ")}]`,
    `verification: ${cite(writeup.inputs?.verification ?? null)}`,
    "---",
    "",
    "## What was done",
    "",
    "## Results",
    "",
  ].join("\n");
}

/**
 * A decision document: the outcome and the verification report and write-up
 * it cites in the front matter, the reason as the body.
 */
export function decisionDocument(
  outcome: Outcome,
  verification: ContentRef,
  writeup: ContentRef,
  reason: string,
): string {
  return `---\noutcome: ${outcome}\nverification: ${cite(verification)}\nwriteup: ${cite(writeup)}\n---\n\n${reason}\n`;
}
