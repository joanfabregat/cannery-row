import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import {
  attention,
  decision,
  hypothesis,
  hypothesisApi,
  page,
  promotedHypothesis,
  review,
  track,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const TRACK = "/api/projects/sardines/tracks/tokenizer";
const CONCERN = "00000000-0000-4000-8000-0000000000e1";
const DISMISSED = "00000000-0000-4000-8000-0000000000e2";
const AGENT = "00000000-0000-4000-8000-0000000000a9";

function concern(overrides: Partial<Schemas["ConcernOut"]> = {}): Schemas["ConcernOut"] {
  return {
    id: CONCERN,
    track: "tokenizer",
    kind: "wrong_assumption",
    state: "open",
    hypothesis: 4,
    attempt: 1,
    body: "The baseline assumes the corpus is deduplicated; it is not.",
    front_matter: { kind: "wrong_assumption", attempt: "#4.1" },
    sha256: "0".repeat(64),
    raised_by: AGENT,
    raised_by_kind: "service",
    raised_by_name: "nightly-agent",
    via_channel: "mcp",
    via_client: "codex",
    raised_at: "2026-03-02T10:00:00Z",
    closed_at: null,
    answered_by_revision: null,
    dismissed_by_name: null,
    dismissal_reason: null,
    ...overrides,
  };
}

function plan(overrides: Partial<Schemas["PlanOut"]> = {}): Schemas["PlanOut"] {
  return {
    track: "tokenizer",
    revision: 1,
    state: "approved",
    based_on: null,
    approach: "Establish a baseline.",
    units: [],
    alignments: [],
    needs_alignment: [],
    answers: [],
    needs_answer: [],
    created_by: AGENT,
    created_by_name: "Ada Lovelace",
    via_channel: "ui",
    via_client: null,
    created_at: "2026-03-01T10:00:00Z",
    updated_at: "2026-03-01T10:00:00Z",
    submitted_at: "2026-03-01T11:00:00Z",
    review_case_id: null,
    reviewed_by_name: "Grace Hopper",
    review_reason: "Ready.",
    reviewed_at: "2026-03-01T12:00:00Z",
    markdown_ref: `${TRACK}/plans/1/plan.md`,
    ...overrides,
  };
}

const notFound = () =>
  json({ error: { code: "not_found", message: "no plan", details: null } }, 404);

function trackPage(concerns: Schemas["ConcernOut"][], draft: Schemas["PlanOut"] | null = null) {
  return {
    [`GET ${TRACK}`]: () => json(track()),
    [`GET ${TRACK}/history`]: () => json(page([])),
    "GET /api/projects/sardines/hypotheses": () => json(page([])),
    [`GET ${TRACK}/plans/current`]: () => json(plan()),
    [`GET ${TRACK}/plans/draft`]: () => (draft ? json(draft) : notFound()),
    [`GET ${TRACK}/plans`]: () => json(page([])),
    [`GET ${TRACK}/concerns`]: () => json(page(concerns)),
  };
}

describe("concerns about a track's plan", () => {
  it("shows the open concern and lets a researcher dismiss it with a reason", async () => {
    const dismissed: Request[] = [];
    signedIn(
      {},
      {
        ...trackPage([
          concern(),
          concern({
            id: DISMISSED,
            kind: "better_idea",
            state: "dismissed",
            hypothesis: null,
            attempt: null,
            body: "Try a larger vocabulary first.",
            closed_at: "2026-03-03T10:00:00Z",
            dismissed_by_name: "Grace Hopper",
            dismissal_reason: "Out of scope for this track.",
          }),
        ]),
        [`POST /api/projects/sardines/concerns/${CONCERN}/dismissal`]: (request) => {
          dismissed.push(request.clone());
          return json(concern({ state: "dismissed" }));
        },
      },
    );
    const { user } = renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Concerns" });
    const open = await within(section).findByRole("list", { name: "Open concerns" });
    expect(within(open).getByText(/the corpus is deduplicated/)).toBeInTheDocument();
    expect(within(open).getByRole("link", { name: "#4.1" })).toHaveAttribute(
      "href",
      "/hypotheses/4/attempts/1",
    );
    expect(within(open).getByText(/nightly-agent \(Agent \(MCP\), codex\)/)).toBeInTheDocument();
    expect(within(section).getByText("Closed concerns (1)")).toBeInTheDocument();
    expect(within(section).getByText(/Out of scope for this track/)).toBeInTheDocument();

    await user.click(within(open).getByRole("button", { name: "Dismiss" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("textbox", { name: "Reason" }), "Checked: it is.");
    await user.click(within(dialog).getByRole("button", { name: "Dismiss the concern" }));
    await waitFor(() => {
      expect(dismissed).toHaveLength(1);
    });
    expect(await dismissed[0]?.json()).toEqual({ reason: "Checked: it is." });
  });

  it("lets a member raise a concern but not dismiss one", async () => {
    const raised: Request[] = [];
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      {
        ...trackPage([concern()]),
        [`POST ${TRACK}/concerns`]: (request) => {
          raised.push(request.clone());
          return json(concern(), 201);
        },
      },
    );
    const { user } = renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Concerns" });
    await within(section).findByRole("list", { name: "Open concerns" });
    expect(within(section).queryByRole("button", { name: "Dismiss" })).toBeNull();
    expect(within(section).queryByRole("button", { name: "Revise the plan" })).toBeNull();

    await user.click(within(section).getByRole("button", { name: "Raise a concern" }));
    const dialog = await screen.findByRole("dialog");
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Kind" }), "blocker");
    await user.type(
      within(dialog).getByRole("textbox", { name: "Argument" }),
      "The dataset is gone.",
    );
    await user.click(within(dialog).getByRole("button", { name: "Raise the concern" }));
    await waitFor(() => {
      expect(raised).toHaveLength(1);
    });
    expect(await raised[0]?.json()).toEqual({
      document: "---\nkind: blocker\n---\nThe dataset is gone.\n",
    });
  });

  it("opens the plan editor with the concern listed first and saves the answer", async () => {
    const answered: Request[] = [];
    const draft = plan({
      revision: 2,
      state: "draft",
      based_on: 1,
      submitted_at: null,
      reviewed_by_name: null,
      review_reason: null,
      reviewed_at: null,
      needs_answer: [concern()],
    });
    signedIn(
      {},
      {
        ...trackPage([concern()], draft),
        [`PUT ${TRACK}/plans/draft/answers/${CONCERN}`]: (request) => {
          answered.push(request.clone());
          return json({
            concern: CONCERN,
            kind: "wrong_assumption",
            state: "open",
            how: "Deduplicate first.",
          });
        },
      },
    );
    const { user, router } = renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Concerns" });
    await user.click(await within(section).findByRole("button", { name: "Revise the plan" }));
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/tracks/tokenizer/plan");
    });
    expect(router.state.location.search).toBe(`?answer=${CONCERN}`);
    const answers = await screen.findByRole("region", { name: "Concerns this revision answers" });
    const how = within(answers).getByRole("textbox", {
      name: "How this revision answers the wrong assumption concern",
    });
    expect(within(answers).getByText(/the corpus is deduplicated/)).toBeInTheDocument();
    await user.type(how, "Deduplicate first.");
    await user.click(within(answers).getByRole("button", { name: "Save the answer" }));
    await waitFor(() => {
      expect(answered).toHaveLength(1);
    });
    expect(await answered[0]?.json()).toEqual({ how: "Deduplicate first." });
  });
});

