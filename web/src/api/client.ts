import createClient, { type Middleware } from "openapi-fetch";

import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

/** The API's error body: `{"error": {"code", "message", "details"}}`. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details: unknown;

  constructor(status: number, code: string, message: string, details: unknown = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.details = details;
  }

  static async fromResponse(response: Response): Promise<ApiError> {
    let body: unknown = null;
    try {
      body = await response.clone().json();
    } catch {
      // Not JSON (a proxy error page, for example): keep the status only.
    }
    const error =
      typeof body === "object" && body !== null && "error" in body
        ? (body as { error: { code?: unknown; message?: unknown; details?: unknown } }).error
        : undefined;
    const code = typeof error?.code === "string" ? error.code : `http_${response.status}`;
    const message =
      typeof error?.message === "string"
        ? error.message
        : `The server answered ${response.status} ${response.statusText}`.trim();
    return new ApiError(response.status, code, message, error?.details ?? null);
  }
}

export function isUnauthenticated(error: unknown): boolean {
  return error instanceof ApiError && error.status === 401;
}

/** The session's CSRF token was missing or stale (the session was replaced, for example). */
export function isCsrfInvalid(error: unknown): boolean {
  return error instanceof ApiError && error.status === 403 && error.code === "csrf_invalid";
}

const UNSAFE_METHODS = new Set(["POST", "PUT", "PATCH", "DELETE"]);
const CSRF_HEADER = "X-CSRF-Token";

let csrfToken: string | null = null;
let refreshCsrf: (() => Promise<void>) | null = null;

/** The session's CSRF token, from `/api/me`; sent as `X-CSRF-Token` on unsafe requests. */
export function setCsrfToken(token: string | null): void {
  csrfToken = token;
}

/**
 * How to get a fresh CSRF token (refetch `/api/me`). When set, an unsafe
 * request refused with `csrf_invalid` is sent once more with the new token.
 */
export function setCsrfRefresher(refresher: (() => Promise<void>) | null): void {
  refreshCsrf = refresher;
}

const csrf: Middleware = {
  onRequest({ request }) {
    if (UNSAFE_METHODS.has(request.method) && csrfToken !== null) {
      request.headers.set(CSRF_HEADER, csrfToken);
    }
    return request;
  },
};

async function isCsrfRefusal(response: Response): Promise<boolean> {
  if (response.status !== 403) return false;
  return (await ApiError.fromResponse(response)).code === "csrf_invalid";
}

/**
 * Sends a request; an unsafe one refused for its CSRF token is retried once,
 * after refreshing the token, if the refresh produced a different one.
 */
async function send(request: Request): Promise<Response> {
  const refresher = refreshCsrf;
  if (!UNSAFE_METHODS.has(request.method) || refresher === null) {
    return globalThis.fetch(request);
  }
  const retry = request.clone();
  const response = await globalThis.fetch(request);
  if (!(await isCsrfRefusal(response))) return response;
  const sent = request.headers.get(CSRF_HEADER);
  try {
    await refresher();
  } catch {
    return response;
  }
  if (csrfToken === null || csrfToken === sent) return response;
  retry.headers.set(CSRF_HEADER, csrfToken);
  return globalThis.fetch(retry);
}

const errors: Middleware = {
  async onResponse({ response }) {
    if (!response.ok) throw await ApiError.fromResponse(response);
    return response;
  },
};

/**
 * The typed API client. The app is served from the API's origin (through the
 * Vite proxy in development), so requests are same-origin and carry the
 * session cookie. A non-2xx answer throws an {@link ApiError}.
 */
export const api = createClient<paths>({
  baseUrl: globalThis.location.origin,
  credentials: "same-origin",
  // Resolves the global fetch on each call, so tests can stub it.
  fetch: send,
});
api.use(csrf, errors);

/** Unwraps an openapi-fetch result; errors were already thrown by the middleware. */
export function unwrap<T>(result: { data?: T; response: Response }): T {
  if (result.data === undefined) {
    throw new ApiError(result.response.status, "empty_response", "The server sent no data");
  }
  return result.data;
}
