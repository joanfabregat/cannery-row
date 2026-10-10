import { humanize, label, statusDomains, statusLabel, statusMeta } from "./labels";

describe("plain-language labels", () => {
  it("names the states the spec calls out", () => {
    expect(statusLabel("hypothesis", "awaiting_human_review")).toBe("Needs review");
    expect(statusLabel("attempt", "awaiting_human_review")).toBe("Needs review");
    expect(statusLabel("authority", "agent_claim")).toBe("Reported by agent");
    expect(statusLabel("authority", "tester_verified")).toBe("Verified");
    expect(statusLabel("authority", "imported_artifact")).toBe("Imported from a run file");
    expect(statusLabel("authority", "imported_transcribed")).toBe("Imported from a document");
  });

  it("covers every hypothesis and attempt state of the lifecycle", () => {
    const hypothesis = [
      "queued",
      "active",
      "awaiting_human_review",
      "promoted",
      "rejected",
      "inconclusive",
      "failed",
      "cancelled",
    ];
    const attempt = [
      "claimed",
      "running",
      "submitted",
      "testing",
      "evaluating",
      "awaiting_human_review",
      "promoted",
      "rejected",
      "inconclusive",
      "failed",
      "cancelled",
      "unreviewed",
    ];
    expect(Object.keys(statusDomains.hypothesis).sort()).toEqual([...hypothesis].sort());
    expect(Object.keys(statusDomains.attempt).sort()).toEqual([...attempt].sort());
  });

  it("never shows an internal name raw", () => {
    for (const domain of Object.values(statusDomains)) {
      for (const [value, meta] of Object.entries(domain)) {
        expect(meta.label).not.toContain("_");
        if (value.includes("_")) expect(meta.label).not.toBe(value);
      }
    }
  });

  it("falls back to readable words for an unknown value", () => {
    expect(statusMeta("hypothesis", "some_new_state")).toEqual({
      label: "Some new state",
      tone: "neutral",
      icon: "inconclusive",
    });
    expect(humanize("")).toBe("Unknown");
    expect(statusLabel("attempt", "toString")).toBe("Tostring");
  });

  it("labels roles, decisions and channels", () => {
    expect(label("role", "researcher")).toBe("Researcher");
    expect(label("decision", "close_failed")).toBe("Close as failed");
    expect(label("reviewKind", "failure")).toBe("Failure review");
    expect(label("channel", "mcp")).toBe("Agent (MCP)");
    expect(label("channel", "carrier_pigeon")).toBe("Carrier pigeon");
  });
});
