/**
 * `reveal` — showing a task that arrived from outside the list.
 *
 * The bug this exists to stop coming back: the detail pane resolves its task
 * out of `tasks`, the rows the CURRENT view loaded, so `select`ing a task that
 * view doesn't hold sets `selectedId` to an id nothing renders and leaves the
 * pane blank. A notification click for a finished background run — which lands
 * in `in_review` — did exactly that whenever you happened to be sitting on
 * Today, and the click read as completely inert.
 *
 * What's pinned here is therefore mostly about NAVIGATION: that a hidden task
 * drags the view to somewhere it can actually be seen, that the filters which
 * would re-hide it are dropped, and — the other half, easy to lose in a
 * refactor — that a task already on screen moves nothing at all.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { FilterSpec, ListFilters, Task, ViewName } from "@/types/task";

/** Rows each view would return, keyed by the view asked for. */
const BY_VIEW: Record<string, Task[]> = {};
/** Every task that exists, findable by id the way `get_task` is: unfiltered. */
const ALL: Record<string, Task> = {};

const listCalls: ListFilters[] = [];
/** Tags per task, so a chip in a test can actually hide a row. */
const TAGS: Record<string, string[]> = {};

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      get: vi.fn(async (id: string) => ALL[id] ?? null),
      list: vi.fn(async (filters: ListFilters) => {
        listCalls.push(filters);
        return BY_VIEW[filters.view ?? "all"] ?? [];
      }),
      // The app runs ONE query now (invariant 21), so this is the mock that
      // matters. Chips are applied crudely — enough to model "a filter is
      // hiding the row we promised to reveal", which is what `landOn` exists
      // for.
      filter: vi.fn(async (spec: FilterSpec) => {
        listCalls.push({ view: spec.view, project: spec.project ?? null });
        const rows = BY_VIEW[spec.view ?? "all"] ?? [];
        const hidden = new Set(spec.exclude_tags ?? []);
        return hidden.size === 0
          ? rows
          : rows.filter((t) => !(TAGS[t.id] ?? []).some((g) => hidden.has(g)));
      }),
      search: vi.fn(async () => []),
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
    tripwires: { runtime: vi.fn(async () => ({})), scripts: vi.fn(async () => []) },
  },
}));

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

/** Register a task as existing, and as visible in `views`. */
function exists(t: Task, ...views: ViewName[]) {
  ALL[t.id] = t;
  for (const v of views) (BY_VIEW[v] ??= []).push(t);
  return t;
}

beforeEach(() => {
  for (const k of Object.keys(BY_VIEW)) delete BY_VIEW[k];
  for (const k of Object.keys(ALL)) delete ALL[k];
  listCalls.length = 0;
  for (const k of Object.keys(TAGS)) delete TAGS[k];
  localStorage.clear();
  useUiStore.setState({ viewFilters: {} });
  useTaskStore.setState({
    tasks: [],
    projectRecords: [],
    view: "today",
    activeProject: null,
    query: "",
    filters: [],
    selectedId: null,
    revealTick: 0,
  });
});

