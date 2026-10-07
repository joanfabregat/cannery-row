import { attemptPath, hypothesisPath } from "./paths";

// As the API's search: a query that is only `#12`, `slug#12` or `#12.3`.
const REF = /^\s*(?:([a-z0-9][a-z0-9-]{0,62})#|#)([1-9][0-9]{0,8})(?:\.([1-9][0-9]{0,8}))?\s*$/;

/**
 * Where a reference query leads: `#12` to hypothesis 12 of the current
 * project, `slug#12` to that project's, `#12.3` to the attempt. Anything
 * else is a text search (null).
 */
export function refTarget(query: string): string | null {
  const match = REF.exec(query);
  if (match === null) return null;
  const [, project, number, sequence] = match;
  if (number === undefined) return null;
  return sequence === undefined
    ? hypothesisPath(number, project)
    : attemptPath(number, sequence, project);
}
