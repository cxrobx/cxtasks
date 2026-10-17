import { Ban, CalendarClock, MessageSquarePlus, X, Zap } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { formatAge, formatDue, isDeferred, isTripwireArmed } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import type { DepEdge, Task, TripwireRuntime } from "@/types/task";

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";

/**
 * "Waiting on": the ONE place the pane says why a task is held, whatever the
 * cause. Three of the four holds are derived elsewhere on the pane — a defer
 * date, an armed tripwire, an open dependency — and each is repeated here as a
 * read-only line, because a task that is plainly waiting on a condition with a
 * field that reads "nothing" is a lie the reader has to disprove by scrolling.
 * The fourth, the written reason, is the only editable part.
 *
 * Shape follows `DateField`: nothing to show → one dashed affordance, and the
 * input only exists once there is a value (or the affordance was clicked).
 * Empty on blur is the release.
 */
export function HoldField({
  task,
  runtime,
  onCommit,
  initialEditing = false,
}: {
  task: Task;
  runtime: TripwireRuntime | undefined;
  onCommit: (reason: string | null) => void;
  /** Open straight into the input — the property strip's "Waiting reason"
   *  pick, where a "Not waiting" button to click first would be a dead step. */
  initialEditing?: boolean;
}) {
  const blockedBy = useTaskStore((s) => s.relations.blocked_by);
  const select = useTaskStore((s) => s.select);
  const [draft, setDraft] = useState(task.blocked_reason ?? "");
  const [editing, setEditing] = useState(initialEditing);

  // A live-refresh can replace the task under us, exactly as in TaskDetail.
  // Only on a real CHANGE of task or version: the mount already starts in that
  // state, and resetting there would throw `initialEditing` away on the first
  // frame. Compared by value rather than skipped by a "first run" flag, which
  // StrictMode's double-invoked mount effect would defeat.
  const seen = useRef(`${task.id}|${task.updated_at}`);
  useEffect(() => {
    const key = `${task.id}|${task.updated_at}`;
    if (seen.current === key) return;
    seen.current = key;
    setDraft(task.blocked_reason ?? "");
    setEditing(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [task.id, task.updated_at]);

  const commit = () => {
    const reason = draft.trim();
    setEditing(false);
    if (reason === (task.blocked_reason ?? "")) return;
    onCommit(reason || null);
  };

  const deferred = isDeferred(task);
  const armed = isTripwireArmed(task);
  const openBlockers: DepEdge[] = blockedBy.filter((b) => b.is_open);
  const derived = deferred || armed || openBlockers.length > 0;
  const showInput = editing || Boolean(task.blocked_reason);

  return (
    <div className="flex flex-col gap-1">
      {deferred && (
        <Line icon={<CalendarClock size={12} />}>
          Starts {formatDue(task.defer_until) ?? "later"}
        </Line>
      )}
      {armed && (
        <Line icon={<Zap size={12} />} title={runtime?.last_note ?? undefined}>
          Until the tripwire trips
          {runtime?.checked_at
            ? ` — checked ${formatAge(runtime.checked_at) ?? "?"} ago: ${
                runtime.last_note?.trim() || runtime.last_outcome || "not yet"
              }`
            : " — not checked yet"}
        </Line>
      )}
      {openBlockers.map((b) => (
        <Line key={b.task_id} icon={<Ban size={12} />}>
          Behind{" "}
          <button
            type="button"
            onClick={() => void select(b.task_id)}
            className="text-content hover:text-accent"
          >
            {b.title}
          </button>
        </Line>
      ))}

      {showInput ? (
        <div className="flex items-center gap-1">
          <input
            autoFocus={editing && !task.blocked_reason}
            value={draft}
            placeholder="e.g. Dana's sign-off on the Learning Hub scope"
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                setDraft(task.blocked_reason ?? "");
                setEditing(false);
              }
            }}
            className={cn(FIELD, task.blocked_reason && "border-error/40")}
          />
          {task.blocked_reason && (
            <button
              type="button"
              aria-label="Release"
              title="Release — back on Open and Today"
              onClick={() => onCommit(null)}
              className="shrink-0 rounded p-1 text-content-faint hover:bg-content/8 hover:text-content"
            >
              <X size={12} />
            </button>
          )}
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setEditing(true)}
          className={cn(
            "flex w-full items-center gap-1.5 rounded-md border border-dashed border-border",
            "px-2 py-1 text-left text-content-faint hover:border-accent hover:text-accent"
          )}
        >
          <MessageSquarePlus size={13} />
          {derived ? "Add a reason" : "Not waiting"}
        </button>
      )}
    </div>
  );
}

function Line({
  icon,
  title,
  children,
}: {
  icon: React.ReactNode;
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <div
      className="flex items-center gap-1.5 truncate rounded-md border border-border/60 px-2 py-1 text-[12px] text-content-muted"
      title={title}
    >
      <span className="shrink-0 text-warning">{icon}</span>
      <span className="truncate">{children}</span>
    </div>
  );
}
