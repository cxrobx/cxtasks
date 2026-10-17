import { beforeEach, describe, expect, it, vi } from "vitest";

/** The store reads localStorage once, at module scope. */
async function freshStore() {
  vi.resetModules();
  const { useUiStore } = await import("@/stores/uiStore");
  return useUiStore.getState();
}

beforeEach(() => localStorage.clear());

describe("Areas section preference", () => {
  it("restores its collapsed state on launch", async () => {
    localStorage.setItem("cxtasks.areasCollapsed", "1");
    expect((await freshStore()).areasCollapsed).toBe(true);
  });
});

describe("Show hidden preference", () => {
  it("starts off, and restores on launch", async () => {
    expect((await freshStore()).showHidden).toBe(false);
    localStorage.setItem("cxtasks.showHidden", "1");
    expect((await freshStore()).showHidden).toBe(true);
  });
});

describe("the launch console's collapse state", () => {
  it("starts collapsed", async () => {
    expect((await freshStore()).launchCollapsed).toBe(true);
  });

  /**
   * It is scoped to the task you are looking at — `TaskDetail` re-collapses it
   * on every `task.id` — so writing it to storage would persist a value that
   * the next task open unconditionally overwrites. A stored preference nobody
   * reads is worse than none: it looks like a setting.
   */
  it("is not persisted, because every task open resets it", async () => {
    const store = await freshStore();
    store.toggleLaunchConsole();
    expect(
      Object.keys(localStorage).filter((k) => k.includes("launch"))
    ).toHaveLength(0);
  });

  it("still toggles within the task you are on", async () => {
    const { useUiStore } = await import("@/stores/uiStore");
    useUiStore.setState({ launchCollapsed: true });
    useUiStore.getState().toggleLaunchConsole();
    expect(useUiStore.getState().launchCollapsed).toBe(false);
    useUiStore.getState().toggleLaunchConsole();
    expect(useUiStore.getState().launchCollapsed).toBe(true);
  });

  /** The flags that ARE remembered must stay that way. */
  it("leaves the genuinely persisted flags alone", async () => {
    localStorage.setItem("cxtasks.missionControl", "1");
    const store = await freshStore();
    expect(store.missionControl).toBe(true);
    expect(store.detailCollapsed).toBe(false);
  });
});

/**
 * The sidebar's view order.
 *
 * Every case here is about the same property: the saved order is a SORT KEY
 * over the shipped list, never a copy of it. Store the list itself and each of
 * these turns into a sidebar that is missing rows, showing dead ones, or
 * rendering one row twice.
 */
describe("orderViews", () => {
  const CANONICAL = ["gsd", "inbox", "today", "all"] as const;

  it("falls back to the shipped order when nothing is saved", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    expect(orderViews([], CANONICAL)).toEqual([...CANONICAL]);
  });

  it("applies a saved permutation", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    expect(orderViews(["today", "all", "gsd", "inbox"], CANONICAL)).toEqual([
      "today",
      "all",
      "gsd",
      "inbox",
    ]);
  });

  /**
   * The case that decides whether this feature survives the next release: add a
   * view and everyone who has ever dragged a row must still see it. Appended at
   * the end, where it is visible and movable.
   */
  it("appends a view the saved order has never heard of", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    expect(orderViews(["today", "gsd"], CANONICAL)).toEqual([
      "today",
      "gsd",
      "inbox",
      "all",
    ]);
  });

  /** A retired view can't leave a dead row — or an `undefined` one — behind. */
  it("drops a saved key that no longer exists", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    expect(
      orderViews(["today", "next", "gsd", "inbox", "all"] as never, CANONICAL)
    ).toEqual(["today", "gsd", "inbox", "all"]);
  });

  /**
   * The concrete case of the rule above: everyone who has used this app has
   * `"upcoming"` in their saved order, and that view is gone. It must drop out
   * silently and leave the rows that remain in the sequence they were dragged
   * into — not renumber them, and not render a row for a view that no longer
   * exists.
   */
  it("drops the retired Upcoming row from a real saved order", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    const SHIPPED = ["gsd", "inbox", "today", "waiting", "in_review", "open", "done"] as const;
    expect(
      orderViews(
        ["today", "upcoming", "gsd", "inbox", "waiting", "in_review", "open", "done"] as never,
        SHIPPED
      )
    ).toEqual(["today", "gsd", "inbox", "waiting", "in_review", "open", "done"]);
  });

  /** A duplicate would render one row twice under one React key. */
  it("collapses a duplicated key", async () => {
    const { orderViews } = await import("@/stores/uiStore");
    expect(orderViews(["all", "all", "gsd"], CANONICAL)).toEqual([
      "all",
      "gsd",
      "inbox",
      "today",
    ]);
  });
});

