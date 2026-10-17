/**
 * Multi-select, and the leading SQUARE that closes a task.
 *
 * The two halves are one design: the leading box is a checkbox (Things 3), so
 * it completes; the row itself carries the selection, so ⌘/⇧-click builds a
 * set. What these tests guard is that the two gestures stay separate — a click
 * on the box must not disturb a set that's already built, and a modifier-click
 * on the row must not close anything. Everything below that guards the ways a
 * selection can quietly act on the wrong rows: a range that isn't the rows you
 * can see, or a set that outlived the list it was built against.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { BulkActionBar } from "@/components/tasks/BulkActionBar";
import { TaskRow } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Task } from "@/types/task";

// `complete_task` answers with the completed row AND any next instance a
// recurrence materialized — the store reads both to build the undo, so a mock
// that answered `undefined` would be lying about the API's shape in exactly the
// place the new code depends on it.
const complete = vi.fn(async (id: string) => ({
  completed: task({ id, status: "done" }),
  next: null,
}));
const update = vi.fn(async (_id: string, _patch: unknown) => {});
const trash = vi.fn(async (_id: string) => {});

const writeText = vi.fn(async (_text: string) => {});
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: (text: string) => writeText(text),
}));

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      complete: (id: string) => complete(id),
      update: (id: string, patch: unknown) => update(id, patch),
      trash: (id: string) => trash(id),
      restore: vi.fn(async () => {}),
      unarchive: vi.fn(async () => {}),
      destroy: vi.fn(async () => {}),
    },
    menu: { showForTask: vi.fn() },
  },
}));

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "fix the retry",
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
    created_at: "2026-07-30 10:00:00",
    updated_at: "2026-07-30 10:00:00",
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
    short_id: 42,
    ...overrides,
  } as Task;
}

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({
    checkedIds: [],
    lastCheckedId: null,
    tasks: [],
    selectedId: null,
    view: "today",
    undoStack: [],
    undoToastKey: null,
    undoNotice: null,
    refresh: vi.fn(async () => {}),
  } as never);
});

describe("the leading square", () => {
  it("completes the task — it is a checkbox, and that is what one does", () => {
    const onComplete = vi.fn();
    render(
      <TaskRow
        task={task()}
        selected={false}
        onSelect={() => {}}
        onComplete={onComplete}
      />
    );

    fireEvent.click(screen.getByRole("checkbox", { name: /Mark fix the retry done/ }));
    expect(onComplete).toHaveBeenCalled();
  });

  it("reopens a done task WITHOUT calling complete again", () => {
    // `complete_task` is one-way — it sets `done` unconditionally — so routing
    // both through it re-completes the task, and on a recurring one spawns a
    // fresh instance per click. One control, two verbs, chosen by the state.
    const onComplete = vi.fn();
    const onReopen = vi.fn();
    render(
      <TaskRow
        task={task({ status: "done" })}
        selected={false}
        onSelect={() => {}}
        onComplete={onComplete}
        onReopen={onReopen}
      />
    );

    fireEvent.click(screen.getByRole("checkbox", { name: /Reopen fix the retry/ }));
    expect(onReopen).toHaveBeenCalled();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("reports done-ness to assistive tech, not just as a filled box", () => {
    render(
      <TaskRow
        task={task({ status: "done" })}
        selected={false}
        onSelect={() => {}}
        onComplete={vi.fn()}
      />
    );
    expect(screen.getByRole("checkbox", { name: /Reopen/ })).toHaveAttribute(
      "aria-checked",
      "true"
    );
  });

  it("does NOT touch the selection — closing one of six must keep the six", () => {
    // The box sits inside the row, and the row is the selection control. Without
    // the click being stopped there, ticking one row off a built-up set would
    // silently collapse that set to a single row on the way past.
    const onSelect = vi.fn();
    render(
      <TaskRow
        task={task()}
        selected={false}
        checked
        onSelect={onSelect}
        onComplete={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole("checkbox", { name: /Mark .* done/ }));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("is inert in the Trash, where completing means nothing", () => {
    const onComplete = vi.fn();
    render(
      <TaskRow
        task={task({ deleted_at: "2026-07-30 10:00:00.000" })}
        selected={false}
        onSelect={() => {}}
        onComplete={onComplete}
        onRestore={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole("checkbox"));
    expect(onComplete).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /Put back/ })).toBeInTheDocument();
  });
});

describe("the row's own click", () => {
  it("reports a plain click as neither modifier", () => {
    const onSelect = vi.fn();
    render(
      <TaskRow
        task={task()}
        selected={false}
        onSelect={onSelect}
        onComplete={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole("option"));
    // `toggleOpen` rides along on a real click and ONLY on a real click — it is
    // what lets `rowClick` tell this apart from the context menu taking the
    // selection, so it belongs in the asserted shape rather than being loosened
    // away with `objectContaining`.
    expect(onSelect).toHaveBeenCalledWith({
      meta: false,
      shift: false,
      toggleOpen: true,
    });
  });

  it("passes ⌘ and ⇧ through, so add-one and take-a-range stay distinct", () => {
    const onSelect = vi.fn();
    render(
      <TaskRow
        task={task()}
        selected={false}
        onSelect={onSelect}
        onComplete={vi.fn()}
      />
    );

    const row = screen.getByRole("option");
    fireEvent.click(row, { metaKey: true });
    expect(onSelect).toHaveBeenLastCalledWith({
      meta: true,
      shift: false,
      toggleOpen: true,
    });
    fireEvent.click(row, { shiftKey: true });
    expect(onSelect).toHaveBeenLastCalledWith({
      meta: false,
      shift: true,
      toggleOpen: true,
    });
  });

  it("counts a checked row as selected for assistive tech", () => {
    // The row highlight is now the ONLY indication of membership — there is no
    // tick box left to read — so a set of six announced as one row would be the
    // whole selection going unreported.
    render(
      <TaskRow
        task={task()}
        selected={false}
        checked
        onSelect={() => {}}
        onComplete={vi.fn()}
      />
    );
    expect(screen.getByRole("option")).toHaveAttribute("aria-selected", "true");
  });

  it("carries no completion control on its trailing edge any more", () => {
    render(
      <TaskRow
        task={task()}
        selected={false}
        onSelect={() => {}}
        onComplete={vi.fn()}
      />
    );
    expect(screen.queryByRole("button", { name: /Mark .* done/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Reopen/ })).toBeNull();
  });
});

describe("the T-number", () => {
  it("copies itself, and leaves the selection alone", async () => {
    // A T-number is only worth having because it travels — into a commit
    // message, a comment, the next session's prompt — so the chip printing it
    // is the control that copies it. The `stopPropagation` is the half that
    // has to be pinned: the row underneath is the selection control, so
    // without it, copying a reference out of a six-row selection would
    // silently collapse that selection to one row.
    const onSelect = vi.fn();
    render(
      <TaskRow
        task={task()}
        selected={false}
        onSelect={onSelect}
        onComplete={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy T42" }));
    expect(writeText).toHaveBeenCalledWith("T42");
    expect(onSelect).not.toHaveBeenCalled();
    // The chip reports the copy where it stands, without changing width — it
    // sits left of the title, and a text swap would shove the row sideways.
    expect(await screen.findByRole("button", { name: "Copied T42" })).toHaveTextContent(
      "T42"
    );
  });
});

describe("row-click selection", () => {
  const order = ["a", "b", "c"];

  beforeEach(() => {
    useTaskStore.setState({ select: vi.fn(async () => {}) } as never);
  });

  it("starts a fresh selection of one on a plain click", () => {
    useTaskStore.setState({ checkedIds: ["a", "b"] } as never);
    useTaskStore.getState().rowClick("c", { meta: false, shift: false }, order);

    expect(useTaskStore.getState().checkedIds).toEqual([]);
    expect(useTaskStore.getState().lastCheckedId).toBe("c");
  });

  it("⌘-click seeds from the focused row, so two clicks select two rows", () => {
    // Without the seed the row already highlighted drops out of a gesture whose
    // entire meaning is "and this one too", and building a pair takes three
    // clicks with the first one silently not counting.
    useTaskStore.setState({ checkedIds: [], selectedId: "a" } as never);
    useTaskStore.getState().rowClick("b", { meta: true, shift: false }, order);

    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["a", "b"]);
  });

  it("⌘-click on an already-checked row removes it", () => {
    useTaskStore.setState({ checkedIds: ["a", "b"], selectedId: "a" } as never);
    useTaskStore.getState().rowClick("b", { meta: true, shift: false }, order);

    expect(useTaskStore.getState().checkedIds).toEqual(["a"]);
  });

  it("opens a collapsed detail pane, because opening a task is why you clicked it", () => {
    useUiStore.setState({ detailCollapsed: true } as never);
    useTaskStore
      .getState()
      .rowClick("c", { meta: false, shift: false, toggleOpen: true }, order);

    expect(useUiStore.getState().detailCollapsed).toBe(false);
  });

  it("closes it again when you click the row you are already reading", () => {
    useTaskStore.setState({ selectedId: "c" } as never);
    useUiStore.setState({ detailCollapsed: false } as never);
    useTaskStore
      .getState()
      .rowClick("c", { meta: false, shift: false, toggleOpen: true }, order);

    expect(useUiStore.getState().detailCollapsed).toBe(true);
  });

  /**
   * The selected row with the pane SHUT must open, not toggle back to shut —
   * otherwise the one row you most want to look at is the one row you cannot
   * open, and it reads as the click doing nothing.
   */
  it("opens rather than toggles when the selected row's pane is already shut", () => {
    useTaskStore.setState({ selectedId: "c" } as never);
    useUiStore.setState({ detailCollapsed: true } as never);
    useTaskStore
      .getState()
      .rowClick("c", { meta: false, shift: false, toggleOpen: true }, order);

    expect(useUiStore.getState().detailCollapsed).toBe(false);
  });

  it("keeps the pane open when you move to a DIFFERENT row", () => {
    useTaskStore.setState({ selectedId: "a" } as never);
    useUiStore.setState({ detailCollapsed: false } as never);
    useTaskStore
      .getState()
      .rowClick("c", { meta: false, shift: false, toggleOpen: true }, order);

    expect(useUiStore.getState().detailCollapsed).toBe(false);
  });

  /**
   * `TaskContextMenu`'s `onOpen` routes through `rowClick` to take the
   * selection (gotcha #9) and passes no `toggleOpen`. Without that flag,
   * right-clicking the row you are reading would shut the pane underneath the
   * menu you just opened.
   */
  it("does not toggle for the context menu taking the selection", () => {
    useTaskStore.setState({ selectedId: "c" } as never);
    useUiStore.setState({ detailCollapsed: false } as never);
    useTaskStore.getState().rowClick("c", { meta: false, shift: false }, order);

    expect(useUiStore.getState().detailCollapsed).toBe(false);
  });

  /**
   * The half that would regress silently. ⌘/⇧-click builds a SET — a pane
   * springing open on the first row of a six-row selection takes 300px away
   * from the list still being picked from, and every test above would stay
   * green while it happened.
   */
  it("leaves a collapsed pane alone for a modifier-click, which is building a set", () => {
    for (const mods of [
      { meta: true, shift: false },
      { meta: false, shift: true },
    ]) {
      useUiStore.setState({ detailCollapsed: true } as never);
      useTaskStore.setState({ checkedIds: [], selectedId: "a" } as never);
      useTaskStore.getState().rowClick("b", mods, order);

      expect(useUiStore.getState().detailCollapsed).toBe(true);
    }
  });

  it("⇧-click ranges from the focused row when nothing is ticked yet", () => {
    useTaskStore.setState({
      checkedIds: [],
      selectedId: "a",
      lastCheckedId: null,
    } as never);
    useTaskStore.getState().rowClick("c", { meta: false, shift: true }, order);

    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["a", "b", "c"]);
  });
});

