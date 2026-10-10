import { screen, waitFor, within } from "@testing-library/react";

import {
  attention,
  comment,
  hypothesis,
  hypothesisApi,
  member,
  page,
  RESEARCHER_ID,
  searchHit,
  track,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

describe("Home", () => {
  it("lists what waits for a researcher, oldest first, with links to the review", async () => {
    signedIn(
      {},
      {
        "GET /api/projects/sardines/attention": () =>
          json(
            attention({
              pending_counts: { result: 1, failure: 1 },
              pending_reviews: [
                {
                  case_id: "00000000-0000-4000-8000-0000000000c1",
                  kind: "failure",
                  subject_revision: 1,
                  opened_at: "2026-03-01T10:00:00Z",
                  hypothesis: 4,
                  hypothesis_ref: "#4",
                  title: "Fewer layers",
                  track: "tokenizer",
                  attempt_ref: "#4.1",
                  verdict: null,
                  failure_stage: "agent",
                  failure_code: "released",
                  failure_reason: "Out of memory",
                  origin: "live",
                },
                {
                  case_id: "00000000-0000-4000-8000-0000000000c2",
                  kind: "result",
                  subject_revision: 3,
                  opened_at: "2026-03-02T10:00:00Z",
                  hypothesis: 12,
                  hypothesis_ref: "#12",
                  title: "Shorter prompts",
                  track: "tokenizer",
                  attempt_ref: "#12.1",
                  verdict: "pass",
                  failure_stage: null,
                  failure_code: null,
                  failure_reason: null,
                  origin: "live",
                },
              ],
              running_count: 1,
              running: [
                {
                  hypothesis: 9,
                  hypothesis_ref: "#9",
                  title: "Warm cache",
                  track: "tokenizer",
                  attempt_ref: "#9.2",
                  state: "running",
                  claimed_at: "2026-03-03T10:00:00Z",
                },
              ],
            }),
          ),
      },
    );
    renderApp("/");
    const queue = await screen.findByRole("region", { name: "Waiting for your review" });
    const links = within(queue).getAllByRole("link");
    expect(links.map((l) => l.getAttribute("href"))).toEqual([
      "/hypotheses/4/review",
      "/hypotheses/12/review",
    ]);
    expect(within(queue).getByText(/1 result and 1 failure/)).toBeInTheDocument();
    const running = screen.getByRole("region", { name: "Running now" });
    expect(within(running).getByRole("link", { name: "#9.2 Warm cache" })).toHaveAttribute(
      "href",
      "/hypotheses/9/attempts/2",
    );
  });

  function stalledAttention(count: number, performer = "runner") {
    // Two hours and a minute ago: "2 hours".
    const since = new Date(Date.now() - (2 * 60 + 1) * 60_000).toISOString();
    const runner = performer === "runner";
    return attention({
      stalled_verification_count: count,
      stalled_verifications: [
        {
          hypothesis: 12,
          hypothesis_ref: "#12",
          title: "Shorter prompts",
          track: "tokenizer",
          attempt_ref: "#12.1",
          performer,
          verifier: runner ? "stock" : null,
          revision: runner ? "p2" : null,
          waiting_since: since,
          message: "verification of #12.1 waits for verifier stock revision p2",
        },
      ],
    });
  }

  /** The paragraph whose whole text is `text`. */
  function paragraph(text: string) {
    return (_: string, element: Element | null) =>
      element?.tagName === "P" && element.textContent === text;
  }

  it("says in plain words which results wait for verification, and for how long", async () => {
    signedIn({}, { "GET /api/projects/sardines/attention": () => json(stalledAttention(1)) });
    renderApp("/");
    const stalled = await screen.findByRole("region", { name: "Waiting for verification" });
    expect(within(stalled).getByRole("link", { name: "#12.1 Shorter prompts" })).toHaveAttribute(
      "href",
      "/hypotheses/12/attempts/1",
    );
    expect(
      within(stalled).getByText(
        paragraph("#12 has been waiting 2 hours for the verifier stock, rules version p2."),
      ),
    ).toBeInTheDocument();
    expect(
      within(stalled).getByText(/These results are waiting to be verified/),
    ).toBeInTheDocument();
    expect(stalled).not.toHaveTextContent(/job/);
  });

  it("says an agent verify run waits for an agent or a researcher", async () => {
    signedIn(
      {},
      { "GET /api/projects/sardines/attention": () => json(stalledAttention(1, "agent")) },
    );
    renderApp("/");
    const stalled = await screen.findByRole("region", { name: "Waiting for verification" });
    expect(
      within(stalled).getByText(
        paragraph("#12 has been waiting 2 hours for an agent or a researcher who did not run it."),
      ),
    ).toBeInTheDocument();
  });

  it("says how many wait when it lists only the oldest", async () => {
    signedIn({}, { "GET /api/projects/sardines/attention": () => json(stalledAttention(7)) });
    renderApp("/");
    const stalled = await screen.findByRole("region", { name: "Waiting for verification" });
    expect(
      within(stalled).getByText(/^7 results are waiting to be verified; here are the oldest\./),
    ).toBeInTheDocument();
  });

  it("shows no stalled verifications section when every verification is picked up", async () => {
    signedIn({}, { "GET /api/projects/sardines/attention": () => json(attention()) });
    renderApp("/");
    expect(await screen.findByRole("region", { name: "Running now" })).toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "Waiting for verification" }),
    ).not.toBeInTheDocument();
  });
});

