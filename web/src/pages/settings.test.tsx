import type { QueryClient } from "@tanstack/react-query";
import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import { dashboardView, metric, member, page, token } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

function view(overrides: Partial<Schemas["ViewOut"]> = {}): Schemas["ViewOut"] {
  return {
    view: dashboardView({ title: "Accuracy over time" }),
    dashboard_revision: null,
    metric: metric(),
    aggregation: null,
    series: [
      {
        science_revision: 1,
        group: { track: "tokenizer" },
        points: [
          {
            x: "2026-03-04T10:00:00Z",
            value: 0.91,
            count: 1,
            attempt_refs: ["#12.1"],
            control_value: 0.9,
            reference_label: null,
            uncertainty: null,
            sample_count: 500,
            missing_reasons: [],
          },
        ],
      },
    ],
    context: {
      science_revisions: [1],
      split: "test",
      authority: "tester_verified",
      rows: 1,
      measured: 1,
      sample_count: 500,
      failed_attempts: 2,
      controls: [],
    },
    truncated: false,
    warnings: [],
    ...overrides,
  };
}

describe("Results", () => {
  it("shows verified values with a table of the same numbers, and agent claims only on request", async () => {
    const { requests } = signedIn(
      {},
      {
        "GET /api/projects/sardines/dashboard": () =>
          json({
            dashboard_revision: null,
            science_revision: 1,
            derived: true,
            views: [dashboardView({ title: "Accuracy over time" })],
          }),
        "GET /api/projects/sardines/dashboard/views/accuracy": () => json(view()),
        "GET /api/projects/sardines/metrics/query": () =>
          json(
            page([
              {
                id: 1,
                attempt_ref: "#12.1",
                hypothesis: 12,
                hypothesis_title: "Shorter prompts",
                hypothesis_state: "promoted",
                attempt_state: "promoted",
                track: "tokenizer",
                science_revision: 1,
                metric: "accuracy",
                split: "test",
                dimensions: {},
                value: 0.95,
                missing_reason: null,
                unit: "ratio",
                direction: "higher_is_better",
                sample_count: null,
                control_value: null,
                control: null,
                uncertainty: null,
                authority: "agent_claim",
                claimed_at: "2026-03-03T10:00:00Z",
                finished_at: null,
                recorded_at: "2026-03-03T10:00:00Z",
              },
            ]),
          ),
      },
    );
    const { user } = renderApp("/results");
    const card = await screen.findByRole("region", { name: "Accuracy over time" });
    expect(
      await within(card).findByRole("figure", { name: "Accuracy over time" }),
    ).toBeInTheDocument();
    expect(within(card).getByText("The same numbers as a table")).toBeInTheDocument();
    const table = within(card).getByRole("table", { name: "Verified values" });
    expect(within(table).getByText("#12.1")).toBeInTheDocument();
    expect(within(card).getByText(/2 failed attempts not shown/)).toBeInTheDocument();
    expect(requests.some((r) => new URL(r.url).pathname.endsWith("/metrics/query"))).toBe(false);

    await user.click(
      within(card).getByRole("button", { name: "Show values reported by the agent" }),
    );
    const claims = await within(card).findByRole("table", { name: /agents' own claims/ });
    expect(within(claims).getByText("Reported by agent")).toBeInTheDocument();
    expect(within(claims).getByText("#12.1 Shorter prompts")).toBeInTheDocument();
    const query = requests.find((r) => new URL(r.url).pathname.endsWith("/metrics/query"));
    expect(new URL(query?.url ?? "").searchParams.get("authority")).toBe("agent_claim");
  });

  it("labels each point's reference and notes results evaluated before references", async () => {
    const base = view();
    const [first] = base.series[0]?.points ?? [];
    if (first === undefined) throw new Error("no point");
    signedIn(
      {},
      {
        "GET /api/projects/sardines/dashboard": () =>
          json({
            dashboard_revision: null,
            science_revision: 1,
            derived: true,
            views: [dashboardView({ chart: "table" })],
          }),
        "GET /api/projects/sardines/dashboard/views/accuracy": () =>
          json(
            view({
              view: dashboardView({ chart: "table", baseline: "control" }),
              series: [
                {
                  science_revision: 1,
                  group: { track: "tokenizer" },
                  points: [
                    {
                      ...first,
                      x: "2026-03-01T10:00:00Z",
                      value: 0.88,
                      attempt_refs: ["#9.1"],
                      control_value: null,
                    },
                    {
                      ...first,
                      control_value: 0.9,
                      reference_label: "best promoted (#42)",
                    },
                  ],
                },
              ],
            }),
          ),
      },
    );
    renderApp("/results");
    const card = await screen.findByRole("region", { name: "Accuracy" });
    const table = await within(card).findByRole("table", { name: "Verified values" });
    expect(within(table).getByRole("columnheader", { name: "Reference" })).toBeInTheDocument();
    expect(within(table).getByText("best promoted (#42)")).toBeInTheDocument();
    expect(
      within(card).getByText(/^No reference for results evaluated before .+\.$/),
    ).toBeInTheDocument();
  });

  it("uses a table alone for a table view", async () => {
    signedIn(
      {},
      {
        "GET /api/projects/sardines/dashboard": () =>
          json({
            dashboard_revision: 2,
            science_revision: 1,
            derived: false,
            views: [dashboardView({ chart: "table" })],
          }),
        "GET /api/projects/sardines/dashboard/views/accuracy": () => json(view()),
      },
    );
    renderApp("/results");
    const card = await screen.findByRole("region", { name: "Accuracy" });
    expect(await within(card).findByRole("table", { name: "Verified values" })).toBeInTheDocument();
    expect(within(card).queryByRole("figure")).not.toBeInTheDocument();
  });
});

describe("personal tokens", () => {
  it("creates a token, shows its secret once, and lists it", async () => {
    const posted: Request[] = [];
    let tokens = [token({ name: "old laptop", revoked_at: "2026-02-01T00:00:00Z" })];
    signedIn(
      {},
      {
        "GET /api/tokens": () => json(page(tokens)),
        "POST /api/tokens": async (request) => {
          posted.push(request.clone());
          const body = (await request.json()) as { name: string };
          const created = token({ name: body.name, scopes: ["read", "write"] });
          tokens = [created, ...tokens];
          return json({ ...created, token: "cr_secret_value" }, 201);
        },
      },
    );
    const { user } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Access tokens" });
    expect(await within(section).findByText("old laptop")).toBeInTheDocument();
    expect(within(section).getByText("Revoked")).toBeInTheDocument();

    await user.click(within(section).getByRole("button", { name: "New token" }));
    const dialog = await screen.findByRole("dialog");
    const name = within(dialog).getByRole("textbox", { name: "Name" });
    const create = within(dialog).getByRole("button", { name: "Create token" });
    expect(create).toBeDisabled();
    await user.type(name, "ci;job");
    expect(within(dialog).getByText(/without “;”/)).toBeInTheDocument();
    expect(create).toBeDisabled();
    await user.clear(name);
    await user.type(name, "laptop CLI");
    await user.click(within(dialog).getByRole("checkbox", { name: /Write/ }));
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "Expires after" }),
      "30 days",
    );
    await user.click(create);

    expect(await within(dialog).findByDisplayValue("cr_secret_value")).toBeInTheDocument();
    expect(await posted[0]?.json()).toEqual({
      name: "laptop CLI",
      scopes: ["read", "write"],
      expires_in_days: 30,
    });
    await user.click(within(dialog).getByRole("button", { name: "Done" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(screen.queryByDisplayValue("cr_secret_value")).not.toBeInTheDocument();
    expect(
      await within(section).findByRole("button", { name: "Revoke laptop CLI" }),
    ).toBeInTheDocument();
  });

  it("revokes a token after confirmation", async () => {
    const live = token();
    const deleted: string[] = [];
    signedIn(
      {},
      {
        "GET /api/tokens": () => json(page([live])),
        [`DELETE /api/tokens/${live.id}`]: (request) => {
          deleted.push(new URL(request.url).pathname);
          return json({ ...live, revoked_at: "2026-03-01T00:00:00Z" });
        },
      },
    );
    const { user } = renderApp("/settings");
    await user.click(await screen.findByRole("button", { name: "Revoke laptop CLI" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Revoke the token" }));
    await waitFor(() => {
      expect(deleted).toEqual([`/api/tokens/${live.id}`]);
    });
  });
});

describe("administration", () => {
  const adminHandlers = {
    "GET /api/tokens": () => json(page([])),
    "GET /api/projects/sardines/members": () => json(page([member()])),
    "GET /api/projects/sardines/service-accounts": () =>
      json(
        page([
          {
            id: "00000000-0000-4000-8000-0000000000e1",
            project: "sardines",
            kind: "agent",
            name: "codex",
            description: "The coding agent",
            created_at: "2026-01-01T00:00:00Z",
            disabled_at: null,
          },
        ]),
      ),
  };

  it("creates a project with a valid short name", async () => {
    const posted: Request[] = [];
    signedIn(
      { admin: true },
      {
        ...adminHandlers,
        "POST /api/projects": (request) => {
          posted.push(request.clone());
          return json(project("herring", "Herring"), 201);
        },
      },
    );
    const { user } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Projects" });
    await user.click(within(section).getByRole("button", { name: "New project" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("textbox", { name: "Title" }), "Herring");
    await user.type(within(dialog).getByRole("textbox", { name: "Short name" }), "Herring!");
    expect(within(dialog).getByRole("button", { name: "Create project" })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: "Short name" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Short name" }), "herring");
    await user.click(within(dialog).getByRole("button", { name: "Create project" }));
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({ slug: "herring", title: "Herring", description: "" });
  });

  it("adds a member by email and changes a role", async () => {
    const put: { path: string; body: unknown }[] = [];
    signedIn(
      { admin: true },
      {
        ...adminHandlers,
        "GET /api/users": () =>
          json(
            page([
              {
                id: "00000000-0000-4000-8000-0000000000b0",
                email: "alan@example.com",
                email_verified: true,
                display_name: "Alan Turing",
                is_admin: false,
              },
            ]),
          ),
        "PUT /api/projects/sardines/members/00000000-0000-4000-8000-0000000000b0": async (
          request,
        ) => {
          put.push({ path: new URL(request.url).pathname, body: await request.json() });
          return json(member({ user_id: "00000000-0000-4000-8000-0000000000b0" }));
        },
        "PUT /api/projects/sardines/members/00000000-0000-4000-8000-0000000000aa": async (
          request,
        ) => {
          put.push({ path: new URL(request.url).pathname, body: await request.json() });
          return json(member({ role: "viewer" }));
        },
      },
    );
    const { user } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Members" });
    await user.selectOptions(
      await within(section).findByRole("combobox", { name: "Role of Grace Hopper" }),
      "Viewer",
    );
    await waitFor(() => {
      expect(put).toHaveLength(1);
    });
    expect(put[0]?.body).toEqual({ role: "viewer" });

    await user.click(within(section).getByRole("button", { name: "Add a member" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("searchbox", { name: "Email" }), "alan");
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Role" }), "Member");
    await user.click(await within(dialog).findByRole("button", { name: "Add alan@example.com" }));
    await waitFor(() => {
      expect(put).toHaveLength(2);
    });
    expect(put[1]).toEqual({
      path: "/api/projects/sardines/members/00000000-0000-4000-8000-0000000000b0",
      body: { role: "member" },
    });
  });

  it("disables a service account only with a reason", async () => {
    const posted: Request[] = [];
    signedIn(
      { admin: true },
      {
        ...adminHandlers,
        "POST /api/projects/sardines/service-accounts/codex/disable": (request) => {
          posted.push(request.clone());
          return json({});
        },
      },
    );
    const { user } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Service accounts" });
    await user.click(await within(section).findByRole("button", { name: "Disable codex" }));
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Disable" });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox", { name: /Reason/ }), "Replaced");
    await user.click(confirm);
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({ reason: "Replaced" });
  });
});

describe("a new token's secret", () => {
  function heldSecrets(client: QueryClient): string {
    return JSON.stringify(
      client
        .getMutationCache()
        .getAll()
        .map((m) => m.state),
    );
  }

  it("never stays in the query client, for a personal token", async () => {
    signedIn(
      {},
      {
        "GET /api/tokens": () => json(page([])),
        "POST /api/tokens": () => json({ ...token(), token: "cr_personal_secret" }, 201),
      },
    );
    const { user, queryClient } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Access tokens" });
    await user.click(within(section).getByRole("button", { name: "New token" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "laptop CLI");
    await user.click(within(dialog).getByRole("button", { name: "Create token" }));
    expect(await within(dialog).findByDisplayValue("cr_personal_secret")).toBeInTheDocument();
    expect(heldSecrets(queryClient)).not.toContain("cr_personal_secret");
    await user.click(within(dialog).getByRole("button", { name: "Done" }));
    await waitFor(() => {
      expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
    });
    expect(screen.queryByDisplayValue("cr_personal_secret")).not.toBeInTheDocument();
  });

  it("never stays in the query client, for a service account's token", async () => {
    signedIn(
      { admin: true },
      {
        "GET /api/tokens": () => json(page([])),
        "GET /api/projects/sardines/members": () => json(page([member()])),
        "GET /api/projects/sardines/service-accounts": () =>
          json(
            page([
              {
                id: "00000000-0000-4000-8000-0000000000e1",
                project: "sardines",
                kind: "agent",
                name: "codex",
                description: null,
                created_at: "2026-01-01T00:00:00Z",
                disabled_at: null,
              },
            ]),
          ),
        "GET /api/projects/sardines/service-accounts/codex/tokens": () => json(page([])),
        "POST /api/projects/sardines/service-accounts/codex/tokens": () =>
          json({ ...token({ kind: "service" }), token: "cr_service_secret" }, 201),
      },
    );
    const { user, queryClient } = renderApp("/settings");
    const section = await screen.findByRole("region", { name: "Service accounts" });
    await user.click(await within(section).findByRole("button", { name: "Tokens" }));
    await user.click(await within(section).findByRole("button", { name: "New token" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "runner");
    await user.click(within(dialog).getByRole("button", { name: "Create token" }));
    expect(await within(dialog).findByDisplayValue("cr_service_secret")).toBeInTheDocument();
    expect(heldSecrets(queryClient)).not.toContain("cr_service_secret");
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    await waitFor(() => {
      expect(heldSecrets(queryClient)).not.toContain("cr_service_secret");
    });
    expect(screen.queryByDisplayValue("cr_service_secret")).not.toBeInTheDocument();
  });
});

describe("administration without the write scope", () => {
  it("lists projects, members and service accounts, with no action that changes them", async () => {
    signedIn(
      { admin: true, scopes: ["read"] },
      {
        "GET /api/tokens": () => json(page([])),
        "GET /api/projects/sardines/members": () => json(page([member()])),
        "GET /api/projects/sardines/service-accounts": () =>
          json(
            page([
              {
                id: "00000000-0000-4000-8000-0000000000e1",
                project: "sardines",
                kind: "agent",
                name: "codex",
                description: null,
                created_at: "2026-01-01T00:00:00Z",
                disabled_at: null,
              },
            ]),
          ),
        "GET /api/projects/sardines/service-accounts/codex/tokens": () => json(page([token()])),
      },
    );
    const { user } = renderApp("/settings");
    const projects = await screen.findByRole("region", { name: "Projects" });
    expect(within(projects).queryByRole("button", { name: "New project" })).toBeNull();
    const members = screen.getByRole("region", { name: "Members" });
    expect(await within(members).findByText("Grace Hopper")).toBeInTheDocument();
    expect(within(members).getByText("Researcher")).toBeInTheDocument();
    expect(within(members).queryByRole("combobox")).toBeNull();
    expect(within(members).queryByRole("button", { name: /Remove|Add a member/ })).toBeNull();
    const services = screen.getByRole("region", { name: "Service accounts" });
    expect(await within(services).findByText("codex")).toBeInTheDocument();
    expect(
      within(services).queryByRole("button", { name: /Disable|New service account/ }),
    ).toBeNull();
    await user.click(within(services).getByRole("button", { name: "Tokens" }));
    expect(await within(services).findByText("laptop CLI")).toBeInTheDocument();
    expect(within(services).queryByRole("button", { name: /Revoke|New token/ })).toBeNull();
  });
});
