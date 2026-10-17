/**
 * The sidebar's node badges count what clicking the node SHOWS.
 *
 * `open_count` comes from the server as "open and top-level", but the list a
 * node opens applies its own chips — saved on the node, inherited from its
 * area, or the baseline. The real DB had pocketbuddy reading "8" over a list
 * that said "Nothing here", because the area had saved `Waiting is not
 * Tripwire, Deferred, Blocked` and the badge had never heard of it. So the
 * store asks `count_filtered` for every node, through the same spec the list
 * runs, and pins the answer on `badge_count`.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { nodeSpec, useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import { DEFAULT_SORT } from "@/types/task";
import type { FilterSpec, Project } from "@/types/task";

const countCalls: FilterSpec[][] = [];
let countAnswer: () => Promise<number[]> = async () => [];

function node(name: string, overrides: Partial<Project> & { kind: Project["kind"] }): Project {
  return {
    id: `p-${name}`,
    name,
    color: null,
    icon: null,
    sort_order: 0,
    archived: false,
    open_count: 0,
    health: null,
    parent: null,
    target_date: null,
    closed_at: null,
    ...overrides,
  };
}

const RECORDS = [
  node("cxventures", { kind: "area", open_count: 16 }),
  node("acme", { kind: "project", parent: "cxventures", open_count: 5 }),
  node("pocketbuddy", { kind: "area", open_count: 8 }),
];

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      get: vi.fn(async () => null),
      list: vi.fn(async () => []),
      search: vi.fn(async () => []),
      filter: vi.fn(async () => []),
      countFiltered: vi.fn(async (specs: FilterSpec[]) => {
        countCalls.push(specs);
        return countAnswer();
      }),
      subtasks: vi.fn(async () => []),
      projects: vi.fn(async () => []),
      counts: vi.fn(async () => ({ inbox: 0, today: 0, upcoming: 0, in_review: 0 })),
      countdowns: vi.fn(async () => ({})),
    },
    runs: { list: vi.fn(async () => []) },
    tags: { list: vi.fn(async () => []), forTask: vi.fn(async () => []), byTask: vi.fn(async () => ({})) },
    deps: {
      blockedIds: vi.fn(async () => []),
      forTask: vi.fn(async () => ({ blocked_by: [], blocking: [], is_blocked: false })),
    },
    comments: { list: vi.fn(async () => []) },
    attachments: { list: vi.fn(async () => []) },
    sections: { list: vi.fn(async () => []) },
    projects: {
      list: vi.fn(async () => RECORDS.map((p) => ({ ...p }))),
      board: vi.fn(async () => []),
      repos: { list: vi.fn(async () => []) },
    },
    tripwires: { runtime: vi.fn(async () => ({})) },
  },
}));

const NOT_DONE = { facet: "status", op: "is_not", value: "done" } as const;
const NOT_WAITING = [
  { facet: "waiting", op: "is_not", value: "tripwire" },
  { facet: "waiting", op: "is_not", value: "deferred" },
  { facet: "waiting", op: "is_not", value: "blocked" },
] as const;

beforeEach(() => {
  countCalls.length = 0;
  countAnswer = async () => [];
  localStorage.clear();
  useUiStore.setState({ sort: DEFAULT_SORT, viewFilters: {} });
  useTaskStore.setState({
    tasks: [],
    projectRecords: [],
    view: "today",
    activeProject: null,
    query: "",
    filters: [],
    selectedId: null,
    checkedIds: [],
    lastCheckedId: null,
  });
});

describe("nodeSpec", () => {
  it("is the node scoped through the server roll-up under its baseline chip", () => {
    expect(nodeSpec("pocketbuddy", {}, RECORDS)).toEqual({
      view: "all",
      project: "pocketbuddy",
      exclude_closed: false,
      exclude_statuses: ["done"],
    });
  });

  it("carries the node's SAVED chips — the pocketbuddy case", () => {
    const saved = { "p-pocketbuddy": [NOT_DONE, ...NOT_WAITING] };
    expect(nodeSpec("pocketbuddy", saved, RECORDS)).toMatchObject({
      project: "pocketbuddy",
      exclude_statuses: ["done"],
      exclude_waiting: ["tripwire", "deferred", "blocked"],
    });
  });

  it("lets a project inherit its area's saved chips, the way the list does", () => {
    const saved = { "p-cxventures": [NOT_DONE, NOT_WAITING[2]] };
    expect(nodeSpec("acme", saved, RECORDS)).toMatchObject({
      project: "acme",
      exclude_waiting: ["blocked"],
    });
  });
});

describe("refresh", () => {
  it("asks one count per node, in sidebar order, and pins each on badge_count", async () => {
    useUiStore.setState({ viewFilters: { "p-pocketbuddy": [NOT_DONE, ...NOT_WAITING] } });
    countAnswer = async () => [12, 4, 0];

    await useTaskStore.getState().refresh();

    expect(countCalls).toHaveLength(1);
    const specs = countCalls[0]!;
    expect(specs.map((s) => s.project)).toEqual(["cxventures", "acme", "pocketbuddy"]);
    expect(specs[2]).toMatchObject({ exclude_waiting: ["tripwire", "deferred", "blocked"] });
    expect(specs[0]!.exclude_waiting).toBeUndefined();

    const records = useTaskStore.getState().projectRecords;
    expect(records.map((p) => [p.name, p.badge_count, p.open_count])).toEqual([
      ["cxventures", 12, 16],
      ["acme", 4, 5],
      // Eight held tasks: the row is still drawn (open_count) but reads 0.
      ["pocketbuddy", 0, 8],
    ]);
  });

  it("leaves badge_count absent when the count fails, rather than failing the list", async () => {
    countAnswer = async () => {
      throw new Error("no such verb");
    };
    await useTaskStore.getState().refresh();
    const state = useTaskStore.getState();
    expect(state.error).toBeNull();
    expect(state.projectRecords.map((p) => p.badge_count)).toEqual([undefined, undefined, undefined]);
  });
});
