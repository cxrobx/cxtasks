import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NotesField } from "@/components/tasks/NotesField";

vi.mock("@/lib/openUrl", () => ({ openUrl: vi.fn() }));

/**
 * jsdom lays nothing out, so `scrollHeight` is 0 — which is the "content fits"
 * case and would make every pin assertion pass vacuously. Same helper as
 * `runResult.test.tsx`, for the same reason.
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

/** The field as `TaskDetailForm` wires it: draft above, commit below. */
function Harness({
  initial = "",
  onCommit = () => {},
}: {
  initial?: string;
  onCommit?: (next: string) => void;
}) {
  const [body, setBody] = useState(initial);
  return (
    <NotesField
      value={body}
      stored={initial}
      onChange={setBody}
      onCommit={onCommit}
    />
  );
}

/** A click the pointer never moved during — i.e. not a resize drag. */
function clickStill(el: HTMLElement) {
  fireEvent.pointerDown(el, { clientX: 20, clientY: 20 });
  fireEvent.click(el, { clientX: 20, clientY: 20 });
}

describe("the Notes field", () => {
  it("renders markdown instead of printing its syntax", () => {
    const { container } = render(
      <Harness
        initial={["## What Apollo can handle", "", "| a | b |", "|---|---|", "| 1 | 2 |"].join(
          "\n"
        )}
      />
    );

    expect(screen.getByText("What Apollo can handle").tagName).toMatch(/^H\d$/);
    expect(container.querySelector("table")).not.toBeNull();
    expect(container.textContent).not.toContain("##");
    expect(container.querySelector("textarea")).toBeNull();
  });

  it("swaps in the editor on a click and back out on blur", () => {
    const { container } = render(<Harness initial="**bold**" />);
    clickStill(container.firstElementChild as HTMLElement);

    const textarea = container.querySelector("textarea") as HTMLTextAreaElement;
    // The editor shows the SOURCE — a renderer you cannot get back to the
    // markdown from is a one-way trip.
    expect(textarea.value).toBe("**bold**");

    fireEvent.blur(textarea);
    expect(container.querySelector("textarea")).toBeNull();
    expect(screen.getByText("bold").tagName).toBe("STRONG");
  });

  it("commits an edit when the editor closes", () => {
    const onCommit = vi.fn();
    const { container } = render(<Harness initial="before" onCommit={onCommit} />);
    clickStill(container.firstElementChild as HTMLElement);

    const textarea = container.querySelector("textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "after" } });
    fireEvent.blur(textarea);

    expect(onCommit).toHaveBeenCalledWith("after");
  });

  /**
   * The other half of gotcha #24's fix: a spurious write on every glance bumps
   * `updated_at`, whose effect resets every draft in the pane — discarding text
   * being typed in another field.
   */
  it("writes nothing when the editor is merely opened and closed", () => {
    const onCommit = vi.fn();
    const { container } = render(<Harness initial="unchanged" onCommit={onCommit} />);
    clickStill(container.firstElementChild as HTMLElement);
    fireEvent.blur(container.querySelector("textarea") as HTMLTextAreaElement);

    expect(onCommit).not.toHaveBeenCalled();
  });

  /** Escape is an exit, not a discard — see the comment on the handler. */
  it("keeps the edit when Escape closes the editor", () => {
    const onCommit = vi.fn();
    const { container } = render(<Harness initial="before" onCommit={onCommit} />);
    clickStill(container.firstElementChild as HTMLElement);

    const textarea = container.querySelector("textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "after" } });
    fireEvent.keyDown(textarea, { key: "Escape" });
    fireEvent.blur(textarea);

    expect(onCommit).toHaveBeenCalledWith("after");
  });

  it("does not open the editor when a resize drag ends inside the box", () => {
    const { container } = render(<Harness initial="a note" />);
    const box = container.firstElementChild as HTMLElement;

    // Grabbing the grip and dragging down: pointerup lands on the box, and the
    // click that follows would otherwise be read as "edit this".
    fireEvent.pointerDown(box, { clientX: 300, clientY: 200 });
    fireEvent.click(box, { clientX: 302, clientY: 340 });

    expect(container.querySelector("textarea")).toBeNull();
  });

  it("carries a resize grip and no max-height, which would clamp it shut", () => {
    withScrollHeight(1000);
    const { container } = render(<Harness initial="a long note" />);
    const box = container.firstElementChild as HTMLElement;

    expect(box.className).toContain("resize-y");
    expect(box.className).not.toMatch(/max-h-/);
  });

  it("pins a note that would overrun the default height", () => {
    withScrollHeight(1000);
    const { container } = render(<Harness initial="a long note" />);
    expect((container.firstElementChild as HTMLElement).style.height).toBe("260px");
  });

  /**
   * The floor is not the target: inside a scrolling pane a long note opens
   * down to the pane's visible bottom, less the room kept for the line under
   * it. jsdom lays nothing out, so the two rects are stubbed — the pane's
   * bottom at 900, the box's top at 200.
   */
  it("opens a long note down to the bottom of the scrolling pane", () => {
    withScrollHeight(5000);
    const rect = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement) {
        const scroller = this.style.overflowY === "auto";
        return { top: scroller ? 0 : 200, bottom: scroller ? 900 : 0 } as DOMRect;
      });
    const { container } = render(
      <div style={{ overflowY: "auto" }}>
        <Harness initial="a long note" />
      </div>
    );
    rect.mockRestore();

    const box = container.querySelector(".cx-notes") as HTMLElement;
    expect(box.style.height).toBe(`${900 - 200 - 48}px`);
  });

  it("never opens a long note below the floor, however little pane is left", () => {
    withScrollHeight(5000);
    const rect = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement) {
        const scroller = this.style.overflowY === "auto";
        return { top: scroller ? 0 : 700, bottom: scroller ? 800 : 0 } as DOMRect;
      });
    const { container } = render(
      <div style={{ overflowY: "auto" }}>
        <Harness initial="a long note" />
      </div>
    );
    rect.mockRestore();

    expect((container.querySelector(".cx-notes") as HTMLElement).style.height).toBe("260px");
  });

  it("leaves a short note at its own height", () => {
    withScrollHeight(48);
    const { container } = render(<Harness initial="short" />);
    expect((container.firstElementChild as HTMLElement).style.height).toBe("");
  });

  /**
   * The height belongs to the wrapper, which outlives the swap — so clicking
   * into a box you dragged taller doesn't snap it back to the default.
   */
  it("keeps a dragged height across the swap into the editor", () => {
    withScrollHeight(1000);
    const { container } = render(<Harness initial="a long note" />);
    const box = container.firstElementChild as HTMLElement;

    box.style.height = "600px"; // what the UA resizer writes
    clickStill(box);

    expect(box.style.height).toBe("600px");
    expect(container.querySelector("textarea")).not.toBeNull();
  });

  it("invites a first note when there is none", () => {
    render(<Harness initial="" />);
    expect(screen.getByText("Add notes…")).toBeTruthy();
  });
});

