import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";

import {
  evaluation,
  assessment,
  attempt,
  hypothesis,
  hypothesisApi,
  report,
  review,
  reviewCase,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const CASE_ID = "00000000-0000-4000-8000-00000000cafe";

function awaitingResult(verdict: Schemas["RequestEvidenceEnvelopeAssessmentVerdict"]) {
  const h = hypothesis({
    state: "awaiting_human_review",
    reviews: [review({ id: CASE_ID, kind: "result", state: "pending", subject_revision: 3 })],
  });
  return {
    ...hypothesisApi(h, {
      attempts: [attempt({ state: "awaiting_human_review" })],
      reports: { 1: report() },
    }),
    [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
      json(
        reviewCase({
          id: CASE_ID,
          evaluation: evaluation({
            assessment: assessment({
              verdict,
              gates: [
                { id: "accuracy_gate", result: verdict === "inconclusive" ? "unknown" : verdict },
              ],
            }),
          }),
        }),
      ),
  };
}

describe("reviewing a result", () => {
  it("needs a reason, confirms, and sends the decision once with an idempotency key", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        ...awaitingResult("pass"),
        [`POST /api/projects/sardines/review-cases/${CASE_ID}/decisions`]: (request) => {
          posted.push(request.clone());
          return json({ id: "d1" }, 201);
        },
      },
    );
    const { user, router } = renderApp("/hypotheses/12/review");
    const accept = await screen.findByRole("button", { name: "Accept" });
    expect(accept).toBeDisabled();
    expect(screen.getByRole("button", { name: "Reject" })).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: /Reason/ }), "   ");
    expect(accept).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: /Reason/ }), "Holds on every split");
    expect(accept).toBeEnabled();

    await user.click(accept);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Holds on every split")).toBeInTheDocument();
    expect(within(dialog).getByText(/changes no baseline/)).toBeInTheDocument();
    expect(posted).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "Confirm: Accept" }));

    expect(await screen.findByText("Your decision (Accept) was recorded.")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/hypotheses/12");
    expect(posted).toHaveLength(1);
    const request = posted[0];
    expect(request?.headers.get("idempotency-key")).toMatch(/.+/);
    expect(await request?.json()).toEqual({
      review_case_id: CASE_ID,
      evidence_revision: 3,
      action: "promote",
      reason: "Holds on every split",
    });
  });

  it("offers Accept only on a pass verdict", async () => {
    signedIn({}, awaitingResult("fail"));
    renderApp("/hypotheses/12/review");
    expect(await screen.findByRole("button", { name: "Reject" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Inconclusive" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Accept" })).not.toBeInTheDocument();
  });

  it("explains a stale decision and offers to reload, keeping the dialog's reason", async () => {
    signedIn(
      {},
      {
        ...awaitingResult("pass"),
        [`POST /api/projects/sardines/review-cases/${CASE_ID}/decisions`]: () =>
          json(
            {
              error: { code: "stale_revision", message: "evidence changed", details: null },
            },
            409,
          ),
      },
    );
    const { user } = renderApp("/hypotheses/12/review");
    await user.type(await screen.findByRole("textbox", { name: /Reason/ }), "Looks right");
    await user.click(screen.getByRole("button", { name: "Reject" }));
    await user.click(await screen.findByRole("button", { name: "Confirm: Reject" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/changed/i);
    expect(
      within(alert).getByRole("button", { name: "Reload the latest version" }),
    ).toBeInTheDocument();
  });
});

describe("reviewing a draft", () => {
  it("approves the revision on screen", async () => {
    const posted: Request[] = [];
    const h = hypothesis({
      state: "draft",
      revision: 2,
      reviews: [review({ kind: "draft", state: "pending", subject_revision: 2 })],
    });
    signedIn(
      {},
      {
        ...hypothesisApi(h),
        "POST /api/projects/sardines/hypotheses/12/draft-review": (request) => {
          posted.push(request.clone());
          return json({ id: "d1" }, 201);
        },
      },
    );
    const { user } = renderApp("/hypotheses/12/review");
    expect(await screen.findByText("Do shorter prompts keep quality?")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ask for changes" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Decline" })).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: /Reason/ }), "Clear and cheap");
    await user.click(screen.getByRole("button", { name: "Approve" }));
    await user.click(await screen.findByRole("button", { name: "Confirm: Approve" }));
    expect(await screen.findByText("Your decision (Approve) was recorded.")).toBeInTheDocument();
    expect(await posted[0]?.json()).toEqual({
      draft_revision: 2,
      action: "approve",
      reason: "Clear and cheap",
    });
  });
});

