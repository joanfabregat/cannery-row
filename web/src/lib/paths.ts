/**
 * Addresses of the app's pages. The project is the one chosen in the
 * sidebar; a link to another project's record names it (`?project=slug`) and
 * opens that project.
 */

function inProject(path: string, project?: string | null): string {
  return project ? `${path}?project=${encodeURIComponent(project)}` : path;
}

export function hypothesisPath(number: number | string, project?: string | null): string {
  return inProject(`/hypotheses/${number}`, project);
}

export function attemptPath(
  number: number | string,
  sequence: number | string,
  project?: string | null,
): string {
  return inProject(`/hypotheses/${number}/attempts/${sequence}`, project);
}

export function reviewPath(number: number | string): string {
  return `/hypotheses/${number}/review`;
}

export function writeupPath(number: number | string): string {
  return `/hypotheses/${number}/writeup`;
}

export function trackPath(slug: string, project?: string | null): string {
  return inProject(`/tracks/${encodeURIComponent(slug)}`, project);
}

/** The editor of a track's open plan revision. */
export function planEditorPath(track: string): string {
  return `${trackPath(track)}/plan`;
}

/** `#12.3` → [12, 3]; anything else → null. */
export function parseAttemptRef(ref: string | null | undefined): [number, number] | null {
  const match = /#(\d+)\.(\d+)$/.exec(ref ?? "");
  return match ? [Number(match[1]), Number(match[2])] : null;
}
