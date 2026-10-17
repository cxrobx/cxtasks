/**
 * Grouping the task list by project.
 *
 * The point of the feature is Inbox/Today/Upcoming — views that cut across the
 * whole portfolio, where "what's due today" is unreadable as one flat column.
 * What's pinned here is mostly the ways a grouped board can quietly lie:
 * headers in an order nobody chose, a shift-click range that ticks rows the
 * user can't see, and a stored preference that rewrites itself as you navigate.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { TaskList } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Project, Section, Task } from "@/types/task";

const update = vi.fn(async (_id: string, _patch: unknown) => {});

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      complete: vi.fn(),
      update: (id: string, patch: unknown) => update(id, patch),
      trash: vi.fn(),
    },
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

/** A finite project filed under an area — the v19 shape that gave grouping
 * something to do inside a scope you'd selected. */
function child(name: string, parent: string, sortOrder: number): Project {
  return { ...project(name, sortOrder), kind: "project" as const, parent };
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

/** A drag needs a `dataTransfer` — `TaskRow.onDragStart` calls `setData` on it,
 * and jsdom ships no DataTransfer, so without this the drag throws before any
 * of the drop wiring is reached. */
const DT = { dataTransfer: { setData: vi.fn(), effectAllowed: "" } };

/** The group headers, in the order they render. */
function groupLabels(): string[] {
  return screen
    // `query`, not `get`: an ungrouped list can legitimately have no buttons at
    // all, and `getAllByRole` throws on none rather than returning [].
    .queryAllByRole("button")
    .map((b) => b.textContent?.trim() ?? "")
    .filter((t) => LABELS.includes(t));
}
const LABELS = [
  "pocketbuddy",
  "artist advisory",
  "cxtasks",
  "cxventures",
  "gov certs",
  "music",
  "No project",
  "Ungrouped",
  "phase 1",
];

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
    loading: false,
    view: "today",
    activeProject: null,
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
  } as never);
  useUiStore.setState({ groupBy: "project" });
});