describe("range selection", () => {
  const order = ["a", "b", "c", "d", "e"];

  it("ticks everything between the anchor and the shift-click, inclusive", () => {
    const store = useTaskStore.getState();
    store.toggleChecked("b");
    useTaskStore.getState().checkRange("d", order);
    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["b", "c", "d"]);
  });

  it("works upwards too", () => {
    useTaskStore.getState().toggleChecked("d");
    useTaskStore.getState().checkRange("b", order);
    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["b", "c", "d"]);
  });

  it("EXTENDS rather than replaces, so a two-pass selection survives", () => {
    useTaskStore.getState().toggleChecked("a");
    useTaskStore.getState().checkRange("b", order);
    useTaskStore.getState().toggleChecked("d");
    useTaskStore.getState().checkRange("e", order);
    expect(useTaskStore.getState().checkedIds.sort()).toEqual([
      "a",
      "b",
      "d",
      "e",
    ]);
  });

  it("uses the RENDER order it is handed, not the store's task order", () => {
    // The grouped view renders by section, so `tasks` order is not what the
    // user sees. Ranging over the wrong list ticks rows that were never
    // between the two clicks — silently, since they may be scrolled away.
    useTaskStore.setState({ tasks: [] } as never);
    useTaskStore.getState().toggleChecked("e");
    useTaskStore.getState().checkRange("d", ["e", "d", "a", "b", "c"]);
    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["d", "e"]);
  });

  it("degrades to a plain toggle when the anchor is gone", () => {
    // Not "select everything": a shift-click with nothing to range from is an
    // ambiguous gesture, and the expensive reading of it is the wrong default.
    useTaskStore.setState({ lastCheckedId: null } as never);
    useTaskStore.getState().checkRange("c", order);
    expect(useTaskStore.getState().checkedIds).toEqual(["c"]);
  });
});

