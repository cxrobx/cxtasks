/**
 * What a drag carries when rows are selected.
 *
 * The board has two gestures that both start with pressing a row: build a set
 * (⌘/⇧-click) and drag it somewhere. They used to disagree — the drag read
 * `dragId` alone, so filing six selected tasks into a section moved one and
 * left the other five where they were, with the selection still lit as if it
 * had all worked. That is the failure these pin: not an error, a silent
 * shortfall you only notice later, going through the section that should have
 * held them.
 *
 * The other half is the boundary. A drag that starts OUTSIDE the set must not
 * be adopted by it, or there is no way to move one task without first tearing
 * down a selection you still want.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { TaskList } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Project, Section, Task } from "@/types/task";

const update = vi.fn(async (id: string, patch: Record<string, unknown>) => ({
  ...task(id),
  ...patch,
}));
const moveTask = vi.fn(async (_id: string, _sectionId: string | null) => {});

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      complete: vi.fn(),
      update: (id: string, patch: Record<string, unknown>) => update(id, patch),
      trash: vi.fn(),
    },
    sections: { moveTask: (id: string, s: string | null) => moveTask(id, s) },
    menu: { showForTask: vi.fn() },
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

function task(id: string, overrides: Partial<Task> = {}): Task {
  return {
    id,
    title: `task ${id}`,
    body: null,
    status: "todo",
    blocked_reason: null,
    priority: 3,
    project: null,
    due_at: null,
    defer_until: null,
    recurrence: null,
    parent_id: null,
    sort_order: 0,
    created_at: "2026-08-01 10:00:00",
    updated_at: "2026-08-01 10:00:00",
    completed_at: null,
    repo_path: null,
    prompt: null,
    bg_allowed: false,
    bg_executor: "claude",
    bg_writes_allowed: false,
    bg_shell_allowed: false,
    reminded_at: null,
    model: null,
    section_id: null,
    deleted_at: null,
    archived_at: null,
    short_id: null,
    ...overrides,
  } as Task;
}

function section(id: string, name: string): Section {
  return {
    id,
    name,
    project: null,
    sort_order: 0,
    target_date: null,
    completed_at: null,
  };
}

function project(name: string, sortOrder: number): Project {
  return {
    id: `p-${name}`,
    name,
    color: null,
    icon: null,
    sort_order: sortOrder,
    archived: false,
    open_count: 0,
    health: null,
    kind: "area" as const,
    parent: null,
    target_date: null,
    closed_at: null,
  };
}

/** jsdom ships no DataTransfer and `onDragStart` calls `setData` on it, so
 * without this the drag throws before any drop wiring is reached. */
const DT = { dataTransfer: { setData: vi.fn(), effectAllowed: "" } };

const row = (id: string) => screen.getByText(`task ${id}`).closest("[role=option]")!;
const header = (name: string) => screen.getByRole("button", { name });

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({
    tasks: [],
    sections: [],
    projectRecords: [],
    checkedIds: [],
    lastCheckedId: null,
    countdowns: {},
    blockedIds: [],
    flashIds: [],
    selectedId: null,
    undoStack: [],
    loading: false,
    view: "today",
    activeProject: null,
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
  } as never);
  useUiStore.setState({ groupBy: "section" });
});

/** Three loose tasks and one section to drop them into. */
function threeLooseTasks() {
  useTaskStore.setState({
    sections: [section("s1", "phase 1")],
    tasks: [task("a"), task("b"), task("c")],
  } as never);
}

