/**
 * The property strip: only what the task HOLDS gets a chip, the `+` offers the
 * rest, and every editor is a popover — so the two things that can go wrong
 * are a held task that stops looking held, and a typed draft lost when the
 * popover closes over it (gotcha #24).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { PropertyStrip } from "@/components/tasks/PropertyStrip";
import { useTaskStore } from "@/stores/taskStore";
import type { DepEdge, Task, TripwireRuntime } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: { tripwires: { scripts: vi.fn(async () => []) } },
}));

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "task",
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
    short_id: 202,
    tripwire: null,
    tripwire_every_minutes: null,
    tripwire_on_trip: null,
    tripwire_state: null,
    tripwire_tripped_at: null,
    ...overrides,
  } as Task;
}

const inDays = (d: number) => new Date(Date.now() + d * 86_400_000).toISOString();

let patch: ReturnType<typeof vi.fn>;
let setTaskTags: ReturnType<typeof vi.fn>;

function mount(
  t: Task,
  {
    blockedBy = [] as DepEdge[],
    runtime,
    sections = [] as { id: string; name: string }[],
    tags = [] as string[],
  }: {
    blockedBy?: DepEdge[];
    runtime?: TripwireRuntime;
    sections?: { id: string; name: string }[];
    tags?: string[];
  } = {}
) {
  useTaskStore.setState({
    patch,
    moveTaskToSection: vi.fn(async () => {}),
    projectRecords: [{ name: "CXVentures" }, { name: "Old" }],
    sections: sections.map((s, i) => ({ ...s, project: null, sort_order: i })),
    tripwireByTask: runtime ? { [t.id]: runtime } : {},
    relations: {
      blocked_by: blockedBy,
      blocking: [],
      is_blocked: blockedBy.some((b) => b.is_open),
      supersedes: [],
      superseded_by: [],
      related: [],
      is_superseded: false,
    },
    select: vi.fn(async () => {}),
    tags: tags.map((name, i) => ({ id: `g${i}`, name, color: null })),
    allTags: [],
    setTaskTags,
  } as never);
  return render(<PropertyStrip task={t} />);
}

const chip = (name: RegExp) => screen.getByRole("button", { name });
const chipNames = () =>
  screen
    .getAllByRole("button")
    .map((b) => b.getAttribute("aria-label") ?? "")
    .filter((n) => n.includes(":"));

/** Radix menus open on a keydown from their trigger (jsdom has no pointer
 *  capture), the way a keyboard user opens one. */
const openMenu = (el: HTMLElement) => fireEvent.keyDown(el, { key: "Enter" });

beforeEach(() => {
  patch = vi.fn(async () => {});
  setTaskTags = vi.fn(async () => {});
});

describe("what the strip shows", () => {
  it("an untriaged task costs two chips and a +", () => {
    mount(task());
    expect(chipNames()).toEqual(["Status: Inbox", "Priority: P2"]);
    expect(screen.getByRole("button", { name: "Add a property" })).toBeInTheDocument();
  });

  it("every property that holds something gets a chip, in the row's words", () => {
    mount(
      task({
        project: "CXVentures",
        due_at: inDays(3),
        defer_until: inDays(2),
        recurrence: "weekly:MO,TH",
        section_id: "s1",
      }),
      { sections: [{ id: "s1", name: "Backlog" }] }
    );
    expect(chip(/^Project: CXVentures$/)).toBeInTheDocument();
    expect(chip(/^Section: Backlog$/)).toBeInTheDocument();
    expect(chip(/^Due: Due /)).toBeInTheDocument();
    expect(chip(/^Starts: Starts /)).toBeInTheDocument();
    expect(chip(/^Repeats: weekly on Mon, Thu$/)).toBeInTheDocument();
  });

  it("a passed due date reads as overdue, without a 'Due' prefix", () => {
    mount(task({ due_at: inDays(-2) }));
    expect(chip(/^Due: Overdue/)).toBeInTheDocument();
  });
});

