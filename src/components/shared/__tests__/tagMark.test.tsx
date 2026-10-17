/**
 * Tag kinds: a label reads `#billable`, a person `@sam`, and the two system
 * tags render as marks — and every tag list orders labels, then people, then
 * system tags, so behaviour flags stop reading as part of the vocabulary.
 */
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { TagMark } from "@/components/shared/TagMark";
import { orderTagsByKind, tagKind } from "@/lib/format";

describe("tag kinds", () => {
  it("classifies labels, people and the system tags", () => {
    expect(tagKind("billable")).toBe("label");
    expect(tagKind("@sam")).toBe("person");
    expect(tagKind("decision")).toBe("system");
    expect(tagKind("Agent-Filed")).toBe("system");
  });

  it("orders labels, then people, then system tags, stably within each", () => {
    const names = ["agent-filed", "@dana", "legal", "decision", "@sam", "seo"];
    expect(orderTagsByKind(names.map((name) => ({ name }))).map((t) => t.name)).toEqual([
      "legal",
      "seo",
      "@dana",
      "@sam",
      "agent-filed",
      "decision",
    ]);
  });
});

describe("TagMark", () => {
  it("renders labels and people as words", () => {
    render(
      <>
        <TagMark name="billable" />
        <TagMark name="@sam" />
      </>
    );
    expect(screen.getByText("#billable")).toBeTruthy();
    expect(screen.getByText("@sam")).toBeTruthy();
  });

  it("renders the system tags as marks, never as #words", () => {
    const { container } = render(
      <>
        <TagMark name="decision" />
        <TagMark name="agent-filed" />
      </>
    );
    expect(screen.getByText("Decision")).toBeTruthy();
    expect(screen.getByLabelText("agent-filed")).toBeTruthy();
    expect(container.textContent).not.toContain("#");
    expect(container.querySelectorAll('[data-tag-kind="system"]').length).toBe(2);
  });

  it("keeps the decision mark's warning colour over the row's faint text", () => {
    render(<TagMark name="decision" className="cx-row-tags text-content-faint/70" />);
    const mark = screen.getByText("Decision");
    expect(mark.className).toContain("text-warning");
    expect(mark.className).not.toContain("text-content-faint");
    expect(mark.className).toContain("cx-row-tags");
  });
});