describe("clipboard images in Notes", () => {
  it("checks the native clipboard when Cmd+V produces no WebKit paste event", async () => {
    const paste = vi.fn();
    const { container } = render(
      <NotesField value="" stored="" onChange={vi.fn()} onCommit={vi.fn()} onPasteImage={paste} />
    );
    fireEvent.click(container.querySelector(".cx-notes")!);
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "v", metaKey: true });
    await waitFor(() => expect(paste).toHaveBeenCalledWith(true));
  });

  it("checks a paste with empty image metadata while Notes is in preview", () => {
    const paste = vi.fn();
    const { container } = render(
      <NotesField value="Existing note" stored="Existing note" onChange={vi.fn()} onCommit={vi.fn()} onPasteImage={paste} />
    );
    const preview = container.querySelector(".cx-notes")!;
    expect(preview).toHaveAttribute("tabindex", "0");
    fireEvent.paste(preview, { clipboardData: { items: [] } });
    expect(paste).toHaveBeenCalledWith(true);
  });

  it("attaches an image paste without inserting clipboard text into the draft", () => {
    const paste = vi.fn();
    const change = vi.fn();
    const { container } = render(
      <NotesField value="Existing note" stored="Existing note" onChange={change} onCommit={vi.fn()} onPasteImage={paste} />
    );
    fireEvent.click(container.querySelector(".cx-notes")!);
    const allowed = fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: { items: [{ type: "image/png" }, { type: "text/plain" }] },
    });
    expect(allowed).toBe(false);
    expect(paste).toHaveBeenCalledOnce();
    expect(change).not.toHaveBeenCalled();
  });

  it("leaves ordinary text paste to the editor", () => {
    const paste = vi.fn();
    const { container } = render(
      <NotesField value="" stored="" onChange={vi.fn()} onCommit={vi.fn()} onPasteImage={paste} />
    );
    fireEvent.click(container.querySelector(".cx-notes")!);
    expect(fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: { items: [{ type: "text/plain" }] },
    })).toBe(true);
    expect(paste).not.toHaveBeenCalled();
  });
});
