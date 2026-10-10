import { render, screen } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";

import { resolveLink } from "@/lib/links";

import { Markdown, Snippet } from "./markdown";

function renderMarkdown(source: string) {
  const router = createMemoryRouter([{ path: "*", element: <Markdown>{source}</Markdown> }]);
  return render(<RouterProvider router={router} />);
}

const origin = window.location.origin;

describe("links in untrusted Markdown", () => {
  it("resolves each kind of address", () => {
    expect(resolveLink("/units/1")).toEqual({ kind: "internal", to: "/units/1" });
    expect(resolveLink("javascript:alert(1)")).toBeNull();
    expect(resolveLink("data:text/html,x")).toBeNull();
    expect(resolveLink("mailto:a@example.com")).toBeNull();
    expect(resolveLink("relative/page")).toBeNull();
    expect(resolveLink("")).toBeNull();
    expect(resolveLink("https://x.example/a")).toEqual({
      kind: "external",
      href: "https://x.example/a",
    });
    // Another site to a browser, never the router.
    expect(resolveLink("//x.example")).toEqual({ kind: "external", href: "http://x.example/" });
    expect(resolveLink("/\\x.example")).toEqual({ kind: "external", href: "http://x.example/" });
    expect(resolveLink("\\\\x.example")).toEqual({ kind: "external", href: "http://x.example/" });
    expect(resolveLink("/\t/x.example")).toEqual({ kind: "external", href: "http://x.example/" });
    // A percent-encoded backslash stays in the path: a page of this app.
    expect(resolveLink("/%5Cx.example")).toEqual({ kind: "internal", to: "/%5Cx.example" });
    expect(new URL("/%5Cx.example", origin).origin).toBe(origin);
  });

  it("opens app paths in the app, other sites in a new tab, and drops other schemes", () => {
    renderMarkdown(
      [
        "[inside](/units/1)",
        "[script](javascript:alert(1))",
        "[proto](//x.example)",
        "[slash](/\\x.example)",
        "[double](\\\\x.example)",
        "[encoded](/%5Cx.example)",
        "[site](https://x.example/)",
      ].join(" "),
    );
    const inside = screen.getByRole("link", { name: "inside" });
    expect(inside).toHaveAttribute("href", "/units/1");
    expect(inside).not.toHaveAttribute("target");

    expect(screen.queryByRole("link", { name: "script" })).not.toBeInTheDocument();
    // Shown as its text, with no address.
    expect(document.body).toHaveTextContent(/inside script proto/);
    expect(document.querySelector("[href*=javascript]")).toBeNull();

    // The Markdown parser may percent-encode a backslash before the link is
    // resolved; whatever it does, no link leaves this site in the same tab.
    for (const name of ["proto", "slash", "double", "encoded"]) {
      const link = screen.queryByRole("link", { name });
      if (link === null) continue;
      const target = new URL(link.getAttribute("href") ?? "", origin);
      if (target.origin === origin) expect(link).not.toHaveAttribute("target");
      else expect(link).toHaveAttribute("target", "_blank");
    }
    for (const name of ["proto", "site"]) {
      const link = screen.getByRole("link", { name });
      expect(link).toHaveAttribute("target", "_blank");
      expect(link).toHaveAttribute("rel", "noopener noreferrer nofollow");
      expect(new URL(link.getAttribute("href") ?? "").origin).not.toBe(origin);
    }

    const encoded = screen.getByRole("link", { name: "encoded" });
    expect(encoded).not.toHaveAttribute("target");
    expect(new URL(encoded.getAttribute("href") ?? "", origin).origin).toBe(origin);
  });
});

describe("search snippets", () => {
  it("highlights between the API's control characters, never on asterisks", () => {
    render(<Snippet text={"a **b** \u0001match\u0002 c \u0002stray"} />);
    const marks = document.querySelectorAll("mark");
    expect([...marks].map((m) => m.textContent)).toEqual(["match"]);
    expect(document.body.textContent).toBe("a **b** match c stray");
  });
});
