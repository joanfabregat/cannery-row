import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import { page, track } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const TRACK = "/api/projects/sardines/tracks/tokenizer";
const RESEARCHER = "00000000-0000-4000-8000-0000000000aa";

const notFound = () =>
  json({ error: { code: "not_found", message: "no plan", details: null } }, 404);

function unit(overrides: Partial<Schemas["PlanUnitOut"]> = {}): Schemas["PlanUnitOut"] {
  return {
    key: "baseline",
    number: 4,
    state: "queued",
    redo_of: null,
    title: "Baseline",
    question: "How well does the current tokenizer do?",
    intervention: "Run it.",
    control: null,
    acceptance: { primary_metric: "score" },
    parameters: null,
    relations: [],
    context: [],
    brief: "# Baseline\n\nKeep the seed fixed.\n",
    science_revision: 1,
    hypothesis_revision: 1,
    ...overrides,
  };
}

function plan(overrides: Partial<Schemas["PlanOut"]> = {}): Schemas["PlanOut"] {
  return {
    track: "tokenizer",
    revision: 1,
    state: "approved",
    based_on: null,
    approach: "Establish a baseline, then vary one thing.",
    units: [unit()],
    alignments: [],
    needs_alignment: [],
    created_by: RESEARCHER,
    created_by_name: "Ada Lovelace",
    via_channel: "mcp",
    via_client: "codex",
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

function revisions(...plans: Schemas["PlanOut"][]) {
  return page(
    plans.map((p) => ({
      revision: p.revision,
      state: p.state,
      based_on: p.based_on,
      units: p.units.length,
      created_by_name: p.created_by_name,
      created_at: p.created_at,
      submitted_at: p.submitted_at,
      reviewed_by_name: p.reviewed_by_name,
      review_reason: p.review_reason,
      reviewed_at: p.reviewed_at,
    })),
  );
}

const trackPage = {
  [`GET ${TRACK}/history`]: () => json(page([])),
  "GET /api/projects/sardines/hypotheses": () => json(page([])),
};

describe("a track's plan", () => {
  it("lets a researcher start the first plan of a planning track", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        ...trackPage,
        [`GET ${TRACK}`]: () => json(track({ state: "planning" })),
        [`GET ${TRACK}/plans/current`]: notFound,
        [`GET ${TRACK}/plans/draft`]: () =>
          posted.length > 0 ? json(plan({ state: "draft", units: [], approach: "" })) : notFound(),
        [`GET ${TRACK}/plans`]: () => json(page([])),
        [`POST ${TRACK}/plans`]: (request) => {
          posted.push(request.clone());
          return json(plan({ state: "draft", units: [], approach: "" }), 201);
        },
      },
    );
    const { user, router } = renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Plan" });
    expect(await within(section).findByText(/No approved plan yet/)).toBeInTheDocument();
    await user.click(await within(section).findByRole("button", { name: "Write the plan" }));
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/tracks/tokenizer/plan");
    });
    expect(posted).toHaveLength(1);
    expect(await screen.findByRole("region", { name: "Approach" })).toBeInTheDocument();
  });

  it("shows the approved plan and lets a researcher review the submitted revision", async () => {
    const reviewed: Request[] = [];
    const submitted = plan({
      revision: 2,
      state: "submitted",
      based_on: 1,
      units: [unit(), unit({ key: "variant", number: null, state: null, title: "Variant" })],
      reviewed_by_name: null,
      review_reason: null,
      reviewed_at: null,
    });
    signedIn(
      {},
      {
        ...trackPage,
        [`GET ${TRACK}`]: () => json(track()),
        [`GET ${TRACK}/plans/current`]: () => json(plan()),
        [`GET ${TRACK}/plans/draft`]: () => json(submitted),
        [`GET ${TRACK}/plans`]: () => json(revisions(submitted, plan())),
        [`POST ${TRACK}/plans/2/review`]: (request) => {
          reviewed.push(request.clone());
          return json({ ...submitted, state: "approved" });
        },
      },
    );
    const { user } = renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Plan" });
    expect(
      (await within(section).findAllByText("Establish a baseline, then vary one thing.")).length,
    ).toBeGreaterThan(0);
    expect(within(section).getAllByRole("link", { name: "#4 Baseline" })[0]).toHaveAttribute(
      "href",
      "/hypotheses/4",
    );
    const review = await within(section).findByRole("region", { name: "Review revision 2" });
    expect(within(review).queryByRole("link", { name: /Variant/ })).not.toBeInTheDocument();
    await user.type(within(review).getByRole("textbox", { name: /Reason/ }), "Looks right.");
    await user.click(within(review).getByRole("button", { name: "Approve" }));
    await user.click(await screen.findByRole("button", { name: "Confirm: Approve" }));
    await waitFor(() => {
      expect(reviewed).toHaveLength(1);
    });
    expect(await reviewed[0]?.json()).toEqual({ action: "approve", reason: "Looks right." });
  });

  it("hides the review and the editor from members", async () => {
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      {
        ...trackPage,
        [`GET ${TRACK}`]: () => json(track()),
        [`GET ${TRACK}/plans/current`]: () => json(plan()),
        [`GET ${TRACK}/plans/draft`]: () => json(plan({ revision: 2, state: "submitted" })),
        [`GET ${TRACK}/plans`]: () => json(revisions(plan())),
      },
    );
    renderApp("/tracks/tokenizer");
    const section = await screen.findByRole("region", { name: "Plan" });
    expect(await within(section).findByRole("region", { name: "Revision 2" })).toBeInTheDocument();
    expect(within(section).queryByRole("region", { name: /Review revision/ })).toBeNull();
    expect(within(section).queryByRole("button", { name: "Start a revision" })).toBeNull();
  });
});