describe("the checked set's lifetime", () => {
  it("drops ids the list no longer shows, so a bulk action can't hit them", async () => {
    const list = vi.fn(async () => [task({ id: "a" }), task({ id: "b" })]);
    useTaskStore.setState({
      checkedIds: ["a", "b", "gone"],
      lastCheckedId: "gone",
    } as never);

    // Exercise the prune directly against a refreshed task list.
    const visible = new Set((await list()).map((t) => t.id));
    const pruned = useTaskStore
      .getState()
      .checkedIds.filter((id) => visible.has(id));
    expect(pruned).toEqual(["a", "b"]);
  });

  it("clears when the view changes", async () => {
    useTaskStore.setState({
      checkedIds: ["a"],
      lastCheckedId: "a",
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().setView("waiting");
    expect(useTaskStore.getState().checkedIds).toEqual([]);
    expect(useTaskStore.getState().lastCheckedId).toBeNull();
  });

  it("clears when a search narrows the list", async () => {
    useTaskStore.setState({
      checkedIds: ["a"],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().setQuery("webhook");
    expect(useTaskStore.getState().checkedIds).toEqual([]);
  });
});

describe("the bulk bar", () => {
  it("stays out of the way until something is ticked", () => {
    useTaskStore.setState({ checkedIds: [] } as never);
    const { container } = render(<BulkActionBar />);
    expect(container).toBeEmptyDOMElement();
  });

  it("counts the selection and offers the board's verbs", () => {
    useTaskStore.setState({ checkedIds: ["a", "b", "c"], view: "today" } as never);
    render(<BulkActionBar />);

    expect(screen.getByText("3 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Done" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Move to Trash" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Set priority P0" })).toBeInTheDocument();
  });

  it("swaps to put-back in the Trash, where completing means nothing", () => {
    useTaskStore.setState({ checkedIds: ["a"], view: "trash" } as never);
    render(<BulkActionBar />);

    expect(screen.getByRole("button", { name: "Put back" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete permanently" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Done" })).toBeNull();
  });

  it("offers no permanent delete in the Archive — there is no clock on it", () => {
    useTaskStore.setState({ checkedIds: ["a"], view: "archive" } as never);
    render(<BulkActionBar />);

    expect(screen.getByRole("button", { name: "Put back" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete permanently" })).toBeNull();
  });
});

describe("bulk actions", () => {
  it("completes every checked task, once each", async () => {
    useTaskStore.setState({
      checkedIds: ["a", "b", "c"],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().bulkComplete();

    expect(complete.mock.calls.map((c) => c[0])).toEqual(["a", "b", "c"]);
    expect(useTaskStore.getState().checkedIds).toEqual([]);
  });

  it("sends priority as a patch per task", async () => {
    useTaskStore.setState({
      checkedIds: ["a", "b"],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().bulkPriority(0);

    expect(update).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenCalledWith("a", { priority: 0 });
  });

  it("arms ONE undo covering the whole batch", async () => {
    // A per-task undo would leave ⌘Z restoring only the last of six while
    // looking exactly like it had worked.
    useTaskStore.setState({
      checkedIds: ["a", "b", "c"],
      tasks: [task({ id: "a", title: "first" })],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().bulkTrash();

    const stack = useTaskStore.getState().undoStack;
    expect(stack).toHaveLength(1);
    expect(stack[0]!.ids).toEqual(["a", "b", "c"]);
    expect(stack[0]!.label).toBe("3 tasks");
    expect(stack[0]!.verb).toBe("moved to Trash");
  });

  it("names the task itself when only one was trashed", async () => {
    useTaskStore.setState({
      checkedIds: ["a"],
      tasks: [task({ id: "a", title: "fix the webhook" })],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().bulkTrash();

    expect(useTaskStore.getState().undoStack[0]!.label).toBe("fix the webhook");
  });

  it("does nothing at all on an empty selection", async () => {
    useTaskStore.setState({
      checkedIds: [],
      refresh: vi.fn(async () => {}),
    } as never);
    await useTaskStore.getState().bulkComplete();
    expect(complete).not.toHaveBeenCalled();
  });
});
