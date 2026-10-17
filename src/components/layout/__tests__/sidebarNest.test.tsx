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
import { createEvent, fireEvent, render, screen, waitFor } from "@testing-library/react";

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
  useUiStore.setState({ areasCollapsed: false, collapsedAreas: ["p-products"], showHidden: true });
});

const rowButton = (name: string) =>
  screen.getByRole("button", { name: new RegExp(`^${name}`) });

/** Drag `from` onto `to`, releasing at `frac` of the target row's height. */
function drag(from: string, to: string, frac: number) {
  render(<Sidebar />);
  const dataTransfer = { setData: vi.fn(), effectAllowed: "" };
  fireEvent.dragStart(rowButton(from), { dataTransfer });
  const li = rowButton(to).closest("li")!;
  li.getBoundingClientRect = () =>
    ({ top: 100, height: 40, bottom: 140, left: 0, right: 200, width: 200 }) as DOMRect;
  // jsdom has no DragEvent, so an init's clientY is silently dropped — set it
  // on the event itself.
  const over = createEvent.dragOver(li, { dataTransfer });
  Object.defineProperty(over, "clientY", { value: 100 + 40 * frac });
  fireEvent(li, over);
  return li;
}

describe("dropping onto an area", () => {
  it("outlines the area and nests an area dropped in its middle, as an area", async () => {
    const li = drag("pocketbuddy", "products", 0.5);
    expect(li).toHaveClass("cx-drop-into");

    fireEvent.drop(li);

    await waitFor(() =>
      expect(setProjectParent).toHaveBeenCalledWith("pocketbuddy", "products")
    );
    // Kept an area — no demotion on the way in any more.
    expect(setProjectKind).not.toHaveBeenCalled();
    expect(reorderProjects).not.toHaveBeenCalled();
  });

  it("nests a project without touching its kind, and opens the area", async () => {
    const li = drag("gov contracting", "products", 0.5);
    expect(li).toHaveClass("cx-drop-into");
    fireEvent.drop(li);

    await waitFor(() =>
      expect(setProjectParent).toHaveBeenCalledWith("gov contracting", "products")
    );
    expect(setProjectKind).not.toHaveBeenCalled();
    // Products was collapsed; the drop opens it so the row you moved is seen
    // landing rather than looking deleted.
    await waitFor(() =>
      expect(useUiStore.getState().collapsedAreas).not.toContain("p-products")
    );
  });

  it("still reorders from the row's edges", () => {
    const li = drag("chris x", "products", 0.1);
    expect(li).toHaveClass("cx-drop-above");
    expect(li).not.toHaveClass("cx-drop-into");

    fireEvent.drop(li);

    expect(reorderProjects).toHaveBeenCalled();
    expect(setProjectParent).not.toHaveBeenCalled();
  });

  it("nests an area that holds projects, taking them with it", async () => {
    // The Artist Advisory → Products move this change exists for.
    const li = drag("cxventures", "products", 0.5);
    expect(li).toHaveClass("cx-drop-into");
    fireEvent.drop(li);
    await waitFor(() =>
      expect(setProjectParent).toHaveBeenCalledWith("cxventures", "products")
    );
  });

  it("offers nothing inside the dragged row's own subtree", () => {
    // gov contracting is inside cxventures: every zone there would be a cycle.
    const li = drag("cxventures", "gov contracting", 0.5);
    expect(li).not.toHaveClass("cx-drop-into");
    expect(li).not.toHaveClass("cx-drop-above");
    expect(li).not.toHaveClass("cx-drop-below");
  });

  it("moves a row to the level it is dropped between — pulling it back out", async () => {
    // gov contracting sits in cxventures; dropped at chris x's top edge it
    // becomes top-level, then takes its place in the order.
    const li = drag("gov contracting", "chris x", 0.1);
    expect(li).toHaveClass("cx-drop-above");
    fireEvent.drop(li);
    await waitFor(() => expect(setProjectParent).toHaveBeenCalledWith("gov contracting", null));
    await waitFor(() => expect(reorderProjects).toHaveBeenCalled());
    expect(setProjectParent.mock.invocationCallOrder[0]).toBeLessThan(
      reorderProjects.mock.invocationCallOrder[0]
    );
  });

  it("shows a refused nest and changes nothing else", async () => {
    setProjectParent.mockResolvedValueOnce("refused");
    const li = drag("pocketbuddy", "products", 0.5);
    fireEvent.drop(li);

    await waitFor(() => expect(screen.getByText("refused")).toBeInTheDocument());
    expect(setProjectKind).not.toHaveBeenCalled();
  });
});

describe("an area inside an area", () => {
  it("is drawn once, one step in, with its own projects a step further", () => {
    useTaskStore.setState({
      projectRecords: [
        node("products", { kind: "area" }),
        node("artist advisory", { kind: "area", parent: "products" }),
        node("retitle", { kind: "project", parent: "artist advisory" }),
      ],
    } as never);
    useUiStore.setState({ collapsedAreas: [] });
    render(<Sidebar />);
    expect(screen.getAllByRole("button", { name: /^artist advisory/ })).toHaveLength(1);
    const indent = (name: string) =>
      (rowButton(name).closest("[style]") as HTMLElement | null)?.style.marginLeft ?? "0px";
    expect(indent("products")).toBe("0px");
    expect(indent("artist advisory")).toBe("16px");
    expect(indent("retitle")).toBe("32px");
  });

  it("stays on the path to where you are when its parent is collapsed", () => {
    useTaskStore.setState({
      projectRecords: [
        node("products", { kind: "area" }),
        node("artist advisory", { kind: "area", parent: "products" }),
        node("retitle", { kind: "project", parent: "artist advisory" }),
        node("pocketbuddy", { kind: "project", parent: "products" }),
      ],
      activeProject: "retitle",
    } as never);
    useUiStore.setState({ collapsedAreas: ["p-products", "p-artist advisory"] });
    render(<Sidebar />);
    // The row you are in, and the area between it and the top, stay drawn;
    // the sibling you are not in goes away with the collapse.
    expect(screen.getByRole("button", { name: /^retitle/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^artist advisory/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^pocketbuddy/ })).toBeNull();
  });
});
