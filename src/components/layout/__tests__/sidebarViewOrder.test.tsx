/**
 * Dragging a row onto the MIDDLE of an area nests it there.
 *
 * The edges of a row still reorder — and move the dragged row to THAT row's
 * level, which is how something comes back out to the top. The middle half of
 * an area's row puts the dragged node inside it, as what it already is: areas
 * nest to any depth and carry everything under them (2026-09-27; before that
 * an area was demoted to a project on the way in, and one holding projects
 * could not go in at all). Nothing may land inside its own subtree.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createEvent, fireEvent, render, screen } from "@testing-library/react";

import { Sidebar } from "@/components/layout/Sidebar";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Project } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: {
    tags: { usage: vi.fn(async () => 0) },
    budget: { status: vi.fn(async () => null) },
    runner: { isInstalled: vi.fn(async () => false) },
    runs: { recent: vi.fn(async () => []) },
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

function node(
  name: string,
  overrides: Partial<Project> & { kind: Project["kind"] }
): Project {
  return {
    id: `p-${name}`,
    name,
    color: null,
    icon: null,
    sort_order: 0,
    archived: false,
    open_count: 1,
    health: null,
    parent: null,
    target_date: null,
    closed_at: null,
    ...overrides,
  };
}

const RECORDS = [
  node("cxventures", { kind: "area" }),
  node("gov contracting", { kind: "project", parent: "cxventures" }),
  node("pocketbuddy", { kind: "area" }),
  node("products", { kind: "area", open_count: 0 }),
  node("chris x", { kind: "area" }),
];

const setProjectKind = vi.fn(async () => null as string | null);
const setProjectParent = vi.fn(async () => null as string | null);
const reorderProjects = vi.fn(async () => {});

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useTaskStore.setState({
    projectRecords: RECORDS,
    activeProject: null,
    allTags: [],
    activeTags: [],
    view: "today",
    query: "",
    filters: [],
    setProject: vi.fn(async () => {}),
    setProjectKind,
    setProjectParent,
    reorderProjects,
  } as never);
  useUiStore.setState({ areasCollapsed: false, collapsedAreas: ["p-products"] });
});
const viewButton = (name: string) =>
  screen.getByRole("button", { name: new RegExp(`^${name}`) });

describe("view drag", () => {
  it("moves Done above Inbox", () => {
    render(<Sidebar />);
    const dataTransfer = { setData: vi.fn(), effectAllowed: "" };
    fireEvent.dragStart(viewButton("Done"), { dataTransfer });
    const li = viewButton("Inbox").closest("li")!;
    li.getBoundingClientRect = () =>
      ({ top: 100, height: 40, bottom: 140, left: 0, right: 200, width: 200 }) as DOMRect;
    const over = createEvent.dragOver(li, { dataTransfer });
    Object.defineProperty(over, "clientY", { value: 105 });
    fireEvent(li, over);
    expect(li).toHaveClass("cx-drop-above");
    fireEvent.drop(li);
    expect(useUiStore.getState().viewOrder.slice(0, 3)).toEqual(["gsd", "done", "inbox"]);
  });
});