describe("the plan editor", () => {
  it("adds a unit, aligns an in-flight unit, checks and submits the draft", async () => {
    const requests: { method: string; path: string; body: unknown }[] = [];
    const record = async (request: Request) => {
      requests.push({
        method: request.method,
        path: new URL(request.url).pathname,
        body:
          request.method === "GET"
            ? null
            : await request
                .clone()
                .json()
                .catch(() => null),
      });
    };
    const draft = plan({
      revision: 3,
      state: "draft",
      based_on: 2,
      units: [],
      needs_alignment: [
        { number: 4, key: "baseline", title: "Baseline", state: "active", obsolete: false },
      ],
    });
    signedIn(
      {},
      {
        [`GET ${TRACK}/plans/draft`]: () => json(draft),
        [`POST ${TRACK}/plans/draft/units`]: async (request) => {
          await record(request);
          return json(unit({ key: "variant", number: null, state: null }), 201);
        },
        [`PUT ${TRACK}/plans/draft/alignments/4`]: async (request) => {
          await record(request);
          return json({});
        },
        [`GET ${TRACK}/plans/draft/check`]: async (request) => {
          await record(request);
          return json({
            revision: 3,
            ready: false,
            problems: [
              {
                code: "missing_alignment",
                path: "alignments/4",
                message: "#4 (Baseline) is active; say whether the plan keeps it",
              },
            ],
          });
        },
        [`POST ${TRACK}/plans/draft/submission`]: async (request) => {
          await record(request);
          return json({ ...draft, state: "submitted" });
        },
      },
    );
    const { user } = renderApp("/tracks/tokenizer/plan");
    const units = await screen.findByRole("region", { name: "Units" });
    await user.click(within(units).getByRole("button", { name: "Add a unit" }));
    const form = await within(units).findByRole("region", { name: "New unit" });
    await user.type(within(form).getByRole("textbox", { name: "Key" }), "variant");
    await user.type(within(form).getByRole("textbox", { name: "Title" }), "Variant");
    await user.type(within(form).getByRole("textbox", { name: "Question" }), "Is it better?");
    await user.type(within(form).getByRole("textbox", { name: "Intervention" }), "Vary it.");
    await user.type(within(form).getByRole("textbox", { name: "Brief" }), "Seed 7.");
    await user.click(within(form).getByRole("button", { name: "Add the unit" }));
    await waitFor(() => {
      expect(requests).toHaveLength(1);
    });
    expect(requests[0]).toMatchObject({
      method: "POST",
      body: {
        key: "variant",
        title: "Variant",
        question: "Is it better?",
        intervention: "Vary it.",
        relations: [],
        context: [],
        brief: "Seed 7.",
      },
    });

    const aligned = await screen.findByRole("region", { name: "Units already done or in flight" });
    await user.selectOptions(
      within(aligned).getByRole("combobox", { name: "Decision for #4" }),
      "obsolete",
    );
    await user.type(
      within(aligned).getByRole("textbox", { name: "Reason for #4" }),
      "The question changed.",
    );
    await user.click(within(aligned).getByRole("button", { name: "Save #4" }));
    await waitFor(() => {
      expect(requests).toHaveLength(2);
    });
    expect(requests[1]?.body).toEqual({ decision: "obsolete", reason: "The question changed." });

    const submit = screen.getByRole("region", { name: "Check and submit" });
    await user.click(within(submit).getByRole("button", { name: "Check the plan" }));
    expect(await within(submit).findByText(/say whether the plan keeps it/)).toBeInTheDocument();
    await user.click(within(submit).getByRole("button", { name: "Submit for review" }));
    await waitFor(() => {
      expect(requests.at(-1)?.path).toBe(`${TRACK}/plans/draft/submission`);
    });
  });

  it("shows a submitted revision read-only", async () => {
    signedIn(
      {},
      {
        [`GET ${TRACK}/plans/draft`]: () => json(plan({ revision: 2, state: "submitted" })),
      },
    );
    renderApp("/tracks/tokenizer/plan");
    expect(await screen.findByText(/waiting for a researcher/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add a unit" })).toBeNull();
  });
});
