import { QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";

import type { Schemas } from "@/api/client";
import { setCsrfToken } from "@/api/client";
import { createQueryClient } from "@/lib/query-client";
import { routes } from "@/routes";
import { ThemeProvider } from "@/theme/theme-provider";

type Handler = (request: Request) => Response | Promise<Response>;

export function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

export const unauthenticated = () =>
  json(
    { error: { code: "unauthenticated", message: "authentication required", details: null } },
    401,
  );

/** Answers `fetch` from handlers keyed by "METHOD /path"; records every request. */
export function mockApi(handlers: Record<string, Handler>) {
  const requests: Request[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : new Request(input, init);
    requests.push(request);
    const key = `${request.method} ${new URL(request.url).pathname}`;
    const handler = handlers[key];
    if (handler === undefined) {
      return json({ error: { code: "not_found", message: `no mock for ${key}` } }, 404);
    }
    return handler(request);
  });
  vi.stubGlobal("fetch", fetch);
  return { requests };
}

export function project(
  slug: string,
  title: string,
  role: string | null = "researcher",
): Schemas["ProjectOut"] {
  return {
    id: `00000000-0000-4000-8000-${slug.padEnd(12, "0").slice(0, 12)}`,
    slug,
    title,
    description: "",
    created_at: "2026-01-01T00:00:00Z",
    role,
  };
}

export const USER_ID = "00000000-0000-4000-8000-000000000001";

export function me({
  admin = false,
  csrf = "csrf-123",
  memberships = [] as Schemas["MembershipOut"][],
  scopes = ["read", "write"],
} = {}): Schemas["MeOut"] {
  return {
    kind: "user",
    user: {
      id: USER_ID,
      email: "ada@example.com",
      email_verified: true,
      display_name: "Ada Lovelace",
      is_admin: admin,
    },
    memberships,
    csrf_token: csrf,
    scopes,
    channel: "ui",
  };
}

/** The memberships `/api/me` reports for these projects (those with a role). */
export function membershipsOf(projects: Schemas["ProjectOut"][]): Schemas["MembershipOut"][] {
  return projects
    .filter((p) => p.role != null)
    .map((p) => ({ project: p.slug, title: p.title, role: p.role ?? null }));
}

/** Mocks a signed-in session with these projects; the user's roles are the projects' roles. */
export function signedIn(
  options: { admin?: boolean; projects?: Schemas["ProjectOut"][]; scopes?: string[] } = {},
  extra: Record<string, Handler> = {},
) {
  const projects = options.projects ?? [project("sardines", "Sardines")];
  const session = me({
    admin: options.admin ?? false,
    memberships: membershipsOf(projects),
    scopes: options.scopes ?? ["read", "write"],
  });
  return mockApi({
    "GET /api/me": () => json(session),
    "GET /api/projects": () => json({ items: projects, next_before: null }),
    ...extra,
  });
}

export function renderApp(path: string) {
  setCsrfToken(null);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  const user = userEvent.setup();
  const view = render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  return { ...view, router, user, queryClient };
}
