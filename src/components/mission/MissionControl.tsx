import { useCallback, useEffect, useState } from "react";
import {
  Bot,
  ChevronRight,
  Circle,
  CircleDot,
  GitBranch,
  Pause,
  ShieldCheck,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { CancelRunButton } from "@/components/tasks/CancelRunButton";
import {
  backgroundTier,
  formatAge,
  formatDue,
  formatShortId,
  formatTokens,
  isOverdue,
  modelLabel,
  parseDbTimestamp,
  PRIORITY_COLORS,
  RUN_STATUS_CLASS,
  RUN_STATUS_LABELS,
  shortenPath,
  TIER_LABELS,
} from "@/lib/format";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import {
  EXECUTOR_LABELS,
  type BudgetStatus,
  type RunWithTask,
  type Task,
} from "@/types/task";

/**
 * The agent dashboard — a lens on the same data the list shows, arranged around
 * the question the list can't answer at a glance: *what is running right now,
 * and what is it costing me?*
 *
 * Everything here is real. The panel deliberately renders nothing rather than a
 * plausible zero when a number isn't known, because the whole point of a spend
 * dashboard is that you can trust it (see `BudgetPanel`'s note on permanent
 * zeros becoming noise).
 */
export function MissionControl() {
  const tasks = useTaskStore((s) => s.tasks);
  const select = useTaskStore((s) => s.select);
  const selectedId = useTaskStore((s) => s.selectedId);
  const runActivityKey = useTaskStore((s) => s.runActivity?.key ?? 0);

  const [runs, setRuns] = useState<RunWithTask[]>([]);
  const [budget, setBudget] = useState<BudgetStatus | null>(null);
  const [doneToday, setDoneToday] = useState<number | null>(null);
  const [queued, setQueued] = useState<Task[]>([]);
  const [needsReview, setNeedsReview] = useState<number | null>(null);

  const loadRuns = useCallback(() => {
    api.runs.recent(24).then(setRuns).catch(() => {});
  }, []);

  const load = useCallback(() => {
    loadRuns();
    api.budget.status().then(setBudget).catch(() => {});
    // Queued work is fetched GLOBALLY, not read off the store's `tasks`.
    // Everything above "Your queue" answers "what are my agents doing", which
    // has nothing to do with which view the sidebar happens to have selected —
    // and mixing the two scopes put an "In review: 0" tile directly above a
    // needs-review card, which is the kind of contradiction that costs a
    // dashboard its credibility outright.
    api.tasks
      .list({ status: "queued", limit: 100 })
      .then(setQueued)
      .catch(() => {});
    // Fetched here rather than read from the store's cached `counts` so that
    // all four tiles share ONE refresh cadence. Sourcing a single tile from the
    // store left it stale next to neighbours polling every 10s — which showed
    // up on screen as "In review: 0" sitting above an in-review card.
    api.tasks
      .list({ status: "in_review", limit: 200 })
      .then((rows) => setNeedsReview(rows.length))
      .catch(() => {});
    // No `done_today` command exists, and adding one to render a single tile
    // isn't worth a migration's worth of surface — the Done view already
    // carries `completed_at`, so the day filter happens here.
    api.tasks
      .list({ view: "done", limit: 300 })
      .then((rows) => setDoneToday(rows.filter(isCompletedToday).length))
      .catch(() => {});
  }, [loadRuns]);

  // Retained as recovery for the best-effort bridge: activity hints provide
  // the fast path below, while polling catches app sleep, close, or a dropped
  // socket write. Finished token counters still land through this same query.
  useEffect(() => {
    load();
    const id = setInterval(load, 10_000);
    return () => clearInterval(id);
  }, [load]);

  // Bridge hints refetch only the run projection. The ten-second poll above is
  // retained as recovery for a best-effort socket notification that was lost
  // while the app slept or was closed.
  useEffect(() => {
    if (runActivityKey > 0) loadRuns();
  }, [loadRuns, runActivityKey]);

  const running = runs.filter((r) => r.status === "running");
  const review = runs.filter(
    (r) => r.status !== "running" && r.task_status === "in_review"
  );
  const attention = runs.filter((run, index) => (run.status === "needs_action" || run.status === "unverified") &&
    run.task_status !== "in_review" && run.task_status !== "done" &&
    run.task_status !== "in_progress" && run.task_status !== "queued" &&
    runs.findIndex((candidate) => candidate.task_id === run.task_id) === index);
  const paused = budget?.paused ?? false;

  const capped =
    budget != null &&
    ((budget.settings.daily_token_cap > 0 &&
      budget.total_tokens_today >= budget.settings.daily_token_cap) ||
      (budget.settings.daily_run_cap > 0 &&
        budget.runs_today >= budget.settings.daily_run_cap));

  return (
    <div className="h-full min-h-0 overflow-y-auto px-4 py-3">
      {/* Banner first: if runs are paused or capped, that is the explanation for
          every zero below it, and burying it under the tiles means reading the
          dashboard as "nothing is happening" instead of "nothing CAN happen". */}
      {(paused || capped) && (
        <div className="mb-3 flex items-center gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-[11px] text-warning">
          {paused ? <Pause size={12} /> : <ShieldCheck size={12} />}
          <span>
            {paused
              ? "Background runs are paused — nothing new will start."
              : "Daily budget spent — no new background runs today."}
          </span>
        </div>
      )}

      <div className="grid grid-cols-4 gap-2.5">
        <Tile
          label="Running"
          value={running.length}
          icon={CircleDot}
          tone={running.length > 0 ? "text-accent" : undefined}
          live={running.length > 0}
        />
        <Tile
          label="Queued"
          value={queued.length}
          icon={Circle}
          tone={queued.length > 0 ? "text-warning" : undefined}
        />
        <Tile
          label="In review"
          value={needsReview}
          icon={GitBranch}
          tone={needsReview ? "text-success" : undefined}
        />
        <Tile label="Done today" value={doneToday} icon={Bot} />
      </div>

      {budget && <BudgetMeter budget={budget} />}

      <Heading>Agent activity</Heading>
      {running.length === 0 && review.length === 0 && attention.length === 0 && queued.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border-subtle px-3 py-6 text-center text-[11px] text-content-faint">
          No agent activity. Give a task a repo and a prompt, then dispatch it.
        </p>
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-2.5">
          {[...running, ...review, ...attention].map((run) => (
            <RunCard
              key={run.id}
              run={run}
              selected={run.task_id === selectedId}
              onSelect={() => void select(run.task_id)}
            />
          ))}
          {queued.map((task) => (
            <QueuedCard
              key={task.id}
              task={task}
              selected={task.id === selectedId}
              onSelect={() => void select(task.id)}
            />
          ))}
        </div>
      )}

      {/* View-scoped, unlike everything above it — the header's secondary slot
          names which view, so the two scopes are labelled rather than guessed. */}
      <Heading>Your queue</Heading>
      <Queue
        tasks={tasks.filter((t) => t.status !== "queued")}
        selectedId={selectedId}
        onSelect={(id) => void select(id)}
      />
    </div>
  );
}

