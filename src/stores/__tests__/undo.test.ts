/**
 * ⌘Z — the undo stack.
 *
 * What this replaced could only take back a delete, and only while its toast
 * was still on screen. Both halves were wrong. The mis-click a todo list
 * actually produces is closing the wrong row, which had no undo at all; and
 * eight seconds is roughly the time it takes to notice, so tying the recovery
 * to the toast meant the recovery expired exactly when it was needed.
 *
 * The two properties most worth pinning are the ones a refactor would quietly
 * break. An inverse must NOT record an inverse of its own, or a second ⌘Z is a
 * redo — indistinguishable from the first one having failed. And undoing the
 * completion of a *recurring* task has to remove the instance that completion
 * materialized, or every undo leaves a duplicate and the list grows by one row
 * each time.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useTaskStore } from "@/stores/taskStore";
import type { Task, TaskPatch } from "@/types/task";

const calls: Array<[string, unknown]> = [];

function record<T>(name: string, arg: unknown, result: T): T {
  calls.push([name, arg]);
  return result;
}

function fixture(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "fix the webhook",
    body: null,
    status: "todo",
    blocked_reason: null,
    priority: 2,
    project: "cxtasks",
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
    short_id: 42,
    ...overrides,
  } as Task;
}

/** Set by a test that needs `complete_task` to answer with a spawned instance. */
let nextInstance: Task | null = null;

/**
 * Extra fields the fake backend writes that the caller never sent — how
 * `db::tasks::update`'s prefills and clears are simulated. Null = a plain echo.
 */
let updateSideEffect: ((patch: TaskPatch) => Partial<Task>) | null = null;

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      complete: vi.fn(async (id: string) =>
        record("complete", id, {
          completed: { ...fixture({ id }), status: "done" },
          next: nextInstance,
        })
      ),
      update: vi.fn(async (id: string, patch: TaskPatch) =>
        record(
          "update",
          { id, patch },
          fixture({ id, ...patch, ...(updateSideEffect?.(patch) ?? {}) })
        )
      ),
      trash: vi.fn(async (id: string) => record("trash", id, undefined)),
      restore: vi.fn(async (id: string) => record("restore", id, fixture({ id }))),
      archive: vi.fn(async (id: string) => record("archive", id, undefined)),
      unarchive: vi.fn(async (id: string) =>
        record("unarchive", id, fixture({ id }))
      ),
      destroy: vi.fn(async (id: string) => record("destroy", id, undefined)),
      reorder: vi.fn(async (ids: string[]) => record("reorder", ids, undefined)),
    },
  },
}));

beforeEach(() => {
  calls.length = 0;
  nextInstance = null;
  updateSideEffect = null;
  useTaskStore.setState({
    tasks: [fixture()],
    subtasks: [],
    selectedId: null,
    undoStack: [],
    undoToastKey: null,
    undoNotice: null,
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
  } as never);
});

/** Everything the store asked the backend to do, in order. */
const verbs = () => calls.map(([verb]) => verb);

describe("what gets recorded", () => {
  it("records a completion — the mis-click this whole feature exists for", async () => {
    await useTaskStore.getState().complete("t1");

    const [entry] = useTaskStore.getState().undoStack;
    expect(entry?.verb).toBe("completed");
    expect(entry?.label).toBe("fix the webhook");
  });

  it("puts the task back at the status it actually held, not at `todo`", async () => {
    // A task completed out of `in_review` belongs back in `in_review`. Guessing
    // `todo` would silently discard the run that put it there.
    useTaskStore.setState({ tasks: [fixture({ status: "in_review" })] } as never);
    await useTaskStore.getState().complete("t1");
    await useTaskStore.getState().undoLast();

    expect(calls).toContainEqual([
      "update",
      { id: "t1", patch: { status: "in_review" } },
    ]);
  });

  it("describes a patch by what it changed, so the toast says something", async () => {
    await useTaskStore.getState().patch("t1", { priority: 0 });
    expect(useTaskStore.getState().undoStack[0]?.verb).toBe("set to P0");
  });

  it("inverts ONLY the keys the patch touched", async () => {
    // Spreading the whole prior task back would rewrite every column, undoing
    // edits the user made in between — and clobbering anything the runner wrote
    // to the row while they were doing it.
    await useTaskStore.getState().patch("t1", { priority: 0 });
    await useTaskStore.getState().undoLast();

    expect(calls).toContainEqual(["update", { id: "t1", patch: { priority: 2 } }]);
  });

  it("records nothing when a patch changes nothing", async () => {
    // Commit-on-blur fields re-send the value they already hold. Recording
    // those fills the stack with no-ops, so ⌘Z appears dead for several presses
    // before reaching the edit that was meant.
    await useTaskStore.getState().patch("t1", { priority: 2 });
    expect(useTaskStore.getState().undoStack).toHaveLength(0);
  });
});

