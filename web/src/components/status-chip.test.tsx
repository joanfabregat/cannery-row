import { render, screen } from "@testing-library/react";

import { statusDomains, type StatusDomain } from "@/lib/labels";

import { StatusChip } from "./status-chip";

describe("StatusChip", () => {
  it("shows the plain word and an icon", () => {
    render(<StatusChip domain="hypothesis" value="awaiting_human_review" />);
    const chip = screen.getByText("Needs review");
    expect(chip).toHaveAttribute("data-tone", "attention");
    const icon = chip.querySelector("svg");
    expect(icon).not.toBeNull();
    expect(icon).toHaveAttribute("aria-hidden", "true");
    expect(screen.queryByText("awaiting_human_review")).not.toBeInTheDocument();
  });

  it("has a word and an icon for every known status", () => {
    for (const [domain, values] of Object.entries(statusDomains)) {
      for (const [value, meta] of Object.entries(values)) {
        const { container, unmount } = render(
          <StatusChip domain={domain as StatusDomain} value={value} />,
        );
        const chip = container.querySelector("[data-slot=status-chip]");
        expect(chip).toHaveTextContent(meta.label);
        expect(chip?.querySelector("svg")).not.toBeNull();
        unmount();
      }
    }
  });

  it("keeps an unknown status readable", () => {
    render(<StatusChip domain="job" value="stuck_in_queue" />);
    expect(screen.getByText("Stuck in queue").querySelector("svg")).not.toBeNull();
  });
});
