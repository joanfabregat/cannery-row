import { screen, waitFor, within } from "@testing-library/react";

import {
  attempt,
  attention,
  decision,
  hypothesis,
  hypothesisApi,
  page,
  promotedHypothesis,
  report,
  nativeReport,
  review,
  summary,
} from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const ACCEPTED =
  "We tried “Shorter prompts”: the verification passed every check, and a researcher accepted it because “The gain holds on every split.”";

function listCalls(requests: Request[]) {
  return requests
    .map((r) => new URL(r.url))
    .filter((url) => url.pathname === "/api/projects/sardines/hypotheses");
}

describe("the hypotheses list", () => {
  it("hides archived hypotheses until asked, and filters by track and status", async () => {
    const { requests } = signedIn(
      { projects: [project("sardines", "Sardines", "viewer")] },
      {
        "GET /api/projects/sardines/hypotheses": (request) => {
          const archived = new URL(request.url).searchParams.get("archived");
          return json(
            page(
              archived === "false"
                ? [summary()]
                : [summary(), summary({ number: 7, title: "Bigger batches", state: "rejected" })],
            ),
          );
        },
        "GET /api/projects/sardines/tracks": () =>
          json(page([{ slug: "tokenizer", title: "Tokenizer" }])),
      },
    );
    const { user, router } = renderApp("/hypotheses");
    expect(await screen.findByRole("link", { name: "#12 Shorter prompts" })).toHaveAttribute(
      "href",
      "/hypotheses/12",
    );
    expect(screen.queryByText("Bigger batches")).not.toBeInTheDocument();
    expect(listCalls(requests).at(-1)?.searchParams.get("archived")).toBe("false");

    await user.click(screen.getByRole("checkbox", { name: /Show archived/ }));
    expect(await screen.findByRole("link", { name: "#7 Bigger batches" })).toBeInTheDocument();
    expect(listCalls(requests).at(-1)?.searchParams.has("archived")).toBe(false);
    expect(router.state.location.search).toBe("?archived=1");

    await user.selectOptions(screen.getByLabelText("Status"), "Rejected");
    await waitFor(() => {
      expect(listCalls(requests).at(-1)?.searchParams.getAll("state")).toEqual(["rejected"]);
    });
    await user.selectOptions(screen.getByLabelText("Track"), "Tokenizer");
    await waitFor(() => {
      expect(listCalls(requests).at(-1)?.searchParams.get("track")).toBe("tokenizer");
    });
  });

  it("pages with the cursor the API returns", async () => {
    const { requests } = signedIn(
      {},
      {
        "GET /api/projects/sardines/hypotheses": (request) =>
          new URL(request.url).searchParams.get("before") === "12"
            ? json(page([summary({ number: 3, title: "Older idea" })]))
            : json(page([summary()], 12)),
      },
    );
    const { user } = renderApp("/hypotheses");
    await screen.findByRole("link", { name: "#12 Shorter prompts" });
    await user.click(screen.getByRole("button", { name: "Older" }));
    expect(await screen.findByRole("link", { name: "#3 Older idea" })).toBeInTheDocument();
    expect(listCalls(requests).at(-1)?.searchParams.get("before")).toBe("12");
    await user.click(screen.getByRole("button", { name: "Newer" }));
    expect(await screen.findByRole("link", { name: "#12 Shorter prompts" })).toBeInTheDocument();
  });
});

