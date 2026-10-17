/**
 * The launch console's rail, as the control surface.
 *
 * These pin the two properties the redesign turns on. **One editor at a time**
 * is what makes the console small — regress it and you are back to the stacked
 * fieldsets it replaced, just with extra clicks. And the **availability warning
 * has to outlive its editor**: it lives at rail level precisely because a closed
 * runner editor must not hide the reason an unattended run would fail, which is
 * the whole point of checking before queue time rather than at it.
 *
 * Driven through the real `TaskDetail` rather than a lifted `ExecutionConsole`,
 * so the store wiring and the collapse preference are exercised too.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { TaskDetail } from "@/components/tasks/TaskDetail";
import { useTaskStore } from "@/stores/taskStore";
import type { BackgroundExecutor, Task, TaskRun } from "@/types/task";

const executorAvailable = vi.fn<(e: BackgroundExecutor) => Promise<string | null>>();

vi.mock("@/lib/tauri", () => ({
  api: {
    dispatch: {
      executorAvailable: (e: BackgroundExecutor) => executorAvailable(e),
      queueBackground: vi.fn(),
      foreground: vi.fn(),
      prompt: vi.fn(),
    },
    runner: { runOnce: vi.fn(), install: vi.fn() },
    appearance: { setNative: vi.fn(async () => {}) },
    tripwires: { scripts: vi.fn(async () => []), runtime: vi.fn(async () => ({})) },
  },
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(async () => {}),
}));

const TASK: Task = {
  id: "t1",
  title: "wire the retry queue",
  body: null,
  status: "todo",
  blocked_reason: null,
  priority: 3,
  project: "cxtasks",
  due_at: null,
  defer_until: null,
  recurrence: null,
  parent_id: null,
  sort_order: 0,
  created_at: "2026-08-01 10:00:00",
  updated_at: "2026-08-01 10:00:00",
  completed_at: null,
  repo_path: "/Users/me/Projects/cxtasks",
  prompt: null,
  bg_allowed: true,
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
  short_id: 42,
  tripwire: null,
  tripwire_every_minutes: null,
  tripwire_on_trip: null,
  tripwire_state: null,
  tripwire_tripped_at: null,
};

function mount(
  overrides: Partial<Task> = {},
  { expand = true, runs = [] as TaskRun[] } = {}
) {
  useTaskStore.setState({
    tasks: [{ ...TASK, ...overrides }],
    selectedId: TASK.id,
    subtasks: [],
    runs,
    repos: [{ name: "cxtasks", path: "/Users/me/Projects/cxtasks" }],
    projectRecords: [],
    projectRepos: [],
    sections: [],
    tags: [],
    comments: [],
    attachments: [],
    relations: { blocked_by: [], blocking: [], is_blocked: false },
    patch: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
    backgroundIdle: null,
    setBackgroundIdle: vi.fn(),
  } as never);
  const result = render(<TaskDetail />);
  // Every task now opens with the console collapsed, so anything exercising
  // the rail has to open it first — through the header, the way a user does.
  if (expand) fireEvent.click(screen.getByTitle(/Show the launch console/));
  return result;
}

const node = (name: RegExp) => screen.getByRole("button", { name });

beforeEach(() => {
  vi.clearAllMocks();
  executorAvailable.mockResolvedValue(null);
});

describe("the launch console rail", () => {
  // The rail used to be a picture of the settings with the real controls
  // duplicated underneath it. Every node being a button IS the change.
  it("offers all four settings as controls", () => {
    mount();
    for (const label of [/Workspace/, /Runner/, /Capability/, /Brief/]) {
      expect(node(label)).toHaveAttribute("aria-expanded", "false");
    }
  });

  it("opens a node's editor and marks that node expanded", () => {
    mount();
    expect(screen.queryByText(/Cannot run commands/)).toBeNull();
    fireEvent.click(node(/Capability/));
    expect(node(/Capability/)).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("radio", { name: /Inspect/ })).toBeChecked();
  });

  /**
   * The property the whole space saving rests on. Two editors open at once is
   * the old always-on stack re-forming one field at a time.
   */
  it("keeps exactly one editor open", () => {
    mount();
    fireEvent.click(node(/Capability/));
    fireEvent.click(node(/Runner/));
    expect(node(/Runner/)).toHaveAttribute("aria-expanded", "true");
    expect(node(/Capability/)).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("radio", { name: /Inspect/ })).toBeNull();
  });

  it("closes the editor when its own node is clicked again", () => {
    mount();
    fireEvent.click(node(/Brief/));
    expect(screen.getByPlaceholderText(/What should the agent do first/)).toBeInTheDocument();
    fireEvent.click(node(/Brief/));
    expect(screen.queryByPlaceholderText(/What should the agent do first/)).toBeNull();
  });

  // The workspace is the only setting that can be genuinely unset, and the rail
  // has to say so — it is the one node that blocks the launch.
  it("names the repo's folder, or says it is missing", () => {
    const { unmount } = mount();
    expect(node(/Workspace/)).toHaveTextContent("cxtasks");
    unmount();
    mount({ repo_path: null });
    expect(node(/Workspace/)).toHaveTextContent("Not set");
  });
});

