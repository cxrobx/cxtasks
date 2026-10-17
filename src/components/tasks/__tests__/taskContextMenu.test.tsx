/**
 * The rendered right-click menu (gotcha #9).
 *
 * Two things here are pinning real bugs rather than describing the component.
 *
 * 1. **The menu has to OPEN.** The row lives inside Radix's trigger, so the
 *    `stopPropagation()` the native version needed in the row's own
 *    `onContextMenu` — to keep the event off ancestors — stops the event
 *    reaching the trigger, and nothing appears at all. That failure is
 *    invisible in a type check and invisible in a diff; it only shows up when
 *    something actually dispatches a `contextmenu`.
 *
 * 2. **Delegating must NOT be able to skip the grant.** The whole point of the
 *    tier submenu is that queueing an unattended agent cannot inherit whatever
 *    capability the row happened to be carrying. So the test asserts the flags
 *    are written BEFORE the queue call, in that order — a race there would run
 *    an agent against a capability that had not landed yet.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import TaskContextMenu from "@/components/tasks/TaskContextMenu";
import { useTaskStore } from "@/stores/taskStore";
import type { Task } from "@/types/task";

const calls: string[] = [];

vi.mock("@/lib/tauri", () => ({
  api: {
    dispatch: {
      foreground: vi.fn(async () => ({ repo_path: "/tmp" })),
      prompt: vi.fn(async () => "seed"),
      queueBackground: vi.fn(async () => {
        calls.push("queue");
        return { runner_installed: true, paused: false, executor: "claude", task: {} };
      }),
    },
  },
}));

vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ writeText: vi.fn() }));

function fixture(over: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "Wire the retry queue",
    body: null,
    status: "todo",
    priority: 2,
    project: "acme",
    due_at: null,
    defer_until: null,
    recurrence: null,
    parent_id: null,
    sort_order: 0,
    created_at: "2026-08-01T00:00:00Z",
    updated_at: "2026-08-01T00:00:00Z",
    completed_at: null,
    repo_path: "~/clients/acme",
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
    short_id: 41,
    tripwire: null,
    tripwire_every_minutes: null,
    tripwire_on_trip: null,
    tripwire_state: null,
    tripwire_tripped_at: null,
    blocked_reason: null,
    ...over,
  };
}

function openMenu(task = fixture()) {
  render(
    <TaskContextMenu task={task}>
      <div data-testid="row">row</div>
    </TaskContextMenu>
  );
  fireEvent.contextMenu(screen.getByTestId("row"));
}

/** Radix opens a submenu on pointer or keyboard, never a plain click, and this
 *  project has no `user-event`. ArrowRight on the focused trigger is both the
 *  deterministic route in jsdom and the one a keyboard user actually takes. */
async function openGrantSubmenu() {
  const trigger = await screen.findByText("Delegate to Claude");
  const item = trigger.closest("[role='menuitem']") as HTMLElement;
  item.focus();
  fireEvent.keyDown(item, { key: "ArrowRight" });
}

afterEach(() => {
  calls.length = 0;
  vi.clearAllMocks();
});

describe("the rendered right-click menu", () => {
  it("opens on contextmenu — the row must not swallow the event", async () => {
    openMenu();
    // If the row ever regains a `stopPropagation()` in its own handler, this is
    // the assertion that fails, and it fails loudly rather than silently
    // shipping a menu nobody can open.
    expect(await screen.findByText("Open in Claude")).toBeTruthy();
  });

  it("offers no way to delegate without naming the grant", async () => {
    openMenu();
    await screen.findByText("Open in Claude");
    // The top level offers the SUBMENU, never a bare "delegate and go".
    expect(screen.queryByText("Delegate to Claude")).toBeTruthy();
    expect(calls).toEqual([]);
  });

  it("writes the capability BEFORE queueing, not after", async () => {
    const patch = vi.fn(async () => {
      calls.push("patch");
    });
    useTaskStore.setState({ patch } as never);

    openMenu();
    await openGrantSubmenu();
    fireEvent.click(await screen.findByText("Build · Edit files"));

    await waitFor(() => expect(calls).toEqual(["patch", "queue"]));
    // `write` is the tier, and the three are mutually exclusive — `shell` false
    // is as load-bearing as `writes` true, since `backgroundTier` reads writes
    // first and a stale shell flag would describe a grant nobody chose.
    expect(patch).toHaveBeenCalledWith("t1", {
      bg_writes_allowed: true,
      bg_shell_allowed: false,
    });
  });

  it("ticks the grant the task already carries, so choosing is confirming", async () => {
    openMenu(fixture({ bg_shell_allowed: true }));
    await openGrantSubmenu();

    const row = (await screen.findByText("Run · Shell")).closest(
      "[role='menuitem']"
    ) as HTMLElement;
    // The tick, specifically — asserting the item merely EXISTS would pass
    // against a menu that never marks the current grant, which is the whole
    // affordance turning "choose a capability" into "confirm this one".
    expect(row.querySelector(".lucide-check")).toBeTruthy();

    const other = (await screen.findByText("Inspect · Read only")).closest(
      "[role='menuitem']"
    ) as HTMLElement;
    expect(other.querySelector(".lucide-check")).toBeNull();
  });

  it("sets any human status from the Status submenu, and Done goes through complete", async () => {
    const patch = vi.fn(async () => {});
    const complete = vi.fn(async () => {});
    useTaskStore.setState({ patch, complete } as never);

    openMenu();
    const trigger = (await screen.findByText("Status")).closest("[role='menuitem']") as HTMLElement;
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ArrowRight" });

    // `queued` is the runner's claim key — offering it would skip every check
    // `queue_background` runs, so it must not be here.
    expect(screen.queryByText("Queued")).toBeNull();
    const current = (await screen.findByText("To do")).closest("[role='menuitem']") as HTMLElement;
    expect(current.querySelector(".lucide-check")).toBeTruthy();

    fireEvent.click(await screen.findByText("In review"));
    await waitFor(() => expect(patch).toHaveBeenCalledWith("t1", { status: "in_review" }));
    expect(complete).not.toHaveBeenCalled();
  });

  it("routes Done through complete, so a recurring task materializes its next instance", async () => {
    const patch = vi.fn(async () => {});
    const complete = vi.fn(async () => {});
    useTaskStore.setState({ patch, complete } as never);

    openMenu();
    const trigger = (await screen.findByText("Status")).closest("[role='menuitem']") as HTMLElement;
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ArrowRight" });
    fireEvent.click(await screen.findByText("Done"));

    await waitFor(() => expect(complete).toHaveBeenCalledWith("t1"));
    expect(patch).not.toHaveBeenCalled();
  });
});
