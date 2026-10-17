/**
 * The two read-only surfaces that report on background runs.
 *
 * Both exist to be trusted, and both gained a way to be quietly wrong once
 * there were two agents: Mission Control can attribute a run to the wrong one,
 * and the budget panel can present a Claude-only setting as if it governed
 * everything. These pin the wording, because the wording IS the feature here.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { MissionControl } from "@/components/mission/MissionControl";
import { BudgetPanel } from "@/components/layout/BudgetPanel";
import { useTaskStore } from "@/stores/taskStore";
import type { BudgetStatus, RunWithTask, Task } from "@/types/task";

const recent = vi.fn<() => Promise<RunWithTask[]>>();
const budgetStatus = vi.fn<() => Promise<BudgetStatus>>();
const listTasks = vi.fn<() => Promise<Task[]>>();
const isInstalled = vi.fn<() => Promise<boolean>>();
const saveBudget = vi.fn();
const resetBudget = vi.fn<() => Promise<BudgetStatus>>();

vi.mock("@/lib/tauri", () => ({
  api: {
    runs: { recent: () => recent() },
    budget: {
      status: () => budgetStatus(),
      save: (s: BudgetSettingsLike) => saveBudget(s),
      reset: () => resetBudget(),
      setPaused: vi.fn(),
    },
    tasks: { list: () => listTasks() },
    runner: { isInstalled: () => isInstalled(), install: vi.fn(), uninstall: vi.fn() },
  },
}));

type BudgetSettingsLike = BudgetStatus["settings"];

function status(): BudgetStatus {
  return {
    settings: {
      max_concurrent: 2,
      timeout_minutes: 20,
      daily_run_cap: 20,
      daily_token_cap: 50_000_000,
      run_token_cap: 25_000_000,
      max_turns: 150,
      model: "claude-sonnet-5",
      tripwire_daily_token_cap: 5_000_000,
    },
    running: 0,
    runs_today: 3,
    output_tokens_today: 400,
    total_tokens_today: 120_000,
    tripwire_tokens_today: 0,
    paused: false,
    window_started_at: "2026-08-02 07:00:00",
    window_reset: false,
  };
}

function run(overrides: Partial<RunWithTask> = {}): RunWithTask {
  return {
    id: "r1",
    task_id: "t1",
    mode: "background",
    status: "running",
    started_at: "2026-07-31 09:00:00",
    finished_at: null,
    session_id: "th_1",
    exit_code: null,
    result_text: null,
    output_tokens: null,
    scratch_dir: null,
    input_tokens: null,
    cache_read_tokens: null,
    cache_write_tokens: null,
    model: null,
    executor: "claude",
    intent: "work",
    task_title: "fix the retry",
    task_short_id: 42,
    task_project: "cxtasks",
    task_status: "in_progress",
    latest_event_summary: null,
    latest_event_phase: null,
    latest_event_seq: null,
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  recent.mockResolvedValue([]);
  budgetStatus.mockResolvedValue(status());
  resetBudget.mockResolvedValue({ ...status(), runs_today: 0, total_tokens_today: 0 });
  saveBudget.mockResolvedValue(status());
  listTasks.mockResolvedValue([]);
  isInstalled.mockResolvedValue(true);
  useTaskStore.setState({ tasks: [], selectedId: null, select: vi.fn() } as never);
});

describe("Mission Control", () => {
  it("shows the latest unfinished outcome even though the task is held outside review", async () => {
    recent.mockResolvedValue([
      run({ id: "r2", status: "needs_action", task_status: "todo", task_title: "Deliver the report" }),
      run({ id: "r1", status: "unverified", task_status: "todo", task_title: "Deliver the report" }),
      run({ id: "r3", task_id: "t2", status: "unverified", task_status: "todo", task_title: "Confirm venue hours" }),
    ]);
    render(<MissionControl />);
    const card = (await screen.findByText("Deliver the report")).closest("button")!;
    expect(card).toHaveTextContent("Needs action");
    expect(screen.getAllByText("Deliver the report")).toHaveLength(1);
    expect((await screen.findByText("Confirm venue hours")).closest("button")).toHaveTextContent("Needs verification");
  });

  it("labels legacy process success without upgrading it into verified readiness", async () => {
    recent.mockResolvedValue([run({ status: "succeeded", task_status: "in_review" })]);
    render(<MissionControl />);
    expect(await screen.findByText("Run finished")).toBeInTheDocument();
    expect(screen.queryByText("Ready for review")).toBeNull();
  });

  // Two runs on the same task can now come from different agents. A card that
  // doesn't say which is a card you can't act on — and the badge has to come
  // off the RUN, not the task, or switching a task relabels its history.
  it("badges each run with the agent that produced it", async () => {
    recent.mockResolvedValue([
      run({ id: "r1", executor: "claude", task_title: "claude work" }),
      run({ id: "r2", executor: "codex", task_title: "codex work", task_id: "t2" }),
    ]);
    render(<MissionControl />);

    const claudeCard = (await screen.findByText("claude work")).closest("button")!;
    const codexCard = (await screen.findByText("codex work")).closest("button")!;
    expect(claudeCard).toHaveTextContent("Claude Code");
    expect(codexCard).toHaveTextContent("Codex");
    expect(codexCard).not.toHaveTextContent("Claude Code");
  });

  it("shows the latest normalized activity on a running card", async () => {
    recent.mockResolvedValue([
      run({
        latest_event_summary: "Read src/legal/Terms.tsx",
        latest_event_phase: "started",
        latest_event_seq: 4,
      }),
    ]);
    render(<MissionControl />);
    expect(await screen.findByText("Read src/legal/Terms.tsx")).toBeInTheDocument();
  });

  // Codex picks a model we never see. Blank reads as a missing field; a named
  // model would be invented. "Codex default" is the only honest third option.
  it("says 'Codex default' when the model was not reported", async () => {
    recent.mockResolvedValue([run({ executor: "codex", model: null })]);
    render(<MissionControl />);
    expect(await screen.findByText("Codex default")).toBeInTheDocument();
  });

  it("prints the explicit model when there is one", async () => {
    recent.mockResolvedValue([run({ executor: "claude", model: "claude-opus-5" })]);
    render(<MissionControl />);
    expect(await screen.findByText("opus-5")).toBeInTheDocument();
    expect(screen.queryByText("Codex default")).toBeNull();
  });

  // An unlabelled model in the budget meter reads as "what every run uses",
  // and it is only Claude's default.
  it("labels the budget meter's model as Claude's", async () => {
    render(<MissionControl />);
    expect(await screen.findByText(/Claude: sonnet-5/)).toBeInTheDocument();
  });

  // The grant is stated before the run starts — that's when knowing it changes
  // what you'd do — and now the destination is stated there too.
  it("names the agent on a queued card", async () => {
    listTasks.mockResolvedValue([
      {
        ...(run() as unknown as Task),
        id: "q1",
        title: "queued codex work",
        status: "queued",
        bg_executor: "codex",
        bg_writes_allowed: false,
        bg_shell_allowed: true,
        short_id: 7,
        repo_path: "/tmp/repo",
      } as Task,
    ]);
    render(<MissionControl />);
    const card = (await screen.findByText("queued codex work")).closest("button")!;
    expect(card).toHaveTextContent("Codex");
    expect(card).toHaveTextContent("commands and tests");
  });
});

describe("BudgetPanel", () => {
  it("scopes the model picker to Claude and says what Codex does instead", async () => {
    render(<BudgetPanel />);
    // Collapsed at rest — the settings live one click in.
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    expect(screen.getByText("Claude default model")).toBeInTheDocument();
    expect(
      screen.getByText(/Codex uses its own CLI default unless a task sets a model/)
    ).toBeInTheDocument();
  });

  // The counters genuinely DO span both agents — one budget, deliberately —
  // so the panel must say that rather than leaving it to be inferred from a
  // Claude-labelled model picker sitting next to unlabelled numbers.
  it("says the caps cover both agents", async () => {
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));
    expect(screen.getByText(/caps and the pause switch cover both agents/)).toBeInTheDocument();
  });

  // A cap only its author can change is one that gets switched off rather than
  // tuned — the first shipped value was 25x too small and could only be raised
  // by a rebuild.
  it("saves an edited token cap in the units it displays", async () => {
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    const field = screen.getByLabelText("Daily token cap");
    expect(field).toHaveValue("50M");
    fireEvent.change(field, { target: { value: "75M" } });
    fireEvent.blur(field);

    expect(saveBudget).toHaveBeenCalledWith(
      expect.objectContaining({ daily_token_cap: 75_000_000 })
    );
  });

  // The two guards that had no control at all until the run that stopped at
  // turn 31 with zero output. They share one component with the daily cap —
  // which is the point, since that component carries the gotcha #24 unmount
  // commit — so each has to prove it reads and writes its OWN field.
  it("edits the per-run token cap and the turn cap independently", async () => {
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    const perRun = screen.getByLabelText("Per-run token cap");
    expect(perRun).toHaveValue("25M");
    fireEvent.change(perRun, { target: { value: "30M" } });
    fireEvent.blur(perRun);
    expect(saveBudget).toHaveBeenCalledWith(
      expect.objectContaining({ run_token_cap: 30_000_000, daily_token_cap: 50_000_000 })
    );

    const turns = screen.getByLabelText("Turn cap");
    expect(turns).toHaveValue("150");
    fireEvent.change(turns, { target: { value: "3" } });
    fireEvent.blur(turns);
    expect(saveBudget).toHaveBeenCalledWith(expect.objectContaining({ max_turns: 3 }));
  });

  // `parseCount` rejects rather than coercing, for the same reason
  // `parseTokens` does: `parseInt("12abc")` is 12, and a turn cap nobody typed
  // is worse than one that didn't save.
  it("reverts an unreadable turn cap instead of coercing it", async () => {
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    const turns = screen.getByLabelText("Turn cap");
    fireEvent.change(turns, { target: { value: "40 turns" } });
    fireEvent.blur(turns);

    expect(saveBudget).not.toHaveBeenCalled();
    expect(turns).toHaveValue("150");
  });

  // `parseTokens` returns null, never 0, because 0 is the stored value meaning
  // "no cap" — a typo must not be the thing that uncaps the day.
  it("reverts an unreadable cap instead of saving anything", async () => {
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    const field = screen.getByLabelText("Daily token cap");
    fireEvent.change(field, { target: { value: "lots" } });
    fireEvent.blur(field);

    expect(saveBudget).not.toHaveBeenCalled();
    expect(field).toHaveValue("50M");
  });

  // Being capped is where you are standing when you need the way out; sending
  // you into a collapsed panel to find it reads as a dead end.
  it("offers the reset from the capped warning without opening the panel", async () => {
    budgetStatus.mockResolvedValue({ ...status(), total_tokens_today: 60_000_000 });
    render(<BudgetPanel />);

    fireEvent.click(await screen.findByRole("button", { name: /Reset the counter/ }));
    expect(resetBudget).toHaveBeenCalled();
  });

  // After a reset the counters are NOT today's spend — the day may have burned
  // its whole budget first. A guard that misreports its own scope is worse than
  // no guard.
  it("stops calling the counters 'today' once they have been reset", async () => {
    budgetStatus.mockResolvedValue({ ...status(), window_reset: true });
    render(<BudgetPanel />);
    fireEvent.click(await screen.findByRole("button", { name: /Background/ }));

    expect(screen.getByText("Tokens since reset")).toBeInTheDocument();
    expect(screen.getByText("Runs since reset")).toBeInTheDocument();
    expect(screen.queryByText("Tokens today")).toBeNull();
  });
});