describe("reveal", () => {
  it("opens the detail pane for a task clicked outside the list", async () => {
    const due = exists(task("due-1"), "today");
    useTaskStore.setState({ tasks: [due], view: "today" });
    useUiStore.setState({ detailCollapsed: true });

    await useTaskStore.getState().reveal(due.id);

    expect(useUiStore.getState().detailCollapsed).toBe(false);
    expect(useTaskStore.getState().selectedId).toBe(due.id);
  });

  it("moves to a view that holds the task when the current one doesn't", async () => {
    // The real case: a background run finished, so the task is in `in_review`,
    // and the user is looking at Today.
    const done = exists(task("run-1", { status: "in_review" }), "all");
    useTaskStore.setState({ tasks: [], view: "today" });

    await useTaskStore.getState().reveal(done.id);

    const s = useTaskStore.getState();
    expect(s.view).toBe("all");
    expect(s.selectedId).toBe("run-1");
    // And it is genuinely on screen, not merely selected — the distinction the
    // whole bug turned on.
    expect(s.tasks.map((t) => t.id)).toContain("run-1");
  });

  it("routes a trashed task to the Trash and an archived one to the Archive", async () => {
    exists(task("gone", { deleted_at: "2026-08-01 09:00:00" }), "trash");
    exists(task("shelved", { archived_at: "2026-08-01 09:00:00" }), "archive");

    await useTaskStore.getState().reveal("gone");
    expect(useTaskStore.getState().view).toBe("trash");

    await useTaskStore.getState().reveal("shelved");
    expect(useTaskStore.getState().view).toBe("archive");
  });

  it("sends a task that is both archived and trashed to the Trash", async () => {
    // Matches the backend, where Trash wins: it is on the purge clock and has
    // to be findable there. Restoring it puts it back in the Archive.
    exists(
      task("both", {
        deleted_at: "2026-08-01 09:00:00",
        archived_at: "2026-07-01 09:00:00",
      }),
      "trash"
    );

    await useTaskStore.getState().reveal("both");
    expect(useTaskStore.getState().view).toBe("trash");
  });

  it("routes a closed task to Done, which `all` deliberately excludes", async () => {
    exists(task("shipped", { status: "done" }), "done");

    await useTaskStore.getState().reveal("shipped");
    expect(useTaskStore.getState().view).toBe("done");
  });

  it("drops the search and the chips that would re-hide the task", async () => {
    // Navigating to the right view is not enough on its own: a live query or a
    // tag chip narrows every view, so the task would land in a list that still
    // refuses to show it.
    exists(task("t1"), "all");
    useTaskStore.setState({
      query: "something else",
      filters: [{ facet: "tag", op: "is", value: "billable" }],
    });

    await useTaskStore.getState().reveal("t1");

    const s = useTaskStore.getState();
    expect(s.query).toBe("");
    expect(s.filters.some((f) => f.facet === "tag")).toBe(false);
    expect(s.tasks.map((t) => t.id)).toContain("t1");
  });

  /** The sharper version, and the reason `landOn` exists: the scope's SAVED
   * default is the thing hiding the row. Entering the scope applies it (it is
   * what that view means), the task is still missing, so the filters fall back
   * to the baseline rather than leaving the user staring at a list that was
   * supposed to contain the task they clicked. */
  it("falls back to the baseline when a saved default hides the revealed task", async () => {
    const t = exists(task("hidden-by-default"), "all");
    TAGS[t.id] = ["someday"];
    useUiStore.setState({
      viewFilters: { all: [{ facet: "tag", op: "is_not", value: "someday" }] },
    });

    await useTaskStore.getState().reveal(t.id);

    const s = useTaskStore.getState();
    expect(s.tasks.map((x) => x.id)).toContain(t.id);
    expect(s.filters.some((f) => f.facet === "tag")).toBe(false);
  });

  it("reveals the PARENT of a subtask, which is the row that exists", async () => {
    // Subtasks are never top-level rows (`include_subtasks` is off), but
    // `list_due_for_reminder` has no such filter — so a subtask with a due date
    // really does fire a banner. Selecting it would render nothing anywhere.
    const parent = exists(task("parent"), "all");
    ALL["child"] = task("child", { parent_id: parent.id });

    await useTaskStore.getState().reveal("child");

    expect(useTaskStore.getState().selectedId).toBe("parent");
  });

  it("leaves the view alone when the task is already on screen", async () => {
    // The other half of the feature. Yanking someone out of the view they chose
    // to show them a row they can already see is its own bug.
    const here = exists(task("here"), "today");
    useTaskStore.setState({ tasks: [here], view: "today", activeProject: "acme" });

    await useTaskStore.getState().reveal("here");

    const s = useTaskStore.getState();
    expect(s.view).toBe("today");
    expect(s.activeProject).toBe("acme");
    expect(s.selectedId).toBe("here");
  });

  it("bumps revealTick so the list scrolls, and only then", async () => {
    // `revealTick` is what the list scrolls on. Keying the scroll to
    // `selectedId` instead would drag the viewport every time a user clicked a
    // row that was merely half off-screen.
    const t = exists(task("t1"), "all");
    useTaskStore.setState({ tasks: [t], view: "all" });

    const before = useTaskStore.getState().revealTick;
    await useTaskStore.getState().reveal("t1");
    expect(useTaskStore.getState().revealTick).toBe(before + 1);

    await useTaskStore.getState().select("t1");
    expect(useTaskStore.getState().revealTick).toBe(before + 1);
  });

  it("does nothing for an id that no longer exists", async () => {
    // A notification outliving its task — clicked after the task was purged.
    // Silence is the right answer; guessing a substitute would be worse.
    useTaskStore.setState({ view: "today" });

    await useTaskStore.getState().reveal("ghost");

    const s = useTaskStore.getState();
    expect(s.view).toBe("today");
    expect(s.selectedId).toBeNull();
    expect(listCalls).toHaveLength(0);
  });
});
