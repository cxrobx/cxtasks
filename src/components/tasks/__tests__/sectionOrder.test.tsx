import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, createEvent, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TaskList } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Section, Task } from "@/types/task";

const reorder = vi.fn(async (_project: string | null, _ids: string[]) => {});
const moveTask = vi.fn(async (_id: string, _section: string | null) => {});

vi.mock("@/lib/tauri", () => ({
  api: {
    sections: {
      reorder: (project: string | null, ids: string[]) => reorder(project, ids),
      moveTask: (id: string, section: string | null) => moveTask(id, section),
    },
    tasks: { complete: vi.fn(), update: vi.fn(), trash: vi.fn() },
    menu: { showForTask: vi.fn() },
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

function section(id: string, sort_order: number, project: string | null = "p"): Section {
  return { id, name: id, sort_order, project, target_date: null, completed_at: null };
}

const task = {
  id: "task-a", title: "Filed task", body: null, status: "todo", priority: 3,
  project: "p", section_id: "a", sort_order: 0,
  created_at: "2026-08-01 10:00:00", updated_at: "2026-08-01 10:00:00",
  due_at: null, defer_until: null, recurrence: null, parent_id: null,
  completed_at: null, deleted_at: null, archived_at: null, short_id: null,
  blocked_reason: null, repo_path: null, prompt: null, bg_allowed: false,
  bg_executor: "claude", bg_writes_allowed: false, bg_shell_allowed: false,
  reminded_at: null, model: null,
} as Task;

const transfer = () => ({ dataTransfer: { setData: vi.fn(), effectAllowed: "", dropEffect: "" } });
const handle = (name: string) => screen.getByRole("button", { name: `Reorder section ${name}` });
const header = (name: string) => screen.getByRole("button", { name });
const block = (name: string) => header(name).closest<HTMLDivElement>("[data-section-id]")!;
const order = () => screen.queryAllByRole("button", { name: /^Reorder section / })
  .map((button) => button.getAttribute("aria-label")?.replace("Reorder section ", ""));

beforeEach(() => {
  vi.clearAllMocks();
  reorder.mockImplementation(async () => {});
  useTaskStore.setState({
    sections: [section("a", 0), section("b", 1), section("c", 2)],
    tasks: [task], activeProject: "p", view: "open", loading: false,
    checkedIds: [], lastCheckedId: null, selectedId: null, countdowns: {},
    blockedIds: [], flashIds: [], tagsByTask: {}, tripwireByTask: {},
    undoStack: [], error: null,
    refresh: vi.fn(async () => {}), select: vi.fn(async () => {}),
  });
  useUiStore.setState({ groupBy: "section", sort: { key: "default", dir: "asc" } });
});

function drag(source: string, target: string, edge: "before" | "after") {
  const dt = transfer();
  fireEvent.dragStart(handle(source), dt);
  // jsdom has no DragEvent constructor, so its fallback Event ignores clientY.
  const over = createEvent.dragOver(block(target), dt);
  Object.defineProperty(over, "clientY", { value: edge === "before" ? -1 : 1 });
  fireEvent(block(target), over);
  fireEvent.drop(block(target), dt);
}

describe("section reordering", () => {
  it("moves a section before another immediately and persists the whole project order", async () => {
    let finish!: () => void;
    reorder.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<TaskList />);
    drag("c", "a", "before");

    expect(order()).toEqual(["c", "a", "b"]);
    expect(reorder).toHaveBeenCalledWith("p", ["c", "a", "b"]);
    expect(useTaskStore.getState().sections.map((s) => s.sort_order)).toEqual([0, 1, 2]);
    expect(useTaskStore.getState().tasks[0]?.section_id).toBe("a");
    expect(moveTask).not.toHaveBeenCalled();
    await act(async () => { finish(); });
  });

  it("can move the first section after the last, including when collapsed", async () => {
    render(<TaskList />);
    fireEvent.click(header("a"));
    drag("a", "c", "after");
    expect(order()).toEqual(["b", "c", "a"]);
    expect(screen.queryByText("Filed task")).not.toBeInTheDocument();
    await waitFor(() => expect(reorder).toHaveBeenCalledWith("p", ["b", "c", "a"]));
    fireEvent.click(header("a"));
    expect(screen.getByText("Filed task")).toBeInTheDocument();
  });

  it("keeps sections reorderable when all are empty and tasks have an explicit sort", async () => {
    useTaskStore.setState({ tasks: [] });
    useUiStore.setState({ sort: { key: "due", dir: "asc" } });
    render(<TaskList />);
    drag("c", "b", "before");
    expect(order()).toEqual(["a", "c", "b"]);
    await waitFor(() => expect(reorder).toHaveBeenCalledWith("p", ["a", "c", "b"]));
  });

  it("supports keyboard reordering and keeps focus on the moved section", async () => {
    render(<TaskList />);
    handle("b").focus();
    fireEvent.keyDown(handle("b"), { key: "ArrowUp" });
    expect(order()).toEqual(["b", "a", "c"]);
    expect(handle("b")).toHaveFocus();
    await waitFor(() => expect(reorder).toHaveBeenCalledTimes(1));
    fireEvent.keyDown(handle("b"), { key: "ArrowUp" });
    expect(reorder).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(handle("b"), { key: "ArrowDown" });
    expect(order()).toEqual(["a", "b", "c"]);
    await waitFor(() => expect(reorder).toHaveBeenCalledTimes(2));
  });

  it("leaves Ungrouped last and does not file tasks during a section drop on it", () => {
    useTaskStore.setState({ tasks: [task, { ...task, id: "loose", title: "Loose", section_id: null }] });
    render(<TaskList />);
    expect(screen.queryByRole("button", { name: "Reorder section Ungrouped" })).not.toBeInTheDocument();
    const dt = transfer();
    fireEvent.dragStart(handle("a"), dt);
    fireEvent.dragOver(header("Ungrouped"), dt);
    fireEvent.drop(header("Ungrouped"), dt);
    expect(order()).toEqual(["a", "b", "c"]);
    expect(reorder).not.toHaveBeenCalled();
    expect(moveTask).not.toHaveBeenCalled();
  });

  it("does not reorder on a canceled drag or a drop back on the source", () => {
    render(<TaskList />);
    const dt = transfer();
    fireEvent.dragStart(handle("a"), dt);
    fireEvent.dragOver(block("c"), { ...dt, clientY: 1 });
    expect(block("c")).toHaveClass("cx-drop-below");
    fireEvent.dragEnd(handle("a"), dt);
    expect(block("c")).not.toHaveClass("cx-drop-below");
    drag("a", "a", "before");
    expect(order()).toEqual(["a", "b", "c"]);
    expect(reorder).not.toHaveBeenCalled();
  });

  it("still files a task into a section without changing section order", async () => {
    render(<TaskList />);
    const dt = transfer();
    fireEvent.dragStart(screen.getByText("Filed task").closest("[role=option]")!, dt);
    fireEvent.dragOver(header("b"), dt);
    fireEvent.drop(header("b"), dt);
    await waitFor(() => expect(moveTask).toHaveBeenCalledWith("task-a", "b"));
    expect(reorder).not.toHaveBeenCalled();
    expect(order()).toEqual(["a", "b", "c"]);
  });

  it("restores the previous order and reports a failed save", async () => {
    reorder.mockRejectedValueOnce(new Error("save failed"));
    render(<TaskList />);
    drag("c", "a", "before");
    await waitFor(() => expect(order()).toEqual(["a", "b", "c"]));
    expect(useTaskStore.getState().error).toContain("save failed");
    expect(useTaskStore.getState().undoStack).toHaveLength(0);
  });

  it("saves and undoes view sections in their original scope", async () => {
    useTaskStore.setState({ activeProject: null, sections: [section("a", 0, null), section("b", 1, null)] });
    await useTaskStore.getState().reorderSections(["b", "a"]);
    expect(reorder).toHaveBeenCalledWith(null, ["b", "a"]);
    useTaskStore.setState({ activeProject: "other", sections: [section("other", 0, "other")] });
    await useTaskStore.getState().undoLast();
    expect(reorder).toHaveBeenLastCalledWith(null, ["a", "b"]);
    expect(useTaskStore.getState().sections.map((s) => s.id)).toEqual(["other"]);
  });

  it("does not restore an old scope when saving fails after navigation", async () => {
    let fail!: (error: Error) => void;
    reorder.mockImplementationOnce(() => new Promise<void>((_, reject) => { fail = reject; }));
    const pending = useTaskStore.getState().reorderSections(["b", "a", "c"]);
    useTaskStore.setState({ activeProject: "other", sections: [section("other", 0, "other")] });
    fail(new Error("save failed"));
    await pending;
    expect(useTaskStore.getState().sections.map((s) => s.id)).toEqual(["other"]);
  });
});