/**
 * The hazard the always-on fieldsets did not have.
 *
 * `repo_path` and `prompt` are drafts committed on BLUR. Now that their fields
 * unmount when another node is clicked — or the console is collapsed — blur is
 * no longer guaranteed: a focused element removed from the DOM does not reliably
 * fire one. Losing it means the typed value stays on screen, is never written,
 * and vanishes at the next live-refresh, which is the worst of the three
 * possible outcomes.
 */
describe("leaving an editor with unsaved text", () => {
  const typedRepo = "/Users/me/Projects/artisttracker";

  it("commits the workspace when another node takes over", () => {
    mount();
    fireEvent.click(node(/Workspace/));
    fireEvent.change(screen.getByPlaceholderText(/Choose a repository/), {
      target: { value: typedRepo },
    });
    fireEvent.click(node(/Runner/));
    expect(useTaskStore.getState().patch).toHaveBeenCalledWith("t1", {
      repo_path: typedRepo,
    });
  });

  it("commits the brief when the console is collapsed", () => {
    mount();
    fireEvent.click(node(/Brief/));
    fireEvent.change(screen.getByPlaceholderText(/What should the agent do first/), {
      target: { value: "start with the failing test" },
    });
    fireEvent.click(screen.getByTitle(/Collapse the launch console/));
    expect(useTaskStore.getState().patch).toHaveBeenCalledWith("t1", {
      prompt: "start with the failing test",
    });
  });

  // Closing an untouched editor must not write. `patch` bumps `updated_at`,
  // which resets every draft in the pane — so a spurious write on every glance
  // at the rail would discard text being typed elsewhere.
  it("writes nothing when the editor was only looked at", () => {
    mount();
    fireEvent.click(node(/Workspace/));
    fireEvent.click(node(/Capability/));
    expect(useTaskStore.getState().patch).not.toHaveBeenCalled();
  });
});

describe("collapsing the console", () => {
  it("hides the rail and the launch action, and states the plan instead", () => {
    mount();
    fireEvent.click(screen.getByTitle(/Collapse the launch console/));
    expect(screen.queryByRole("button", { name: /Delegate to Claude/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Capability/ })).toBeNull();
    // Collapsed still has to say what it is set to, or it is just missing.
    expect(screen.getByText("cxtasks · Claude · Inspect")).toBeInTheDocument();
  });

  it("opens collapsed", () => {
    mount({}, { expand: false });
    expect(screen.queryByRole("button", { name: /Capability/ })).toBeNull();
    expect(screen.getByTitle(/Show the launch console/)).toBeInTheDocument();
  });

  /**
   * Per-task, not a remembered preference. Expanding is a decision about the
   * task in front of you; most tasks carry no execution context at all, so a
   * console that stayed open followed you across the whole board after one use.
   */
  it("re-collapses when another task is opened", () => {
    mount();
    expect(node(/Capability/)).toBeInTheDocument();

    act(() => {
      useTaskStore.setState({
        tasks: [{ ...TASK, id: "another-task", short_id: 43 }],
        selectedId: "another-task",
      } as never);
    });

    expect(screen.queryByRole("button", { name: /Capability/ })).toBeNull();
    expect(screen.getByTitle(/Show the launch console/)).toBeInTheDocument();
  });

  /**
   * The reset keys on `task.id` ALONE. The draft-reset effect beside it also
   * fires on `updated_at`, which bumps on every patch — including the ones a
   * background run writes back — so sharing it would shut the console while
   * you were using it.
   */
  it("stays open when the same task is merely updated", () => {
    mount();
    act(() => {
      useTaskStore.setState({
        tasks: [{ ...TASK, updated_at: "2026-08-02 12:00:00" }],
      } as never);
    });
    expect(node(/Capability/)).toBeInTheDocument();
  });
});

