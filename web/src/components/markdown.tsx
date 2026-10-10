import type { ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import { Link } from "react-router";
import remarkGfm from "remark-gfm";

import { resolveLink } from "@/lib/links";
import { unitPath } from "@/lib/paths";
import { cn } from "@/lib/utils";

/**
 * Markdown from reports, descriptions and comments is untrusted. It is
 * rendered by react-markdown, which builds React elements from the syntax
 * tree and never injects HTML: raw HTML in the source is dropped
 * (`skipHtml`), link URLs go through its safe URL filter (no `javascript:`)
 * and `resolveLink` (app paths open in the app, other sites' http(s)
 * addresses in a new tab, anything else is shown as its text), and images are
 * shown as their description, never fetched, so a report cannot make a
 * reader's browser call another site. `#12` and `slug#12` mentions become
 * links to the unit.
 */

// As the backend's mention pattern: a bare "#N" or "slug#N" not glued to a
// preceding word, path or anchor ("page#12" in a URL, "C#7").
const MENTION = /(?<![\p{L}\p{N}_#/.-])(?:([a-z0-9][a-z0-9-]{0,62})#|#)([1-9][0-9]{0,8})\b/gu;

interface MdNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdNode[];
}

function splitMentions(value: string): MdNode[] {
  const parts: MdNode[] = [];
  let last = 0;
  for (const match of value.matchAll(MENTION)) {
    const index = match.index;
    if (index > last) parts.push({ type: "text", value: value.slice(last, index) });
    parts.push({
      type: "link",
      url: unitPath(match[2] ?? "", match[1]),
      children: [{ type: "text", value: match[0] }],
    });
    last = index + match[0].length;
  }
  if (last < value.length) parts.push({ type: "text", value: value.slice(last) });
  return parts;
}

function linkMentions(node: MdNode): void {
  if (!node.children || node.type === "link" || node.type === "linkReference") return;
  node.children = node.children.flatMap((child) => {
    if (child.type === "text" && typeof child.value === "string") return splitMentions(child.value);
    linkMentions(child);
    return [child];
  });
}

function remarkMentions() {
  return (tree: MdNode) => {
    linkMentions(tree);
  };
}

const components: Components = {
  a: ({ href, children }) => {
    const link = resolveLink(href);
    if (link === null) return <>{children}</>;
    if (link.kind === "internal") {
      return (
        <Link to={link.to} className="font-medium underline underline-offset-4 hover:no-underline">
          {children}
        </Link>
      );
    }
    return (
      <a
        href={link.href}
        rel="noopener noreferrer nofollow"
        target="_blank"
        className="font-medium underline underline-offset-4 hover:no-underline"
      >
        {children}
      </a>
    );
  },
  img: ({ alt }) => (
    <span className="rounded bg-muted px-1.5 py-0.5 text-sm text-muted-foreground">
      Image{alt ? `: ${alt}` : ""}
    </span>
  ),
};

export function Markdown({ children, className }: { children: string; className?: string }) {
  return (
    <div
      className={cn(
        "space-y-3 text-sm leading-relaxed break-words [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:text-[0.85em] [&_h1]:text-lg [&_h1]:font-semibold [&_h2]:text-base [&_h2]:font-semibold [&_h3]:font-semibold [&_li]:ml-5 [&_ol]:list-decimal [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-muted [&_pre]:p-3 [&_table]:w-full [&_table]:text-left [&_td]:border [&_td]:px-2 [&_td]:py-1 [&_th]:border [&_th]:px-2 [&_th]:py-1 [&_ul]:list-disc [&_blockquote]:border-l-4 [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
        className,
      )}
    >
      <ReactMarkdown skipHtml remarkPlugins={[remarkGfm, remarkMentions]} components={components}>
        {children}
      </ReactMarkdown>
    </div>
  );
}

/**
 * The API wraps a search snippet's matched words in these two control
 * characters, which it strips from the text itself: they cannot be typed in a
 * document, unlike `**`.
 */
const MARK_START = "\u0001";
const MARK_END = "\u0002";

/** A search snippet: plain text whose matched words are highlighted. */
export function Snippet({ text }: { text: string }): ReactNode {
  const parts: { text: string; marked: boolean }[] = [];
  for (const [index, chunk] of text.split(MARK_START).entries()) {
    const end = chunk.indexOf(MARK_END);
    if (index === 0 || end === -1) {
      parts.push({ text: chunk.replaceAll(MARK_END, ""), marked: false });
      continue;
    }
    parts.push({ text: chunk.slice(0, end), marked: true });
    parts.push({ text: chunk.slice(end + 1).replaceAll(MARK_END, ""), marked: false });
  }
  return (
    <>
      {parts.map((part, index) =>
        part.marked ? (
          <mark key={index} className="rounded-sm bg-status-attention-bg px-0.5 text-foreground">
            {part.text}
          </mark>
        ) : part.text ? (
          <span key={index}>{part.text}</span>
        ) : null,
      )}
    </>
  );
}
