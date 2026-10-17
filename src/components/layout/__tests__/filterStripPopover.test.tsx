/**
 * Dismissing the filter strip's facet popover.
 *
 * The bug this pins is invisible in isolation and invisible in jsdom unless you
 * bring the adversary with you: Tauri injects a BUBBLE-phase `mousedown`
 * listener on `document` that calls `stopImmediatePropagation()` for any click
 * inside a `data-tauri-drag-region` (gotcha #15). The header the filter bar sits
 * ABOVE is one, so a bubble-phase click-outside listener on `window` never saw
 * clicks on the app's top chrome — the popover only closed when you clicked
 * lower down the window. The strip itself is not a drag region, but it opens
 * DOWNWARD from a row directly under one, so every dismissing click still
 * has to cross it.
 *
 * So `dragRegion()` below is not scaffolding; it IS the test. Drop it and this
 * file passes against the broken version, which is the same trap
 * gotcha #12's tight-loop comment describes.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { FilterStrip } from "@/components/layout/FilterStrip";
import { useTaskStore } from "@/stores/taskStore";

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      get: vi.fn(async () => null),
      list: vi.fn(async () => []),
      search: vi.fn(async () => []),
      filter: vi.fn(async () => []),
      subtasks: vi.fn(async () => []),
      projects: vi.fn(async () => []),
      counts: vi.fn(async () => ({ inbox: 0, today: 0, upcoming: 0, in_review: 0 })),
      countdowns: vi.fn(async () => ({})),
    },
    runs: { list: vi.fn(async () => []) },
    tags: {
      list: vi.fn(async () => []),
      forTask: vi.fn(async () => []),
      byTask: vi.fn(async () => ({})),
    },
    deps: {
      blockedIds: vi.fn(async () => []),
      forTask: vi.fn(async () => ({ blocked_by: [], blocking: [], is_blocked: false })),
    },
    comments: { list: vi.fn(async () => []) },
    attachments: { list: vi.fn(async () => []) },
    sections: { list: vi.fn(async () => []) },
    projects: {
      list: vi.fn(async () => []),
      board: vi.fn(async () => []),
      repos: { list: vi.fn(async () => []) },
    },
  },
}));

/**
 * A faithful stand-in for Tauri's injected `drag.js`: bubble phase, on
 * `document`, swallowing the event outright for anything inside a drag region.
 * It also `preventDefault()`s, which is what leaves the caret in the search
 * field after the click — the reason reopening can't be left to `onFocus`.
 */
function installTauriDragScript(): () => void {
  const onDown = (e: MouseEvent) => {
    const el = e.target as HTMLElement | null;
    if (el?.closest("[data-tauri-drag-region]")) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  };
  document.addEventListener("mousedown", onDown);
  return () => document.removeEventListener("mousedown", onDown);
}

function renderStrip() {
  const view = render(
    <div>
      {/* The real header carries `deep` and sits directly above the strip —
          see AppLayout. */}
      <header data-tauri-drag-region="deep">
        <h1>Today</h1>
      </header>
      <FilterStrip />
      <main>
        <button type="button">a task row</button>
      </main>
    </div>
  );
  return { ...view, trigger: screen.getByRole("button", { name: "Filter" }) };
}

/** The pane opens on Status, so its own values are the tell. */
const popoverOpen = () => screen.queryByText("In review") !== null;

let uninstall: () => void;

beforeEach(() => {
  uninstall = installTauriDragScript();
  useTaskStore.setState({
    query: "",
    filters: [],
    allTags: [],
    projectRecords: [],
    view: "today",
    activeProject: null,
  });
});

afterEach(() => uninstall());

describe("the facet popover", () => {
  it("closes on a click in the header, which is a window drag region", () => {
    const { trigger } = renderStrip();
    fireEvent.click(trigger);
    expect(popoverOpen()).toBe(true);

    // The exact click that did nothing: the heading, inside the drag region,
    // two inches from the popover.
    fireEvent.mouseDown(screen.getByText("Today"));
    expect(popoverOpen()).toBe(false);
  });

  it("closes on a click anywhere else in the window", () => {
    const { trigger } = renderStrip();
    fireEvent.click(trigger);
    fireEvent.mouseDown(screen.getByText("a task row"));
    expect(popoverOpen()).toBe(false);
  });

  it("stays open for a click inside itself", () => {
    const { trigger } = renderStrip();
    fireEvent.click(trigger);
    fireEvent.mouseDown(screen.getByText("In review"));
    expect(popoverOpen()).toBe(true);
  });

  it("reopens on the next click on the trigger", () => {
    const { trigger } = renderStrip();
    fireEvent.click(trigger);
    fireEvent.mouseDown(screen.getByText("Today"));
    expect(popoverOpen()).toBe(false);

    fireEvent.click(trigger);
    expect(popoverOpen()).toBe(true);
  });

  /** The operator toggle is what makes a negative filter reachable at all —
   * `is not` has no other entry point in the UI. */
  it("offers is / is not on a value pane", () => {
    const { trigger } = renderStrip();
    fireEvent.click(trigger);
    expect(screen.getByRole("button", { name: "is not" })).toBeTruthy();
  });
});
