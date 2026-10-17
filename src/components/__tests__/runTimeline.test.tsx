import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RunTimeline } from "@/components/tasks/RunTimeline";
import { useTaskStore } from "@/stores/taskStore";
import type { RunEvent, TaskRun } from "@/types/task";

const events = vi.fn<(runId: string, afterSeq: number) => Promise<RunEvent[]>>();

vi.mock("@/lib/tauri", () => ({
  api: { runs: { events: (runId: string, afterSeq: number) => events(runId, afterSeq) } },
}));

function run(status: TaskRun["status"] = "running"): TaskRun {
  return {
    id: "run-1",
    task_id: "task-1",
    mode: "background",
    status,
    started_at: "2026-08-01 12:00:00",
    finished_at: status === "running" ? null : "2026-08-01 12:01:00",
    session_id: null,
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
  };
}

function event(
  id: number,
  seq: number,
  phase: RunEvent["phase"],
  callId = "call-1",
  // Distinct by default. Adjacent rows with the SAME summary collapse into one
  // counted row now, so a fixture that repeats itself measures the collapse
  // rather than whatever the test came for.
  summary = `Read src/${callId}.rs`
): RunEvent {
  return {
    id,
    run_id: "run-1",
    seq,
    occurred_at: `2026-08-01 12:00:0${seq}.000`,
    kind: "tool",
    call_id: callId,
    tool_name: "Read",
    phase,
    summary,
    detail_json: null,
  };
}

/** jsdom lays nothing out, so the scroll geometry has to be supplied. */
function stubScrollBox(el: HTMLElement, { height = 240, content = 900 } = {}) {
  Object.defineProperty(el, "clientHeight", { value: height, configurable: true });
  Object.defineProperty(el, "scrollHeight", { value: content, configurable: true });
}

beforeEach(() => {
  events.mockReset();
  useTaskStore.setState({ runActivity: null });
});

describe("RunTimeline", () => {
  it("folds a tool start and result into one successful activity", async () => {
    events.mockResolvedValue([event(1, 1, "started"), event(2, 2, "succeeded")]);
    render(<RunTimeline run={run()} />);
    expect(await screen.findByText("Read src/call-1.rs")).toBeInTheDocument();
    expect(screen.getAllByText("Read src/call-1.rs")).toHaveLength(1);
    expect(screen.getByLabelText("Succeeded")).toBeInTheDocument();
  });

  /**
   * A real run emits ninety-odd tool calls whose labels repeat, and reading the
   * sixth `Bash python3` tells you nothing the first did — it just scrolls the
   * interesting row off the top.
   */
  it("collapses a burst of identical calls into one counted row", async () => {
    const same = (id: number, call: string) =>
      event(id, id, "succeeded", call, "Bash python3");
    events.mockResolvedValue([same(1, "a"), same(2, "b"), same(3, "c")]);
    render(<RunTimeline run={run()} />);

    expect(await screen.findByText("Bash python3")).toBeInTheDocument();
    expect(screen.getAllByText("Bash python3")).toHaveLength(1);
    expect(screen.getByText("×3")).toBeInTheDocument();
  });

  /**
   * ADJACENCY is the whole rule. Grouping every matching label in the run would
   * reorder the timeline and hide that the agent came back to something — the
   * sequence is the information here.
   */
  it("keeps two separate bursts separate", async () => {
    events.mockResolvedValue([
      event(1, 1, "succeeded", "a", "Bash python3"),
      event(2, 2, "succeeded", "b", "Read src/lib.rs"),
      event(3, 3, "succeeded", "c", "Bash python3"),
    ]);
    render(<RunTimeline run={run()} />);

    await screen.findByText("Read src/lib.rs");
    expect(screen.getAllByText("Bash python3")).toHaveLength(2);
    expect(screen.queryByText("×2")).toBeNull();
  });

  /**
   * A failure is the row you opened the timeline for. Burying it inside "×7"
   * with its neighbours is how you miss it.
   */
  it("never folds a failure into the burst around it", async () => {
    events.mockResolvedValue([
      event(1, 1, "succeeded", "a", "Bash python3"),
      event(2, 2, "failed", "b", "Bash python3"),
      event(3, 3, "succeeded", "c", "Bash python3"),
    ]);
    render(<RunTimeline run={run()} />);

    await screen.findAllByText("Bash python3");
    expect(screen.getAllByText("Bash python3")).toHaveLength(3);
  });

  it("marks an unmatched tool start interrupted after the run ends", async () => {
    events.mockResolvedValue([event(1, 1, "started")]);
    render(<RunTimeline run={run("failed")} />);
    // Finished timelines start collapsed; opening them is an explicit history read.
    screen.getByRole("button", { name: /Activity/i }).click();
    expect(await screen.findByLabelText("Interrupted")).toBeInTheDocument();
  });

  // The list is capped, so a live run's newest calls land below the fold. If
  // the tail isn't followed the pane silently shows the first ten calls of a
  // run forever, which reads as the run having stalled.
  it("follows the tail of a live run, and stops once the reader scrolls back", async () => {
    events.mockResolvedValue([event(1, 1, "succeeded")]);
    const { container } = render(<RunTimeline run={run()} />);
    await screen.findByText("Read src/call-1.rs");

    const box = container.querySelector<HTMLElement>(".overflow-y-auto");
    expect(box).not.toBeNull();
    stubScrollBox(box!);

    events.mockResolvedValue([event(2, 2, "succeeded", "call-2")]);
    useTaskStore.setState({
      runActivity: { key: 1, kind: "run-progress", taskId: "task-1", runId: "run-1" },
    });
    await screen.findAllByText(/Read src\/call-/);
    await waitFor(() => expect(box!.scrollTop).toBe(900));

    // Reading back through the history must survive the next batch.
    box!.scrollTop = 0;
    fireEvent.scroll(box!);
    events.mockResolvedValue([event(3, 3, "succeeded", "call-3")]);
    useTaskStore.setState({
      runActivity: { key: 2, kind: "run-progress", taskId: "task-1", runId: "run-1" },
    });
    await waitFor(() => expect(screen.getAllByLabelText("Succeeded")).toHaveLength(3));
    expect(box!.scrollTop).toBe(0);
  });
});