describe("Home", () => {
  it("lists the open concerns and names the decider of an automatic decision", async () => {
    const { requests } = signedIn(
      {},
      {
        "GET /api/projects/sardines/concerns": () => json(page([concern()])),
        "GET /api/projects/sardines/attention": () =>
          json(
            attention({
              pending_counts: { decision: 1, failure: 0 },
              pending_reviews: [
                {
                  case_id: "00000000-0000-4000-8000-0000000000c2",
                  kind: "decision",
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
                  decider: "nightly-decider",
                },
              ],
            }),
          ),
      },
    );
    renderApp("/");
    const concerns = await screen.findByRole("region", { name: "Concerns about plans" });
    expect(await within(concerns).findByRole("link", { name: "tokenizer" })).toHaveAttribute(
      "href",
      "/tracks/tokenizer",
    );
    expect(within(concerns).getByText(/the corpus is deduplicated/)).toBeInTheDocument();
    const listed = requests
      .map((r) => new URL(r.url))
      .find((url) => url.pathname === "/api/projects/sardines/concerns");
    expect(listed?.searchParams.get("state")).toBe("open");
    const queue = screen.getByRole("region", { name: "Decisions to take" });
    expect(
      within(queue).getByText(/The decider nightly-decider decides this one automatically/),
    ).toBeInTheDocument();
  });

  it("does not show the concern queue to members", async () => {
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      { "GET /api/projects/sardines/attention": () => json(attention()) },
    );
    renderApp("/");
    expect(await screen.findByRole("region", { name: "Recent outcomes" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Concerns about plans" })).toBeNull();
  });
});

describe("an automatic decision", () => {
  it("says the decider step decided, not a researcher", async () => {
    const { attempts } = promotedHypothesis();
    const h = hypothesis({
      state: "promoted",
      reviews: [
        review({
          kind: "decision",
          decisions: [
            decision({
              actor_user_id: null,
              actor_service_id: "00000000-0000-4000-8000-0000000000d1",
              decider_revision: "decider-1",
              via_channel: "runner",
              reason: "Every check passed",
            }),
          ],
        }),
      ],
    });
    signedIn({}, hypothesisApi(h, { attempts }));
    renderApp("/hypotheses/12");
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent(
      "the decider step accepted it because “Every check passed.”",
    );
    expect(
      screen.getByText(/Decided automatically by the decider step \(revision decider-1\)/),
    ).toBeInTheDocument();
  });
});
