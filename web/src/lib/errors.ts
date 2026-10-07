import { ApiError } from "@/api/client";

function detailMessages(details: unknown): string[] {
  if (!Array.isArray(details)) return [];
  return details.flatMap((item: unknown) => {
    if (typeof item !== "object" || item === null) return [];
    const message = (item as { message?: unknown }).message;
    return typeof message === "string" && message ? [message] : [];
  });
}

/**
 * What went wrong, in plain words, for a failed request. Refusals keep the
 * server's own explanation, which names what was in the way.
 */
export function describeError(error: unknown): string {
  if (!(error instanceof ApiError)) {
    return "Cannery Row is not answering right now. Try again in a moment.";
  }
  switch (error.code) {
    case "stale_revision":
      return "Someone changed this in the meantime. Reload the page to see the latest version, then try again.";
    case "conflict":
      return `This cannot be done right now: ${error.message}.`;
    case "forbidden":
      return "You are not allowed to do this.";
    case "not_found":
      return "This could not be found. It may not exist, or you may not have access to it.";
    case "validation_failed": {
      const details = detailMessages(error.details);
      const reasons = details.length > 0 ? details.join("; ") : error.message;
      return `Some information is missing or not valid: ${reasons}.`;
    }
    case "unauthenticated":
      return "Your session has ended. Sign in again to continue.";
    default:
      return error.status >= 500
        ? "Cannery Row is not answering right now. Try again in a moment."
        : `This could not be done: ${error.message}.`;
  }
}

export function isNotFound(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404;
}