describe("the saved view order", () => {
  it("starts empty, meaning no opinion", async () => {
    expect((await freshStore()).viewOrder).toEqual([]);
  });

  it("round-trips through storage", async () => {
    const store = await freshStore();
    store.setViewOrder(["today", "inbox", "gsd"]);
    expect((await freshStore()).viewOrder).toEqual(["today", "inbox", "gsd"]);
  });

  /**
   * Degrades to "no opinion", not to a throw — this is read at module scope, so
   * anything that escapes it is a blank app rather than a lost preference. Same
   * discipline as `readGroupBy`.
   */
  it("survives a corrupt entry", async () => {
    localStorage.setItem("cxtasks.viewOrder", "{not json");
    expect((await freshStore()).viewOrder).toEqual([]);
  });

  it("survives an entry that parses but isn't a list of names", async () => {
    localStorage.setItem("cxtasks.viewOrder", '{"inbox":1}');
    expect((await freshStore()).viewOrder).toEqual([]);
  });

  it("keeps only the string entries of a mixed list", async () => {
    localStorage.setItem("cxtasks.viewOrder", '["today",null,7,"gsd"]');
    expect((await freshStore()).viewOrder).toEqual(["today", "gsd"]);
  });
});

describe("the sort preference", () => {
  it("starts at the view's own order, which is the app before this existed", async () => {
    expect((await freshStore()).sort).toEqual({ key: "default", dir: "asc" });
  });

  it("comes back after a restart", async () => {
    (await freshStore()).setSort({ key: "due", dir: "desc" });
    expect((await freshStore()).sort).toEqual({ key: "due", dir: "desc" });
  });

  /**
   * Every unreadable state means "no opinion", and the no-opinion sort is
   * `default` — `readGroupBy`'s rule. A key from a newer build must degrade to
   * the view's own order rather than reaching the backend as a sort column it
   * has never heard of, which comes back as a validation error the user cannot
   * connect to anything they did.
   */
  it("degrades to default for anything it cannot read", async () => {
    for (const raw of ['{"key":"colour","dir":"asc"}', '{"key":"due"}', "{{", "null", "7"]) {
      localStorage.setItem("cxtasks.sortBy", raw);
      expect((await freshStore()).sort).toEqual({ key: "default", dir: "asc" });
    }
  });
});

describe("saved view filters", () => {
  it("start empty and survive a restart, keyed by view or project id", async () => {
    expect((await freshStore()).viewFilters).toEqual({});
    (await freshStore()).saveViewFilters("p-123", [
      { facet: "status", op: "is_not", value: "in_review" },
    ]);
    expect((await freshStore()).viewFilters).toEqual({
      "p-123": [{ facet: "status", op: "is_not", value: "in_review" }],
    });
  });

  it("forgets one scope without touching the others", async () => {
    const s = await freshStore();
    s.saveViewFilters("a", [{ facet: "status", op: "is_not", value: "done" }]);
    s.saveViewFilters("b", [{ facet: "priority", op: "is", value: "0" }]);
    s.forgetViewFilters("a");
    const after = await freshStore();
    expect(after.viewFilters).toEqual({
      b: [{ facet: "priority", op: "is", value: "0" }],
    });
  });

  /**
   * A half-written chip is worse than none: it is replayed straight into a
   * query, so it would reach the backend as a facet with an undefined value
   * and come back as an error about something the user never typed. Dropping
   * it lands the scope on its baseline instead — the state it had before
   * anything was saved.
   */
  it("drops entries it cannot trust rather than replaying them", async () => {
    localStorage.setItem(
      "cxtasks.viewFilters",
      JSON.stringify({
        ok: [{ facet: "status", op: "is_not", value: "done" }],
        partial: [{ facet: "status", op: "is_not" }],
        wrong: "not an array",
      })
    );
    const s = await freshStore();
    expect(s.viewFilters.ok).toHaveLength(1);
    expect(s.viewFilters.partial).toEqual([]);
    expect(s.viewFilters.wrong).toBeUndefined();
  });

  it("survives unparseable storage", async () => {
    localStorage.setItem("cxtasks.viewFilters", "{{{");
    expect((await freshStore()).viewFilters).toEqual({});
  });
});

describe("row density", () => {
  it("starts comfortable — the row this app has always drawn", async () => {
    expect((await freshStore()).density).toBe("comfortable");
  });

  it("comes back after a restart and stamps the attribute", async () => {
    (await freshStore()).setDensity("compact");
    expect(document.documentElement.getAttribute("data-density")).toBe("compact");
    expect((await freshStore()).density).toBe("compact");
  });

  it("degrades to comfortable for anything it cannot read", async () => {
    for (const raw of ["cosy", "", "ULTRA"]) {
      localStorage.setItem("cxtasks.density", raw);
      expect((await freshStore()).density).toBe("comfortable");
    }
  });

  /**
   * The virtualizer sizes its scrollbar from the estimate until each row is
   * measured, so an estimate that does not follow the padding makes the thumb
   * jump on the first scroll (gotcha #33). Pinned as a pair — every mode needs
   * a number, and denser has to mean smaller — because the failure is a
   * scrollbar that feels broken rather than anything that errors.
   */
  it("has a row estimate for every mode, decreasing with density", async () => {
    const { DENSITY_OPTIONS, DENSITY_ROW_ESTIMATE } = await import("@/stores/uiStore");
    const sizes = DENSITY_OPTIONS.map((o) => DENSITY_ROW_ESTIMATE[o.value]);
    expect(sizes).toHaveLength(DENSITY_OPTIONS.length);
    expect(sizes.every((n) => Number.isFinite(n) && n > 0)).toBe(true);
    expect([...sizes].sort((a, b) => b - a)).toEqual(sizes);
  });
});