function Heading({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-2 mt-4 text-[10px] font-semibold uppercase tracking-wider text-content-muted">
      {children}
    </div>
  );
}

function Tile({
  label,
  value,
  icon: Icon,
  tone,
  live,
}: {
  label: string;
  /** `null` = not loaded yet. Renders as a dash, never as a confident 0. */
  value: number | null;
  icon: typeof Circle;
  tone?: string;
  live?: boolean;
}) {
  return (
    <div className="rounded-lg border border-border-subtle bg-surface p-3">
      <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wider text-content-faint">
        <Icon size={11} className={cn(tone, live && "animate-pulse")} />
        {label}
      </div>
      <div className="mt-0.5 text-[20px] font-semibold tabular-nums leading-tight">
        {value ?? "—"}
      </div>
    </div>
  );
}

/**
 * The same number the sidebar guards, given the room to be read at a glance.
 * `total_tokens_today` — not output tokens — because output alone understates
 * consumption by orders of magnitude (invariant 6).
 */
function BudgetMeter({ budget }: { budget: BudgetStatus }) {
  const { settings, total_tokens_today, runs_today, running, window_reset } = budget;
  const cap = settings.daily_token_cap;
  const pct = cap > 0 ? Math.min(100, (total_tokens_today / cap) * 100) : 0;
  // Same rule as the sidebar's: after a manual reset this is not today's
  // spend, and the two surfaces reading one number must not label it
  // differently — that is worse than either label alone.
  const since = window_reset ? "since reset" : "today";

  return (
    <div className="mt-2.5 rounded-lg border border-border-subtle bg-surface px-3 py-2.5">
      <div className="flex items-baseline gap-2 text-[11px]">
        <span className="uppercase tracking-wider text-content-faint">
          Tokens {since}
        </span>
        <span className="ml-auto font-mono tabular-nums text-content-secondary">
          {formatTokens(total_tokens_today)}
          {cap > 0 && (
            <span className="text-content-faint">/{formatTokens(cap)}</span>
          )}
        </span>
      </div>
      {cap > 0 && (
        <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-content/10">
          <div
            className={cn(
              "h-full rounded-full transition-all duration-500",
              pct > 85 ? "bg-warning" : "bg-accent"
            )}
            style={{ width: `${pct}%` }}
          />
        </div>
      )}
      <div className="mt-1.5 flex gap-3 text-[10px] text-content-faint">
        <span className="tabular-nums">
          {runs_today} runs {since}
        </span>
        <span className="tabular-nums">
          {running} / {settings.max_concurrent} concurrent
        </span>
        {/* Labelled, because with two agents an unlabelled model name reads as
            "what everything runs on" and it is only Claude's default. */}
        <span className="ml-auto font-mono">
          Claude: {modelLabel(settings.model)}
        </span>
      </div>
    </div>
  );
}

