import { screen } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import { attention, hypothesis, hypothesisApi, page, review, track } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

/**
 * What each role sees. Forbidden actions are absent (not merely disabled);
 * an administrator who is not a member reads but does not act.
 */
interface Case {
  name: string;
  projects: Schemas["ProjectOut"][];
  admin?: boolean;
  comment: boolean;
  research: boolean;
}

const CASES: Case[] = [
  {
    name: "viewer",
    projects: [project("sardines", "Sardines", "viewer")],
    comment: false,
    research: false,
  },
  {
    name: "member",
    projects: [project("sardines", "Sardines", "member")],
    comment: true,
    research: false,
  },
  {
    name: "researcher",
    projects: [project("sardines", "Sardines", "researcher")],
    comment: true,
    research: true,
  },
  {
    name: "administrator who is not a member",
    projects: [project("sardines", "Sardines", null)],
    admin: true,
    comment: false,
    research: false,
  },
];

function pendingResult() {
  return hypothesis({
    state: "deciding",
    reviews: [review({ kind: "decision", state: "pending" })],
  });
}

function present(shown: boolean, element: HTMLElement | null) {
  if (shown) expect(element).toBeInTheDocument();
  else expect(element).not.toBeInTheDocument();
}

describe.each(CASES)("as a $name", ({ projects, admin, comment, research }) => {
  it("sees the review and comment actions a role allows on a hypothesis", async () => {
    signedIn({ projects, admin: admin ?? false }, hypothesisApi(pendingResult()));
    renderApp("/hypotheses/12");
    await screen.findByTestId("outcome-sentence");
    await screen.findByRole("heading", { level: 2, name: "Comments" });
    present(research, screen.queryByRole("link", { name: "Decide" }));
    present(comment, screen.queryByRole("textbox", { name: "Add a comment" }));
  });

  it("sees the review queue on Home only when able to review", async () => {
    signedIn(
      { projects, admin: admin ?? false },
      { "GET /api/projects/sardines/attention": () => json(attention()) },
    );
    renderApp("/");
    await screen.findByRole("heading", { level: 2, name: "Recent outcomes" });
    present(research, screen.queryByRole("heading", { level: 2, name: "Decisions to take" }));
  });

  it("manages tracks only when a researcher", async () => {
    signedIn(
      { projects, admin: admin ?? false },
      {
        "GET /api/projects/sardines/tracks": () => json(page([track()])),
        "GET /api/projects/sardines/tracks/tokenizer": () => json(track()),
        "GET /api/projects/sardines/tracks/tokenizer/history": () => json(page([])),
        "GET /api/projects/sardines/hypotheses": () => json(page([])),
      },
    );
    renderApp("/tracks");
    await screen.findByRole("link", { name: /Tokenizer/ });
    present(research, screen.queryByRole("button", { name: "New track" }));
  });

  it("sees the track actions only when a researcher", async () => {
    signedIn(
      { projects, admin: admin ?? false },
      {
        "GET /api/projects/sardines/tracks/tokenizer": () => json(track()),
        "GET /api/projects/sardines/tracks/tokenizer/history": () => json(page([])),
        "GET /api/projects/sardines/hypotheses": () => json(page([])),
      },
    );
    renderApp("/tracks/tokenizer");
    await screen.findByRole("heading", { level: 1, name: "Tokenizer" });
    for (const name of ["Edit", "Pause", "Archive"]) {
      present(research, screen.queryByRole("button", { name }));
    }
  });
});