describe("a held task still reads as held", () => {
  it("a pending start date is tinted like the row's deferral badge", () => {
    mount(task({ defer_until: inDays(4) }));
    expect(chip(/^Starts:/).className).toContain("text-warning");
  });

  it("an armed tripwire says what the runner last said", () => {
    mount(
      task({ tripwire: "script:crawled teacherhero.org", tripwire_state: "armed", tripwire_every_minutes: 1440 }),
      {
        runtime: {
          task_id: "t1",
          checked_at: new Date(Date.now() - 2 * 3_600_000).toISOString(),
          last_outcome: "not_yet",
          last_note: "3 of 9 crawled",
          updated_at: new Date().toISOString(),
        } as TripwireRuntime,
      }
    );
    const tw = screen.getByTestId("tripwire-chip");
    expect(tw.textContent).toMatch(/^3 of 9 crawled · 2h$/);
    expect(tw.className).toContain("text-warning");
  });

  it("an open blocker gets a Waiting chip even with no reason written", () => {
    mount(task(), {
      blockedBy: [
        { task_id: "b", title: "Ship the API", status: "todo", priority: 2, project: null, is_open: true },
        { task_id: "c", title: "Closed one", status: "done", priority: 2, project: null, is_open: false },
      ],
    });
    const waiting = chip(/^Waiting on: Behind Ship the API$/);
    expect(waiting.className).toContain("text-error");
  });

  it("a written reason is the chip", () => {
    mount(task({ blocked_reason: "Dana's sign-off" }));
    expect(chip(/^Waiting on: Dana's sign-off$/)).toBeInTheDocument();
  });
});

describe("the + menu", () => {
  it("offers only what is missing, and Section only where sections exist", async () => {
    mount(task({ project: "CXVentures", recurrence: "daily" }));
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    const items = (await screen.findAllByRole("menuitem")).map((i) => i.textContent);
    // Tag is always on offer — a task takes any number of them.
    expect(items).toEqual(["Due date", "Start date", "Tripwire", "Waiting reason", "Tag"]);
  });

  it("adding a date writes one at once — a date input is never mounted empty", async () => {
    mount(task());
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Due date" }));
    expect(patch).toHaveBeenCalledWith("t1", { due_at: expect.any(String) });
  });

  it("adding a waiting reason opens straight into the input", async () => {
    mount(task());
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Waiting reason" }));
    expect(await screen.findByPlaceholderText(/sign-off/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Not waiting" })).toBeNull();
  });
});

describe("the status menu", () => {
  it("carries each status's one-line definition, where you choose it", async () => {
    mount(task());
    openMenu(chip(/^Status:/));
    expect(await screen.findByText("Captured, not yet triaged.")).toBeInTheDocument();
    expect(screen.getByText("Triaged and actionable now.")).toBeInTheDocument();
    // `queued` is the runner's claim key and is never offered by hand.
    expect(screen.queryByRole("menuitemradio", { name: /Queued/ })).toBeNull();
  });

  it("a pick writes the status", async () => {
    mount(task());
    openMenu(chip(/^Status:/));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^To do/ }));
    expect(patch).toHaveBeenCalledWith("t1", { status: "todo" });
  });
});

describe("leaving a popover with a draft in it (gotcha #24)", () => {
  it("closing FLUSHES the draft — the unmounted input never blurs on its own", async () => {
    mount(task({ recurrence: "daily" }));
    fireEvent.click(chip(/^Repeats:/));
    const input = await screen.findByRole("textbox", { name: "Repeat rule" });
    input.focus();
    fireEvent.change(input, { target: { value: "weekly:MO" } });

    // Toggling the chip closes the popover without ever moving focus — the
    // exact path where the input is removed while still focused.
    fireEvent.click(chip(/^Repeats:/));
    expect(patch).toHaveBeenCalledWith("t1", { recurrence: "weekly:MO" });
  });

  it("Escape is cancel: nothing is written", async () => {
    mount(task({ recurrence: "daily" }));
    fireEvent.click(chip(/^Repeats:/));
    const input = await screen.findByRole("textbox", { name: "Repeat rule" });
    input.focus();
    fireEvent.change(input, { target: { value: "weekly:MO" } });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(screen.queryByRole("textbox", { name: "Repeat rule" })).toBeNull();
    expect(patch).not.toHaveBeenCalled();
  });

  it("opening and closing without an edit writes nothing", async () => {
    mount(task({ recurrence: "daily" }));
    fireEvent.click(chip(/^Repeats:/));
    (await screen.findByRole("textbox", { name: "Repeat rule" })).focus();
    fireEvent.click(chip(/^Repeats:/));
    expect(patch).not.toHaveBeenCalled();
  });
});

describe("a property being added", () => {
  it("keeps its chip while the popover is open, and drops it if closed empty", async () => {
    vi.useFakeTimers();
    try {
      mount(task());
      openMenu(screen.getByRole("button", { name: "Add a property" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Repeat" }));
      expect(chip(/^Repeats: Repeats…$/)).toBeInTheDocument();

      fireEvent.keyDown(screen.getByRole("textbox", { name: "Repeat rule" }), { key: "Escape" });
      act(() => {
        vi.advanceTimersByTime(500);
      });
      expect(screen.queryByRole("button", { name: /^Repeats:/ })).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

/**
 * Tags moved up from their own section below Notes. They read as the row
 * prints them, and adding one goes through a popover — so the draft-loss rule
 * (gotcha #24) applies to it exactly as to any property.
 */
describe("tags", () => {
  it("render as the row prints them, and are not counted as properties", () => {
    mount(task(), { tags: ["billable", "@sam"] });
    const chips = screen.getAllByTestId("tag-chip").map((c) => c.textContent);
    expect(chips).toEqual(["#billable", "@sam"]);
    expect(chipNames()).toEqual(["Status: Inbox", "Priority: P2"]);
  });

  it("the x removes one", () => {
    mount(task(), { tags: ["billable", "globex"] });
    fireEvent.click(screen.getByRole("button", { name: "Remove tag billable" }));
    expect(setTaskTags).toHaveBeenCalledWith("t1", ["globex"]);
  });

  it("+ → Tag adds one on Enter, normalised the way the old editor did", async () => {
    mount(task(), { tags: ["globex"] });
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Tag" }));
    const input = await screen.findByRole("combobox", { name: "Tag" });
    fireEvent.change(input, { target: { value: "#Billable" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(setTaskTags).toHaveBeenCalledWith("t1", ["globex", "billable"]);
  });

  it("closing the popover mid-type still adds the tag (gotcha #24)", async () => {
    mount(task());
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Tag" }));
    const input = await screen.findByRole("combobox", { name: "Tag" });
    input.focus();
    fireEvent.change(input, { target: { value: "seo" } });
    // Toggle the pending chip: the popover closes with the input still focused.
    fireEvent.click(chip(/^Tag: Tag…$/));
    expect(setTaskTags).toHaveBeenCalledWith("t1", ["seo"]);
  });

  it("a tag it already has is not added twice", async () => {
    mount(task(), { tags: ["seo"] });
    openMenu(screen.getByRole("button", { name: "Add a property" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Tag" }));
    const input = await screen.findByRole("combobox", { name: "Tag" });
    fireEvent.change(input, { target: { value: "seo" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(setTaskTags).not.toHaveBeenCalled();
  });
});

describe("project reassignment", () => {
  it("saves a keyboard-selected project on the first Enter", async () => {
    mount(task({ project: "Old" }));
    fireEvent.click(chip(/^Project: Old$/));
    const input = await screen.findByRole("combobox", { name: "Project" });
    await screen.findByRole("option", { name: "CXVentures" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(patch).toHaveBeenCalledTimes(1);
    expect(patch).toHaveBeenCalledWith("t1", { project: "CXVentures" });
    expect(screen.queryByRole("combobox", { name: "Project" })).toBeNull();
  });

  it("does not save again when the current project is picked", async () => {
    mount(task({ project: "Old" }));
    fireEvent.click(chip(/^Project: Old$/));
    fireEvent.mouseDown(await screen.findByRole("option", { name: "Old" }));
    expect(patch).not.toHaveBeenCalled();
    expect(screen.queryByRole("combobox", { name: "Project" })).toBeNull();
  });

  it("saves a typed project before Enter unmounts the input", async () => {
    mount(task({ project: "Old" }));
    fireEvent.click(chip(/^Project: Old$/));
    const input = await screen.findByRole("combobox", { name: "Project" });
    fireEvent.change(input, { target: { value: "  New project  " } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(patch).toHaveBeenCalledTimes(1);
    expect(patch).toHaveBeenCalledWith("t1", { project: "New project" });
    expect(screen.queryByRole("combobox", { name: "Project" })).toBeNull();
  });

  it("saves a picked project immediately and closes the editor", async () => {
    mount(task({ project: "Old" }));
    fireEvent.click(chip(/^Project: Old$/));
    await screen.findByRole("combobox", { name: "Project" });
    fireEvent.mouseDown(await screen.findByRole("option", { name: "CXVentures" }));
    expect(patch).toHaveBeenCalledTimes(1);
    expect(patch).toHaveBeenCalledWith("t1", { project: "CXVentures" });
    expect(screen.queryByRole("combobox", { name: "Project" })).toBeNull();
  });
});
