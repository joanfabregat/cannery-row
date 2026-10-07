import { safeLogoutUrl, safeReturnTo, signInHref } from "./session";

describe("safeReturnTo", () => {
  it.each(["/", "/results", "/tracks?state=active#top", "/search?q=a%2Fb"])(
    "keeps the same-site path %j",
    (value) => {
      expect(safeReturnTo(value)).toBe(value);
    },
  );

  it.each([
    null,
    undefined,
    "",
    "results",
    "//evil.example/x",
    "/\\evil.example",
    "/\t/evil.example",
    "/\n/evil.example",
    "/x\u0000",
    "/x\u007f",
    "https://evil.example/",
    "javascript:alert(1)",
  ])("sends %j home", (value) => {
    expect(safeReturnTo(value)).toBe("/");
  });

  it("builds the login address from a safe path only", () => {
    expect(signInHref("/\t/evil.example")).toBe("/auth/login?return_to=%2F");
    expect(signInHref("/results")).toBe("/auth/login?return_to=%2Fresults");
  });
});

describe("safeLogoutUrl", () => {
  const idp = "https://idp.example/session/end?client_id=x";

  it("follows an https logout address", () => {
    expect(safeLogoutUrl(idp, "https:")).toBe(idp);
    expect(safeLogoutUrl(idp, "http:")).toBe(idp);
  });

  it("follows plain http only while the app itself is on http", () => {
    const local = "http://localhost:3001/session/end";
    expect(safeLogoutUrl(local, "http:")).toBe(local);
    expect(safeLogoutUrl(local, "https:")).toBeNull();
  });

  it.each([
    null,
    undefined,
    "",
    "javascript:alert(1)",
    "data:text/html,hi",
    "/session/end",
    "not a url",
  ])("refuses %j", (value) => {
    expect(safeLogoutUrl(value, "https:")).toBeNull();
  });
});
