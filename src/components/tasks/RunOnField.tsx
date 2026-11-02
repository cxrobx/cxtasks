/**
 * Which machine this task runs on, for background runs and tripwires alike.
 *
 * One field in two places, because a tripwire task may never open the
 * background console and a background task may have no tripwire. "Default" is a
 * real choice with a real machine behind it, so the row names that machine.
 *
 * A machine that is off doesn't refuse the choice: the task waits for it, and
 * this says so. A machine that could never run the task is the queue's refusal
 * to make, but saying so here costs nothing and saves the trip.
 */
import { useEffect, useState } from "react";

import { Select } from "@/components/shared/Select";
import { api, type HostEntry, type ToolTier } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import type { Task } from "@/types/task";

/** Radix refuses an empty value, and a host id is never this. */
const DEFAULT = "__default__";

/** Why this choice won't run soon, if there is a reason. */
function warning(
  task: Task,
  hosts: HostEntry[],
  tier: ToolTier | undefined,
): string | null {
  const host = task.run_on
    ? hosts.find((h) => h.capability.id === task.run_on)
    : hosts.find((h) => h.capability.takes_unassigned);
  if (task.run_on && !host) {
    return "That machine no longer exists. Pick another one.";
  }
  if (!host) return "No machine takes unassigned work. Choose one in Settings.";
  if (tier && tier !== "read-only" && host.capability.max_tier === "read-only") {
    return `${host.capability.label} is read-only, so it can't run this at this access level.`;
  }
  if (!host.alive) {
    return `${host.capability.label} isn't reporting in. This waits for it.`;
  }
  return null;
}

export function RunOnField({ task, tier }: { task: Task; tier?: ToolTier }) {
  const patch = useTaskStore((s) => s.patch);
  const [hosts, setHosts] = useState<HostEntry[] | null>(null);

  useEffect(() => {
    let live = true;
    api.hosts
      .list()
      .then((list) => live && setHosts(list))
      .catch(() => live && setHosts([]));
    return () => {
      live = false;
    };
  }, []);

  // One machine has nothing to choose between.
  if (!hosts || hosts.length < 2) return null;

  const fallback = hosts.find((h) => h.capability.takes_unassigned);
  const options = [
    {
      value: DEFAULT,
      label: fallback ? `Default (${fallback.capability.label})` : "Default",
    },
    ...hosts.map((h) => ({ value: h.capability.id, label: h.capability.label })),
  ];
  const note = warning(task, hosts, tier);

  return (
    <div data-testid="run-on">
      <div className="flex items-center gap-2">
        <span className="shrink-0 whitespace-nowrap text-[11px] font-medium">Run on</span>
        <Select
          aria-label="Run on"
          className="min-w-0 flex-1"
          value={task.run_on ?? DEFAULT}
          options={options}
          onValueChange={(value) =>
            void patch(task.id, { run_on: value === DEFAULT ? null : value })
          }
        />
      </div>
      {note && (
        <p className="mt-1 text-[10.5px] leading-relaxed text-warning">{note}</p>
      )}
    </div>
  );
}
