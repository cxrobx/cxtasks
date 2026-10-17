/**
 * The filter strip's store half: chips compile into one `FilterSpec`, collide
 * sanely, and every navigation lands on the scope's own filters.
 *
 * What's pinned is the CONTRACT chips now have with the view. They REFINE it,
 * so every query carries the view and there is one path; and navigation resets
 * them to what the scope means rather than to nothing, because "no chips" is
 * not neutral — every open view applies `not Done`, so arriving with none
 * would show a year of finished work under a title that says Today.
 *
 * Both halves fail silently: the wrong query still returns tasks, just not the
 * ones the strip on screen claims.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { compileFilters, upsertFilter, useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import { DEFAULT_SORT } from "@/types/task";
import type { ActiveFilter, FilterSpec } from "@/types/task";

const filterCalls: FilterSpec[] = [];
const searchCalls: string[] = [];
const listCalls: unknown[] = [];

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      get: vi.fn(async () => null),
      list: vi.fn(async (filters: unknown) => {
        listCalls.push(filters);
        return [];
      }),
      search: vi.fn(async (query: string) => {
        searchCalls.push(query);
        return [];
      }),
      filter: vi.fn(async (spec: FilterSpec) => {
        filterCalls.push(spec);
        return [];
      }),
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

const chip = (
  facet: ActiveFilter["facet"],
  value: string,
  op: ActiveFilter["op"] = "is"
): ActiveFilter => ({ facet, op, value });

beforeEach(() => {
  filterCalls.length = 0;
  searchCalls.length = 0;
  listCalls.length = 0;
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

/** `.at(-1)` is ES2022 and this project targets ES2021 — see tsconfig. */
const lastFilter = (): FilterSpec | undefined => filterCalls[filterCalls.length - 1];

describe("compileFilters", () => {
  it("groups same-facet values into one OR list and keeps facets apart", () => {
    const spec = compileFilters(
      [
        chip("project", "acme"),
        chip("project", "initech"),
        chip("status", "done"),
        chip("tag", "billable"),
        chip("priority", "0"),
      ],
      ""
    );
    expect(spec).toEqual({
      projects: ["acme", "initech"],
      statuses: ["done"],
      tags: ["billable"],
      priorities: [0],
    });
  });

  it("routes date chips into the op-named slots", () => {
    const spec = compileFilters(
      [chip("due", "2026-08-01", "before"), chip("created", "2026-07-01", "after")],
      ""
    );
    expect(spec).toEqual({
      due_before: "2026-08-01",
      created_after: "2026-07-01",
    });
  });

  it("carries the free text only when it says something", () => {
    expect(compileFilters([chip("status", "done")], "   ").query).toBeUndefined();
    expect(compileFilters([chip("status", "done")], "webhook").query).toBe("webhook");
  });
});

describe("upsertFilter", () => {
  it("treats an exact duplicate as a no-op, returning the SAME array", () => {
    const existing = [chip("status", "done")];
    // Identity, not equality — `addFilter` uses it to skip the refresh.
    expect(upsertFilter(existing, chip("status", "done"))).toBe(existing);
  });

  it("accumulates different values of one facet", () => {
    const out = upsertFilter([chip("project", "acme")], chip("project", "initech"));
    expect(out).toHaveLength(2);
  });

  /** Two "due before" bounds can only mean the tighter one; keeping both
   * renders a chip that does nothing. Replacement is per SLOT — a `before`
   * chip must not evict an `after` chip, which together mean a range. */
  it("replaces the chip holding the same date slot, and only that one", () => {
    const start = [chip("due", "2026-08-01", "before"), chip("due", "2026-07-01", "after")];
    const out = upsertFilter(start, chip("due", "2026-08-15", "before"));
    expect(out).toHaveLength(2);
    expect(out.find((f) => f.op === "before")?.value).toBe("2026-08-15");
    expect(out.find((f) => f.op === "after")?.value).toBe("2026-07-01");
  });
});