describe("running an inverse", () => {
  it("does NOT record an inverse of its own — ⌘Z is not a redo", async () => {
    await useTaskStore.getState().complete("t1");
    expect(useTaskStore.getState().undoStack).toHaveLength(1);

    await useTaskStore.getState().undoLast();
    expect(useTaskStore.getState().undoStack).toHaveLength(0);
  });

  it("walks back through several actions, newest first", async () => {
    await useTaskStore.getState().patch("t1", { priority: 0 });
    await useTaskStore.getState().trash("t1");
    expect(useTaskStore.getState().undoStack.map((e) => e.verb)).toEqual([
      "set to P0",
      "moved to Trash",
    ]);

    await useTaskStore.getState().undoLast();
    expect(verbs()).toContain("restore");
    await useTaskStore.getState().undoLast();
    expect(calls).toContainEqual(["update", { id: "t1", patch: { priority: 2 } }]);
    expect(useTaskStore.getState().undoStack).toHaveLength(0);
  });

  it("is a no-op on an empty stack rather than an error", async () => {
    await useTaskStore.getState().undoLast();
    expect(calls).toEqual([]);
  });

  it("confirms in the toast slot, because ⌘Z has no other feedback", async () => {
    await useTaskStore.getState().complete("t1");
    await useTaskStore.getState().undoLast();
    expect(useTaskStore.getState().undoNotice).toMatch(/Undone/);
  });
});

describe("recurrence", () => {
  it("takes back the instance the completion materialized", async () => {
    // Completing a repeating task creates its next occurrence. An undo that
    // only reopened the original would leave that occurrence behind, so the
    // list grows by one row on every complete/undo cycle.
    nextInstance = fixture({ id: "spawned", status: "todo" });
    await useTaskStore.getState().complete("t1");

    expect(useTaskStore.getState().undoStack[0]?.ids).toEqual(["t1", "spawned"]);

    await useTaskStore.getState().undoLast();
    expect(calls).toContainEqual(["trash", "spawned"]);
    expect(calls).toContainEqual(["update", { id: "t1", patch: { status: "todo" } }]);
  });
});

describe("the toast and the stack are separate lifetimes", () => {
  it("keeps the entry after the toast is dismissed", async () => {
    // This is the whole point of the split. Dismissing — or waiting out — the
    // toast used to discard the undo, so a mis-click noticed a minute later had
    // no recovery but the Trash, and only if it was a delete at all.
    await useTaskStore.getState().complete("t1");
    useTaskStore.getState().dismissUndo();

    expect(useTaskStore.getState().undoToastKey).toBeNull();
    expect(useTaskStore.getState().undoStack).toHaveLength(1);

    await useTaskStore.getState().undoLast();
    expect(verbs()).toContain("update");
  });

  it("shows the newest action, replacing whatever was up", async () => {
    await useTaskStore.getState().complete("t1");
    const first = useTaskStore.getState().undoToastKey;
    await useTaskStore.getState().trash("t1");

    expect(useTaskStore.getState().undoToastKey).not.toBe(first);
    expect(useTaskStore.getState().undoStack).toHaveLength(2);
  });
});

