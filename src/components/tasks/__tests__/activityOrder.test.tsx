/**
 * The Activity feed: comments and runs on ONE timeline, newest first.
 *
 * The pane is read for "what happened last", so the newest entry sits at the
 * top where a glance finds it. The DB is the opposite on purpose — every
 * agent-facing consumer (`dispatch::history_block`, the MCP's `get_task`)
 * takes the newest N and prints them oldest-first inside that window, so a
 * run reads a transcript in the order it happened. That split is why this is
 * asserted against a *chronological* store array: a future "simplification"
 * that flips `db::comments::for_task` instead would leave this test green
 * while quietly handing every dispatched agent a backwards history.
 *
 * Runs arrive in the OPPOSITE order (newest-first, `db::runs::list_for_task`),
 * which is what the interleaving cases pin.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { activityEntries, TaskActivity } from "@/components/tasks/TaskActivity";
import { useTaskStore } from "@/stores/taskStore";
import type { Comment, Task, TaskRun } from "@/types/task";

const foreground = vi.hoisted(() => vi.fn(async () => {}));

vi.mock("@/lib/tauri", () => ({
  api: {
    comments: { list: vi.fn(async () => []), add: vi.fn(), remove: vi.fn() },
    runs: { events: vi.fn(async () => []) },
    dispatch: { foreground },
  },
}));

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (p: string) => p }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

const task = { id: "t1", title: "fix the retry" } as Task;

function comment(id: string, body: string, created_at: string): Comment {
  return { id, task_id: "t1", author: "me", body, created_at };
}

function run(id: string, started_at: string, result_text: string): TaskRun {
  return {
    id,
    task_id: "t1",
    // Foreground, so the entry carries no timeline to load.
    mode: "foreground",
    status: "dispatched",
    started_at,
    finished_at: null,
    session_id: null,
    exit_code: null,
    result_text,
    output_tokens: null,
    scratch_dir: null,
    input_tokens: null,
    cache_read_tokens: null,
    cache_write_tokens: null,
    model: null,
    executor: "claude",
    intent: "work",
  } as TaskRun;
}

const entryTexts = () => screen.getAllByRole("listitem").map((li) => li.textContent ?? "");

describe("activity ordering", () => {
  beforeEach(() => {
    useTaskStore.setState({ comments: [], attachments: [] });
  });

  it("renders the newest comment first", () => {
    // Stored order is chronological — that is what the backend hands back.
    useTaskStore.setState({
      comments: [
        comment("c1", "oldest", "2026-08-01 09:00:00.000"),
        comment("c2", "middle", "2026-08-01 09:00:00.500"),
        comment("c3", "newest", "2026-08-01 09:00:01.000"),
      ],
    });
    render(<TaskActivity task={task} runs={[]} />);

    const bodies = entryTexts();
    expect(bodies[0]).toContain("newest");
    expect(bodies[2]).toContain("oldest");
  });

  it("does not mutate the store's array while ordering it", () => {
    const stored = [
      comment("c1", "oldest", "2026-08-01 09:00:00.000"),
      comment("c2", "newest", "2026-08-01 09:00:01.000"),
    ];
    useTaskStore.setState({ comments: stored });
    const { rerender } = render(<TaskActivity task={task} runs={[]} />);
    rerender(<TaskActivity task={task} runs={[]} />);

    expect(useTaskStore.getState().comments.map((c) => c.body)).toEqual(["oldest", "newest"]);
    expect(entryTexts()[0]).toContain("newest");
  });

  it("interleaves runs with comments by time, whatever order each source arrives in", () => {
    useTaskStore.setState({
      comments: [
        comment("c1", "first comment", "2026-08-01 09:00:00.000"),
        comment("c2", "third comment", "2026-08-01 11:00:00.000"),
      ],
    });
    // Newest-first, as the backend returns runs.
    const runs = [
      run("r2", "2026-08-01 12:00:00", "fourth: second run"),
      run("r1", "2026-08-01 10:00:00", "second: first run"),
    ];
    render(<TaskActivity task={task} runs={runs} />);

    const texts = entryTexts();
    expect(texts[0]).toContain("fourth");
    expect(texts[1]).toContain("third");
    expect(texts[2]).toContain("second");
    expect(texts[3]).toContain("first comment");
  });

  it("breaks a timestamp tie by each source's own order, so a render can't reshuffle it", () => {
    const same = "2026-08-01 09:00:00.000";
    const order = activityEntries(
      [comment("a", "earlier", same), comment("b", "later", same)],
      []
    ).map((e) => (e.kind === "comment" ? e.comment.body : ""));
    expect(order).toEqual(["later", "earlier"]);
  });

  it("shows the exact time in 12-hour form on hover", () => {
    useTaskStore.setState({
      comments: [comment("c1", "evening update", "2026-09-23 21:27:17.237")],
    });
    render(<TaskActivity task={task} runs={[]} />);
    expect(screen.getByTitle("2026-09-23 9:27:17 PM")).toBeInTheDocument();
  });
});

describe("the composer", () => {
  it("offers Post only once there is something to post", () => {
    useTaskStore.setState({ comments: [], attachments: [] });
    render(<TaskActivity task={task} runs={[]} />);
    expect(screen.queryByRole("button", { name: "Post" })).toBeNull();
    fireEvent.change(screen.getByRole("textbox", { name: "Add a comment" }), {
      target: { value: "hello" },
    });
    expect(screen.getByRole("button", { name: "Post" })).toBeInTheDocument();
  });
});

describe("continuing incomplete work", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useTaskStore.setState({ comments: [], attachments: [], refresh: vi.fn(async () => {}), selectedId: null });
  });

  const incomplete = (id: string, status: TaskRun["status"] = "needs_action") => ({
    ...run(id, "2026-08-01 12:00:00", "Prepared the requested report."),
    status, finished_at: "2026-08-01 12:02:00",
  });

  it("offers a local continuation for the latest incomplete result", async () => {
    render(<TaskActivity task={{ ...task, status: "todo" }} runs={[incomplete("r1")]} />);
    expect(screen.getByText("Needs action")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue in Claude" }));
    await waitFor(() => expect(foreground).toHaveBeenCalledWith("t1"));
    expect(useTaskStore.getState().refresh).toHaveBeenCalled();
  });

  it("does not reopen stale incomplete work after a newer ready report", () => {
    render(<TaskActivity task={{ ...task, status: "in_review" }} runs={[incomplete("r2", "ready_for_review"), incomplete("r1")]} />);
    expect(screen.getByText("Ready for review")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Continue in Claude" })).toBeNull();
  });

  it.each(["done", "in_progress", "queued"] as const)("does not launch a continuation for a task at %s", (status) => {
    render(<TaskActivity task={{ ...task, status }} runs={[incomplete("r1", "unverified")]} />);
    expect(screen.getByText("Needs verification")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Continue in Claude" })).toBeNull();
  });
});
