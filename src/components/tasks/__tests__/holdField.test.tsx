/**
 * "Waiting on" is the one place the pane says WHY a task is held, whatever
 * the cause — so a task on an armed tripwire must not read "Not waiting", and
 * a written reason must be editable and releasable in place.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { HoldField } from "@/components/tasks/HoldField";
import { useTaskStore } from "@/stores/taskStore";
import type { Task, TripwireRuntime } from "@/types/task";

vi.mock("@/lib/tauri", () => ({ api: {} }));

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "task",
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
    short_id: 154,
    tripwire: null,
    tripwire_every_minutes: null,
    tripwire_on_trip: null,
    tripwire_state: null,
    tripwire_tripped_at: null,
    ...overrides,
  } as Task;
}

const noRelations = {
  blocked_by: [],
  blocking: [],
  is_blocked: false,
  supersedes: [],
  superseded_by: [],
  related: [],
  is_superseded: false,
};

beforeEach(() => {
  useTaskStore.setState({ relations: noRelations, select: vi.fn(async () => {}) } as never);
});

describe("the Waiting on field", () => {
  it("collapses to one affordance when nothing holds the task", () => {
    render(<HoldField task={task()} runtime={undefined} onCommit={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Not waiting" })).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("names an armed tripwire with its last check, and offers a reason on top", () => {
    const runtime = {
      task_id: "t1",
      checked_at: "2026-08-01 10:00:00",
      last_outcome: "not_yet",
      last_note: "0 intake installs invoiced yet",
    } as unknown as TripwireRuntime;
    render(
      <HoldField
        task={task({ tripwire: "script:lsa-install-sticks 4", tripwire_state: "armed" })}
        runtime={runtime}
        onCommit={vi.fn()}
      />
    );
    expect(screen.getByText(/Until the tripwire trips/).textContent).toContain(
      "0 intake installs invoiced yet"
    );
    expect(screen.getByRole("button", { name: "Add a reason" })).toBeTruthy();
    expect(screen.queryByText("Not waiting")).toBeNull();
  });

  it("names a defer date and an open blocker", () => {
    useTaskStore.setState({
      relations: {
        ...noRelations,
        blocked_by: [
          { task_id: "b", title: "the blocker", status: "todo", priority: 3, project: null, is_open: true },
          { task_id: "c", title: "a closed one", status: "done", priority: 3, project: null, is_open: false },
        ],
        is_blocked: true,
      },
    } as never);
    render(
      <HoldField
        task={task({ defer_until: "2099-01-01T00:00:00.000Z" })}
        runtime={undefined}
        onCommit={vi.fn()}
      />
    );
    expect(screen.getByText(/^Starts /)).toBeTruthy();
    expect(screen.getByRole("button", { name: "the blocker" })).toBeTruthy();
    expect(screen.queryByText("a closed one")).toBeNull();
  });

  it("shows a written reason as the editable part, and the × releases it", () => {
    const onCommit = vi.fn();
    render(
      <HoldField task={task({ blocked_reason: "Dana's sign-off" })} runtime={undefined} onCommit={onCommit} />
    );
    const input = screen.getByRole("textbox") as HTMLInputElement;
    expect(input.value).toBe("Dana's sign-off");
    fireEvent.change(input, { target: { value: "Dana's sign-off on scope" } });
    fireEvent.blur(input);
    expect(onCommit).toHaveBeenLastCalledWith("Dana's sign-off on scope");
    fireEvent.click(screen.getByRole("button", { name: "Release" }));
    expect(onCommit).toHaveBeenLastCalledWith(null);
  });

  it("opens the input from the affordance and commits on Enter", () => {
    const onCommit = vi.fn();
    render(<HoldField task={task()} runtime={undefined} onCommit={onCommit} />);
    fireEvent.click(screen.getByRole("button", { name: "Not waiting" }));
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "an API key" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.blur(input);
    expect(onCommit).toHaveBeenCalledWith("an API key");
  });
});
