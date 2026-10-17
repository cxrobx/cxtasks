import {
  Archive,
  ArchiveRestore,
  CalendarClock,
  CalendarOff,
  CalendarPlus,
  CircleDot,
  Clipboard,
  FolderOpen,
  Flag,
  Moon,
  RotateCcw,
  Terminal,
  Trash2,
} from "lucide-react";
import { open as openPath } from "@tauri-apps/plugin-shell";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import * as Menu from "@/components/shared/Menu";
import { ACCESS_OPTIONS } from "./TaskDetail";
import { api } from "@/lib/tauri";
import { backgroundTier, STATUS_LABELS } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import { EXECUTOR_LABELS, USER_STATUSES, type Task } from "@/types/task";

/**
 * Right-click menu for a task row.
 *
 * Replaces the `NSMenu` this used to pop from Rust, and the move is not only
 * cosmetic — it deletes a workaround. The native route mutated the DB in Rust
 * and told the window afterwards, so the store never saw the action and could
 * not record its own inverse; Rust had to snapshot each task before mutating
 * and ship the before-state back over a `task-undoable` event purely so ⌘Z
 * would work here as it does everywhere else. Every item below now calls the
 * same store action the buttons call, so undo is simply how the store already
 * behaves and there is nothing to keep in step.
 *
 * Order matches the menu it replaces: delegation first (it is the reason to
 * right-click a row at all), then the three fields worth changing without opening
 * the detail pane, then the destructive tail.
 */

const PRIORITIES = [
  { p: 0, label: "P0 — Highest" },
  { p: 1, label: "P1" },
  { p: 2, label: "P2" },
  { p: 3, label: "P3 — Lowest" },
];

/** 5pm local, `n` days out — the same instant the native menu wrote, so a due
 *  date set from the menu still lands where the rest of the app expects it. */
function atFivePm(daysOut: number): string {
  const d = new Date();
  d.setDate(d.getDate() + daysOut);
  d.setHours(17, 0, 0, 0);
  return d.toISOString();
}

