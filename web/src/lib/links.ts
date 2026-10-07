export type ResolvedLink = { kind: "internal"; to: string } | { kind: "external"; href: string };

/**
 * How a link from untrusted Markdown is followed: `internal` for a path in
 * this app (opened by the router), `external` for an http(s) address on
 * another site (opened in a new tab, at the address a browser would open),
 * `null` for anything else (other schemes such as `javascript:`, relative
 * addresses), which is shown as plain text.
 *
 * A path is internal only when it starts with one slash not followed by
 * another slash or a backslash (a browser reads `//host` and `/\host` as
 * another site) and still resolves to this origin once parsed (the URL
 * parser drops tabs and newlines, so `/\t/host` is `//host`). A
 * percent-encoded backslash (`/%5Chost`) stays encoded in the path: it names
 * a page of this app, which shows as not found.
 */
export function resolveLink(href: string | null | undefined): ResolvedLink | null {
  if (!href) return null;
  const origin = window.location.origin;
  let url: URL;
  try {
    url = new URL(href, origin);
  } catch {
    return null;
  }
  if (/^\/(?![/\\])/.test(href) && url.origin === origin) return { kind: "internal", to: href };
  if ((url.protocol === "http:" || url.protocol === "https:") && url.origin !== origin) {
    return { kind: "external", href: url.href };
  }
  return null;
}
