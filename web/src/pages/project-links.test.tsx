import { screen, waitFor, within } from "@testing-library/react";

import { projectStorageKey } from "@/projects/project-context";
import { comment, hypothesis, hypothesisApi, page, summary } from "@/test/fixtures";
import { json, project, renderApp, signedIn, USER_ID } from "@/test/render";

const UNREADABLE = "You cannot open project secret, or it does not exist.";

describe("a link to a project the user cannot open", () => {
  it("says so on a hypothesis, attempt, review and track page, never showing the current project's record", async () => {
    signedIn({}, hypothesisApi(hypothesis()));
    for (const path of [
      "/hypotheses/12?project=secret",
      "/hypotheses/12/attempts/1?project=secret",
      "/hypotheses/12/review?project=secret",
      "/tracks/tokenizer?project=secret",
    ]) {
      const { unmount } = renderApp(path);
      expect(await screen.findByText(UNREADABLE)).toBeInTheDocument();
      expect(screen.queryByText("Shorter prompts")).not.toBeInTheDocument();
      unmount();
    }
  });

  it("says so after following a mention in a comment", async () => {
    signedIn(
      {},
      hypothesisApi(hypothesis(), { comments: [comment({ body_markdown: "Like secret#12" })] }),
    );
    const { user, router } = renderApp("/hypotheses/12");
    const section = await screen.findByRole("region", { name: "Comments" });
    await user.click(await within(section).findByRole("link", { name: "secret#12" }));
    expect(await screen.findByText(UNREADABLE)).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/hypotheses/12");
    expect(screen.queryByTestId("outcome-sentence")).not.toBeInTheDocument();
  });

  it("says so after a search jump", async () => {
    signedIn({}, hypothesisApi(hypothesis()));
    const { user, router } = renderApp("/tracks");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.type(box, "secret#12{Enter}");
    expect(await screen.findByText(UNREADABLE)).toBeInTheDocument();
    expect(router.state.location.search).toBe("?project=secret");
    expect(screen.queryByTestId("outcome-sentence")).not.toBeInTheDocument();
  });
});

describe("the project switcher on a linked page", () => {
  it("wins over the link, drops it from the address and remembers the choice", async () => {
    const anchovies = hypothesis({ title: "Anchovy idea", project: "anchovies" });
    signedIn(
      { projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")] },
      {
        ...hypothesisApi(hypothesis()),
        ...hypothesisApi(anchovies, { project: "anchovies" }),
      },
    );
    const { user, router } = renderApp("/hypotheses/12?project=sardines");
    const trigger = await screen.findByRole("button", { name: "Switch project" });
    await waitFor(() => {
      expect(trigger).toHaveTextContent("Sardines");
    });
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent("Shorter prompts");

    await user.click(trigger);
    await user.click(await screen.findByRole("menuitemradio", { name: "Anchovies" }));
    await waitFor(() => {
      expect(trigger).toHaveTextContent("Anchovies");
    });
    expect(router.state.location.pathname).toBe("/hypotheses/12");
    expect(router.state.location.search).toBe("");
    expect(await screen.findByTestId("outcome-sentence")).toHaveTextContent("Anchovy idea");
    expect(localStorage.getItem(projectStorageKey(USER_ID))).toBe("anchovies");
  });
});

describe("the hypotheses list across projects", () => {
  it("starts the new project at its first page, never showing the old project's rows", async () => {
    const anchovyCalls: URL[] = [];
    let releaseAnchovies: () => void = () => undefined;
    const anchoviesReady = new Promise<void>((resolve) => {
      releaseAnchovies = resolve;
    });
    signedIn(
      { projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")] },
      {
        "GET /api/projects/sardines/hypotheses": (request) => {
          const before = new URL(request.url).searchParams.get("before");
          if (before === null) return json(page([summary({ title: "Sardine page one" })], 12));
          if (before === "12") return json(page([summary({ title: "Sardine page two" })], 8));
          return json(page([summary({ number: 3, title: "Sardine page three" })]));
        },
        "GET /api/projects/anchovies/hypotheses": async (request) => {
          anchovyCalls.push(new URL(request.url));
          await anchoviesReady;
          return json(page([summary({ number: 1, title: "Anchovy first" })]));
        },
      },
    );
    localStorage.setItem(projectStorageKey(USER_ID), "sardines");
    const { user } = renderApp("/hypotheses");
    await screen.findByText("Sardine page one");
    await user.click(screen.getByRole("button", { name: "Older" }));
    await screen.findByText("Sardine page two");
    await user.click(screen.getByRole("button", { name: "Older" }));
    await screen.findByText("Sardine page three");

    await user.click(screen.getByRole("button", { name: "Switch project" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Anchovies" }));
    await waitFor(() => {
      expect(anchovyCalls).toHaveLength(1);
    });
    expect(screen.queryByText(/Sardine page/)).not.toBeInTheDocument();
    releaseAnchovies();
    expect(await screen.findByText("Anchovy first")).toBeInTheDocument();
    expect(screen.queryByText(/Sardine page/)).not.toBeInTheDocument();
    expect(anchovyCalls.every((url) => !url.searchParams.has("before"))).toBe(true);
    expect(screen.getByRole("button", { name: "Newer" })).toBeDisabled();
  });
});
