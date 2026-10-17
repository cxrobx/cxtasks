/**
 * The executor choice, as the user actually meets it.
 *
 * These assert on rendered text and on the exact patch sent, because those are
 * the two places this feature can be wrong in a way nothing else catches: a
 * label that names the wrong agent sends work somewhere the user didn't intend,
 * and a patch that carries extra fields undoes the atomic executor switch the
 * backend performs.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { BackgroundAgentPicker } from "@/components/tasks/TaskDetail";
import { DispatchBar } from "@/components/tasks/DispatchBar";
import { useTaskStore } from "@/stores/taskStore";
import type { BackgroundExecutor, Task, TaskRun } from "@/types/task";

const executorAvailable = vi.fn<(e: BackgroundExecutor) => Promise<string | null>>();
const queueBackground = vi.fn();

vi.mock("@/lib/tauri", () => ({
  api: {
    dispatch: {
      executorAvailable: (e: BackgroundExecutor) => executorAvailable(e),
      queueBackground: (id: string) => queueBackground(id),
      foreground: vi.fn(),
      prompt: vi.fn(),
    },
    runner: { runOnce: vi.fn(), install: vi.fn() },
  },
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(async () => {}),
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
    repo_path: "/tmp/repo",
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
    ...overrides,
  };
}

const patch = vi.fn(async () => {});

beforeEach(() => {
  vi.clearAllMocks();
  executorAvailable.mockResolvedValue(null);
  useTaskStore.setState({
    patch,
    refresh: vi.fn(async () => {}),
    backgroundIdle: null,
    setBackgroundIdle: vi.fn(),
  } as never);
});

describe("BackgroundAgentPicker", () => {
  it("offers both agents and marks the current one", () => {
    render(<BackgroundAgentPicker task={task({ bg_executor: "codex" })} />);
    const claude = screen.getByRole("radio", { name: /Claude Code/ });
    const codex = screen.getByRole("radio", { name: /Codex/ });
    expect(claude).not.toBeChecked();
    expect(codex).toBeChecked();
  });

  /**
   * **Only `bg_executor` may be in the patch.** The backend moves the enabled
   * flag between the per-executor columns and clears an incompatible model in
   * the same write. Sending `bg_allowed` or `model` alongside would either
   * fight that or split it into two writes with a visibly wrong state between
   * them.
   */
  it("sends the switch and nothing else", async () => {
    render(<BackgroundAgentPicker task={task({ bg_executor: "claude" })} />);
    fireEvent.click(screen.getByRole("radio", { name: /Codex/ }));
    await waitFor(() => expect(patch).toHaveBeenCalled());
    expect(patch).toHaveBeenCalledWith("t1", { bg_executor: "codex" });
  });

  // Re-selecting the current agent would be a no-op patch — but the backend
  // treats any executor write as a potential switch, and a same-executor write
  // must not look like one to the model-clearing rule.
  it("does not patch when the current agent is re-selected", () => {
    render(<BackgroundAgentPicker task={task({ bg_executor: "codex" })} />);
    fireEvent.click(screen.getByRole("radio", { name: /Codex/ }));
    expect(patch).not.toHaveBeenCalled();
  });

  it("warns when the selected agent is not usable", async () => {
    executorAvailable.mockResolvedValue("Codex is not logged in. Run `codex login`.");
    render(<BackgroundAgentPicker task={task({ bg_executor: "codex" })} />);
    expect(await screen.findByText(/not logged in/i)).toBeInTheDocument();
  });

  // Claude is the incumbent and is checked by the runner anyway; probing it on
  // every render would put a subprocess behind opening a task.
  it("does not probe availability for Claude", () => {
    render(<BackgroundAgentPicker task={task({ bg_executor: "claude" })} />);
    expect(executorAvailable).not.toHaveBeenCalled();
  });

  // A failed probe is not the same claim as "the agent is missing". Rendering
  // it as one would put a false warning in front of a working setup.
  it("stays quiet when the availability probe itself fails", async () => {
    executorAvailable.mockRejectedValue(new Error("ipc down"));
    render(<BackgroundAgentPicker task={task({ bg_executor: "codex" })} />);
    await waitFor(() => expect(executorAvailable).toHaveBeenCalled());
    expect(screen.queryByRole("radio", { name: /Codex/ })).toBeChecked();
    expect(screen.queryByText(/not installed|not logged in/i)).toBeNull();
  });
});

