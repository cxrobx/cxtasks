/**
 * Tripwires on the board and in the pane.
 *
 * Three claims, each of which fails silently if it breaks: the badge appears
 * ONLY while armed (a tripped task with a badge reads as still waiting); the
 * field sends exactly the patch it owns (a spec patch carrying a cadence would
 * undo the server's joint resolution); and the runner-not-running tell fires
 * on the three-interval rule and never for a manual tripwire.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { TaskRow } from "@/components/tasks/TaskList";
import { TripwireField } from "@/components/tasks/TripwireField";
import {
  describeTripwire,
  isMonitor,
  isTripwireArmed,
  isTripwireWatching,
  parseTripwireSpec,
  tripwireUnchecked,
} from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import type { Task, TripwireRuntime } from "@/types/task";

const scripts = vi.fn(async () => ["pocketbuddy-first-user"]);

vi.mock("@/lib/tauri", () => ({
  api: {
    tripwires: { scripts: () => scripts() },
    tasks: { complete: vi.fn(), update: vi.fn(), trash: vi.fn() },
    menu: { showForTask: vi.fn() },
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "TRIPWIRE: first outside user",
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
    created_at: "2026-08-24 10:00:00",
    updated_at: "2026-08-24 10:00:00",
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
    short_id: 118,
    tripwire: "script:pocketbuddy-first-user",
    tripwire_every_minutes: 360,
    tripwire_on_trip: null,
    tripwire_state: "armed",
    tripwire_tripped_at: null,
    ...overrides,
  };
}

function runtime(overrides: Partial<TripwireRuntime> = {}): TripwireRuntime {
  return {
    task_id: "t1",
    requested_at: null,
    lease_id: null,
    lease_host: null,
    lease_spec: null,
    lease_request_at: null,
    lease_expires_at: null,
    checked_at: null,
    last_outcome: null,
    last_note: null,
    failures: 0,
    updated_at: "2026-08-24 10:00:00",
    ...overrides,
  };
}

const patch = vi.fn(async () => {});

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({
    patch,
    refresh: vi.fn(async () => {}),
    checkedIds: [],
    lastCheckedId: null,
    tasks: [],
    selectedId: null,
    undoStack: [],
    undoToastKey: null,
    undoNotice: null,
  } as never);
});

describe("the row badge", () => {
  it("shows only while ARMED — tripped and errored tasks are back on the board", () => {
    const { rerender } = render(
      <TaskRow task={task()} selected={false} onSelect={() => {}} onComplete={() => {}} />
    );
    expect(screen.getByTestId("tripwire-badge")).toBeInTheDocument();

    rerender(
      <TaskRow
        task={task({ tripwire_state: "tripped" })}
        selected={false}
        onSelect={() => {}}
        onComplete={() => {}}
      />
    );
    expect(screen.queryByTestId("tripwire-badge")).toBeNull();

    rerender(
      <TaskRow
        task={task({ tripwire_state: "error" })}
        selected={false}
        onSelect={() => {}}
        onComplete={() => {}}
      />
    );
    expect(screen.queryByTestId("tripwire-badge")).toBeNull();

    rerender(
      <TaskRow
        task={task({ tripwire: null, tripwire_state: null })}
        selected={false}
        onSelect={() => {}}
        onComplete={() => {}}
      />
    );
    expect(screen.queryByTestId("tripwire-badge")).toBeNull();
  });

  it("carries the last check in its tooltip", () => {
    render(
      <TaskRow
        task={task()}
        tripwire={runtime({ checked_at: "2026-08-24 09:00:00", last_outcome: "not_yet", last_note: "0 users" })}
        selected={false}
        onSelect={() => {}}
        onComplete={() => {}}
      />
    );
    expect(screen.getByTestId("tripwire-badge").getAttribute("title")).toMatch(
      /not yet — 0 users/
    );
  });
});

describe("the field", () => {
  it("commits a renamed script as ONE spec patch and nothing else", async () => {
    render(<TripwireField task={task()} runtime={runtime()} />);
    await waitFor(() => expect(scripts).toHaveBeenCalled());

    const name = screen.getByLabelText("Tripwire script") as HTMLInputElement;
    expect(name.value).toBe("pocketbuddy-first-user");
    fireEvent.change(name, { target: { value: "stripe-signup-cleared" } });
    fireEvent.blur(name);
    expect(patch).toHaveBeenCalledWith("t1", { tripwire: "script:stripe-signup-cleared" });
  });

  it("folds the argument into the spec — it is $1, never a field of its own", () => {
    render(<TripwireField task={task()} runtime={runtime()} />);
    const arg = screen.getByLabelText("Tripwire argument");
    fireEvent.change(arg, { target: { value: "prod" } });
    fireEvent.blur(arg);
    expect(patch).toHaveBeenCalledWith("t1", { tripwire: "script:pocketbuddy-first-user prod" });
  });

  it("does not write when the draft matches what is stored", () => {
    render(<TripwireField task={task()} runtime={runtime()} />);
    fireEvent.blur(screen.getByLabelText("Tripwire script"));
    expect(patch).not.toHaveBeenCalled();
  });

  it("'Check now' stamps a request, and is refused while deferred", () => {
    const { rerender } = render(<TripwireField task={task()} runtime={runtime()} />);
    fireEvent.click(screen.getByRole("button", { name: /Check now/ }));
    expect(patch).toHaveBeenCalledWith("t1", { tripwire_check_requested: true });

    patch.mockClear();
    rerender(
      <TripwireField task={task({ defer_until: "2999-01-01T00:00:00.000Z" })} runtime={runtime()} />
    );
    const button = screen.getByRole("button", { name: /Check now/ });
    expect(button).toBeDisabled();
    expect(button.getAttribute("title")).toMatch(/Deferred/);
  });

  it("offers Re-arm only once tripped or errored, and writes exactly 'armed'", () => {
    const { rerender } = render(<TripwireField task={task()} runtime={runtime()} />);
    expect(screen.queryByRole("button", { name: /Re-arm/ })).toBeNull();

    rerender(<TripwireField task={task({ tripwire_state: "tripped" })} runtime={runtime()} />);
    fireEvent.click(screen.getByRole("button", { name: /Re-arm/ }));
    expect(patch).toHaveBeenCalledWith("t1", { tripwire_state: "armed" });
    expect(screen.queryByRole("button", { name: /Check now/ })).toBeNull();
  });

  it("says when the runner has gone quiet", () => {
    // The component reads the real clock, so the fixture's last touch is
    // pinned far enough in the past that no timezone or test-run time can
    // put it inside three intervals.
    const t = task({ updated_at: "2026-01-01 00:00:00" });
    const stale = runtime({ updated_at: "2026-01-01 00:00:00" });
    expect(tripwireUnchecked(t, stale)).toBe(true);
    render(<TripwireField task={t} runtime={stale} />);
    expect(screen.getByTestId("tripwire-unchecked")).toBeInTheDocument();
  });
});

describe("the helpers", () => {
  it("armed is derived from the row and never from status alone", () => {
    expect(isTripwireArmed(task())).toBe(true);
    expect(isTripwireArmed(task({ tripwire_state: "tripped" }))).toBe(false);
    expect(isTripwireArmed(task({ tripwire: null, tripwire_state: "armed" }))).toBe(false);
    expect(isTripwireArmed(task({ status: "done" }))).toBe(false);
  });

  it("parses the spec the way the Rust grammar prints it", () => {
    expect(parseTripwireSpec("script:a")).toEqual({ name: "a", arg: null });
    expect(parseTripwireSpec("script:a prod eu")).toEqual({ name: "a", arg: "prod eu" });
    expect(parseTripwireSpec("agent:the moon is full")).toBeNull();
    expect(parseTripwireSpec(null)).toBeNull();
  });

  it("the three-interval rule never fires for a manual tripwire", () => {
    const now = new Date("2030-01-01T00:00:00Z");
    expect(tripwireUnchecked(task({ tripwire_every_minutes: null }), runtime(), now)).toBe(false);
    // Freshly checked: quiet.
    const fresh = runtime({ checked_at: "2029-12-31 23:00:00" });
    expect(tripwireUnchecked(task(), fresh, now)).toBe(false);
    // Just over 18h since the last check at a 6h cadence: the tell.
    const stale = runtime({ checked_at: "2029-12-31 05:00:00" });
    expect(tripwireUnchecked(task(), stale, now)).toBe(true);
    expect(describeTripwire(task(), stale, now)).toMatch(/is the runner running/);
  });
});

/**
 * A MONITOR is a standing check: completing it re-arms it, so a done monitor
 * is still watched. The UI has to say so — the whole reason this exists is
 * that T107 looked closable and closing it silently ended its check.
 */
