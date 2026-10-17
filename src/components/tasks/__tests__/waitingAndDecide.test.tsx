/**
 * The Waiting view and Today's Decide strip — two lists that group by
 * something other than project or section.
 *
 * Waiting groups by WHY a task is held back, in the order the runner judges
 * the gates (date first — invariant 26), ignores the stored grouping
 * preference, and prints under each row the one fact the group can't say: the
 * tripwire's last note, the start date, the blocker. Decide puts the open
 * decisions first on Today and closes one with a typed line as its handoff.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { TaskList } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Task, TripwireRuntime } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: { complete: vi.fn(), update: vi.fn(), trash: vi.fn() },
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
    project: "cxventures",
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
    tripwire: null,
    tripwire_every_minutes: null,
    tripwire_on_trip: null,
    tripwire_state: null,
    tripwire_tripped_at: null,
    ...overrides,
  } as Task;
}

const HEADERS = [
  "Until a date",
  "Until a condition",
  "Behind another task",
  "Waiting on something",
  "Decide",
  "Today",
  "cxventures",
];

/** Group headers only — the Decide line's submit button also says "Decide",
 * and a header is the one button rendered in small caps. */
function headers(): string[] {
  return screen
    .queryAllByRole("button")
    .filter((b) => b.className.includes("uppercase"))
    .map((b) => b.textContent?.trim() ?? "")
    .filter((t) => HEADERS.includes(t));
}

const complete = vi.fn(async (_id: string, _answer?: string) => {});

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
    tagsByTask: {},
    tripwireByTask: {},
    selectedId: null,
    loading: false,
    view: "waiting",
    activeProject: null,
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
    complete,
  } as never);
  useUiStore.setState({ groupBy: "project" });
});

describe("the Waiting view", () => {
  it("groups by reason in gate order, whatever the grouping preference says", () => {
    useTaskStore.setState({
      tasks: [
        task("blocked", { blocked_reason: "waiting on the W-9" }),
        task("behind"),
        task("armed", { tripwire: "script:pocketbuddy-users 50", tripwire_state: "armed" }),
        task("deferred", { defer_until: "2099-01-01T00:00:00.000Z" }),
        // Deferred AND armed: the date is judged first, so it files under the date.
        task("both", {
          defer_until: "2099-01-01T00:00:00.000Z",
          tripwire: "script:x",
          tripwire_state: "armed",
        }),
      ],
      blockedIds: ["behind"],
      tripwireByTask: {
        armed: {
          task_id: "armed",
          checked_at: "2026-08-25 04:05:25",
          last_outcome: "not_yet",
          last_note: "0/50 outside users — 4 signups, 0 paying",
        } as unknown as TripwireRuntime,
      },
    });
    render(<TaskList />);

    expect(headers()).toEqual([
      "Until a date",
      "Until a condition",
      "Behind another task",
      "Waiting on something",
    ]);
    // No project header, although the preference is "project" and every row
    // has one — Waiting is grouped by reason and by nothing else.
    expect(screen.queryByRole("radiogroup", { name: "Group tasks by" })).toBeNull();

    const lines = screen.getAllByTestId("waiting-evidence").map((e) => e.textContent);
    expect(lines.some((l) => l?.startsWith("Last check: 0/50 outside users"))).toBe(true);
    expect(lines.some((l) => l?.startsWith("Starts "))).toBe(true);
    expect(lines.some((l) => l?.startsWith("Waiting on another task"))).toBe(true);
    expect(lines.some((l) => l?.startsWith("waiting on the W-9"))).toBe(true);
    expect(lines.every((l) => /filed .+ ago$/.test(l ?? ""))).toBe(true);
  });

  it("drops an empty reason rather than drawing a header over nothing", () => {
    useTaskStore.setState({
      tasks: [task("deferred", { defer_until: "2099-01-01T00:00:00.000Z" })],
    });
    render(<TaskList />);
    expect(headers()).toEqual(["Until a date"]);
  });
});

describe("Today's Decide strip", () => {
  it("puts the decisions first and closes one with the typed line as its handoff", () => {
    useTaskStore.setState({
      view: "today",
      tasks: [
        task("work", { title: "fix the webhook", due_at: "2026-08-25T17:00:00.000Z" }),
        task("d1", { title: "pause or continue the intake sequence?" }),
      ],
      tagsByTask: { d1: ["decision"] },
    });
    useUiStore.setState({ groupBy: "none" });
    render(<TaskList />);

    expect(headers()).toEqual(["Decide", "Today"]);

    const line = screen.getByRole("textbox", {
      name: "Decide: pause or continue the intake sequence?",
    });
    expect(screen.queryByRole("textbox", { name: "Decide: fix the webhook" })).toBeNull();

    fireEvent.change(line, { target: { value: "pause it — 25 touches, zero honest replies" } });
    fireEvent.submit(line.closest("form")!);
    expect(complete).toHaveBeenCalledWith("d1", "pause it — 25 touches, zero honest replies");
  });

  it("is absent when nothing is tagged, so a plain Today is unchanged", () => {
    useTaskStore.setState({
      view: "today",
      tasks: [task("work", { due_at: "2026-08-25T17:00:00.000Z" })],
    });
    useUiStore.setState({ groupBy: "none" });
    render(<TaskList />);
    expect(headers()).toEqual([]);
  });

  it("renders a person tag as @name, not #@name", () => {
    useTaskStore.setState({
      view: "today",
      tasks: [task("t", { due_at: "2026-08-25T17:00:00.000Z" })],
      tagsByTask: { t: ["@sam", "govcon"] },
    });
    // Grouped, so the rows render in jsdom — the flat list is virtualized
    // and measures nothing there.
    useUiStore.setState({ groupBy: "project" });
    render(<TaskList />);
    expect(screen.getByText("@sam")).toBeTruthy();
    expect(screen.getByText("#govcon")).toBeTruthy();
  });
});