describe("DispatchBar", () => {
  // The label names the INTENT and the DESTINATION. This button is the last
  // thing you see before something runs unattended, so "Run in background" —
  // which no longer says what will happen — is exactly the wrong amount of
  // information, and "Queue for …" named an intermediate status rather than
  // the thing being asked for. The agent has to stay in the label either way.
  it("names the agent the task will be delegated to", () => {
    const { rerender } = render(<DispatchBar task={task({ bg_executor: "claude" })} reviewTarget={null} />);
    expect(screen.getByRole("button", { name: /Delegate to Claude/ })).toBeInTheDocument();

    rerender(<DispatchBar task={task({ bg_executor: "codex" })} reviewTarget={null} />);
    expect(screen.getByRole("button", { name: /Delegate to Codex/ })).toBeInTheDocument();
  });

  // Foreground is Claude whatever the background agent is, and the label has to
  // keep saying so or the two controls read as one setting.
  it("keeps the foreground button on Claude regardless of the background agent", () => {
    render(<DispatchBar task={task({ bg_executor: "codex" })} reviewTarget={null} />);
    expect(screen.getByRole("button", { name: /Open in Claude/ })).toBeInTheDocument();
  });

  it("treats the explicit queue action as the unattended opt-in", () => {
    render(<DispatchBar task={task({ bg_allowed: false })} reviewTarget={null} />);
    expect(screen.getByRole("button", { name: /Delegate to Claude/ })).toBeEnabled();
  });

  it("still requires a bounded workspace before queueing", () => {
    render(<DispatchBar task={task({ bg_allowed: false, repo_path: null })} reviewTarget={null} />);
    expect(screen.getByRole("button", { name: /Delegate to Claude/ })).toBeDisabled();
  });

  it("confirms which agent picked the task up", async () => {
    queueBackground.mockResolvedValue({
      task: task({ bg_executor: "codex" }),
      runner_installed: true,
      paused: false,
      executor: "codex",
    });
    render(<DispatchBar task={task({ bg_executor: "codex" })} reviewTarget={null} />);
    fireEvent.click(screen.getByRole("button", { name: /Delegate to Codex/ }));
    expect(await screen.findByText(/Queued for Codex\./)).toBeInTheDocument();
  });

  // The idle-runner notice is the fix for "Queued, and then nothing ever
  // happens". It has to survive the executor becoming a variable.
  it("still says nothing will pick it up when the runner is off", async () => {
    queueBackground.mockResolvedValue({
      task: task({ bg_executor: "codex" }),
      runner_installed: false,
      paused: false,
      executor: "codex",
    });
    render(<DispatchBar task={task({ bg_executor: "codex" })} reviewTarget={null} />);
    fireEvent.click(screen.getByRole("button", { name: /Delegate to Codex/ }));
    expect(
      await screen.findByText(/Queued for Codex — but the background runner is off/)
    ).toBeInTheDocument();
  });

  it("surfaces a refusal from the queue-time preflight", async () => {
    queueBackground.mockRejectedValue("The Codex CLI (`codex`) wasn't found on PATH.");
    render(<DispatchBar task={task({ bg_executor: "codex" })} reviewTarget={null} />);
    fireEvent.click(screen.getByRole("button", { name: /Delegate to Codex/ }));
    expect(await screen.findByText(/wasn't found on PATH/)).toBeInTheDocument();
  });
});

/**
 * Arming a review is the one gesture in this pane that writes four fields at
 * once and then must never write again. Both halves are asserted: what the
 * click sends, and — three times over — what nothing else may send.
 */
describe("the review control", () => {
  function handoff(overrides: Partial<TaskRun> = {}): TaskRun {
    return {
      id: "r1",
      task_id: "t1",
      mode: "background",
      status: "succeeded",
      started_at: "2026-08-01 10:00:00",
      finished_at: "2026-08-01 10:04:00",
      session_id: null,
      exit_code: 0,
      result_text: "Found the 429: a missing retry on lib/webhook.ts:88.",
      output_tokens: 141,
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

  // Showing it sooner makes the queue guard's refusal the user's introduction
  // to the feature — a control whose first interaction is being told no.
  it("stays hidden until a run has left something to review", () => {
    const { rerender } = render(<DispatchBar task={task()} reviewTarget={null} />);
    expect(screen.queryByRole("radio", { name: /Review/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Delegate to Claude/ })).toBeInTheDocument();

    // A finished run that left nothing is not a handoff — `lastHandoffRun`
    // resolves it to null, so the control must still be absent.
    rerender(<DispatchBar task={task()} reviewTarget={null} />);
    expect(screen.queryByRole("radio", { name: /Review/ })).toBeNull();

    rerender(<DispatchBar task={task()} reviewTarget={handoff()} />);
    expect(screen.getByRole("radio", { name: /Review/ })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Continue/ })).toBeChecked();
  });

  // The last thing you read before something runs unattended has to be what
  // will actually happen.
  it("names the review in the delegate button", () => {
    render(
      <DispatchBar
        task={task({ bg_intent: "review", bg_executor: "codex" })}
        reviewTarget={handoff()}
      />
    );
    expect(screen.getByRole("button", { name: /Review with Codex/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Delegate to/ })).toBeNull();
  });

  /**
   * **Exactly once, with all four fields.** Catches both the forgotten half
   * (an armed review still holding `Edit`, or still on the agent that wrote
   * the thing it is auditing) and the three-sequential-patches version, whose
   * extra `updated_at` bumps fire `TaskDetailForm`'s draft reset and erase text
   * being typed elsewhere in the pane (gotcha #24).
   */
  it("preselects the other agent and a read-only capability in one write", () => {
    render(
      <DispatchBar
        task={task({
          bg_executor: "claude",
          bg_writes_allowed: true,
          bg_shell_allowed: true,
        })}
        reviewTarget={handoff({ executor: "claude" })}
      />
    );
    fireEvent.click(screen.getByRole("radio", { name: /Review/ }));

    expect(patch).toHaveBeenCalledTimes(1);
    expect(patch).toHaveBeenCalledWith("t1", {
      bg_intent: "review",
      bg_writes_allowed: false,
      bg_shell_allowed: false,
      bg_executor: "codex",
    });
  });

  /**
   * The preselection runs on the GESTURE, never on render. Implemented as an
   * effect on `[task.bg_intent]` instead, it re-fires on remount — so clicking
   * away to another task and back re-applies the suggestion over an override
   * the user made in between, silently, visible only after the run.
   */
  it("leaves a capability the user changed after arming the review alone", () => {
    const armed = task({
      bg_intent: "review",
      bg_executor: "codex",
      bg_writes_allowed: true,
    });
    const { rerender } = render(<DispatchBar task={armed} reviewTarget={handoff()} />);
    expect(patch).not.toHaveBeenCalled();

    rerender(<DispatchBar task={armed} reviewTarget={handoff()} />);
    expect(patch).not.toHaveBeenCalled();
  });

  // Disarming is deliberately asymmetric — restoring the previous tier and
  // executor means remembering values the DB never stored.
  it("does not restore the agent when the review is disarmed", () => {
    render(
      <DispatchBar
        task={task({ bg_intent: "review", bg_executor: "codex" })}
        reviewTarget={handoff()}
      />
    );
    fireEvent.click(screen.getByRole("radio", { name: /Continue/ }));
    expect(patch).toHaveBeenCalledTimes(1);
    expect(patch).toHaveBeenCalledWith("t1", { bg_intent: "work" });
  });

  /**
   * `finish_run` writes `bg_intent` back to `work` and the bridge repatches, so
   * the control must read the task directly. Local state would survive the
   * reset and leave the button promising a review already spent.
   */
  it("follows the server when a finished run disarms the review", () => {
    const { rerender } = render(
      <DispatchBar task={task({ bg_intent: "review" })} reviewTarget={handoff()} />
    );
    expect(screen.getByRole("button", { name: /Review with Claude/ })).toBeInTheDocument();

    rerender(
      <DispatchBar
        task={task({ bg_intent: "work", updated_at: "2026-08-01 10:05:00" })}
        reviewTarget={handoff()}
      />
    );
    expect(screen.getByRole("button", { name: /Delegate to Claude/ })).toBeInTheDocument();
    expect(patch).not.toHaveBeenCalled();
  });

  // Read off the POST-queue row, so a run that finished between paint and click
  // does not leave the toast promising a review that was queued as work.
  it("says a review was queued, not a work run", async () => {
    queueBackground.mockResolvedValue({
      task: task({ bg_intent: "review", bg_executor: "codex" }),
      runner_installed: true,
      paused: false,
      executor: "codex",
    });
    render(
      <DispatchBar
        task={task({ bg_intent: "review", bg_executor: "codex" })}
        reviewTarget={handoff()}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: /Review with Codex/ }));
    expect(await screen.findByText(/Queued a review for Codex\./)).toBeInTheDocument();
  });

  // One shared tail per branch, so the gotcha #18 fix can't be half-maintained.
  it("keeps the idle-runner warning identical for a review", async () => {
    queueBackground.mockResolvedValue({
      task: task({ bg_intent: "review", bg_executor: "codex" }),
      runner_installed: false,
      paused: false,
      executor: "codex",
    });
    render(
      <DispatchBar
        task={task({ bg_intent: "review", bg_executor: "codex" })}
        reviewTarget={handoff()}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: /Review with Codex/ }));
    expect(
      await screen.findByText(
        /Queued a review for Codex — but the background runner is off/
      )
    ).toBeInTheDocument();
  });

  // Reachable by arming over MCP even though the UI hides the toggle.
  it("surfaces the refusal to review a task with no handoff", async () => {
    queueBackground.mockRejectedValue(
      "Nothing to review yet — no finished run has left a handoff on this task."
    );
    render(
      <DispatchBar task={task({ bg_intent: "review" })} reviewTarget={handoff()} />
    );
    fireEvent.click(screen.getByRole("button", { name: /Review with Claude/ }));
    expect(await screen.findByText(/Nothing to review yet/)).toBeInTheDocument();
  });
});