describe("a hypothesis page", () => {
  it("opens with one sentence: what was tried, what happened, what was decided and why", async () => {
    const { h, attempts, reports } = promotedHypothesis();
    signedIn({}, hypothesisApi(h, { attempts, reports }));
    renderApp("/hypotheses/12");
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent(ACCEPTED);
    const outcome = screen.getByRole("region", { name: "Outcome" });
    expect(within(outcome).getByText("Halved the system prompt")).toBeInTheDocument();
    expect(
      within(outcome).getByText(
        "The verification passed every check: Accuracy held within the margin",
      ),
    ).toBeInTheDocument();
    expect(within(outcome).getByText(/Accepted by a researcher on/)).toBeInTheDocument();
  });

  it("shows attempts, evidence with verified and claimed values apart, decisions and details", async () => {
    const { h, attempts, reports } = promotedHypothesis();
    signedIn({}, hypothesisApi(h, { attempts, reports }));
    renderApp("/hypotheses/12");
    await screen.findByTestId("outcome-sentence");
    for (const name of ["Attempts", "Verification", "Decisions", "Comments"]) {
      expect(screen.getByRole("heading", { level: 2, name })).toBeInTheDocument();
    }
    expect(screen.getByRole("link", { name: /#12\.1/ })).toHaveAttribute(
      "href",
      "/hypotheses/12/attempts/1",
    );
    const evidence = screen.getByRole("region", { name: "Measurements" });
    expect(within(evidence).getAllByText("Verified").length).toBeGreaterThan(0);
    expect(within(evidence).getAllByText("Reported by agent").length).toBeGreaterThan(0);
    const decisions = screen.getByRole("region", { name: "Decisions" });
    expect(within(decisions).getByText(/The gain holds on every split/)).toBeInTheDocument();
    expect(screen.getByText("Details")).toBeInTheDocument();
  });

  it("says a failure plainly", async () => {
    const failed = attempt({
      state: "failed",
      failures: [
        {
          stage: "verify",
          code: "timeout",
          reason: "The verifier ran out of time",
          details: {},
          created_at: "2026-03-04T10:00:00Z",
          requeued: false,
        },
      ],
    });
    const h = hypothesis({
      state: "failed",
      reviews: [
        review({
          kind: "failure",
          decisions: [decision({ action: "stop", reason: "The dataset is gone" })],
        }),
      ],
    });
    signedIn({}, hypothesisApi(h, { attempts: [failed] }));
    renderApp("/hypotheses/12");
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent(
      "We tried “Shorter prompts”, but the work could not produce a result; a researcher closed it as failed because “The dataset is gone.”",
    );
    expect(
      screen.getByText("Attempt #12.1 failed: The verifier ran out of time"),
    ).toBeInTheDocument();
  });

  it("says when there is no such hypothesis", async () => {
    signedIn();
    renderApp("/hypotheses/99");
    expect(
      await screen.findByText("There is no hypothesis #99 in Sardines, or you cannot read it."),
    ).toBeInTheDocument();
  });

  it("renders a report's markdown without its raw HTML", async () => {
    const { h, attempts } = promotedHypothesis();
    const unsafe = report({
      report: nativeReport({
        what_was_tried: "Halved the system prompt",
        findings: 'Held at **91%**.<script>alert(1)</script><img src="https://x.test/a.png">',
      }),
    });
    signedIn({}, hypothesisApi(h, { attempts, reports: { 1: unsafe } }));
    const { container } = renderApp("/hypotheses/12");
    await screen.findByTestId("outcome-sentence");
    expect(screen.getAllByText("91%").some((node) => node.tagName === "STRONG")).toBe(true);
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector('img[src="https://x.test/a.png"]')).toBeNull();
  });
});

describe("a viewer finds an outcome and its reason in three clicks", () => {
  it("from Home: the recent outcome, then the sentence", async () => {
    const { h, attempts, reports } = promotedHypothesis();
    signedIn(
      { projects: [project("sardines", "Sardines", "viewer")] },
      {
        "GET /api/projects/sardines/attention": () =>
          json(
            attention({
              recent_outcomes: [
                {
                  hypothesis: 12,
                  hypothesis_ref: "#12",
                  title: "Shorter prompts",
                  hypothesis_state: "promoted",
                  track: "tokenizer",
                  attempt_ref: "#12.1",
                  action: "promote",
                  reason: "The gain holds on every split",
                  decided_at: "2026-03-05T10:00:00Z",
                  origin: "live",
                },
              ],
            }),
          ),
        ...hypothesisApi(h, { attempts, reports }),
      },
    );
    const { user } = renderApp("/");
    await user.click(await screen.findByRole("link", { name: "#12 Shorter prompts" }));
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent(ACCEPTED);
  });

  it("from the list: Hypotheses, Show archived, then the row", async () => {
    const h = hypothesis({
      number: 7,
      title: "Bigger batches",
      state: "rejected",
      reviews: [
        review({
          kind: "decision",
          decisions: [decision({ action: "reject", reason: "Latency doubled" })],
        }),
      ],
    });
    signedIn(
      { projects: [project("sardines", "Sardines", "viewer")] },
      {
        "GET /api/projects/sardines/attention": () => json(attention()),
        "GET /api/projects/sardines/tracks": () => json(page([])),
        "GET /api/projects/sardines/hypotheses": (request) =>
          json(
            page(
              new URL(request.url).searchParams.get("archived") === "false"
                ? []
                : [summary({ number: 7, title: "Bigger batches", state: "rejected" })],
            ),
          ),
        ...hypothesisApi(h, {
          attempts: [attempt({ number: 7, ref: "#7.1", state: "rejected" })],
          reports: { 1: report({ verification: null }) },
        }),
      },
    );
    const { user } = renderApp("/");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    await user.click(within(nav).getByRole("link", { name: "Hypotheses" }));
    await user.click(await screen.findByRole("checkbox", { name: /Show archived/ }));
    await user.click(await screen.findByRole("link", { name: "#7 Bigger batches" }));
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent(
      "We tried “Bigger batches”, and a researcher rejected it because “Latency doubled.”",
    );
  });
});