describe("Search", () => {
  const results = (request: Request) => {
    const url = new URL(request.url);
    const kinds = url.searchParams.getAll("kind");
    const items = kinds.includes("comment")
      ? [searchHit({ kind: "comment", snippet: "the \u0001tokenizer\u0002 looks slow" })]
      : [
          searchHit(),
          searchHit({ kind: "comment", snippet: "the \u0001tokenizer\u0002 looks slow" }),
        ];
    return json({
      items,
      next_before: null,
      total: items.length,
      facets: {
        kind: { hypothesis: 1, comment: 1 },
        project: { sardines: 2 },
        hypothesis_state: { promoted: 2 },
      },
    });
  };

  it("shows results with highlighted words and filters them by facet", async () => {
    const { requests } = signedIn({}, { "GET /api/search": results });
    const { user, router } = renderApp("/search?q=tokenizer");
    expect(await screen.findByText("2 results for “tokenizer”")).toBeInTheDocument();
    expect(screen.getAllByText("tokenizer").some((n) => n.tagName === "MARK")).toBe(true);
    // One project: no project facet.
    expect(screen.queryByRole("group", { name: "Project" })).not.toBeInTheDocument();
    const type = screen.getByRole("group", { name: "Type" });
    await user.click(within(type).getByRole("checkbox", { name: "Comment" }));
    expect(await screen.findByText("1 result for “tokenizer”")).toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=tokenizer&kind=comment");
    const last = new URL(requests.at(-1)?.url ?? "");
    expect(last.searchParams.getAll("kind")).toEqual(["comment"]);
  });

  it("jumps straight to a hypothesis or an attempt from a reference", async () => {
    signedIn({}, hypothesisApi(hypothesis()));
    const { user, router } = renderApp("/tracks");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.type(box, "#12{Enter}");
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/hypotheses/12");
    });
    await user.clear(box);
    await user.type(box, "sardines#12.3{Enter}");
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/hypotheses/12/attempts/3");
    });
    expect(router.state.location.search).toBe("?project=sardines");
  });

  it("follows a reference typed in the address, too", async () => {
    signedIn({}, hypothesisApi(hypothesis()));
    const { router } = renderApp("/search?q=%2312");
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/hypotheses/12");
    });
  });
});

