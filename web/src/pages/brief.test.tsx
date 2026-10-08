import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import { attention, page } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const RESEARCHER = "00000000-0000-4000-8000-0000000000aa";

function brief(overrides: Partial<Schemas["BriefOut"]> = {}): Schemas["BriefOut"] {
  return {
    revision: 2,
    document: "---\ntitle: Sardine counts\ngoal: Count sardines.\n---\n# Domain\n\nThe bay.\n",
    front_matter: { title: "Sardine counts", goal: "Count sardines." },
    title: "Sardine counts",
    goal: "Count sardines.",
    body: "# Domain\n\nThe bay.\n",
    sha256: "ab".repeat(32),
    created_by: RESEARCHER,
    created_by_name: "Ada Lovelace",
    via_channel: "mcp",
    via_client: "codex",
    created_at: "2026-03-01T10:00:00Z",
    ...overrides,
  };
}

function revision(number: number, title: string): Schemas["BriefRevisionOut"] {
  return {
    revision: number,
    title,
    goal: "Count sardines.",
    sha256: "cd".repeat(32),
    created_by: RESEARCHER,
    created_by_name: "Ada Lovelace",
    via_channel: "ui",
    via_client: null,
    created_at: "2026-03-01T10:00:00Z",
  };
}

const notFound = () =>
  json({ error: { code: "not_found", message: "no brief", details: null } }, 404);

describe("the brief page", () => {
  it("shows the current brief and its revisions, and a researcher revises it", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        "GET /api/projects/sardines/brief": () => json(brief()),
        "GET /api/projects/sardines/brief/revisions": () =>
          json(page([revision(2, "Sardine counts"), revision(1, "Sardines")])),
        "POST /api/projects/sardines/brief": (request) => {
          posted.push(request.clone());
          return json(brief({ revision: 3 }), 201);
        },
      },
    );
    const { user } = renderApp("/brief");
    const current = await screen.findByRole("region", { name: "Sardine counts" });
    expect(within(current).getByText("Count sardines.")).toBeInTheDocument();
    expect(within(current).getByRole("heading", { name: "Domain" })).toBeInTheDocument();
    expect(within(current).getByText("Ada Lovelace, through MCP with codex")).toBeInTheDocument();
    const revisions = await screen.findByRole("region", { name: "Revisions" });
    expect(within(revisions).getByText("Revision 1: Sardines")).toBeInTheDocument();

    await user.click(within(current).getByRole("button", { name: "Revise" }));
    const form = await screen.findByRole("region", { name: "Revise the brief" });
    const goal = within(form).getByRole("textbox", { name: "Goal" });
    await user.clear(goal);
    await user.type(goal, 'Count "every" sardine.');
    await user.click(within(form).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({
      document:
        '---\ntitle: "Sardine counts"\ngoal: "Count \\"every\\" sardine."\n---\n# Domain\n\nThe bay.\n',
      expected_revision: 2,
    });
  });

  it("lets a researcher write the first brief", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        "GET /api/projects/sardines/brief": notFound,
        "POST /api/projects/sardines/brief": (request) => {
          posted.push(request.clone());
          return json(brief({ revision: 1 }), 201);
        },
      },
    );
    const { user } = renderApp("/brief");
    expect(await screen.findByText(/This project has no brief yet/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Write the brief" }));
    const form = await screen.findByRole("region", { name: "Write the brief" });
    const save = within(form).getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    await user.type(within(form).getByRole("textbox", { name: "Title" }), "Sardines");
    await user.type(within(form).getByRole("textbox", { name: "Goal" }), "Count them.");
    await user.type(within(form).getByRole("textbox", { name: "Body" }), "The bay.");
    await user.click(save);
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({
      document: '---\ntitle: "Sardines"\ngoal: "Count them."\n---\nThe bay.',
      expected_revision: 0,
    });
  });

  it("shows a member the brief without a way to revise it", async () => {
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      {
        "GET /api/projects/sardines/brief": () => json(brief()),
        "GET /api/projects/sardines/brief/revisions": () =>
          json(page([revision(2, "Sardine counts")])),
      },
    );
    renderApp("/brief");
    const current = await screen.findByRole("region", { name: "Sardine counts" });
    expect(within(current).queryByRole("button", { name: "Revise" })).not.toBeInTheDocument();
  });

  it("puts the brief's goal on the home page", async () => {
    signedIn(
      {},
      {
        "GET /api/projects/sardines/brief": () => json(brief()),
        "GET /api/projects/sardines/attention": () => json(attention()),
      },
    );
    renderApp("/");
    const card = await screen.findByRole("region", { name: "Brief" });
    expect(within(card).getByText("Count sardines.")).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: "Read the brief" })).toHaveAttribute(
      "href",
      "/brief",
    );
  });
});