describe("refresh routing", () => {
  /** ONE query path. Text, chips, the view and the node all ride on a single
   * `FilterSpec` — while they were two builders, a facet could mean one thing
   * in the sidebar and another in the strip, which is what invariant 21 was
   * rewritten to prevent. */
  it("sends everything down the one filter path, carrying the view", async () => {
    await useTaskStore.getState().setQuery("webhook");
    expect(searchCalls).toHaveLength(0);
    expect(filterCalls).toHaveLength(1);
    expect(filterCalls[0]).toMatchObject({ query: "webhook", view: "today" });
  });

  it("keeps the view on the query when a chip is added", async () => {
    await useTaskStore.getState().addFilter(chip("priority", "0"));
    expect(lastFilter()).toMatchObject({ view: "today", priorities: [0] });
  });

  /** The negative operator has to reach a DIFFERENT field, or "is not Done"
   * would silently ask for exactly what it excludes. */
  it("routes an is_not chip into the exclude_ slot", async () => {
    await useTaskStore.getState().addFilter(chip("status", "done", "is_not"));
    expect(lastFilter()).toMatchObject({ exclude_statuses: ["done"] });
    expect(lastFilter()?.statuses).toBeUndefined();
  });

  /** `exclude_closed` must go over as FALSE. The server defaults it to true
   * for view-less callers, so leaving it off would re-apply the "not Done"
   * rule the user just removed — the chip would come off and change nothing. */
  it("always says it owns its own closure rule", async () => {
    await useTaskStore.getState().setQuery("anything");
    expect(lastFilter()?.exclude_closed).toBe(false);
  });

  it("does not refresh for a duplicate chip", async () => {
    await useTaskStore.getState().addFilter(chip("status", "in_review"));
    const before = filterCalls.length;
    await useTaskStore.getState().addFilter(chip("status", "in_review"));
    expect(filterCalls).toHaveLength(before);
  });

  it("carries the sort", async () => {
    useUiStore.setState({ sort: { key: "due", dir: "desc" } });
    await useTaskStore.getState().setQuery("x");
    expect(lastFilter()?.sort).toEqual({ key: "due", dir: "desc" });
  });
});

describe("navigation", () => {
  /** Navigation used to clear the chips outright. It resets them to the
   * scope's own filters now: a view click that landed on an EMPTY strip would
   * drop the `not Done` every open view applies, and a year of finished work
   * would appear under a title that says Inbox. */
  it("every sidebar navigation resets the chips to the view's defaults", async () => {
    const notDone = { facet: "status", op: "is_not", value: "done" };
    const enter = () => useTaskStore.setState({ filters: [chip("priority", "0")] });

    enter();
    await useTaskStore.getState().setView("inbox");
    expect(useTaskStore.getState().filters).toEqual([notDone]);

    enter();
    await useTaskStore.getState().setProject("acme");
    expect(useTaskStore.getState().filters).toEqual([notDone]);

    // Done is the exception: `not Done` there is a guaranteed empty list.
    enter();
    await useTaskStore.getState().setView("done");
    expect(useTaskStore.getState().filters).toEqual([]);
  });

  /** A saved default IS what that node means, so entering it applies one. */
  it("applies a node's saved filters when entering it", async () => {
    useTaskStore.setState({
      projectRecords: [
        { id: "p1", name: "Acme", parent: null, kind: "project" } as never,
      ],
    });
    useUiStore.setState({
      viewFilters: { p1: [{ facet: "waiting", op: "is_not", value: "tripwire" }] },
    });
    await useTaskStore.getState().setProject("Acme");
    expect(useTaskStore.getState().filters).toEqual([
      { facet: "waiting", op: "is_not", value: "tripwire" },
    ]);
    expect(lastFilter()).toMatchObject({ exclude_waiting: ["tripwire"] });
  });

  /** A project with nothing of its own inherits its AREA's — project → area
   * may inherit (invariant 22), the direction `primary_repo` already falls
   * back in. Without it, a default saved on the area would stop applying the
   * moment you clicked into a project underneath it. */
  it("a project with no saved filters inherits its area's", async () => {
    useTaskStore.setState({
      projectRecords: [
        { id: "area", name: "CXVentures", parent: null, kind: "area" } as never,
        { id: "proj", name: "Acme", parent: "CXVentures", kind: "project" } as never,
      ],
    });
    useUiStore.setState({
      viewFilters: { area: [{ facet: "status", op: "is_not", value: "in_review" }] },
    });
    await useTaskStore.getState().setProject("Acme");
    expect(useTaskStore.getState().filters).toEqual([
      { facet: "status", op: "is_not", value: "in_review" },
    ]);
  });

  it("clearSearch drops the text and returns to the scope's filters", async () => {
    useTaskStore.setState({ query: "webhook", filters: [chip("priority", "0")] });
    await useTaskStore.getState().clearSearch();
    const s = useTaskStore.getState();
    expect(s.query).toBe("");
    expect(s.filters).toEqual([{ facet: "status", op: "is_not", value: "done" }]);
  });

  /** A tag click is a chip like any other now — it must not wipe the view's
   * own filters on its way in. */
  it("selecting a tag keeps the view's defaults and adds the tag", async () => {
    await useTaskStore.getState().setView("today");
    await useTaskStore.getState().selectOnlyTag("billable");
    expect(useTaskStore.getState().filters).toEqual([
      { facet: "status", op: "is_not", value: "done" },
      { facet: "tag", op: "is", value: "billable" },
    ]);
  });
});