describe("reviewing a failure", () => {
  it("offers to try again or close as failed", async () => {
    const h = hypothesis({
      state: "awaiting_human_review",
      reviews: [review({ id: CASE_ID, kind: "failure", state: "pending" })],
    });
    signedIn(
      {},
      {
        ...hypothesisApi(h, { attempts: [attempt({ state: "failed" })] }),
        [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
          json(
            reviewCase({
              id: CASE_ID,
              kind: "failure",
              evaluation: null,
              failure: {
                stage: "tester",
                code: "timeout",
                reason: "The tester ran out of time",
                details: {},
                log_refs: [],
                created_at: "2026-03-04T10:00:00Z",
              },
            }),
          ),
      },
    );
    renderApp("/hypotheses/12/review");
    expect(await screen.findByRole("button", { name: "Try again" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Close as failed" })).toBeDisabled();
    expect(screen.getAllByText(/The tester ran out of time/).length).toBeGreaterThan(0);
  });
});

describe("who can review", () => {
  for (const role of ["viewer", "member"]) {
    it(`sends a ${role} back to the hypothesis`, async () => {
      signedIn({ projects: [project("sardines", "Sardines", role)] }, awaitingResult("pass"));
      renderApp("/hypotheses/12/review");
      expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
      expect(screen.queryByRole("textbox", { name: /Reason/ })).not.toBeInTheDocument();
    });
  }

  it("does not let an administrator who is not a member decide", async () => {
    signedIn(
      { admin: true, projects: [project("sardines", "Sardines", null)] },
      awaitingResult("pass"),
    );
    renderApp("/hypotheses/12/review");
    expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
  });

  it("does not let a read-only session decide", async () => {
    signedIn({ scopes: ["read"] }, awaitingResult("pass"));
    renderApp("/hypotheses/12/review");
    expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
  });
});

describe("a decision retried after a network error", () => {
  const failureCase = () => {
    const h = hypothesis({
      state: "awaiting_human_review",
      reviews: [review({ id: CASE_ID, kind: "failure", state: "pending" })],
    });
    return {
      ...hypothesisApi(h, { attempts: [attempt({ state: "failed" })] }),
      [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
        json(
          reviewCase({
            id: CASE_ID,
            kind: "failure",
            evaluation: null,
            failure: {
              stage: "tester",
              code: "timeout",
              reason: "The tester ran out of time",
              details: {},
              log_refs: [],
              created_at: "2026-03-04T10:00:00Z",
            },
          }),
        ),
    };
  };
  const draftCase = () =>
    hypothesisApi(
      hypothesis({
        state: "draft",
        reviews: [review({ kind: "draft", state: "pending", subject_revision: 1 })],
      }),
    );
  const flows = [
    {
      name: "draft",
      handlers: draftCase,
      post: "POST /api/projects/sardines/hypotheses/12/draft-review",
      choice: "Approve",
    },
    {
      name: "result",
      handlers: () => awaitingResult("pass"),
      post: `POST /api/projects/sardines/review-cases/${CASE_ID}/decisions`,
      choice: "Accept",
    },
    {
      name: "failure",
      handlers: failureCase,
      post: `POST /api/projects/sardines/review-cases/${CASE_ID}/decisions`,
      choice: "Try again",
    },
  ];

  for (const flow of flows) {
    it(`keeps its idempotency key on Confirm again, and takes a new one on a new confirmation (${flow.name})`, async () => {
      const keys: (string | null)[] = [];
      signedIn(
        {},
        {
          ...flow.handlers(),
          [flow.post]: (request) => {
            keys.push(request.headers.get("idempotency-key"));
            if (keys.length < 3) throw new TypeError("Failed to fetch");
            return json({ id: "d1" }, 201);
          },
        },
      );
      const { user } = renderApp("/hypotheses/12/review");
      await user.type(await screen.findByRole("textbox", { name: /Reason/ }), "Worth it");
      await user.click(screen.getByRole("button", { name: flow.choice }));
      const confirm = await screen.findByRole("button", { name: `Confirm: ${flow.choice}` });
      await user.click(confirm);
      await screen.findByRole("alert");
      await user.click(screen.getByRole("button", { name: `Confirm: ${flow.choice}` }));
      await waitFor(() => {
        expect(keys).toHaveLength(2);
      });
      await screen.findByRole("alert");
      expect(keys[0]).toMatch(/.+/);
      expect(keys[1]).toBe(keys[0]);

      await user.click(screen.getByRole("button", { name: "Go back" }));
      await waitFor(() => {
        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      });
      await user.click(screen.getByRole("button", { name: flow.choice }));
      await user.click(await screen.findByRole("button", { name: `Confirm: ${flow.choice}` }));
      expect(
        await screen.findByText(`Your decision (${flow.choice}) was recorded.`),
      ).toBeInTheDocument();
      expect(keys).toHaveLength(3);
      expect(keys[2]).toMatch(/.+/);
      expect(keys[2]).not.toBe(keys[0]);
    });
  }
});
