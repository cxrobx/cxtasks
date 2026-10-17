/**
 * Below Notes: one launch line, then Activity · Subtasks · Links.
 *
 * What can go wrong is what a tab HIDES: a blocker on a tab you didn't pick,
 * a default decided against lists that hadn't loaded yet, and the set-aside
 * actions moving somewhere a held-out task can't reach them. Driven through
 * the real `TaskDetail`, the way `launchConsole.test.tsx` is.
 */
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TaskDetail } from "@/components/tasks/TaskDetail";
import { useTaskStore } from "@/stores/taskStore";
import type { Task, TaskRelations } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: {
    dispatch: {
      executorAvailable: vi.fn(async () => null),
      queueBackground: vi.fn(),
      foreground: vi.fn(),
      prompt: vi.fn(),
    },
    runner: { runOnce: vi.fn(), install: vi.fn() },
    appearance: { setNative: vi.fn(async () => {}) },
    tripwires: { scripts: vi.fn(async () => []), runtime: vi.fn(async () => ({})) },
  },
}));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ writeText: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (p: string) => p }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

const TASK = {
  id: "t1",
  title: "coach panel empty state",
  body: null,
  status: "inbox",
  blocked_reason: null,
  priority: 2,
  project: null,
  due_at: null,
  defer_until: null,
  recurrence: null,
  parent_id: null,
  sort_order: 0,
  created_at: "2026-09-01 10:00:00",
  updated_at: "2026-09-01 10:00:00",
  completed_at: null,
  repo_path: null,
  prompt: null,
  bg_allowed: false,
  bg_executor: "claude",
  bg_intent: "work",
  bg_writes_allowed: false,
  bg_shell_allowed: false,
  bg_remote_allowed: false,
  bg_mcp_servers: null,
  reminded_at: null,
  model: null,
  section_id: null,
  deleted_at: null,
  archived_at: null,
  short_id: 211,
  tripwire: null,
  tripwire_every_minutes: null,
  tripwire_on_trip: null,
  tripwire_state: null,
  tripwire_tripped_at: null,
} as Task;

const OTHER = { ...TASK, id: "t2", title: "cut the studio export", short_id: 188 } as Task;

const NO_RELATIONS: TaskRelations = {
  blocked_by: [],
  blocking: [],
  is_blocked: false,
  supersedes: [],
  superseded_by: [],
  related: [],
  is_superseded: false,
};

const store = {
  trash: vi.fn(async () => {}),
  archive: vi.fn(async () => {}),
  addDependency: vi.fn(async () => null),
};

function mount(
  overrides: Partial<Task> = {},
  { subtasks = [] as Task[], relations = NO_RELATIONS } = {}
) {
  useTaskStore.setState({
    tasks: [{ ...TASK, ...overrides }, OTHER],
    selectedId: TASK.id,
    subtasks,
    runs: [],
    repos: [],
    projectRecords: [],
    projectRepos: [],
    sections: [],
    tags: [],
    allTags: [],
    comments: [],
    attachments: [],
    relations,
    tripwireByTask: {},
    patch: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
    ...store,
  } as never);
  return render(<TaskDetail />);
}

const tab = (name: RegExp) => screen.getByRole("tab", { name });

beforeEach(() => vi.clearAllMocks());

describe("a bare task", () => {
  it("renders no section that only says it is empty, and no native select", () => {
    const { container } = mount();
    for (const empty of [/Nothing blocking this/, /No continuity links/, /Never dispatched/]) {
      expect(screen.queryByText(empty)).toBeNull();
    }
    // Gotcha #6: WKWebView pops a native <select> up light-grey.
    expect(container.querySelector("select")).toBeNull();
    expect(tab(/Activity/)).toHaveAttribute("aria-selected", "true");
  });

  it("offers the launch console as one faint line when there is no workspace", () => {
    mount();
    expect(screen.getByTitle(/Show the launch console/)).toHaveTextContent("Hand to an agent…");
    expect(screen.queryByText("Needs repo")).toBeNull();
  });
});

