import { screen, waitFor, within } from "@testing-library/react";

import { leaveApp } from "@/lib/leave-app";
import { projectStorageKey } from "@/projects/project-context";
import {
  json,
  me,
  mockApi,
  project,
  renderApp,
  signedIn,
  unauthenticated,
  USER_ID,
} from "@/test/render";

vi.mock("@/lib/leave-app", () => ({ leaveApp: vi.fn() }));

const projectKey = projectStorageKey(USER_ID);

function pause(ms = 20) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe("signing in", () => {
  it("sends an unauthenticated visitor to the sign-in page, keeping where they were going", async () => {
    mockApi({ "GET /api/me": unauthenticated });
    const { router } = renderApp("/tracks?state=active");
    const button = await screen.findByRole("link", { name: "Sign in" });
    expect(router.state.location.pathname).toBe("/sign-in");
    expect(button).toHaveAttribute(
      "href",
      `/auth/login?return_to=${encodeURIComponent("/tracks?state=active")}`,
    );
    expect(screen.getByRole("img", { name: "Cannery Row" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Main" })).not.toBeInTheDocument();
  });

  it("ignores a return address on another site", async () => {
    mockApi({ "GET /api/me": unauthenticated });
    renderApp("/sign-in?return_to=//evil.example/x");
    expect(await screen.findByRole("link", { name: "Sign in" })).toHaveAttribute(
      "href",
      "/auth/login?return_to=%2F",
    );
  });

  it("goes straight to the page when already signed in", async () => {
    signedIn();
    const { router } = renderApp("/sign-in?return_to=/results");
    expect(await screen.findByRole("heading", { level: 1, name: "Results" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/results");
  });
});

describe("the shell", () => {
  it("has the six navigation entries in plain words", async () => {
    signedIn();
    renderApp("/");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((link) => link.textContent)).toEqual([
      "Home",
      "Tracks",
      "Hypotheses",
      "Results",
      "Search",
      "Settings",
    ]);
    expect(within(nav).getByRole("link", { name: "Home" })).toHaveAttribute("aria-current", "page");
  });

  it("shows each entry's page with its title", async () => {
    signedIn();
    const { user } = renderApp("/");
    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Main" });
    for (const name of ["Tracks", "Hypotheses", "Results", "Search", "Settings"]) {
      await user.click(within(nav).getByRole("link", { name }));
      expect(await screen.findByRole("heading", { level: 1, name })).toBeInTheDocument();
      expect(within(nav).getByRole("link", { name })).toHaveAttribute("aria-current", "page");
      expect(document.title).toBe(`${name} · Cannery Row`);
    }
  });

  it("starts with a skip link to the page content", async () => {
    signedIn();
    const { user } = renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "Home" });
    await user.tab();
    const skip = screen.getByRole("link", { name: "Skip to content" });
    expect(skip).toHaveFocus();
    expect(skip).toHaveAttribute("href", "#main");
    expect(document.getElementById("main")).toHaveAttribute("tabindex", "-1");
  });

  it("searches from the global bar", async () => {
    signedIn(
      {},
      { "GET /api/search": () => json({ items: [], next_before: null, total: 0, facets: {} }) },
    );
    const { user, router } = renderApp("/tracks");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.type(box, "tokenizer{Enter}");
    expect(await screen.findByText("0 results for “tokenizer”")).toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=tokenizer");
  });

  it("focuses the search bar with the / key", async () => {
    signedIn();
    const { user } = renderApp("/");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.keyboard("/");
    expect(box).toHaveFocus();
  });

  it("shows a page for an unknown address", async () => {
    signedIn();
    renderApp("/nowhere");
    expect(
      await screen.findByRole("heading", { level: 1, name: "Page not found" }),
    ).toBeInTheDocument();
  });
});

describe("project switcher", () => {
  it("is hidden when the user has one project", async () => {
    signedIn({ projects: [project("sardines", "Sardines")] });
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "Home" });
    expect(screen.queryByRole("button", { name: "Switch project" })).not.toBeInTheDocument();
    expect(screen.getByTestId("project-name")).toHaveTextContent("Sardines");
  });

  it("switches between several projects", async () => {
    const { requests } = signedIn({
      projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")],
    });
    const { user } = renderApp("/");
    const trigger = await screen.findByRole("button", { name: "Switch project" });
    expect(trigger).toHaveTextContent("Anchovies");
    const attentionFor = () =>
      requests
        .map((r) => new URL(r.url).pathname)
        .filter((path) => path.endsWith("/attention"))
        .at(-1);
    await waitFor(() => {
      expect(attentionFor()).toBe("/api/projects/anchovies/attention");
    });
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitemradio", { name: "Sardines" }));
    await waitFor(() => {
      expect(attentionFor()).toBe("/api/projects/sardines/attention");
    });
    expect(trigger).toHaveTextContent("Sardines");
    expect(localStorage.getItem(projectKey)).toBe("sardines");
  });
});

describe("settings", () => {
  it("hides the administration sections from non-administrators", async () => {
    signedIn({ admin: false });
    renderApp("/settings");
    expect(
      await screen.findByRole("heading", { level: 2, name: "Access tokens" }),
    ).toBeInTheDocument();
    for (const name of ["Projects", "Members", "Service accounts"]) {
      expect(screen.queryByRole("heading", { level: 2, name })).not.toBeInTheDocument();
    }
  });

  it("shows them to administrators", async () => {
    signedIn({ admin: true });
    renderApp("/settings");
    for (const name of ["Access tokens", "Projects", "Members", "Service accounts"]) {
      expect(await screen.findByRole("heading", { level: 2, name })).toBeInTheDocument();
    }
  });
});

describe("user menu", () => {
  it("signs out through the backend, with the CSRF token, and returns to sign-in", async () => {
    const { requests } = signedIn({}, { "POST /auth/logout": () => json({ logout_url: null }) });
    const { user, router } = renderApp("/hypotheses");
    await user.click(await screen.findByRole("button", { name: "Account: Ada Lovelace" }));
    await user.click(await screen.findByRole("menuitem", { name: "Sign out" }));
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
    const logout = requests.find((r) => r.url.endsWith("/auth/logout"));
    expect(logout?.method).toBe("POST");
    expect(logout?.headers.get("X-CSRF-Token")).toBe("csrf-123");
  });

  it("switches to the dark theme and remembers it", async () => {
    signedIn();
    const { user } = renderApp("/");
    await user.click(await screen.findByRole("button", { name: "Account: Ada Lovelace" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Dark" }));
    await waitFor(() => {
      expect(document.documentElement).toHaveClass("dark");
    });
    expect(localStorage.getItem("cannery-row.theme")).toBe("dark");
  });
});

describe("an expired session", () => {
  it("sends the user to sign-in when a later request is refused", async () => {
    mockApi({
      "GET /api/me": () => json({ kind: "user", memberships: [], scopes: [], channel: "ui" }),
      "GET /api/projects": unauthenticated,
    });
    renderApp("/results");
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
  });
});

describe("focus after navigation", () => {
  it("moves to the page when a sidebar link is followed", async () => {
    signedIn();
    const { user } = renderApp("/");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    await user.click(within(nav).getByRole("link", { name: "Tracks" }));
    await screen.findByRole("heading", { level: 1, name: "Tracks" });
    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("moves to the page, not the menu button, when a link in the mobile menu is followed", async () => {
    signedIn();
    const { user } = renderApp("/");
    await user.click(await screen.findByRole("button", { name: "Open menu" }));
    const menu = await screen.findByRole("dialog", { name: "Menu" });
    await user.click(within(menu).getByRole("link", { name: "Results" }));
    await screen.findByRole("heading", { level: 1, name: "Results" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    // Radix restores focus on close after a tick; it must not win.
    await pause();
    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("puts focus back on the menu button when the menu is closed without navigating", async () => {
    signedIn();
    const { user } = renderApp("/");
    const trigger = await screen.findByRole("button", { name: "Open menu" });
    await user.click(trigger);
    await screen.findByRole("dialog", { name: "Menu" });
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(trigger).toHaveFocus();
    });
  });

  it("moves to the page from the skip link", async () => {
    signedIn();
    const { user } = renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "Home" });
    await user.tab();
    expect(screen.getByRole("link", { name: "Skip to content" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("main")).toHaveFocus();
  });
});

describe("the / shortcut", () => {
  it("leaves another field alone", async () => {
    signedIn();
    const { user } = renderApp("/");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    const other = document.createElement("input");
    other.setAttribute("aria-label", "Other");
    document.body.append(other);
    try {
      await user.click(other);
      await user.keyboard("a/b");
      expect(other).toHaveFocus();
      expect(other).toHaveValue("a/b");
      expect(box).not.toHaveFocus();
    } finally {
      other.remove();
    }
  });

  it("is ignored with a modifier key", async () => {
    signedIn();
    const { user } = renderApp("/");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.keyboard("{Control>}/{/Control}");
    await user.keyboard("{Alt>}/{/Alt}");
    await user.keyboard("{Meta>}/{/Meta}");
    expect(box).not.toHaveFocus();
  });

  it("is ignored while a menu is open", async () => {
    signedIn();
    const { user } = renderApp("/");
    const box = await screen.findByRole("searchbox", { name: "Search" });
    await user.click(screen.getByRole("button", { name: "Account: Ada Lovelace" }));
    await screen.findByRole("menu");
    await user.keyboard("/");
    expect(box).not.toHaveFocus();
  });
});

describe("the project list", () => {
  it("says the service is not answering, not that the user has no project, when it fails", async () => {
    let calls = 0;
    mockApi({
      "GET /api/me": () => json(me()),
      "GET /api/projects": () => {
        calls += 1;
        // The query retries twice before giving up.
        return calls <= 3
          ? json({ error: { code: "unavailable", message: "down", details: null } }, 503)
          : json({ items: [project("sardines", "Sardines")], next_before: null });
      },
    });
    const { user } = renderApp("/");
    const retry = await screen.findByRole("button", { name: "Try again" }, { timeout: 8000 });
    expect(screen.getByText("Cannery Row is not answering right now.")).toBeInTheDocument();
    expect(screen.queryByText(/You do not belong to a project yet/)).not.toBeInTheDocument();
    await user.click(retry);
    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeInTheDocument();
    expect(screen.getByTestId("project-name")).toHaveTextContent("Sardines");
  }, 15_000);

  it("reads every page", async () => {
    const { requests } = mockApi({
      "GET /api/me": () => json(me()),
      "GET /api/projects": (request) =>
        new URL(request.url).searchParams.get("before") === "anchovies"
          ? json({ items: [project("sardines", "Sardines")], next_before: null })
          : json({ items: [project("anchovies", "Anchovies")], next_before: "anchovies" }),
    });
    const { user } = renderApp("/");
    await user.click(await screen.findByRole("button", { name: "Switch project" }));
    const items = await screen.findAllByRole("menuitemradio");
    expect(items.map((item) => item.textContent)).toEqual(["Anchovies", "Sardines"]);
    const pages = requests
      .filter((r) => new URL(r.url).pathname === "/api/projects")
      .map((r) => new URL(r.url).search);
    expect(pages).toEqual(["?limit=200", "?limit=200&before=anchovies"]);
  });

  it("tells a user with no project so, without a switcher", async () => {
    signedIn({ projects: [] });
    renderApp("/");
    expect(
      await screen.findByText(
        "You do not belong to a project yet. An administrator can add you to one.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Switch project" })).not.toBeInTheDocument();
    expect(screen.queryByTestId("project-name")).not.toBeInTheDocument();
  });

  it("restores the project this user chose last time", async () => {
    localStorage.setItem(projectKey, "sardines");
    signedIn({ projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")] });
    renderApp("/");
    expect(await screen.findByRole("button", { name: "Switch project" })).toHaveTextContent(
      "Sardines",
    );
  });

  it("ignores another user's choice in the same browser", async () => {
    localStorage.setItem(projectStorageKey("someone-else"), "sardines");
    signedIn({ projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")] });
    renderApp("/");
    expect(await screen.findByRole("button", { name: "Switch project" })).toHaveTextContent(
      "Anchovies",
    );
    expect(localStorage.getItem(projectStorageKey("someone-else"))).toBe("sardines");
  });

  it("falls back to the first project and forgets one the user can no longer open", async () => {
    localStorage.setItem(projectKey, "herring");
    signedIn({ projects: [project("anchovies", "Anchovies"), project("sardines", "Sardines")] });
    renderApp("/");
    expect(await screen.findByRole("button", { name: "Switch project" })).toHaveTextContent(
      "Anchovies",
    );
    await waitFor(() => {
      expect(localStorage.getItem(projectKey)).toBeNull();
    });
  });
});

describe("signing out", () => {
  beforeEach(() => {
    vi.mocked(leaveApp).mockClear();
  });

  async function signOut(user: ReturnType<typeof renderApp>["user"]) {
    await user.click(await screen.findByRole("button", { name: "Account: Ada Lovelace" }));
    await user.click(await screen.findByRole("menuitem", { name: "Sign out" }));
  }

  it("goes on to the identity provider's https logout", async () => {
    const url = "https://idp.example/session/end?client_id=x";
    signedIn({}, { "POST /auth/logout": () => json({ logout_url: url }) });
    const { user } = renderApp("/");
    await signOut(user);
    await waitFor(() => {
      expect(leaveApp).toHaveBeenCalledWith(url);
    });
  });

  it("refuses a logout address that is not a web page", async () => {
    signedIn({}, { "POST /auth/logout": () => json({ logout_url: "javascript:alert(1)" }) });
    const { user, router } = renderApp("/");
    await signOut(user);
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
    expect(leaveApp).not.toHaveBeenCalled();
  });

  it("still signs out locally when the session had already ended", async () => {
    signedIn({}, { "POST /auth/logout": unauthenticated });
    const { user, router } = renderApp("/results");
    await signOut(user);
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
  });

  it("still signs out locally when the API fails", async () => {
    signedIn(
      {},
      {
        "POST /auth/logout": () =>
          json({ error: { code: "unavailable", message: "down", details: null } }, 503),
      },
    );
    const { user, router } = renderApp("/results");
    await signOut(user);
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
  });

  it("refreshes a stale CSRF token and retries once", async () => {
    let meCalls = 0;
    let logoutCalls = 0;
    const { requests } = mockApi({
      "GET /api/me": () => {
        meCalls += 1;
        return json(me({ csrf: meCalls === 1 ? "csrf-old" : "csrf-new" }));
      },
      "GET /api/projects": () =>
        json({ items: [project("sardines", "Sardines")], next_before: null }),
      "POST /auth/logout": (request) => {
        logoutCalls += 1;
        return request.headers.get("X-CSRF-Token") === "csrf-new"
          ? json({ logout_url: null })
          : json({ error: { code: "csrf_invalid", message: "stale", details: null } }, 403);
      },
    });
    const { user, router } = renderApp("/");
    await signOut(user);
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
    expect(logoutCalls).toBe(2);
    const sent = requests
      .filter((r) => r.url.endsWith("/auth/logout"))
      .map((r) => r.headers.get("X-CSRF-Token"));
    expect(sent).toEqual(["csrf-old", "csrf-new"]);
  });

  it("retries a CSRF refusal only once", async () => {
    let logoutCalls = 0;
    mockApi({
      "GET /api/me": () => json(me({ csrf: `csrf-${String(Math.random())}` })),
      "GET /api/projects": () =>
        json({ items: [project("sardines", "Sardines")], next_before: null }),
      "POST /auth/logout": () => {
        logoutCalls += 1;
        return json({ error: { code: "csrf_invalid", message: "stale", details: null } }, 403);
      },
    });
    const { user, router } = renderApp("/");
    await signOut(user);
    expect(await screen.findByRole("link", { name: "Sign in" })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/sign-in");
    expect(logoutCalls).toBe(2);
  });
});
