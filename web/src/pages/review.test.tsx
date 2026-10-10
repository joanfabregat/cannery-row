import { screen, waitFor, within } from "@testing-library/react";

import type { Schemas } from "@/api/client";
import type { Writeup } from "@/api/types";

import {
  attempt,
  unit,
  unitApi,
  report,
  review,
  reviewCase,
  verificationDocument,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const CASE_ID = "00000000-0000-4000-8000-00000000cafe";

const VERIFICATION = { ref: "00000000-0000-4000-8000-0000000000e1", sha256: "a".repeat(64) };
const WRITEUP_ID = "00000000-0000-4000-8000-0000000000f1";

function writeup(overrides: Partial<Writeup> = {}): Writeup {
  return {
    unit: 12,
    unit_ref: "#12",
    unit_state: "deciding",
    status: "written",
    job_id: "00000000-0000-4000-8000-0000000000d1",
    attempt_ref: "#12.1",
    claimed_by: null,
    claimed_by_user: null,
    inputs: { attempts: [1], verification: VERIFICATION },
    context: null,
    writeup: {
      id: WRITEUP_ID,
      sha256: "b".repeat(64),
      front_matter: { summary: "Shorter prompts held on every split." },
      body_markdown: "## Results\n\nThe verifier measured 0.44.",
      written_by_user: null,
      written_by_service: "00000000-0000-4000-8000-0000000000a9",
      created_at: "2026-03-04T11:00:00Z",
    },
    skip_reason: null,
    ...overrides,
  };
}

function awaitingResult(verdict: Schemas["Verdict"], w: Writeup = writeup()) {
  const h = unit({
    state: "deciding",
    reviews: [review({ id: CASE_ID, kind: "decision", state: "pending", subject_revision: 3 })],
  });
  return {
    ...unitApi(h, {
      attempts: [attempt({ state: "verified" })],
      reports: { 1: report() },
    }),
    "GET /api/projects/sardines/units/12/writeup": () => json(w),
    [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
      json(
        reviewCase({
          id: CASE_ID,
          verification: verificationDocument({
            verdict,
            gates: [
              { id: "accuracy_gate", result: verdict === "inconclusive" ? "unknown" : verdict },
            ],
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
    const { user, router } = renderApp("/units/12/review");
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
    expect(router.state.location.pathname).toBe("/units/12");
    expect(posted).toHaveLength(1);
    const request = posted[0];
    expect(request?.headers.get("idempotency-key")).toMatch(/.+/);
    expect(await request?.json()).toEqual({
      review_case_id: CASE_ID,
      document: `---\noutcome: promote\nverification: {"ref":"${VERIFICATION.ref}","sha256":"${VERIFICATION.sha256}"}\nwriteup: {"ref":"${WRITEUP_ID}","sha256":"${"b".repeat(64)}"}\n---\n\nHolds on every split\n`,
    });
  });

  it("shows the write-up before the decision", async () => {
    signedIn({}, awaitingResult("pass"));
    renderApp("/units/12/review");
    const section = await screen.findByRole("region", { name: "Write-up" });
    expect(
      await within(section).findByText("Shorter prompts held on every split."),
    ).toBeInTheDocument();
  });

  it("shows a skipped write-up's reason and cites no write-up", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      {
        ...awaitingResult(
          "fail",
          writeup({ status: "skipped", writeup: null, skip_reason: "The numbers say it all." }),
        ),
        [`POST /api/projects/sardines/review-cases/${CASE_ID}/decisions`]: (request) => {
          posted.push(request.clone());
          return json({ id: "d1" }, 201);
        },
      },
    );
    const { user } = renderApp("/units/12/review");
    expect(await screen.findByText("No write-up: The numbers say it all.")).toBeInTheDocument();
    await user.type(screen.getByRole("textbox", { name: /Reason/ }), "Below the bar");
    await user.click(screen.getByRole("button", { name: "Reject" }));
    await user.click(await screen.findByRole("button", { name: "Confirm: Reject" }));
    await screen.findByText("Your decision (Reject) was recorded.");
    const body = (await posted[0]?.json()) as { document: string };
    expect(body.document).toContain("\noutcome: reject\n");
    expect(body.document).toContain("\nwriteup: null\n");
  });

  it("offers only to close a stopped unit as failed", async () => {
    const handlers = awaitingResult(
      "fail",
      writeup({ inputs: { attempts: [1], verification: null } }),
    );
    signedIn(
      {},
      {
        ...handlers,
        [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
          json(reviewCase({ id: CASE_ID, verification: null })),
      },
    );
    renderApp("/units/12/review");
    expect(await screen.findByRole("button", { name: "Close as failed" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Reject" })).not.toBeInTheDocument();
  });

  it("offers Accept only on a pass verdict", async () => {
    signedIn({}, awaitingResult("fail"));
    renderApp("/units/12/review");
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
    const { user } = renderApp("/units/12/review");
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

describe("reviewing a failure", () => {
  it("offers to try again or stop", async () => {
    const h = unit({
      state: "active",
      reviews: [review({ id: CASE_ID, kind: "failure", state: "pending" })],
    });
    signedIn(
      {},
      {
        ...unitApi(h, { attempts: [attempt({ state: "failed" })] }),
        [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
          json(
            reviewCase({
              id: CASE_ID,
              kind: "failure",
              verification: null,
              failure: {
                stage: "verify",
                code: "timeout",
                reason: "The verifier ran out of time",
                details: {},
                log_refs: [],
                created_at: "2026-03-04T10:00:00Z",
              },
            }),
          ),
      },
    );
    renderApp("/units/12/review");
    expect(await screen.findByRole("button", { name: "Try again" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeDisabled();
    expect(screen.getAllByText(/The verifier ran out of time/).length).toBeGreaterThan(0);
  });
});

describe("who can review", () => {
  for (const role of ["viewer", "member"]) {
    it(`sends a ${role} back to the unit`, async () => {
      signedIn({ projects: [project("sardines", "Sardines", role)] }, awaitingResult("pass"));
      renderApp("/units/12/review");
      expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
      expect(screen.queryByRole("textbox", { name: /Reason/ })).not.toBeInTheDocument();
    });
  }

  it("does not let an administrator who is not a member decide", async () => {
    signedIn(
      { admin: true, projects: [project("sardines", "Sardines", null)] },
      awaitingResult("pass"),
    );
    renderApp("/units/12/review");
    expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
  });

  it("does not let a read-only session decide", async () => {
    signedIn({ scopes: ["read"] }, awaitingResult("pass"));
    renderApp("/units/12/review");
    expect(await screen.findByText(/Only researchers record decisions/)).toBeInTheDocument();
  });
});

describe("a decision retried after a network error", () => {
  const failureCase = () => {
    const h = unit({
      state: "active",
      reviews: [review({ id: CASE_ID, kind: "failure", state: "pending" })],
    });
    return {
      ...unitApi(h, { attempts: [attempt({ state: "failed" })] }),
      [`GET /api/projects/sardines/review-cases/${CASE_ID}`]: () =>
        json(
          reviewCase({
            id: CASE_ID,
            kind: "failure",
            verification: null,
            failure: {
              stage: "verify",
              code: "timeout",
              reason: "The verifier ran out of time",
              details: {},
              log_refs: [],
              created_at: "2026-03-04T10:00:00Z",
            },
          }),
        ),
    };
  };
  const flows = [
    {
      name: "decision",
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
      const { user } = renderApp("/units/12/review");
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