describe("monitors", () => {
  const monitor = (overrides: Partial<Task> = {}) =>
    task({ tripwire_on_trip: "monitor", tripwire_every_minutes: 10080, ...overrides });

  it("are badged on the row in every state, done included", () => {
    const { rerender } = render(
      <TaskRow task={monitor({ status: "done" })} selected={false} onSelect={() => {}} onComplete={() => {}} />
    );
    expect(screen.getByTestId("monitor-badge")).toBeInTheDocument();
    expect(screen.queryByTestId("tripwire-badge")).toBeNull();

    rerender(
      <TaskRow
        task={monitor({ tripwire_state: "tripped" })}
        selected={false}
        onSelect={() => {}}
        onComplete={() => {}}
      />
    );
    expect(screen.getByTestId("monitor-badge")).toBeInTheDocument();
  });

  it("read as a monitor in the pane, can be checked while done, and say 'all clear'", () => {
    render(
      <TripwireField
        task={monitor({ status: "done" })}
        runtime={runtime({ checked_at: "2026-08-24 10:00:00", last_outcome: "not_yet" })}
      />
    );
    expect(screen.getByTestId("monitor-state")).toHaveTextContent("Monitor · every 1w");
    expect(screen.getByText(/all clear/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Check now/ })).toBeEnabled();
  });

  it("have no Re-arm button — completing one re-arms it", () => {
    const { rerender } = render(
      <TripwireField task={monitor({ tripwire_state: "tripped" })} runtime={runtime()} />
    );
    expect(screen.queryByRole("button", { name: /Re-arm/ })).toBeNull();
    rerender(<TripwireField task={monitor({ tripwire_state: "error" })} runtime={runtime()} />);
    expect(screen.queryByRole("button", { name: /Re-arm/ })).toBeNull();
    expect(screen.getByText(/complete it to re-arm/)).toBeInTheDocument();
  });

  it("are watched while done, but never HOLD anything", () => {
    expect(isMonitor(monitor())).toBe(true);
    expect(isMonitor(task())).toBe(false);
    expect(isTripwireWatching(monitor({ status: "done" }))).toBe(true);
    expect(isTripwireArmed(monitor({ status: "done" }))).toBe(false);
    // A gate stops being watched when it's done.
    expect(isTripwireWatching(task({ status: "done" }))).toBe(false);
  });
});
