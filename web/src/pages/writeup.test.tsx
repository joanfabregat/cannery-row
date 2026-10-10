import { screen, within } from "@testing-library/react";

import type { Writeup } from "@/api/types";
import { attention, hypothesis, hypothesisApi } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const VERIFICATION = { ref: "00000000-0000-4000-8000-0000000000e1", sha256: "a".repeat(64) };

function pending(overrides: Partial<Writeup> = {}): Writeup {
  return {
    hypothesis: 12,
    hypothesis_ref: "#12",
    hypothesis_state: "documenting",
    status: "pending",
    job_id: "00000000-0000-4000-8000-0000000000d1",
    attempt_ref: "#12.2",
    claimed_by: null,
    claimed_by_user: null,
    inputs: { attempts: [1, 2], verification: VERIFICATION },
    context: "/api/projects/sardines/context?phase=document",
    writeup: null,
    skip_reason: null,
    ...overrides,
  };
}

function documenting(extra = {}) {
  return {
    ...hypothesisApi(hypothesis({ state: "documenting" })),
    "GET /api/projects/sardines/hypotheses/12/writeup": () => json(pending()),
    ...extra,
  };
}

describe("writing a hypothesis up", () => {
  it("starts from the front matter the write-up must state, and records it in one action", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      documenting({
        "POST /api/projects/sardines/hypotheses/12/writeup": (request: Request) => {
          posted.push(request.clone());
          return json(pending({ status: "written", hypothesis_state: "deciding" }), 201);
        },
      }),
    );
    const { user, router } = renderApp("/hypotheses/12/writeup");
    const box = await screen.findByRole("textbox", { name: "Write-up" });
    const template = (box as HTMLTextAreaElement).value;
    expect(template).toContain("attempts: [1, 2]");
    expect(template).toContain(`verification: {"ref":"${VERIFICATION.ref}"`);
    await user.click(screen.getByRole("button", { name: "Write it up" }));
    expect(
      await screen.findByText("#12 is written up and waits for its decision."),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/hypotheses/12");
    expect(posted).toHaveLength(1);
    expect(await posted[0]?.json()).toEqual({ document: template });
  });

  it("skips the write-up only with a reason", async () => {
    const posted: Request[] = [];
    signedIn(
      {},
      documenting({
        "POST /api/projects/sardines/hypotheses/12/writeup/skip": (request: Request) => {
          posted.push(request.clone());
          return json(pending({ status: "skipped", skip_reason: "Nothing to add" }));
        },
      }),
    );
    const { user } = renderApp("/hypotheses/12/writeup");
    const skip = await screen.findByRole("button", { name: "Skip the write-up" });
    expect(skip).toBeDisabled();
    await user.type(screen.getByRole("textbox", { name: /Reason/ }), "Nothing to add");
    await user.click(skip);
    await screen.findByText(/The write-up of #12 was skipped/);
    expect(await posted[0]?.json()).toEqual({ reason: "Nothing to add" });
  });

  it("lets only a researcher write it up", async () => {
    signedIn({ projects: [project("sardines", "Sardines", "member")] }, documenting());
    renderApp("/hypotheses/12/writeup");
    expect(
      await screen.findByText("Waiting for an agent or a researcher to write it up."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Write it up" })).not.toBeInTheDocument();
  });

  it("offers Write it up on a hypothesis being written up", async () => {
    signedIn({}, documenting());
    renderApp("/hypotheses/12");
    expect(await screen.findByRole("link", { name: "Write it up" })).toHaveAttribute(
      "href",
      "/hypotheses/12/writeup",
    );
  });

  it("lists the write-ups to do on Home", async () => {
    signedIn(
      {},
      {
        "GET /api/projects/sardines/attention": () =>
          json(
            attention({
              pending_writeup_count: 1,
              pending_writeups: [
                {
                  hypothesis: 12,
                  hypothesis_ref: "#12",
                  title: "Shorter prompts",
                  track: "tokenizer",
                  attempt_ref: "#12.2",
                  attempt_state: "verified",
                  job_id: "00000000-0000-4000-8000-0000000000d1",
                  job_state: "pending",
                  claimed_by: null,
                  claimed_by_user: null,
                  waiting_since: "2026-03-04T10:00:00Z",
                },
              ],
            }),
          ),
      },
    );
    renderApp("/");
    const queue = await screen.findByRole("region", { name: "Write-ups to do" });
    expect(within(queue).getByRole("link", { name: "#12 Shorter prompts" })).toHaveAttribute(
      "href",
      "/hypotheses/12/writeup",
    );
    expect(within(queue).getByText("To write")).toBeInTheDocument();
  });
});
