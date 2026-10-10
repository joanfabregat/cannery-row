import { screen, waitFor, within } from "@testing-library/react";

import { attempt, attention, message, page, unit, unitApi } from "@/test/fixtures";
import { json, project, renderApp, signedIn } from "@/test/render";

const QUESTION = "00000000-0000-4000-8000-0000000000f1";
const BLOCKING = "00000000-0000-4000-8000-0000000000f2";
const NOTE = "00000000-0000-4000-8000-0000000000f3";
const BASE = "/api/projects/sardines/units/12";

describe("questions on Home", () => {
  it("lists the open questions, blocking ones first, and answers one", async () => {
    const answered: Request[] = [];
    const { requests } = signedIn(
      {},
      {
        "GET /api/projects/sardines/attention": () => json(attention()),
        "GET /api/projects/sardines/concerns": () => json(page([])),
        "GET /api/projects/sardines/questions": () =>
          json(
            page([
              message({ id: QUESTION }),
              message({
                id: BLOCKING,
                blocking: true,
                default: null,
                body: "The test split is missing. Wait for it?",
              }),
            ]),
          ),
        [`POST /api/projects/sardines/questions/${BLOCKING}/answer`]: (request) => {
          answered.push(request.clone());
          return json(message({ id: BLOCKING, state: "answered" }));
        },
      },
    );
    const { user } = renderApp("/");
    const queue = await screen.findByRole("region", { name: "Questions" });
    const list = await within(queue).findByRole("list", { name: "Open questions" });
    const items = within(list).getAllByRole("listitem");
    expect(items[0]).toHaveTextContent("Blocking question");
    expect(items[0]).toHaveTextContent("The test split is missing. Wait for it?");
    expect(items[1]).toHaveTextContent("Proceeding meanwhile on: Keep the seed fixed.");
    expect(within(items[0] as HTMLElement).getByRole("link", { name: "#12.1" })).toHaveAttribute(
      "href",
      "/units/12/attempts/1",
    );
    const listed = requests
      .map((r) => new URL(r.url))
      .find((url) => url.pathname === "/api/projects/sardines/questions");
    expect(listed?.searchParams.get("state")).toBe("open");

    await user.click(within(items[0] as HTMLElement).getByRole("button", { name: "Answer" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(
      within(dialog).getByRole("textbox", { name: "Your answer" }),
      "Use 10% of train.",
    );
    await user.click(within(dialog).getByRole("button", { name: "Send the answer" }));
    await waitFor(() => {
      expect(answered).toHaveLength(1);
    });
    expect(await answered[0]?.json()).toEqual({ body: "Use 10% of train." });
  });

  it("does not show the question queue to members", async () => {
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      { "GET /api/projects/sardines/attention": () => json(attention()) },
    );
    renderApp("/");
    expect(await screen.findByRole("region", { name: "Recent outcomes" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Questions" })).toBeNull();
  });
});

describe("questions on the unit page", () => {
  it("shows the answers and escalates an open question into a concern", async () => {
    const escalated: Request[] = [];
    const h = unit({ state: "active" });
    signedIn(
      {},
      {
        ...unitApi(h, { attempts: [attempt({ state: "running", finished_at: null })] }),
        [`GET ${BASE}/messages`]: () =>
          json(
            page([
              message({ id: QUESTION }),
              message({
                id: BLOCKING,
                blocking: true,
                default: null,
                state: "answered",
                body: "Which tokenizer?",
                answer: {
                  id: "00000000-0000-4000-8000-0000000000f4",
                  body: "The byte-level one.",
                  author: "00000000-0000-4000-8000-0000000000aa",
                  author_name: "Grace Hopper",
                  created_at: "2026-03-02T12:00:00Z",
                  acknowledged_at: "2026-03-02T12:01:00Z",
                },
              }),
            ]),
          ),
        [`POST /api/projects/sardines/questions/${QUESTION}/escalation`]: (request) => {
          escalated.push(request.clone());
          return json(message({ id: QUESTION, state: "escalated" }));
        },
      },
    );
    const { user } = renderApp("/units/12");
    const section = await screen.findByRole("region", { name: "Questions" });
    const open = within(section).getByRole("list", { name: "Open questions" });
    expect(within(open).getByText(/change the learning rate/)).toBeInTheDocument();
    expect(within(section).getByText("Answered questions (1)")).toBeInTheDocument();
    expect(within(section).getByText("The byte-level one.")).toBeInTheDocument();
    expect(within(section).getByText(/read by the performer/)).toBeInTheDocument();

    await user.click(within(open).getByRole("button", { name: "Raise as a concern" }));
    const dialog = await screen.findByRole("dialog");
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Kind" }), "blocker");
    await user.type(within(dialog).getByRole("textbox", { name: "Note" }), "The plan never says.");
    await user.click(within(dialog).getByRole("button", { name: "Raise the concern" }));
    await waitFor(() => {
      expect(escalated).toHaveLength(1);
    });
    expect(await escalated[0]?.json()).toEqual({ kind: "blocker", note: "The plan never says." });
  });

  it("lets a member read the questions but not answer them", async () => {
    const h = unit({ state: "active" });
    signedIn(
      { projects: [project("sardines", "Sardines", "member")] },
      {
        ...unitApi(h, { attempts: [attempt({ state: "running", finished_at: null })] }),
        [`GET ${BASE}/messages`]: () => json(page([message({ id: QUESTION })])),
      },
    );
    renderApp("/units/12");
    const section = await screen.findByRole("region", { name: "Questions" });
    expect(within(section).getByText(/change the learning rate/)).toBeInTheDocument();
    expect(within(section).queryByRole("button", { name: "Answer" })).toBeNull();
  });
});

describe("steering an attempt", () => {
  it("marks the unacknowledged note and posts a new one", async () => {
    const posted: Request[] = [];
    const h = unit({ state: "active" });
    signedIn(
      {},
      {
        ...unitApi(h, { attempts: [attempt({ state: "running", finished_at: null })] }),
        [`GET ${BASE}/attempts/1/steering`]: () =>
          json({
            items: [
              message({
                id: NOTE,
                kind: "steer",
                blocking: null,
                default: null,
                state: null,
                body: "Log the seed in every run.",
                author_kind: "user",
                author_name: "Grace Hopper",
                via_channel: "ui",
                via_client: null,
              }),
            ],
          }),
        [`POST ${BASE}/attempts/1/steering`]: (request) => {
          posted.push(request.clone());
          return json(message({ kind: "steer" }), 201);
        },
      },
    );
    const { user } = renderApp("/units/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Steering" });
    const notes = await within(section).findByRole("list", { name: "Steering notes" });
    expect(within(notes).getByText("Log the seed in every run.")).toBeInTheDocument();
    expect(within(notes).getByText("not acknowledged yet")).toBeInTheDocument();
    expect(within(section).getByRole("link", { name: "Open the transcript" })).toHaveAttribute(
      "href",
      "/units/12/attempts/1/transcript",
    );
    await user.type(
      within(section).getByRole("textbox", { name: "Steering note" }),
      "Stop after the baseline.",
    );
    await user.click(within(section).getByRole("button", { name: "Send the note" }));
    await waitFor(() => {
      expect(posted).toHaveLength(1);
    });
    expect(await posted[0]?.json()).toEqual({ body: "Stop after the baseline." });
  });

  it("offers no steering once the attempt is over", async () => {
    const h = unit({ state: "promoted" });
    signedIn({}, unitApi(h, { attempts: [attempt()] }));
    renderApp("/units/12/attempts/1");
    const section = await screen.findByRole("region", { name: "Steering" });
    expect(await within(section).findByText("No steering note was posted.")).toBeInTheDocument();
    expect(within(section).queryByRole("textbox", { name: "Steering note" })).toBeNull();
  });
});

describe("the transcript timeline", () => {
  it("collapses tool calls, highlights the conversation and adds unrecorded messages", async () => {
    const h = unit({ state: "active" });
    const { requests } = signedIn(
      {},
      {
        ...unitApi(h, { attempts: [attempt({ state: "running", finished_at: null })] }),
        [`GET ${BASE}/messages`]: () =>
          json(
            page([
              message({ id: QUESTION, created_at: "2026-03-02T10:00:05Z" }),
              message({
                id: NOTE,
                kind: "steer",
                body: "Log the seed in every run.",
                author_name: "Grace Hopper",
                created_at: "2026-03-02T10:00:01Z",
              }),
            ]),
          ),
        [`GET ${BASE}/attempts/1/transcript`]: () =>
          json({
            unit: 12,
            attempt: 1,
            events: [
              {
                index: 0,
                event: { ts: "2026-03-02T10:00:00Z", kind: "assistant", content: "Reading." },
              },
              {
                index: 1,
                event: {
                  ts: "2026-03-02T10:00:02Z",
                  kind: "tool_call",
                  tool: "get_brief",
                  content: { project: "sardines" },
                },
              },
              {
                index: 2,
                event: {
                  ts: "2026-03-02T10:00:03Z",
                  kind: "steer",
                  message: NOTE,
                  content: "Read: log the seed.",
                },
              },
            ],
            next_after: null,
            total_events: 3,
            bytes: 300,
            sealed: false,
            artifact: null,
          }),
      },
    );
    renderApp("/units/12/attempts/1/transcript");
    const timeline = await screen.findByRole("list", { name: "Transcript" });
    const items = within(timeline).getAllByRole("listitem");
    expect(items.map((item) => item.dataset.kind)).toEqual([
      "assistant",
      "tool_call",
      "steer",
      "question",
    ]);
    const details = (items[1] as HTMLElement).querySelector("details");
    expect(details?.open).toBe(false);
    expect(items[1]).toHaveTextContent("get_brief");
    expect(items[2]).toHaveTextContent("Read: log the seed.");
    expect(items[3]).toHaveTextContent("not in the transcript");
    expect(screen.getByText(/Live: 3 events so far/)).toBeInTheDocument();
    const read = requests
      .map((r) => new URL(r.url))
      .find((url) => url.pathname === `${BASE}/attempts/1/transcript`);
    expect(read?.searchParams.get("limit")).toBe("1000");
  });
});