function RunCard({
  run,
  selected,
  onSelect,
}: {
  run: RunWithTask;
  selected: boolean;
  onSelect: () => void;
}) {
  const live = run.status === "running";
  const tokens =
    (run.input_tokens ?? 0) +
    (run.cache_read_tokens ?? 0) +
    (run.cache_write_tokens ?? 0) +
    (run.output_tokens ?? 0);

  return (
    // The card IS a button, so Stop cannot live inside it — nesting a button in
    // a button is invalid markup and React warns. It sits as a sibling in a
    // positioned wrapper instead, which also keeps a click on Stop from
    // selecting the card on its way through.
    <div className="relative">
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "rounded-lg border p-3 text-left transition-colors",
        // Selection has to be visible on the card itself, not only in the
        // detail pane: that pane is collapsible, and with it shut a click that
        // changed nothing on screen reads as a dead control.
        selected ? "bg-accent/12" : "bg-surface hover:bg-elevated",
        live
          ? "border-accent/30"
          : run.status === "ready_for_review" || run.status === "succeeded"
            ? "border-border-subtle"
            : run.status === "needs_action" || run.status === "unverified"
              ? "border-warning/25"
            : "border-error/25"
      )}
    >
      <div className="flex items-center gap-1.5 text-[10.5px]">
        {live ? (
          <>
            <span className="relative flex size-1.5">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-accent opacity-60" />
              <span className="relative inline-flex size-1.5 rounded-full bg-accent" />
            </span>
            <span className="font-medium text-accent">
              Running · {formatAge(run.started_at)}
            </span>
          </>
        ) : (
          <>
            {/* Outcome labels match the detail pane, including unfinished work. */}
            <GitBranch size={10} className={RUN_STATUS_CLASS[run.status]} />
            <span className={cn("font-medium", RUN_STATUS_CLASS[run.status])}>
              {RUN_STATUS_LABELS[run.status] ?? run.status}
            </span>
          </>
        )}
        {/* Which agent, then fg/bg. The executor comes first because it is the
            one that changes what the result means — two runs on the same task
            can now have been produced by different agents, and a card that
            doesn't say which is a card you can't act on. Read off the RUN, not
            the task: switching the task's agent must not relabel history. */}
        <span
          className="ml-auto shrink-0 rounded bg-content/8 px-1 text-[9px] text-content-muted"
          title={`Produced by ${EXECUTOR_LABELS[run.executor]}`}
        >
          {EXECUTOR_LABELS[run.executor]}
        </span>
        <span className="shrink-0 rounded bg-content/8 px-1 font-mono text-[9px] text-content-muted">
          {run.mode === "background" ? "bg" : "fg"}
        </span>
      </div>

      <div className="mt-1.5 flex items-baseline gap-1.5">
        {run.task_short_id !== null && (
          <span className="shrink-0 font-mono text-[10px] tabular-nums text-content-faint">
            {formatShortId(run.task_short_id)}
          </span>
        )}
        <span className="truncate text-[12px] font-medium">{run.task_title}</span>
      </div>

      <div className="mt-0.5 flex items-center gap-1.5 text-[10px] text-content-faint">
        {run.task_project && <span className="truncate">#{run.task_project}</span>}
        {/* "Codex default" rather than blank when no model was recorded: the
            CLI picked one and we genuinely don't know which, so naming a model
            would be inventing it and showing nothing reads as a missing field. */}
        {modelLabel(run.model, run.executor) && (
          <span className="truncate font-mono">
            {modelLabel(run.model, run.executor)}
          </span>
        )}
      </div>

      {/* Absent rather than 0 while a run is in flight: the counters are written
          when it finishes, so a live "0 tokens" would be a lie about the one
          number this panel exists to keep honest. */}
      {tokens > 0 && (
        <div className="mt-1.5 font-mono text-[10.5px] tabular-nums text-content-secondary">
          {tokens.toLocaleString()}{" "}
          <span className="text-content-faint">tokens</span>
        </div>
      )}

      {run.latest_event_summary && (
        <div className="mt-2 flex min-w-0 items-center gap-1.5 border-t border-border-subtle pt-2 text-[10.5px] text-content-secondary">
          <span
            className={cn(
              "size-1.5 shrink-0 rounded-full",
              run.latest_event_phase === "failed"
                ? "bg-error"
                : run.latest_event_phase === "succeeded"
                  ? "bg-success"
                  : "animate-pulse bg-accent"
            )}
          />
          <span className="truncate">{run.latest_event_summary}</span>
        </div>
      )}
    </button>
    {live && (
      <CancelRunButton
        runId={run.id}
        className="absolute bottom-2 right-2 bg-elevated"
      />
    )}
    </div>
  );
}