describe("the tabs", () => {
  it("default to the first one holding something", () => {
    mount({}, { subtasks: [{ ...OTHER, id: "s1", title: "write the copy", parent_id: TASK.id }] });
    expect(tab(/Subtasks/)).toHaveAttribute("aria-selected", "true");
    expect(tab(/Subtasks/)).toHaveTextContent("0/1");
    expect(screen.getByText("write the copy")).toBeInTheDocument();
  });

  it("re-derive the default when the lists arrive after the pane mounted", () => {
    mount();
    expect(tab(/Activity/)).toHaveAttribute("aria-selected", "true");
    act(() => {
      useTaskStore.setState({
        relations: { ...NO_RELATIONS, related: [{ task_id: "t2", short_id: 188, title: "cut the studio export", status: "todo", priority: 2, project: null, created_at: "" }] },
      });
    });
    expect(tab(/Links/)).toHaveAttribute("aria-selected", "true");
  });

  it("a picked tab stays picked", () => {
    mount({}, { subtasks: [{ ...OTHER, id: "s1", title: "write the copy", parent_id: TASK.id }] });
    fireEvent.click(tab(/Links/));
    expect(tab(/Links/)).toHaveAttribute("aria-selected", "true");
  });

  /**
   * The one link that changes what you can do must not be hidden by being on
   * a tab you didn't pick: its count goes red on the closed tab.
   */
  it("an open blocker turns the Links count red while another tab is showing", () => {
    mount(
      {},
      {
        subtasks: [{ ...OTHER, id: "s1", title: "write the copy", parent_id: TASK.id }],
        relations: {
          ...NO_RELATIONS,
          blocked_by: [{ task_id: "t2", title: "cut the studio export", status: "todo", priority: 2, project: null, is_open: true }],
          is_blocked: true,
        },
      }
    );
    expect(tab(/Subtasks/)).toHaveAttribute("aria-selected", "true");
    const count = within(tab(/Links/)).getByText("1");
    expect(count.className).toMatch(/text-error/);
  });
});

describe("the Links tab", () => {
  it("lists each link with its kind, and adds a blocker from the app's own picker", async () => {
    mount(
      {},
      {
        relations: {
          ...NO_RELATIONS,
          related: [{ task_id: "t9", short_id: 9, title: "an older idea", status: "todo", priority: 2, project: null, created_at: "" }],
        },
      }
    );
    fireEvent.click(tab(/Links/));
    expect(screen.getByText("relates to")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Blocked by…/ }));
    const input = screen.getByRole("combobox", { name: "Task this one waits on" });
    // A pick writes the option's text into the field; that is the commit.
    fireEvent.change(input, { target: { value: "T188 · cut the studio export" } });
    expect(store.addDependency).toHaveBeenCalledWith("t1", "t2");
  });

  it("typing part of a title does not link anything", () => {
    mount();
    fireEvent.click(tab(/Links/));
    fireEvent.click(screen.getByRole("button", { name: /Blocked by…/ }));
    fireEvent.change(screen.getByRole("combobox", { name: "Task this one waits on" }), {
      target: { value: "cut the" },
    });
    expect(store.addDependency).not.toHaveBeenCalled();
  });
});

describe("setting a task aside", () => {
  /** Radix menus open on a keydown from their trigger in jsdom. */
  it("Archive and Move to Trash live behind ⋯", async () => {
    mount();
    expect(screen.queryByRole("button", { name: /Move to Trash/ })).toBeNull();
    fireEvent.keyDown(screen.getByRole("button", { name: "More actions" }), { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitem", { name: /Move to Trash/ }));
    expect(store.trash).toHaveBeenCalledWith("t1");
  });

  it("a task already set aside keeps its way back in plain sight, and no ⋯", () => {
    mount({ archived_at: "2026-09-20 10:00:00" });
    expect(screen.getByRole("button", { name: /Move back to tasks/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "More actions" })).toBeNull();
  });
});
