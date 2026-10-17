import { useState } from "react";
import { Plus } from "lucide-react";
import { cn } from "@/lib/utils";
import { STATUS_LABELS } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import type { Task, TaskRun } from "@/types/task";
import { TaskActivity } from "@/components/tasks/TaskActivity";
import { linkCount, TaskLinks } from "@/components/tasks/TaskLinks";

type Tab = "activity" | "subtasks" | "links";

/**
 * Everything below Notes and the launch console, as three tabs.
 *
 * It was seven headed sections — Tags, Subtasks, Blocked by, Continuity, Runs,
 * Comments, and the set-aside footer — rendering whether or not they held
 * anything. On a bare task, the common case, three of them existed only to say
 * so ("Nothing blocking this.", "No continuity links.", "Never dispatched."),
 * two inputs were always open, and the half ran ~600px below the note. Tags
 * moved up into the property strip, Archive/Trash behind the pane's ⋯, and
 * the rest is here: the half is only ever as tall as ONE tab.
 *
 * **Each tab's count is what keeps a closed tab honest.** "Is anything in
 * there" is answered without opening it, and the Links count goes red while a
 * blocker is open — a blocker is the one link that changes what you can do,
 * and it must not be hidden by being on a tab you didn't pick. (The property
 * strip's Waiting chip says it too.)
 *
 * **The tab is per-task**, like the launch console's collapse: the parent
 * form is keyed on `task.id`, so a new task starts from its own default. The
 * default is DERIVED until you pick — the first tab with something in it —
 * because runs, comments and relations load after the pane mounts, and a
 * default fixed at mount would be decided against empty lists.
 */
export function DetailTabs({
  task,
  subtasks,
  runs,
}: {
  task: Task;
  subtasks: Task[];
  runs: TaskRun[];
}) {
  const comments = useTaskStore((s) => s.comments);
  const relations = useTaskStore((s) => s.relations);
  const [picked, setPicked] = useState<Tab | null>(null);

  const events = comments.length + runs.length;
  const links = linkCount(relations);
  const done = subtasks.filter((s) => s.status === "done").length;
  const tab: Tab =
    picked ?? (events > 0 ? "activity" : subtasks.length > 0 ? "subtasks" : links > 0 ? "links" : "activity");

  const tabs: { key: Tab; label: string; count: string | null; tone?: string }[] = [
    { key: "activity", label: "Activity", count: events ? String(events) : null },
    {
      key: "subtasks",
      label: "Subtasks",
      count: subtasks.length ? `${done}/${subtasks.length}` : null,
    },
    {
      key: "links",
      label: "Links",
      count: links ? String(links) : null,
      tone: relations.is_blocked
        ? "bg-error/15 text-error"
        : relations.is_superseded
          ? "bg-warning/15 text-warning"
          : undefined,
    },
  ];

  return (
    <section className="flex flex-col gap-3">
      <div role="tablist" aria-label="Task details" className="flex items-center gap-0.5 border-b border-border-subtle">
        {tabs.map((t) => (
          <button
            key={t.key}
            type="button"
            role="tab"
            id={`detail-tab-${t.key}`}
            aria-selected={tab === t.key}
            aria-controls="detail-tabpanel"
            onClick={() => setPicked(t.key)}
            className={cn(
              "-mb-px flex items-center gap-1.5 border-b-2 px-2 pb-1.5 pt-0.5 text-[12px]",
              tab === t.key
                ? "border-accent text-content"
                : "border-transparent text-content-faint hover:text-content-secondary"
            )}
          >
            {t.label}
            {t.count && (
              <span
                className={cn(
                  "rounded-full px-1.5 text-[10px] tabular-nums",
                  t.tone ?? "bg-content/8 text-content-muted"
                )}
              >
                {t.count}
              </span>
            )}
          </button>
        ))}
      </div>

      <div role="tabpanel" id="detail-tabpanel" aria-labelledby={`detail-tab-${tab}`}>
        {tab === "activity" && <TaskActivity task={task} runs={runs} />}
        {tab === "subtasks" && <Subtasks task={task} subtasks={subtasks} />}
        {tab === "links" && <TaskLinks task={task} />}
      </div>
    </section>
  );
}

function Subtasks({ task, subtasks }: { task: Task; subtasks: Task[] }) {
  const addSubtask = useTaskStore((s) => s.addSubtask);
  const [draft, setDraft] = useState("");
  return (
    <div className="flex flex-col gap-1.5">
      {subtasks.length > 0 && (
        <ul className="flex flex-col gap-1">
          {subtasks.map((s) => (
            <li key={s.id} className="flex items-center gap-2 text-content-secondary">
              <span className="w-14 shrink-0 text-[11px] text-content-faint">{STATUS_LABELS[s.status]}</span>
              <span className={cn("truncate", s.status === "done" && "text-content-faint line-through")}>
                {s.title}
              </span>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-center gap-1.5">
        <Plus size={13} className="shrink-0 text-content-faint" />
        {/* Borderless until hovered or focused: an input that is always open
            shouldn't read as one more form field on every task. */}
        <input
          value={draft}
          aria-label="Add subtask"
          placeholder="Add subtask"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && draft.trim()) {
              void addSubtask(task.id, draft);
              setDraft("");
            }
          }}
          className="w-full rounded-md border border-transparent bg-transparent px-1.5 py-1 text-content placeholder:text-content-faint hover:border-border-subtle focus:border-border focus:bg-input"
        />
      </div>
    </div>
  );
}