function QueuedCard({
  task,
  selected,
  onSelect,
}: {
  task: Task;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "rounded-lg border border-border-subtle p-3 text-left transition-colors",
        selected ? "bg-accent/12" : "bg-surface hover:bg-elevated"
      )}
    >
      <div className="flex items-center gap-1.5 text-[10.5px]">
        <Circle size={10} className="text-warning" />
        <span className="font-medium text-warning">Queued</span>
        {/* Read-only is the DEFAULT for background runs (invariant 7), so the
            card states the grant before it runs — that's when knowing changes
            what you'd do. `shell` reads as its own tier rather than folding
            into "read-only", because a shell can write. The agent is named for
            the same reason: this is still a decision you can change. */}
        <span className="ml-auto shrink-0 rounded bg-content/8 px-1 text-[9px] text-content-muted">
          {EXECUTOR_LABELS[task.bg_executor]}
        </span>
        <span className="shrink-0 rounded bg-content/8 px-1 text-[9px] text-content-muted">
          {TIER_LABELS[backgroundTier(task)]}
        </span>
      </div>
      <div className="mt-1.5 flex items-baseline gap-1.5">
        {task.short_id !== null && (
          <span className="shrink-0 font-mono text-[10px] tabular-nums text-content-faint">
            {formatShortId(task.short_id)}
          </span>
        )}
        <span className="truncate text-[12px] font-medium">{task.title}</span>
      </div>
      <div className="mt-0.5 truncate text-[10px] text-content-faint">
        {task.repo_path ? shortenPath(task.repo_path) : "no repo"}
      </div>
    </button>
  );
}

function Queue({
  tasks,
  selectedId,
  onSelect,
}: {
  tasks: Task[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  if (tasks.length === 0) {
    return (
      <p className="rounded-lg border border-dashed border-border-subtle px-3 py-6 text-center text-[11px] text-content-faint">
        Nothing here.
      </p>
    );
  }
  return (
    <div className="overflow-hidden rounded-lg border border-border-subtle">
      {tasks.map((task, i) => {
        const due = formatDue(task.due_at);
        return (
          <button
            type="button"
            key={task.id}
            onClick={() => onSelect(task.id)}
            className={cn(
              "flex w-full items-center gap-2 px-3 py-2 text-left",
              i > 0 && "border-t border-border-subtle",
              task.id === selectedId ? "bg-accent/12" : "bg-surface hover:bg-elevated"
            )}
          >
            <span
              className={cn(
                "w-4 shrink-0 font-mono text-[10px] font-semibold tabular-nums",
                PRIORITY_COLORS[task.priority]
              )}
            >
              P{task.priority}
            </span>
            {task.short_id !== null && (
              <span className="shrink-0 font-mono text-[10px] tabular-nums text-content-faint">
                {formatShortId(task.short_id)}
              </span>
            )}
            <span className="min-w-0 flex-1 truncate text-[12.5px]">{task.title}</span>
            {task.project && (
              <span className="shrink-0 text-[10px] text-content-faint">
                #{task.project}
              </span>
            )}
            {due && (
              <span
                className={cn(
                  "shrink-0 text-[11px] tabular-nums",
                  isOverdue(task) ? "font-medium text-error" : "text-content-muted"
                )}
              >
                {due}
              </span>
            )}
            <ChevronRight size={12} className="shrink-0 text-content-faint" />
          </button>
        );
      })}
    </div>
  );
}

/** `completed_at` is a bookkeeping timestamp — UTC, space-separated, no `Z` —
 * so it must go through `parseDbTimestamp`, never `new Date` (see gotcha in
 * `format.ts`). "Today" is the user's local day, matching the runner's own
 * daily counters. */
function isCompletedToday(task: Task): boolean {
  const at = parseDbTimestamp(task.completed_at);
  if (!at) return false;
  const now = new Date();
  return (
    at.getFullYear() === now.getFullYear() &&
    at.getMonth() === now.getMonth() &&
    at.getDate() === now.getDate()
  );
}
