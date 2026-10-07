import { json, mockApi } from "@/test/render";

import { api, ApiError, setCsrfRefresher, setCsrfToken } from "./client";

describe("API client", () => {
  afterEach(() => {
    setCsrfToken(null);
  });

  it("sends the CSRF token on unsafe requests only", async () => {
    const { requests } = mockApi({
      "GET /api/tokens": () => json({ items: [], next_before: null }),
      "POST /api/tokens": () => json({}, 201),
      "DELETE /api/tokens/abc": () => json({}),
    });
    setCsrfToken("csrf-xyz");
    await api.GET("/api/tokens");
    await api.POST("/api/tokens", {
      body: { name: "laptop", expires_in_days: 30, scopes: ["read"] },
    });
    await api.DELETE("/api/tokens/{token_id}", { params: { path: { token_id: "abc" } } });
    expect(requests.map((r) => [r.method, r.headers.get("X-CSRF-Token")])).toEqual([
      ["GET", null],
      ["POST", "csrf-xyz"],
      ["DELETE", "csrf-xyz"],
    ]);
    expect(requests.every((r) => r.credentials === "same-origin")).toBe(true);
  });

  it("throws the API's error code and message", async () => {
    mockApi({
      "GET /api/tokens": () =>
        json({ error: { code: "forbidden", message: "nope", details: { a: 1 } } }, 403),
    });
    const error: unknown = await api.GET("/api/tokens").catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 403,
      code: "forbidden",
      message: "nope",
      details: { a: 1 },
    });
  });

  it("copes with an error that is not JSON", async () => {
    mockApi({ "GET /api/tokens": () => new Response("Bad gateway", { status: 502 }) });
    const error: unknown = await api.GET("/api/tokens").catch((e: unknown) => e);
    expect(error).toMatchObject({ status: 502, code: "http_502" });
  });
});

describe("a refused CSRF token", () => {
  afterEach(() => {
    setCsrfToken(null);
    setCsrfRefresher(null);
  });

  const refused = (code: string) => () =>
    json({ error: { code, message: "refused", details: null } }, 403);

  it("is refreshed and the request sent once more, with its body", async () => {
    const { requests } = mockApi({
      "POST /api/tokens": async (request) =>
        request.headers.get("X-CSRF-Token") === "fresh"
          ? json(await request.json(), 201)
          : refused("csrf_invalid")(),
    });
    setCsrfToken("stale");
    const refresher = vi.fn(() => {
      setCsrfToken("fresh");
      return Promise.resolve();
    });
    setCsrfRefresher(refresher);
    const body = { name: "laptop", expires_in_days: 30, scopes: ["read" as const] };
    const { data } = await api.POST("/api/tokens", { body });
    expect(data).toEqual(body);
    expect(refresher).toHaveBeenCalledTimes(1);
    expect(requests.map((r) => r.headers.get("X-CSRF-Token"))).toEqual(["stale", "fresh"]);
  });

  it("is not retried when the refresh brings no new token", async () => {
    const { requests } = mockApi({ "POST /api/tokens": refused("csrf_invalid") });
    setCsrfToken("stale");
    setCsrfRefresher(() => Promise.resolve());
    const error: unknown = await api
      .POST("/api/tokens", { body: { name: "x", expires_in_days: 1, scopes: ["read"] } })
      .catch((e: unknown) => e);
    expect(error).toMatchObject({ status: 403, code: "csrf_invalid" });
    expect(requests).toHaveLength(1);
  });

  it("does not cover other refusals", async () => {
    const { requests } = mockApi({ "POST /api/tokens": refused("forbidden") });
    const refresher = vi.fn(() => Promise.resolve());
    setCsrfRefresher(refresher);
    const error: unknown = await api
      .POST("/api/tokens", { body: { name: "x", expires_in_days: 1, scopes: ["read"] } })
      .catch((e: unknown) => e);
    expect(error).toMatchObject({ status: 403, code: "forbidden" });
    expect(refresher).not.toHaveBeenCalled();
    expect(requests).toHaveLength(1);
  });
});
