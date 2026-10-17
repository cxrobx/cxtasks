import { useMemo, useState } from "react";
import { Plus, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { formatShortId } from "@/lib/format";
import { Combobox } from "@/components/shared/Combobox";
import { useTaskStore } from "@/stores/taskStore";
import type { Task, TaskRelations } from "@/types/task";

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content placeholder:text-content-faint";

type Kind = "blocked_by" | "blocking" | "superseded_by" | "supersedes" | "related";

const KIND_LABEL: Record<Kind, string> = {
  blocked_by: "blocked by",
  blocking: "blocks",
  superseded_by: "superseded by",
  supersedes: "supersedes",
  related: "relates to",
};

interface Link {
  kind: Kind;
  taskId: string;
  title: string;
  reference: string | null;
  /** Only a blocker can be done-and-therefore-harmless; it reads struck through. */
  closed: boolean;
  onRemove: (() => Promise<void>) | null;
}

/** How many links the Links tab holds — the tab's count. */
export function linkCount(r: TaskRelations): number {
  return (
    r.blocked_by.length +
    r.blocking.length +
    (r.supersedes ?? []).length +
    (r.superseded_by ?? []).length +
    (r.related ?? []).length
  );
}

/**
 * Every task-to-task link, in ONE list with the kind on each row.
 *
 * These were three headed sections — Blocked by, Blocking, Continuity — and on
 * most tasks all three said they were empty. The kinds still MEAN different
 * things (a blocker is a peer this task waits on; `supersedes` preserves a
 * conclusion, invariant 27), which is why every row carries its kind as a
 * label rather than the kinds being flattened; what they no longer get is a
 * heading each. Subtasks stay OUT of this list — a subtask is *part of* this
 * task, not a peer — and have their own tab.
 *
 * The pickers are the app's own `Combobox`. They were native `<select>`s,
 * which WKWebView pops up light-grey out of the dark pane (gotcha #6).
 */
export function TaskLinks({ task }: { task: Task }) {
  const relations = useTaskStore((s) => s.relations);
  const tasks = useTaskStore((s) => s.tasks);
  const addDependency = useTaskStore((s) => s.addDependency);
  const removeDependency = useTaskStore((s) => s.removeDependency);
  const addRelation = useTaskStore((s) => s.addRelation);
  const removeRelation = useTaskStore((s) => s.removeRelation);
  const select = useTaskStore((s) => s.select);

  const [adding, setAdding] = useState<"blocker" | "relation" | null>(null);
  const [kind, setKind] = useState<"supersedes" | "relates_to">("supersedes");
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  // A persisted/mocked store from the pre-v26 shape can survive a hot reload.
  // Treat its absent fields as empty until the first refresh replaces it.
  const supersedes = relations.supersedes ?? [];
  const supersededBy = relations.superseded_by ?? [];
  const related = relations.related ?? [];

  const links: Link[] = [
    ...relations.blocked_by.map((b) => ({
      kind: "blocked_by" as const,
      taskId: b.task_id,
      title: b.title,
      reference: null,
      closed: !b.is_open,
      onRemove: () => removeDependency(task.id, b.task_id),
    })),
    ...relations.blocking.map((b) => ({
      kind: "blocking" as const,
      taskId: b.task_id,
      title: b.title,
      reference: null,
      closed: false,
      // Removing a "blocks" edge is the OTHER task's decision, made on its
      // own Blocked-by row — the shipped list never offered it either.
      onRemove: null,
    })),
    ...supersededBy.map((e) => ({
      kind: "superseded_by" as const,
      taskId: e.task_id,
      title: e.title,
      reference: formatShortId(e.short_id),
      closed: false,
      onRemove: () => removeRelation(e.task_id, task.id, "supersedes"),
    })),
    ...supersedes.map((e) => ({
      kind: "supersedes" as const,
      taskId: e.task_id,
      title: e.title,
      reference: formatShortId(e.short_id),
      closed: false,
      onRemove: () => removeRelation(task.id, e.task_id, "supersedes"),
    })),
    ...related.map((e) => ({
      kind: "related" as const,
      taskId: e.task_id,
      title: e.title,
      reference: formatShortId(e.short_id),
      closed: false,
      onRemove: () => removeRelation(task.id, e.task_id, "relates_to"),
    })),
  ];

  /**
   * What the picker offers: anything loaded that isn't this task or already
   * linked the same way. Keyed by the text a pick writes into the field, which
   * is how a pick is told apart from typing — `Combobox` only sets the value.
   */
  const candidates = useMemo(() => {
    const taken = new Set(
      adding === "blocker"
        ? relations.blocked_by.map((b) => b.task_id)
        : [...supersedes, ...supersededBy, ...related].map((e) => e.task_id)
    );
    const byText = new Map<string, string>();
    for (const t of tasks) {
      if (t.id === task.id || taken.has(t.id)) continue;
      const ref = formatShortId(t.short_id);
      byText.set(ref ? `${ref} · ${t.title}` : t.title, t.id);
    }
    return byText;
  }, [adding, tasks, task.id, relations.blocked_by, supersedes, supersededBy, related]);

  const close = () => {
    setAdding(null);
    setDraft("");
    setError(null);
  };

  const pick = async (text: string) => {
    setDraft(text);
    const id = candidates.get(text);
    if (!id) return;
    const err =
      adding === "blocker" ? await addDependency(task.id, id) : await addRelation(task.id, id, kind);
    // A cycle rejection belongs next to the control that caused it, not in a
    // global error banner the user has to go hunting for.
    if (err) setError(err.replace(/^Invalid input: /, ""));
    else close();
  };

  return (
    <section className="flex flex-col gap-1.5">
      {links.length > 0 && (
        <ul className="flex flex-col">
          {links.map((l) => (
            <li key={`${l.kind}-${l.taskId}`} className="group flex items-center gap-2 py-0.5">
              <span
                className={cn(
                  "w-[76px] shrink-0 rounded px-1 py-px text-center text-[10px] font-medium",
                  l.kind === "blocked_by" && !l.closed
                    ? "bg-error/15 text-error"
                    : l.kind === "blocked_by"
                      ? "bg-success/15 text-success"
                      : l.kind === "superseded_by" || l.kind === "supersedes"
                        ? "bg-warning/12 text-warning"
                        : "bg-content/8 text-content-muted"
                )}
              >
                {KIND_LABEL[l.kind]}
              </span>
              {l.reference && (
                <span className="shrink-0 rounded bg-content/10 px-1 py-px text-[10px] text-content-faint">
                  {l.reference}
                </span>
              )}
              <button
                type="button"
                onClick={() => void select(l.taskId)}
                className={cn(
                  "min-w-0 flex-1 truncate text-left hover:underline",
                  l.closed ? "text-content-faint line-through" : "text-content-secondary"
                )}
              >
                {l.title}
              </button>
              {l.onRemove && (
                <button
                  type="button"
                  aria-label={`Remove link to ${l.title}`}
                  onClick={() => void l.onRemove?.()}
                  className="shrink-0 opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100"
                >
                  <X size={11} className="text-content-faint hover:text-error" />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="flex flex-wrap gap-1">
        {(
          [
            ["blocker", "Blocked by…"],
            ["relation", "Supersedes / relates…"],
          ] as const
        ).map(([k, label]) => (
          <button
            key={k}
            type="button"
            aria-expanded={adding === k}
            onClick={() => (adding === k ? close() : (close(), setAdding(k)))}
            className={cn(
              "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] hover:bg-content/6 hover:text-content",
              adding === k ? "bg-accent/10 text-accent" : "text-content-faint"
            )}
          >
            <Plus size={11} />
            {label}
          </button>
        ))}
      </div>

      {adding && (
        <div className="flex flex-col gap-1.5">
          {adding === "relation" && (
            <div className="flex gap-1 text-[11px]">
              {(
                [
                  ["supersedes", "This supersedes…"],
                  ["relates_to", "This relates to…"],
                ] as const
              ).map(([k, label]) => (
                <button
                  key={k}
                  type="button"
                  aria-pressed={kind === k}
                  onClick={() => setKind(k)}
                  className={cn(
                    "rounded-md border px-2 py-0.5",
                    kind === k
                      ? "border-accent/40 bg-accent/10 text-content"
                      : "border-border-subtle text-content-muted hover:bg-content/6"
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          )}
          <Combobox
            autoFocus
            value={draft}
            aria-label={adding === "blocker" ? "Task this one waits on" : "Task to link"}
            placeholder={candidates.size ? "Find a task…" : "No other tasks in the current view"}
            onValueChange={(v) => void pick(v)}
            options={[...candidates.keys()].map((value) => ({ value }))}
            onKeyDown={(e) => {
              if (e.key === "Escape") close();
            }}
            className={FIELD}
            panelClassName="text-[12px]"
          />
          {error && <p className="text-[11px] text-error">{error}</p>}
        </div>
      )}
    </section>
  );
}