describe("entries that can no longer run", () => {
  it("drops those naming a hard-deleted task", async () => {
    // The one action with no way back. An entry pointing at a destroyed row
    // would fail against a task that no longer exists, which reads as "undo is
    // broken" rather than as "that one is gone for good".
    await useTaskStore.getState().complete("t1");
    await useTaskStore.getState().destroy("t1");

    expect(useTaskStore.getState().undoStack).toHaveLength(0);
    expect(useTaskStore.getState().undoToastKey).toBeNull();
  });

  it("keeps entries about other tasks", async () => {
    useTaskStore.setState({ tasks: [fixture(), fixture({ id: "t2" })] } as never);
    await useTaskStore.getState().complete("t1");
    await useTaskStore.getState().destroy("t2");

    expect(useTaskStore.getState().undoStack).toHaveLength(1);
  });
});

/**
 * The inverse is diffed from the task the SERVER returned, not from the keys
 * the caller sent — because `db::tasks::update` writes fields nobody asked it
 * to, and an undo blind to those puts the task back in a state that never
 * existed.
 */
describe("side effects the caller never asked for", () => {
  /**
   * **The reachable one.** Moving a task between projects also rewrites
   * `repo_path` (invariant 17's prefill). Inverting only the applied keys
   * restored the project name and left the workspace pointing into the project
   * you just left — recreating the exact stale-workspace dispatch footgun the
   * prefill exists to prevent, with the launch console showing the wrong repo
   * as though you had picked it.
   */
  it("puts back a workspace the server rewrote when the project changed", async () => {
    useTaskStore.setState({
      tasks: [fixture({ project: "pocketbuddy", repo_path: "/Projects/finance-app" })],
    } as never);
    // What Rust does: project → its primary repo.
    updateSideEffect = (patch) =>
      patch.project === "cxventures" ? { repo_path: "/Projects/cxventures" } : {};

    await useTaskStore.getState().patch("t1", { project: "cxventures" });
    await useTaskStore.getState().undoLast();

    expect(calls).toContainEqual([
      "update",
      {
        id: "t1",
        patch: { project: "pocketbuddy", repo_path: "/Projects/finance-app" },
      },
    ]);
  });

  // The iOS half of a project is a deliberate choice, and the prefill restores
  // a project's PRIMARY repo — so an undo that leaned on the prefill instead of
  // naming the old path would silently move the task off the secondary repo.
  it("restores a secondary repo the prefill would not have chosen", async () => {
    useTaskStore.setState({
      tasks: [
        fixture({ project: "pocketbuddy", repo_path: "/Projects/xcode/pocketbuddy" }),
      ],
    } as never);
    updateSideEffect = () => ({ repo_path: "/Projects/cxventures" });

    await useTaskStore.getState().patch("t1", { project: "cxventures" });
    await useTaskStore.getState().undoLast();

    const [, last] = calls[calls.length - 1] as [string, { patch: TaskPatch }];
    expect(last.patch.repo_path).toBe("/Projects/xcode/pocketbuddy");
  });

  // Switching executor clears an incompatible model server-side (invariant 15).
  // Rust honours an explicit `model` in the same patch, so the inverse restores
  // both together.
  it("puts back a model the executor switch cleared", async () => {
    useTaskStore.setState({
      tasks: [fixture({ bg_executor: "claude", model: "claude-opus-5" })],
    } as never);
    updateSideEffect = (patch) => (patch.bg_executor ? { model: null } : {});

    await useTaskStore.getState().patch("t1", { bg_executor: "codex" });
    await useTaskStore.getState().undoLast();

    expect(calls).toContainEqual([
      "update",
      { id: "t1", patch: { bg_executor: "claude", model: "claude-opus-5" } },
    ]);
  });

  // The no-op guard still has to hold, or ⌘Z appears dead for several presses:
  // every commit-on-blur field re-sends the value it already holds.
  it("still records nothing when the server changed nothing", async () => {
    await useTaskStore.getState().patch("t1", { title: "fix the webhook" });
    expect(useTaskStore.getState().undoStack).toHaveLength(0);
  });
});