describe("dragging a row that is part of the selection", () => {
  it("files the WHOLE set into the section, once each", async () => {
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(moveTask).toHaveBeenCalledTimes(2));
    expect(moveTask.mock.calls.map((c) => c[0]).sort()).toEqual(["a", "b"]);
    expect(moveTask).toHaveBeenCalledWith("a", "s1");
  });

  it("carries the set onto a project group too", async () => {
    // Both groupings are one descriptor, so a drop that moved a set into a
    // section and a single row into a project would be one gesture meaning two
    // different things depending on a toggle two panes away.
    useUiStore.setState({ groupBy: "project" });
    useTaskStore.setState({
      tasks: [task("a"), task("b"), task("c", { project: "cxtasks" })],
      projectRecords: [project("cxtasks", 0)],
      checkedIds: ["a", "b"],
    } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);
    fireEvent.drop(header("cxtasks"), DT);

    await waitFor(() => expect(update).toHaveBeenCalledTimes(2));
    expect(update).toHaveBeenCalledWith("a", { project: "cxtasks" });
    expect(update).toHaveBeenCalledWith("b", { project: "cxtasks" });
  });

  it("takes the whole set back OUT of its section via Ungrouped", async () => {
    useTaskStore.setState({
      sections: [section("s1", "phase 1")],
      tasks: [task("a", { section_id: "s1" }), task("b", { section_id: "s1" })],
      checkedIds: ["a", "b"],
    } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);
    fireEvent.drop(header("Ungrouped"), DT);

    await waitFor(() => expect(moveTask).toHaveBeenCalledTimes(2));
    // Explicit null — that bucket is a real target, not the absence of one.
    expect(moveTask).toHaveBeenCalledWith("a", null);
    expect(moveTask).toHaveBeenCalledWith("b", null);
  });

  it("writes only the rows that actually move", async () => {
    // A set can straddle the target. Re-filing the row that's already there
    // changes nothing, bumps `updated_at`, and resets every draft in the detail
    // pane for it (gotcha #24).
    useTaskStore.setState({
      sections: [section("s1", "phase 1")],
      tasks: [task("a", { section_id: "s1" }), task("b")],
      checkedIds: ["a", "b"],
    } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("b"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(moveTask).toHaveBeenCalledTimes(1));
    expect(moveTask).toHaveBeenCalledWith("b", "s1");
  });

  it("arms ONE undo for the move, not one per row", async () => {
    // N entries would mean N ⌘Zs to take back a single drag, with the board
    // half-moved in between.
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(useTaskStore.getState().undoStack).toHaveLength(1));
    const entry = useTaskStore.getState().undoStack[0]!;
    expect(entry.ids.sort()).toEqual(["a", "b"]);
    expect(entry.label).toBe("2 tasks");
  });

  it("clears the selection afterwards, like every other bulk action", async () => {
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(useTaskStore.getState().checkedIds).toEqual([]));
  });
});

describe("dragging a row that is NOT part of the selection", () => {
  it("moves only that row and leaves the set alone", async () => {
    // Otherwise there is no way to move one task without first clearing a
    // selection you still want — and the set would silently follow a row the
    // user never included in it.
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("c"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(moveTask).toHaveBeenCalledTimes(1));
    expect(moveTask).toHaveBeenCalledWith("c", "s1");
    expect(useTaskStore.getState().checkedIds).toEqual(["a", "b"]);
  });

  it("names the task itself in the undo when only one moved", async () => {
    threeLooseTasks();

    render(<TaskList />);
    fireEvent.dragStart(row("c"), DT);
    fireEvent.drop(header("phase 1"), DT);

    await waitFor(() => expect(useTaskStore.getState().undoStack).toHaveLength(1));
    expect(useTaskStore.getState().undoStack[0]!.label).toBe("task c");
  });
});

describe("the rows that say what's coming along", () => {
  it("dims every row in the moving set, not just the one under the cursor", () => {
    // The dimming is the only thing on screen that promises six rows will move
    // rather than one.
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("a"), DT);

    expect(row("a")).toHaveClass("cx-dragging");
    expect(row("b")).toHaveClass("cx-dragging");
    expect(row("c")).not.toHaveClass("cx-dragging");
  });

  it("dims only the grabbed row when it sits outside the set", () => {
    threeLooseTasks();
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);

    render(<TaskList />);
    fireEvent.dragStart(row("c"), DT);

    expect(row("c")).toHaveClass("cx-dragging");
    expect(row("a")).not.toHaveClass("cx-dragging");
  });
});