describe("comments", () => {
  it("renders mentions as links, and lets members add comments and edit their own", async () => {
    const posted: Request[] = [];
    const edited: Request[] = [];
    const mine = comment({ body_markdown: "See #7 for context", revision: 2 });
    const theirs = comment({ author_user_id: RESEARCHER_ID, body_markdown: "Agreed" });
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      {
        ...hypothesisApi(hypothesis(), { comments: [mine, theirs], members: [member()] }),
        "POST /api/projects/sardines/hypotheses/12/comments": (request) => {
          posted.push(request.clone());
          return json(comment({ body_markdown: "New note" }), 201);
        },
        [`PUT /api/projects/sardines/comments/${mine.id}`]: (request) => {
          edited.push(request.clone());
          return json({ ...mine, revision: 3 });
        },
        [`GET /api/projects/sardines/comments/${mine.id}/revisions`]: () =>
          json(
            page([
              { revision: 1, body_markdown: "See 7", created_at: "2026-03-05T11:00:00Z" },
              {
                revision: 2,
                body_markdown: "See #7 for context",
                created_at: "2026-03-05T12:00:00Z",
              },
            ]),
          ),
      },
    );
    const { user } = renderApp("/hypotheses/12");
    const section = await screen.findByRole("region", { name: "Comments" });
    expect(await within(section).findByRole("link", { name: "#7" })).toHaveAttribute(
      "href",
      "/hypotheses/7",
    );
    expect(within(section).getByText("Grace Hopper")).toBeInTheDocument();
    // Only one's own comment has an Edit button.
    expect(within(section).getAllByRole("button", { name: /^Edit/ })).toHaveLength(1);

    await user.click(within(section).getByRole("button", { name: "Show history" }));
    expect(await within(section).findByText("See 7")).toBeInTheDocument();

    await user.click(within(section).getByRole("button", { name: /^Edit/ }));
    const editor = within(section).getByRole("textbox", { name: "Edit your comment" });
    await user.clear(editor);
    await user.type(editor, "See #7, then #9");
    await user.click(within(section).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(edited).toHaveLength(1);
    });
    expect(await edited[0]?.json()).toEqual({
      expected_revision: 2,
      body_markdown: "See #7, then #9",
    });

    const box = within(section).getByRole("textbox", { name: "Add a comment" });
    expect(within(section).getByRole("button", { name: "Add comment" })).toBeDisabled();
    await user.type(box, "New note");
    await user.click(within(section).getByRole("button", { name: "Add comment" }));
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({ body_markdown: "New note" });
  });
});

describe("track management", () => {
  it("pauses a track with a required reason", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        "GET /api/projects/sardines/tracks/tokenizer": () => json(track({ revision: 4 })),
        "GET /api/projects/sardines/tracks/tokenizer/history": () => json(page([])),
        "GET /api/projects/sardines/hypotheses": () => json(page([])),
        "POST /api/projects/sardines/tracks/tokenizer/transitions": (request) => {
          posted.push(request.clone());
          return json(track({ state: "paused", revision: 5 }));
        },
      },
    );
    const { user } = renderApp("/tracks/tokenizer");
    await user.click(await screen.findByRole("button", { name: "Pause" }));
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Pause" });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox", { name: /Reason/ }), "Budget spent");
    await user.click(confirm);
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({
      to_state: "paused",
      expected_revision: 4,
      reason: "Budget spent",
    });
  });

  it("offers Reactivate on an archived track", async () => {
    signedIn(
      {},
      {
        "GET /api/projects/sardines/tracks/tokenizer": () => json(track({ state: "archived" })),
        "GET /api/projects/sardines/tracks/tokenizer/history": () => json(page([])),
        "GET /api/projects/sardines/hypotheses": () => json(page([])),
      },
    );
    renderApp("/tracks/tokenizer");
    expect(await screen.findByRole("button", { name: "Reactivate" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
  });
});

describe("names of people", () => {
  it("reads every page of the member list", async () => {
    const later = member({
      user_id: "00000000-0000-4000-8000-0000000000bb",
      display_name: "Alan Turing",
    });
    const { requests } = signedIn(
      {},
      {
        ...hypothesisApi(hypothesis(), {
          comments: [comment({ author_user_id: later.user_id, body_markdown: "Hello" })],
        }),
        "GET /api/projects/sardines/members": (request) =>
          new URL(request.url).searchParams.get("before") === RESEARCHER_ID
            ? json(page([later]))
            : json(page([member()], RESEARCHER_ID)),
      },
    );
    renderApp("/hypotheses/12");
    const section = await screen.findByRole("region", { name: "Comments" });
    expect(await within(section).findByText("Alan Turing")).toBeInTheDocument();
    const pages = requests
      .map((r) => new URL(r.url))
      .filter((url) => url.pathname === "/api/projects/sardines/members")
      .map((url) => url.searchParams.get("before"));
    expect(pages).toEqual([null, RESEARCHER_ID]);
  });
});
