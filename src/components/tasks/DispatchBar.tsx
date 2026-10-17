import { useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Clipboard,
  GitBranch,
  Loader2,
  Moon,
  Play,
  Power,
  Search,
  Terminal,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import { INTENT_HINTS, otherExecutor } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import {
  BACKGROUND_INTENTS,
  EXECUTOR_LABELS,
  type BackgroundIntent,
  type Task,
  type TaskRun,
} from "@/types/task";

/**
 * Dispatch controls. Rendered for every task, including a plain todo with no
 * `repo_path` — foreground dispatch falls back to the user's home directory
 * (`resolve_repo_or_home` on the Rust side) rather than refusing to dispatch.
 * The delegate button stays disabled without an explicit repo_path: an
 * unattended run needs a bounded workspace. Clicking it is itself the opt-in;
 * there is no second checkbox repeating that decision. The toasts still speak
 * of the task being "queued", which is not a leftover — delegating is the
 * request and `queued` is the state it lands in, and that is the word the
 * status badge will be showing a second later.
 *
 * **"Open in Claude" is Claude whatever the task's background agent is**, and
 * says so in its label. Foreground opens a session the user drives, and the
 * write-back it is told to perform (`complete_task`) is a CXTasks MCP tool.
 */
export function DispatchBar({
  task,
  reviewTarget,
}: {
  task: Task;
  /**
   * The run a review would audit — `lastHandoffRun(runs)`, resolved by the
   * caller. **Required, with no default**, because this component's label can
   * now be wrong: a caller that forgot to pass it should be a compile error
   * rather than a button that quietly reads "Delegate to…" on an armed review.
   *
   * Passed down rather than read from `taskStore.runs` here — those are the
   * SELECTED task's runs, and the "scoped to THIS task" coupling below already
   * bit once.
   */
  reviewTarget: TaskRun | null;
}) {
  const refresh = useTaskStore((s) => s.refresh);
  const patch = useTaskStore((s) => s.patch);
  const setBackgroundIdle = useTaskStore((s) => s.setBackgroundIdle);
  // Scoped to THIS task: queueing something else, then opening this one, must
  // not show a warning about a queue that isn't this task's.
  const idle = useTaskStore((s) =>
    s.backgroundIdle?.taskId === task.id ? s.backgroundIdle : null
  );
  const [busy, setBusy] = useState<null | "fg" | "bg" | "copy" | "wake">(null);
  const [message, setMessage] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [readinessOverrideFor, setReadinessOverrideFor] = useState<string | null>(null);
  const setIdle = (v: { installed: boolean; paused: boolean } | null) =>
    setBackgroundIdle(v ? { taskId: task.id, ...v } : null);
  const agent = EXECUTOR_LABELS[task.bg_executor];
  const actionAgent = task.bg_executor === "claude" ? "Claude" : agent;
  const mutationCapable = task.bg_shell_allowed || task.bg_writes_allowed;
  const reviewing = task.bg_intent === "review";

  /**
   * Arm or disarm the review. **The preselection happens on this gesture and
   * nowhere else** — there is deliberately no effect watching `bg_intent`.
   *
   * That prohibition is the whole rule, because the obvious implementation IS
   * an effect, and an effect re-fires on remount (`TaskDetailForm` is keyed on
   * `task.id`). Clicking away to another task and back would then re-apply the
   * suggestion over an override the user made in between — silently, and only
   * visible after the run.
   *
   * **One patch carrying all four fields, never three calls.** Each patch bumps
   * `updated_at`, and `TaskDetailForm` resets every draft on
   * `[task.id, task.updated_at]` (gotcha #24) — so three writes is three
   * chances to erase text being typed elsewhere in the pane.
   *
   * The preselections are real writes rather than a virtual default resolved in
   * Rust at queue time, for two reasons. A Rust-side default would derive the
   * TIER from the intent, which is precisely what the skew argument in
   * `src-tauri/src/intent.rs` forbids. And a real write is what flips
   * `bg_executor`, which is what makes the rail's availability probe fire and
   * say "Codex is not logged in" while the decision is still free.
   */
  const armIntent = (next: BackgroundIntent) => {
    if (next === task.bg_intent) return;
    if (next === "work") {
      // Deliberately asymmetric: disarming does NOT restore the previous tier
      // and executor. Remembering them is hidden state that goes stale, and the
      // rail is already showing the current values two inches above — the
      // receipt is on screen. The exact "put it back" is ⌘Z.
      void patch(task.id, { bg_intent: "work" });
      return;
    }
    const author = reviewTarget?.executor;
    const suggested = author ? otherExecutor(author) : task.bg_executor;
    void patch(task.id, {
      bg_intent: "review",
      bg_writes_allowed: false,
      bg_shell_allowed: false,
      ...(suggested !== task.bg_executor ? { bg_executor: suggested } : {}),
    });
  };

  const run = async (
    which: "fg" | "bg" | "copy" | "wake",
    fn: () => Promise<string>
  ) => {
    setBusy(which);
    setFailed(false);
    try {
      setMessage(await fn());
    } catch (e) {
      setFailed(true);
      setMessage(String(e));
    } finally {
      setBusy(null);
      void refresh();
    }
  };

  return (
    <div className="execution-console__dispatch flex flex-col gap-2.5 border-t border-accent/15 bg-accent/7 px-4 py-3.5">
      {/* Its OWN row, not a fourth button: `.execution-console__actions` is
          pinned to two columns under 320px, and a row costs no CSS. It sits
          here rather than as a fifth rail node because the rail holds standing
          configuration hand-tuned to exactly four columns, while intent is a
          per-dispatch act — and gotcha #24's lesson is that it must sit next to
          the button it changes, not behind a panel you'd discover it after
          deciding.

          Hidden entirely until a finished run has left a handoff. Showing it
          sooner means the first interaction with the feature is the queue
          guard's refusal. `task.bg_intent === "review"` is ORed in so an armed
          review whose target somehow vanished can still be disarmed — the
          control and the button must never disagree about what will run. */}
      {(reviewTarget !== null || reviewing) && (
        <fieldset className="flex flex-col gap-1.5">
          <legend className="sr-only">What this run does</legend>
          <div className="grid grid-cols-2 gap-2">
            {BACKGROUND_INTENTS.map((i) => (
              <label
                key={i.value}
                title={INTENT_HINTS[i.value]}
                className={cn(
                  "relative min-w-0 cursor-default rounded-lg border px-2.5 py-1.5 text-center text-[11px] font-medium transition-colors",
                  task.bg_intent === i.value
                    ? "border-accent/40 bg-accent/10 text-content"
                    : "border-border-subtle bg-content/3 text-content-secondary hover:border-border hover:bg-content/6"
                )}
              >
                <input
                  type="radio"
                  className="sr-only"
                  name={`bg-intent-${task.id}`}
                  checked={task.bg_intent === i.value}
                  onChange={() => armIntent(i.value)}
                />
                {i.label}
              </label>
            ))}
          </div>
          {/* Only when armed. A line saying "this run does the work" on every
              task grows the bar for the 95% case and makes the mark mean
              nothing — the argument `PRIORITY_EDGE` makes for a transparent P3. */}
          {reviewing && (
            <p className="text-[10.5px] leading-snug text-content-faint">
              Audits the last run&apos;s handoff · doesn&apos;t do the work
            </p>
          )}
        </fieldset>
      )}

      <div className="flex items-center gap-1.5 text-[10.5px] leading-snug text-content-faint">
        {mutationCapable ? (
          <GitBranch size={10} className="shrink-0 text-accent" />
        ) : (
          <Search size={10} className="shrink-0 text-accent" />
        )}
        {mutationCapable
          ? "Snapshot on launch · changes can be rolled back"
          : "Read-only run · workspace changes are denied"}
      </div>

      <div className="execution-console__actions grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-2">
        {/* The label names the DESTINATION and the INTENT, not the mechanism.
            "Queue for …" described the first intermediate state rather than
            what you're asking for, which made handing the work off entirely
            read as a lesser sibling of "Open in Claude". With two agents the
            destination still has to be in the label: this button is the last
            thing you see before something runs unattended, and reading
            "Delegate to Codex" is how you catch the wrong executor while it's
            still free to notice.

            The accessible name is the visible text, not a longer variant of it
            — `aria-label` overrides the content, so naming the agent "Claude
            Code" here while the button reads "Claude" leaves a screen reader
            and the screen disagreeing about the same control. */}
        <button
          type="button"
          aria-label={
            reviewing ? `Review with ${actionAgent}` : `Delegate to ${actionAgent}`
          }
          disabled={busy !== null || !task.repo_path}
          title={
            !task.repo_path
              ? "Add a repo path first — unattended runs need a bounded workspace"
              : reviewing
                ? `Hand the last run's handoff to ${agent} for a second opinion`
                : `Hand this task to the background runner (${agent})`
          }
          onClick={() =>
            run("bg", async () => {
              if (readinessOverrideFor !== task.id) {
                try {
                  const readiness = await api.jev.assessReadiness(task.id);
                  if (readiness?.should_block) {
                    setReadinessOverrideFor(task.id);
                    return `Jev recommends checking this task first: ${readiness.note}. Click Delegate again to queue it anyway.`;
                  }
                } catch (e) {
                  // A judgment service outage must not make the runner
                  // unavailable. Queueing already has its own hard guards.
                  console.warn("Jev readiness check failed", e);
                }
              }
              setReadinessOverrideFor(null);
              const r = await api.dispatch.queueBackground(task.id);
              const who = EXECUTOR_LABELS[r.executor];
              // Read off the POST-queue row, not the prop captured at render:
              // a run that finished between paint and click resets the intent,
              // and the toast must not promise a review that will not happen.
              const queued =
                r.task.bg_intent === "review"
                  ? `Queued a review for ${who}`
                  : `Queued for ${who}`;
              // Queued either way — but say so honestly when the queue has no
              // reader. Silence here is the whole bug: the badge flips to
              // "Queued" and then nothing ever happens, with nothing on screen
              // to explain why. One shared tail per branch, so the two intents'
              // idle copy cannot drift.
              if (!r.runner_installed || r.paused) {
                setIdle({ installed: r.runner_installed, paused: r.paused });
                return r.paused
                  ? `${queued} — but background runs are paused by the kill switch.`
                  : `${queued} — but the background runner is off, so nothing will pick it up.`;
              }
              setIdle(null);
              return `${queued}. The runner picks it up on its next tick.`;
            })
          }
          className={cn(
            "execution-console__queue flex min-w-0 items-center justify-center gap-1.5 rounded-lg bg-accent px-2.5 py-2.5 text-[11.5px] font-semibold leading-tight text-white",
            "hover:bg-accent-hover disabled:opacity-40"
          )}
        >
          {busy === "bg" ? (
            <Loader2 size={13} className="shrink-0 animate-spin" />
          ) : (
            <Moon size={13} className="shrink-0" />
          )}
          {/* Wraps rather than truncates: the agent name is the part that
              must stay readable, and "Delegate anyway to Codex" overflowed
              the 1fr column at ordinary pane widths. */}
          <span className="min-w-0 text-center">
            {readinessOverrideFor === task.id
              ? `Delegate anyway to ${actionAgent}`
              : `${reviewing ? "Review with" : "Delegate to"} ${actionAgent}`}
          </span>
        </button>

        <button
          type="button"
          disabled={busy !== null}
          onClick={() =>
            run("fg", async () => {
              const r = await api.dispatch.foreground(task.id);
              return `Opened a Claude session in ${r.repo_path}`;
            })
          }
          className={cn(
            "execution-console__open flex shrink-0 items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-2.5 text-[11px] font-medium text-content-secondary",
            "hover:bg-content/8 hover:text-content disabled:opacity-50"
          )}
        >
          {busy === "fg" ? (
            <Loader2 size={12} className="animate-spin" />
          ) : (
            <Terminal size={12} />
          )}
          Open in Claude
        </button>

        <button
          type="button"
          aria-label="Copy prompt"
          disabled={busy !== null}
          title="Copy the seed prompt to paste into a session you open yourself"
          onClick={() =>
            run("copy", async () => {
              await writeText(await api.dispatch.prompt(task.id));
              return "Seed prompt copied.";
            })
          }
          className="execution-console__copy shrink-0 rounded-lg border border-border bg-surface p-2.5 text-content-muted hover:bg-content/8 hover:text-content disabled:opacity-50"
        >
          <Clipboard size={12} />
        </button>
      </div>

      {message && (
        <p className={cn("text-[10.5px] leading-relaxed", failed ? "text-error" : "text-content-muted")}>
          {message}
        </p>
      )}

      {/* Two ways out, deliberately both offered.
          "Run once" is the one that matches what was actually clicked: you asked
          for THIS task to run, not for a LaunchAgent that dispatches unattended
          agents at every login from now on. Auto-installing on a context-menu
          click would quietly make that second, much larger decision for you —
          so it stays a button with its own label, and it's the secondary one. */}
      {idle && (
        <div className="flex flex-wrap items-center gap-2 border-t border-border-subtle pt-2">
          {idle.paused ? (
            <span className="text-[11px] text-content-faint">
              Clear the kill switch to let runs resume.
            </span>
          ) : (
            <>
              <button
                type="button"
                disabled={busy !== null}
                onClick={() =>
                  run("wake", async () => {
                    await api.runner.runOnce();
                    setIdle(null);
                    return "Draining the queue once — nothing installed.";
                  })
                }
                className="flex items-center gap-1.5 rounded-md bg-accent px-2 py-1 font-medium text-white hover:bg-accent-hover disabled:opacity-50"
              >
                {busy === "wake" ? (
                  <Loader2 size={12} className="animate-spin" />
                ) : (
                  <Play size={12} />
                )}
                Run it now
              </button>
              <button
                type="button"
                disabled={busy !== null}
                title="Installs a LaunchAgent that drains the queue at every login"
                onClick={() =>
                  run("wake", async () => {
                    const ok = await api.runner.install();
                    if (ok) setIdle(null);
                    return ok
                      ? "Background runner enabled — it drains the queue every 20s."
                      : "Could not enable the runner.";
                  })
                }
                className="flex items-center gap-1.5 rounded-md border border-border px-2 py-1 text-content-secondary hover:bg-content/8 disabled:opacity-50"
              >
                <Power size={12} />
                Turn the runner on
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