export default function TaskContextMenu({
  task,
  children,
  triggerClassName,
  onOpen,
}: {
  task: Task;
  children: React.ReactNode;
  triggerClassName?: string;
  /** Fired as the menu opens, so the row can take the selection first. It lives
   *  HERE rather than on the row's own `onContextMenu` because the row is
   *  inside Radix's trigger: a handler there that calls `stopPropagation` — as
   *  the old one did, to keep the event off ancestors — stops the event
   *  reaching the trigger and the menu never opens at all. */
  onOpen?: () => void;
}) {
  const patch = useTaskStore((s) => s.patch);
  const complete = useTaskStore((s) => s.complete);
  const trash = useTaskStore((s) => s.trash);
  const restore = useTaskStore((s) => s.restore);
  const archive = useTaskStore((s) => s.archive);
  const unarchive = useTaskStore((s) => s.unarchive);

  /**
   * Set the grant, then queue. In that order and both awaited: the runner reads
   * the tier off the row, so queueing first would race a run against the
   * capability it is supposed to be bounded by.
   *
   * The flags are written exactly as the launch console writes them — the three
   * tiers are mutually exclusive, and `backgroundTier` reads `writes` first.
   */
  const delegate = async (grant: (typeof ACCESS_OPTIONS)[number]["value"]) => {
    await patch(task.id, {
      bg_writes_allowed: grant === "write",
      bg_shell_allowed: grant === "shell",
    });
    const r = await api.dispatch.queueBackground(task.id);
    // Queued either way — but say so when the queue has no reader. Silence here
    // is the whole bug: the badge flips to "Queued" and then nothing ever
    // happens, with nothing on screen to explain why. The native menu had no
    // return value to inspect and had to be told over an event from Rust; this
    // route just reads the result, which is why that event no longer exists.
    if (r.runner_installed && !r.paused) return;
    const store = useTaskStore.getState();
    store.setBackgroundIdle({
      taskId: task.id,
      installed: r.runner_installed,
      paused: r.paused,
    });
    // Select it, so the notice and its two escape hatches land next to the
    // thing that was queued.
    void store.select(task.id);
  };

  // Mirrors `Task::is_dispatchable` — a whitespace-only repo is not a repo.
  const hasRepo = Boolean(task.repo_path && task.repo_path.trim() !== "");
  const trashed = task.deleted_at !== null;
  const archived = task.archived_at !== null;
  // The agent's short action name. `EXECUTOR_LABELS.claude` is "Claude Code",
  // which is the product; the verb wants the handoff. Matches DispatchBar.
  const agent = task.bg_executor === "claude" ? "Claude" : EXECUTOR_LABELS[task.bg_executor];
  const tier = backgroundTier(task);

  return (
    <Menu.Root onOpenChange={(open) => open && onOpen?.()}>
      <Menu.Trigger className={triggerClassName}>{children}</Menu.Trigger>
      <Menu.Content>
        {/* Always enabled: a task with no repo dispatches at $HOME rather than
            refusing to (invariant 12 — foreground only). */}
        <Menu.Item
          icon={Terminal}
          label="Open in Claude"
          onSelect={() => void api.dispatch.foreground(task.id)}
        />
        {/* "Delegate to", not "Queue for": `queued` is a status this passes
            through, not the thing being asked for. A repo stays the visible
            gate because an unattended agent needs a bounded workspace.

            THE TIER IS A SUBMENU RATHER THAN A DEFAULT, and that is the point
            of it. Queueing straight from one item would hand an unattended
            agent whatever grant the task happened to be carrying — a value set
            days ago in another pane, invisible at the moment it matters. The
            one gesture that starts an unsupervised run is the one place the
            grant has to be said out loud, so there is no "just delegate": every
            path through this item names what the agent may touch. The current
            grant is ticked, so choosing is confirming rather than re-deciding
            from nothing. */}
        <Menu.SubMenu icon={Moon} label={`Delegate to ${agent}`}>
          <Menu.Label>What {agent} may touch</Menu.Label>
          {ACCESS_OPTIONS.map((option) => (
            <Menu.Item
              key={option.value}
              icon={option.icon}
              label={`${option.label} · ${option.short}`}
              disabled={!hasRepo}
              checked={tier === option.value}
              onSelect={() => void delegate(option.value)}
            />
          ))}
        </Menu.SubMenu>
        <Menu.Item
          icon={Clipboard}
          label="Copy Seed Prompt"
          onSelect={() => {
            void api.dispatch.prompt(task.id).then((p) => writeText(p));
          }}
        />

        <Menu.Separator />

        <Menu.SubMenu icon={CircleDot} label="Status">
          {/* USER_STATUSES, not TASK_STATUSES: `queued` is the runner's claim
              key, and picking it here would enter the drain queue past every
              check `queue_background` runs first — "Delegate to" is that door.
              A queued task therefore shows no tick, which is accurate. */}
          {USER_STATUSES.map((s) => (
            <Menu.Item
              key={s}
              label={STATUS_LABELS[s]}
              checked={task.status === s}
              onSelect={() => {
                if (s === task.status) return;
                // Done goes through `complete`, never a bare status patch: it
                // is what materializes the next instance of a recurring task
                // and records an undo that takes that instance back away.
                if (s === "done") void complete(task.id);
                else void patch(task.id, { status: s });
              }}
            />
          ))}
        </Menu.SubMenu>

        <Menu.SubMenu icon={Flag} label="Priority">
          {PRIORITIES.map(({ p, label }) => (
            <Menu.Item
              key={p}
              label={label}
              // P0 is the HIGHEST priority (invariant 9), so this list reads
              // top-down as most to least urgent, not the reverse.
              checked={task.priority === p}
              onSelect={() => void patch(task.id, { priority: p })}
            />
          ))}
        </Menu.SubMenu>

        <Menu.SubMenu icon={CalendarClock} label="Due">
          <Menu.Item
            icon={CalendarPlus}
            label="Due Today"
            onSelect={() => void patch(task.id, { due_at: atFivePm(0) })}
          />
          <Menu.Item
            icon={CalendarPlus}
            label="Due Tomorrow"
            onSelect={() => void patch(task.id, { due_at: atFivePm(1) })}
          />
          <Menu.Item
            icon={CalendarOff}
            label="Clear Due Date"
            disabled={task.due_at === null}
            // Explicit null, not omission: absent means "leave alone" in
            // `TaskPatch`, so omitting it would make this item do nothing.
            onSelect={() => void patch(task.id, { due_at: null })}
          />
        </Menu.SubMenu>

        <Menu.Separator />

        <Menu.Item
          icon={FolderOpen}
          label="Reveal Repo in Finder"
          disabled={!hasRepo}
          onSelect={() => {
            if (task.repo_path) void openPath(task.repo_path);
          }}
        />
        <Menu.Item
          icon={archived ? ArchiveRestore : Archive}
          label={archived ? "Move Back to Tasks" : "Archive"}
          // While in the Trash, restore first — archiving something already set
          // aside twice over has no meaning.
          disabled={trashed}
          onSelect={() => {
            if (archived) void unarchive(task.id);
            else void archive(task.id);
          }}
        />
        {/* "Move to Trash", not "Delete": the label is the promise that this is
            undoable, and it matches ⌘⌫ in Finder. Permanent deletion lives in
            the Trash view behind a confirm and is deliberately absent here. */}
        <Menu.Item
          icon={trashed ? RotateCcw : Trash2}
          label={trashed ? "Put Back" : "Move to Trash"}
          danger={!trashed}
          onSelect={() => {
            if (trashed) void restore(task.id);
            else void trash(task.id);
          }}
        />
      </Menu.Content>
    </Menu.Root>
  );
}