describe("grouping a view by project", () => {
  it("groups the rows and counts each group", () => {
    useTaskStore.setState({
      tasks: [
        task("a", { project: "pocketbuddy", title: "bank sync" }),
        task("b", { project: "cxtasks", title: "group by project" }),
        task("c", { project: "pocketbuddy", title: "FIRE chart" }),
      ],
      projectRecords: [project("pocketbuddy", 0), project("cxtasks", 1)],
    } as never);

    render(<TaskList />);

    expect(groupLabels()).toEqual(["pocketbuddy", "cxtasks"]);
    // The count belongs to the group, not the view — a header that always
    // reads the same number is a header nobody reads.
    const header = screen.getByRole("button", { name: "pocketbuddy" }).parentElement!;
    expect(header.textContent).toContain("2");
  });

  it("orders groups the way the sidebar does, not alphabetically", () => {
    // `zeta` first is a hand-set order (`reorderProjects`). Sorting these by
    // name here would silently throw that away, and the board would disagree
    // with the sidebar two panes to its left.
    useTaskStore.setState({
      tasks: [task("a", { project: "alpha" }), task("b", { project: "zeta" })],
      projectRecords: [project("zeta", 0), project("alpha", 1)],
    } as never);

    render(<TaskList />);

    const labels = screen
      .getAllByRole("button")
      .map((b) => b.textContent?.trim())
      .filter((t) => t === "zeta" || t === "alpha");
    expect(labels).toEqual(["zeta", "alpha"]);
  });

  it("puts project-less tasks in their own bucket, last", () => {
    useTaskStore.setState({
      tasks: [task("a", { project: null }), task("b", { project: "cxtasks" })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    render(<TaskList />);

    expect(groupLabels()).toEqual(["cxtasks", "No project"]);
  });

  it("stays a flat list when nothing carries a project", () => {
    // Otherwise the whole view gets one "No project" header over it, which
    // costs a row and says nothing.
    useTaskStore.setState({
      tasks: [task("a"), task("b")],
    } as never);

    render(<TaskList />);

    // No header at all, which is the assertion — the rows themselves aren't
    // checkable here because the ungrouped path is the VIRTUALIZED one, and
    // jsdom measures its scroll element at zero height, so it yields no items.
    // Every other test in this file renders through the grouped path, which is
    // deliberately not virtualized (gotcha #13).
    expect(groupLabels()).toEqual([]);
    expect(screen.queryByText("No project")).not.toBeInTheDocument();
  });

  it("is never applied to the Trash", () => {
    // Carving deleted work up by the project it used to be in is filing you'd
    // only ever do to things you're not doing.
    useTaskStore.setState({
      view: "trash",
      tasks: [task("a", { project: "cxtasks", deleted_at: "2026-08-01 09:00:00" })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    render(<TaskList />);

    expect(screen.queryByRole("button", { name: "cxtasks" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Trash" })).toBeInTheDocument();
  });
});

describe("the grouping control", () => {
  it("reflects the mode actually in force", () => {
    useTaskStore.setState({
      tasks: [task("a", { project: "cxtasks" })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    render(<TaskList />);

    expect(screen.getByRole("radio", { name: "Project" })).toHaveAttribute(
      "aria-checked",
      "true"
    );
    expect(screen.getByRole("radio", { name: "Sections" })).toHaveAttribute(
      "aria-checked",
      "false"
    );
  });

  it("falls back inside a project WITHOUT rewriting the preference", () => {
    // Every task in a project's list is already in that project, so the mode
    // can't apply — but storing the fallback would mean the mode never comes
    // back when you leave, which is a preference that edits itself as you
    // navigate.
    useTaskStore.setState({
      activeProject: "cxtasks",
      tasks: [task("a", { project: "cxtasks" })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    render(<TaskList />);

    const projectOption = screen.getByRole("radio", { name: "Project" });
    expect(projectOption).toBeDisabled();
    expect(projectOption).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByRole("button", { name: "cxtasks" })).not.toBeInTheDocument();
    expect(useUiStore.getState().groupBy).toBe("project");
  });

  it("comes back to life inside an AREA that holds projects", () => {
    // An area's list rolls its projects up (`db::projects::expand_scope`), so
    // there really is more than one project in it — and carving that back apart
    // is the only way to read an area whose work is spread across three. The
    // old rule ("inside anything, everything shares a project") predates areas
    // existing.
    useTaskStore.setState({
      activeProject: "cxventures",
      tasks: [
        task("a", { project: "cxventures", title: "renew the LLC" }),
        task("b", { project: "gov certs", title: "SAM.gov" }),
      ],
      projectRecords: [
        project("cxventures", 0),
        child("gov certs", "cxventures", 1),
      ],
    } as never);

    render(<TaskList />);

    expect(screen.getByRole("radio", { name: "Project" })).toBeEnabled();
    expect(groupLabels()).toEqual(["cxventures", "gov certs"]);
  });

  it("stays dead inside an area holding nothing", () => {
    // The greyed-out state is still the right answer for most of the sidebar:
    // an area with no projects under it is one pile with one name on it.
    useTaskStore.setState({
      activeProject: "music",
      tasks: [task("a", { project: "music" })],
      projectRecords: [
        project("music", 0),
        // Another area's project, to pin that the lookup is by PARENT and not
        // "does a project exist anywhere".
        child("gov certs", "cxventures", 1),
      ],
    } as never);

    render(<TaskList />);

    expect(screen.getByRole("radio", { name: "Project" })).toBeDisabled();
    expect(groupLabels()).toEqual([]);
  });

  it("only offers + Section while sections are what's on screen", () => {
    useTaskStore.setState({
      tasks: [task("a", { project: "cxtasks" })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    const { rerender } = render(<TaskList />);
    expect(screen.queryByRole("button", { name: "Section" })).not.toBeInTheDocument();

    useUiStore.setState({ groupBy: "section" });
    rerender(<TaskList />);
    expect(screen.getByRole("button", { name: "Section" })).toBeInTheDocument();
  });
});

describe("a shift-click range in a grouped list", () => {
  it("spans the rows between the two clicks ON SCREEN, not in `tasks` order", () => {
    // `tasks` arrives interleaved (a, b, c) but renders grouped as
    // (a, c | b). Ranging over the array would tick `b`, which sits in a
    // different group entirely — the one thing a range selection must never do.
    useTaskStore.setState({
      tasks: [
        task("a", { project: "pocketbuddy" }),
        task("b", { project: "cxtasks" }),
        task("c", { project: "pocketbuddy" }),
      ],
      projectRecords: [project("pocketbuddy", 0), project("cxtasks", 1)],
    } as never);

    render(<TaskList />);

    // Selection lives on the ROW now — the leading box closes the task. So the
    // range is built the way it is used: click a row, shift-click another.
    const row = (id: string) =>
      screen.getByText(`task ${id}`).closest("[role=option]")!;
    fireEvent.click(row("a"));
    fireEvent.click(row("c"), { shiftKey: true });

    expect(useTaskStore.getState().checkedIds.sort()).toEqual(["a", "c"]);
  });
});

describe("dragging a task onto a project group", () => {
  it("re-files it into that project", () => {
    useTaskStore.setState({
      tasks: [
        task("a", { project: "pocketbuddy" }),
        task("b", { project: "cxtasks" }),
      ],
      projectRecords: [project("pocketbuddy", 0), project("cxtasks", 1)],
    } as never);

    render(<TaskList />);

    fireEvent.dragStart(screen.getByText("task a").closest("[role=option]")!, DT);
    // Dropped on the HEADER, not the block — the drop is meant to bubble, so
    // anywhere in the group means the same thing.
    fireEvent.drop(screen.getByRole("button", { name: "cxtasks" }), DT);

    expect(update).toHaveBeenCalledWith("a", { project: "cxtasks" });
  });

  it("takes it out of every project when dropped on No project", () => {
    useTaskStore.setState({
      tasks: [task("a", { project: "cxtasks" }), task("b", { project: null })],
      projectRecords: [project("cxtasks", 0)],
    } as never);

    render(<TaskList />);

    fireEvent.dragStart(screen.getByText("task a").closest("[role=option]")!, DT);
    fireEvent.drop(screen.getByRole("button", { name: "No project" }), DT);

    // Explicit null, not omitted: `TaskPatch` reads an absent key as "leave
    // alone", so a cleared project has to be sent as one.
    expect(update).toHaveBeenCalledWith("a", { project: null });
  });
});

describe("sections, after the two grouped renders became one", () => {
  it("still group, with their Ungrouped bucket", () => {
    useUiStore.setState({ groupBy: "section" });
    useTaskStore.setState({
      sections: [section("s1", "phase 1")],
      tasks: [task("a", { section_id: "s1" }), task("b")],
    } as never);

    render(<TaskList />);

    expect(groupLabels()).toEqual(["phase 1", "Ungrouped"]);
    expect(screen.getByRole("button", { name: "Delete section phase 1" })).toBeInTheDocument();
  });
});