/**
 * Whether a task may leave this Mac.
 *
 * The routing machinery underneath was complete for a while with no way to
 * reach it — every task said "this Mac only" and nothing could say otherwise,
 * so a second host could be running and correctly claim nothing at all. These
 * pin the control that closes that, and the one rule it enforces.
 */
describe("the may-run-elsewhere switch", () => {
  const remoteSwitch = () => screen.getByRole("checkbox", { name: /another host/ });

  it("is off by default and says what this Mac only means", () => {
    mount();
    fireEvent.click(node(/Capability/));
    expect((remoteSwitch() as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText(/This Mac only/)).toBeTruthy();
  });

  it("writes the flag and nothing else", () => {
    mount();
    fireEvent.click(node(/Capability/));
    fireEvent.click(remoteSwitch());
    // Exactly one key. The tool grant is a separate decision and a patch that
    // rewrote it here would silently change what an unattended agent may touch.
    expect(useTaskStore.getState().patch).toHaveBeenCalledWith("t1", {
      bg_remote_allowed: true,
    });
  });

  /**
   * **Not a UI opinion.** A remote host works from a mirror — a throwaway clone
   * — so there is no tree to write back to, and `can_checkpoint` is false
   * there, which makes `routing::satisfies` refuse anything above read-only.
   * An enabled switch that produces a refusal at queue time is worse than a
   * disabled one that explains itself.
   */
  it("is disabled above read-only, and says why", () => {
    mount({ bg_writes_allowed: true });
    fireEvent.click(node(/Capability/));
    expect((remoteSwitch() as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText(/no tree to write back to/)).toBeTruthy();
  });

  it("explains that the live tree still wins once it is on", () => {
    mount({ bg_remote_allowed: true });
    fireEvent.click(node(/Capability/));
    expect(screen.getByText(/still runs here when this Mac is awake/)).toBeTruthy();
  });
});

describe("the executor availability warning", () => {
  /**
   * The regression this guards is silent: move the probe back inside the runner
   * editor and a Codex task on a machine without Codex looks completely fine
   * until the queued run fails, which is exactly the "discovered after you've
   * decided" failure the check exists to prevent.
   */
  it("stays visible while the runner editor is closed", async () => {
    executorAvailable.mockResolvedValue("Codex is not logged in. Run `codex login`.");
    mount({ bg_executor: "codex" });
    expect(await screen.findByText(/not logged in/)).toBeInTheDocument();
    expect(node(/Runner/)).toHaveAttribute("aria-expanded", "false");
  });

  // ...and is not then printed twice by the picker once that editor is opened.
  it("is not duplicated when the runner editor is open", async () => {
    executorAvailable.mockResolvedValue("Codex is not logged in. Run `codex login`.");
    mount({ bg_executor: "codex" });
    await screen.findByText(/not logged in/);
    fireEvent.click(node(/Runner/));
    await waitFor(() =>
      expect(screen.getAllByText(/not logged in/)).toHaveLength(1)
    );
  });

  it("is absent for Claude, and costs no probe", () => {
    mount();
    expect(executorAvailable).not.toHaveBeenCalled();
    expect(screen.queryByText(/not logged in|not installed/)).toBeNull();
  });
});

/**
 * A critique must not read identically in the chronology to the work it
 * critiques — that is the one distinction the whole feature turns on, and the
 * runs list is where a human meets it.
 */
describe("the runs list", () => {
  function run(overrides: Partial<TaskRun>): TaskRun {
    return {
      id: "r",
      task_id: TASK.id,
      mode: "background",
      status: "succeeded",
      started_at: "2026-08-01 10:00:00",
      finished_at: "2026-08-01 10:04:00",
      session_id: null,
      exit_code: 0,
      result_text: null,
      output_tokens: null,
      scratch_dir: null,
      input_tokens: null,
      cache_read_tokens: null,
      cache_write_tokens: null,
      model: null,
      executor: "claude",
      intent: "work",
      ...overrides,
    };
  }

  it("tells a review run apart from the work it critiques", () => {
    mount(
      {},
      {
        runs: [
          run({ id: "r2", intent: "review", executor: "codex" }),
          run({ id: "r1", intent: "work", executor: "claude" }),
        ],
      }
    );
    // The review names itself AND the agent that produced it — "which agent
    // said this" is load-bearing precisely on a critique.
    expect(screen.getByText("background review")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    // The work run keeps the plain mode label, with no executor chip: a badge
    // on every row would make the badge mean nothing.
    expect(screen.getByText("background")).toBeInTheDocument();
    expect(screen.queryByText("Claude Code")).toBeNull();
  });
});
