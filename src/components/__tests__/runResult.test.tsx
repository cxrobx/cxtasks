import { afterEach, describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { RunResult } from "@/components/tasks/RunResult";

/**
 * jsdom lays nothing out, so `scrollHeight` is always 0 — which is precisely
 * the "content fits" case and would make every pin assertion pass vacuously.
 * Each test states the content height it is describing.
 */
function withScrollHeight(px: number) {
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
    configurable: true,
    get: () => px,
  });
}

afterEach(() => {
  Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
});

describe("RunResult", () => {
  it("carries a resize grip and no max-height, which would clamp it shut", () => {
    withScrollHeight(1000);
    const { container } = render(<RunResult>a long handoff</RunResult>);
    const box = container.firstElementChild as HTMLElement;

    expect(box.className).toContain("resize-y");
    // The resizer writes an inline height; a max-height would clamp it, so the
    // grip would drag and grow nothing. The cap is applied by pinning instead.
    expect(box.className).not.toMatch(/max-h-/);
  });

  it("pins a result that would overrun the default height", () => {
    withScrollHeight(1000);
    const { container } = render(<RunResult>a long handoff</RunResult>);
    expect((container.firstElementChild as HTMLElement).style.height).toBe("224px");
  });

  it("leaves a short result at its own height", () => {
    withScrollHeight(48);
    const { container } = render(<RunResult>done</RunResult>);
    expect((container.firstElementChild as HTMLElement).style.height).toBe("");
  });

  it("does not snap a dragged box back when the result re-renders", () => {
    withScrollHeight(1000);
    const { container, rerender } = render(<RunResult>a long handoff</RunResult>);
    const box = container.firstElementChild as HTMLElement;

    // What the UA resizer does when the user drags the grip.
    box.style.height = "600px";
    rerender(<RunResult>a long handoff, now finished</RunResult>);

    expect(box.style.height).toBe("600px");
  });

  it("leads with unfinished delivery and presents evidence without machine JSON", () => {
    const { container } = render(<RunResult>{'Prepared the report.\nCXTASKS_RESULT: {"status":"needs_action","verified":["Reviewed the saved report"],"remaining":["Send it using the requested account"]}'}</RunResult>);
    expect(screen.getByText("Still needed")).toBeInTheDocument();
    expect(screen.getByText("Send it using the requested account")).toBeInTheDocument();
    expect(screen.getByText("Evidence reported")).toBeInTheDocument();
    expect(container.textContent).not.toContain("CXTASKS_RESULT:");
    expect(container.textContent!.indexOf("Still needed")).toBeLessThan(container.textContent!.indexOf("Prepared the report"));
  });

  it("keeps existing handoffs readable and hides malformed protocol footers", () => {
    const { container, rerender } = render(<RunResult>Existing research handoff.</RunResult>);
    expect(container).toHaveTextContent("Existing research handoff.");
    rerender(<RunResult>{'Needs verification: invalid report.\nCXTASKS_RESULT: broken'}</RunResult>);
    expect(container).toHaveTextContent("Needs verification: invalid report.");
    expect(container.textContent).not.toContain("CXTASKS_RESULT:");
  });
});
