import { useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  Pause,
  Play,
  RotateCcw,
  Settings,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Select } from "@/components/shared/Select";
import { api } from "@/lib/tauri";
import {
  formatShortId,
  formatTokenCap,
  formatTokens,
  parseDbTimestamp,
  parseTokens,
} from "@/lib/format";
import {
  BG_MODELS,
  type BudgetSettings,
  type BudgetStatus,
  type RunWithTask,
} from "@/types/task";
import { useUiStore } from "@/stores/uiStore";
import { useTaskStore } from "@/stores/taskStore";
import { CancelRunButton } from "@/components/tasks/CancelRunButton";

/**
 * Background consumption, always visible at the bottom of the sidebar.
 *
 * This exists because the cost of these runs is otherwise invisible: they burn
 * the Claude **subscription** rate limit rather than a metered bill, so the
 * first sign of overuse is a session saying you're out of usage. Two prior
 * agent-swarm experiments were aborted for burn — this counter is the thing
 * that makes it noticeable before that happens.
 *
 * **Collapsed by default, and deliberately so.** The panel used to render every
 * diagnostic at full volume in every state — `0 / 2` running, `0 / 20` runs,
 * `0 / 2.0M` tokens — including the state where the runner isn't even installed
 * and none of those numbers *can* move. Permanent zeros are noise, and noise is
 * what a spend guard cannot afford to become: a panel you've learned to skip
 * doesn't warn you. So at rest it's one line, and the two things that must
 * survive collapse — the token bar and the capped warning — render outside the
 * disclosure. Everything else is one click away.
 */
export function BudgetPanel() {
  const [status, setStatus] = useState<BudgetStatus | null>(null);
  const [runnerInstalled, setRunnerInstalled] = useState<boolean | null>(null);
  /**
   * Which runs are live, not just how many.
   *
   * "1 running" answers the wrong question. The count tells you the subsystem
   * is busy; what you actually want when you read it is WHICH task — because
   * the next thing you do is go look at it, or stop it, and a bare number gives
   * you no way to get there. So the pill names the tasks and each name selects
   * one.
   */
  const [live, setLive] = useState<RunWithTask[]>([]);
  const [open, setOpen] = useState(false);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);
  const select = useTaskStore((s) => s.select);

  const load = useCallback(() => {
    api.budget
      .status()
      .then(setStatus)
      .catch(() => {
        /* the panel is informational — never block the app on it */
      });
    api.runner.isInstalled().then(setRunnerInstalled).catch(() => {});
    // `recent` is ordered running-first, so a small limit is enough to name
    // everything live — concurrency is capped in the low single digits.
    api.runs
      .recent(12)
      .then((rows) => setLive(rows.filter((r) => r.status === "running")))
      .catch(() => {});
  }, []);

  useEffect(() => {
    load();
    const id = setInterval(load, 15_000);
    return () => clearInterval(id);
  }, [load]);

  /**
   * The 15s poll is fine for counters and far too slow for a list of what is
   * running RIGHT NOW — a stopped run would sit in it for up to fifteen
   * seconds, which reads as the Stop button not having worked. The runner
   * already reports progress over the bridge, so follow that: any run event is
   * a reason to re-read who is live.
   */
  const runActivity = useTaskStore((s) => s.runActivity);
  useEffect(() => {
    if (runActivity) load();
  }, [load, runActivity?.key]);

  const toggleRunner = async () => {
    try {
      setRunnerInstalled(
        runnerInstalled ? await api.runner.uninstall() : await api.runner.install()
      );
    } catch {
      load();
    }
  };

  if (!status) return null;

  const {
    settings,
    running,
    runs_today,
    total_tokens_today,
    tripwire_tokens_today,
    paused,
    window_reset,
    window_started_at,
  } = status;
  const runCapped = settings.daily_run_cap > 0 && runs_today >= settings.daily_run_cap;
  const tokenCapped =
    settings.daily_token_cap > 0 && total_tokens_today >= settings.daily_token_cap;
  const capped = runCapped || tokenCapped;
  const tokenPct =
    settings.daily_token_cap > 0
      ? Math.min(100, (total_tokens_today / settings.daily_token_cap) * 100)
      : 0;

  // After a manual reset these counters are NOT today's spend — the day may
  // have burned its whole budget before the reset. Saying "today" anyway would
  // make the guard misreport its own scope, which is the one thing a spend
  // guard can't do; so the word changes with the window.
  const since = window_reset ? "since reset" : "today";
  const resetAt = window_reset ? parseDbTimestamp(window_started_at) : null;
  // Nothing to forgive, nothing to offer. Runs count too: the run cap can trip
  // on cheap runs with the token meter barely moved.
  const spentSomething = total_tokens_today > 0 || runs_today > 0;

  const togglePause = async () => {
    try {
      await api.budget.setPaused(!paused);
    } finally {
      load();
    }
  };

  const resetWindow = async () => {
    try {
      setStatus(await api.budget.reset());
    } catch {
      load();
    }
  };

  // T-numbers only — the panel is ~200px wide and a title would truncate to
  // nothing. A task with no short id contributes none, which is what the
  // length check below compares against.
  const liveLabels = live
    .map((r) => (r.task_short_id !== null ? formatShortId(r.task_short_id) : null))
    .filter((v): v is string => v !== null);

  // One word for the whole subsystem, most-urgent-first: a paused or capped
  // runner is why nothing is happening, and that outranks the run count.
  const state: { label: string; tone: string; dot?: string; title?: string } = capped
    ? { label: "Capped", tone: "text-warning", dot: "bg-warning" }
    : paused
      ? { label: "Paused", tone: "text-warning", dot: "bg-warning" }
      : running > 0
        ? {
            // NO WORD, because the dot already is one. The row holds
            // "BACKGROUND" + this + a chevron + the gear inside a 208px
            // sidebar, and the sum overflows by a hair — which is why the
            // label read "BACKGR…" even after this was cut to a bare count.
            // A pulsing accent dot beside a number in a panel called
            // Background does not also need to be told it is running, and
            // dropping the word is ~48px, far more than tightening any gap
            // could find. The `title` says it in full for anyone who wants it.
            //
            // One run names itself; past one the count goes here and the names
            // go to the strip below, where there is room for a Stop each.
            //
            // `running` (from the budget query) stays the source of the COUNT.
            // `live` is a separate read and the two can be a beat apart, so the
            // name only shows when they agree — otherwise the pill would name
            // one task while two were going.
            label:
              running === 1 && liveLabels.length === 1
                ? liveLabels[0]
                : String(running),
            title: `${running} background run${running === 1 ? "" : "s"} in progress`,
            tone: "text-content-secondary",
            dot: "bg-accent",
          }
        : runnerInstalled === false
          ? { label: "Off", tone: "text-content-faint" }
          : { label: "Idle", tone: "text-content-faint" };

  const Chevron = open ? ChevronDown : ChevronRight;

  return (
    <div className="flex shrink-0 flex-col border-t border-border-subtle pb-2 pt-1.5 text-[11px]">
      {/* The gear is a SIBLING of the disclosure, not a child of it. Nesting it
          inside would be invalid markup and, more practically, every click on
          the gear would also toggle the panel open behind the dialog. */}
      <div className="mx-1 flex items-center gap-1">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          aria-controls="background-detail"
          className="flex min-w-0 flex-1 items-center justify-between gap-1.5 rounded px-1 py-1 text-left hover:bg-content/8"
        >
          {/* `tracking-wide`, not `tracking-wider`, unlike the PROJECTS and
              TAGS headers above: this is the one section header sharing its row
              with a status pill, a chevron and the gear, and the extra letter
              spacing was ~4px of the handful that pushed it into an ellipsis. */}
          <span className="min-w-0 truncate font-semibold uppercase tracking-wide text-content-faint">
            Background
          </span>
          {/* The state is one phrase — `2 running` wrapping onto a second line
              makes the footer grow a row and the strip jump as runs start and
              stop. It never wraps and never shrinks; the label truncates
              instead, since "BACKGROU…" still reads as the section it names. */}
          <span className="flex shrink-0 items-center gap-1 whitespace-nowrap">
            {state.dot && (
              <span
                className={cn(
                  "size-1.5 rounded-full",
                  state.dot,
                  running > 0 && !capped && !paused && "animate-pulse"
                )}
              />
            )}
            <span className={cn("tabular-nums", state.tone)} title={state.title}>
              {state.label}
            </span>
            <Chevron size={11} className="text-content-faint" />
          </span>
        </button>

        {/* App settings, parked in the sidebar footer rather than the header.
            It belongs with the other things you set and forget, and the header
            was collecting an icon per preference. It is not *about* background
            spend — it only shares the strip. */}
        <button
          type="button"
          aria-label="Appearance settings"
          title="Appearance"
          onClick={() => setSettingsOpen(true)}
          className="shrink-0 rounded p-1 text-content-faint transition-colors duration-75 hover:bg-content/8 hover:text-content"
        >
          <Settings size={12} />
        </button>
      </div>

      {/* Outside the disclosure on purpose: the bar is the at-a-glance form of
          the only number that actually gates runs, so collapsing must not hide
          it. Absent when there's no cap or nothing spent — an empty track is
          just another permanent zero. */}
      {tokenPct > 0 && (
        <div className="mx-2.5 mt-1 h-[3px] overflow-hidden rounded-full bg-content/10">
          <div
            className={cn(
              "h-full rounded-full transition-all",
              tokenPct > 85 ? "bg-warning" : "bg-accent/70"
            )}
            style={{ width: `${tokenPct}%` }}
          />
        </div>
      )}

      {open && (
        <div id="background-detail" className="mt-1.5 flex flex-col gap-0.5 px-2.5">
          {/* What is actually running, before the counters that describe it.
              The pill above can only carry a number, so identity lives here —
              with a Stop on each, since reaching one from the sidebar beats
              hunting down the task first.

              HORIZONTAL, and that is the point rather than a style. Stacking
              these vertically makes the footer grow a row per run, so the whole
              sidebar column shifts as runs start and finish — the same jump the
              state pill is `shrink-0`/`whitespace-nowrap` to avoid. A strip is
              fixed-height whether one thing is running or six, and the
              concurrency cap is low single digits, so scrolling is the rare
              case rather than the normal one.
              `overscroll-contain` keeps a flick at the end of the strip from
              scrolling the sidebar behind it. */}
          {live.length > 0 && (
            <div className="-mx-0.5 mb-1.5 flex gap-1 overflow-x-auto overscroll-contain border-b border-border-subtle px-0.5 pb-1.5">
              {live.map((r) => (
                <span
                  key={r.id}
                  className="flex shrink-0 items-center gap-1 rounded border border-border-subtle bg-content/5 py-0.5 pl-1.5 pr-0.5"
                >
                  <button
                    type="button"
                    onClick={() => void select(r.task_id)}
                    // The title carries what the chip cannot — the strip is
                    // ~180px and a truncated title is worse than none.
                    title={r.task_title}
                    className="shrink-0 font-mono text-[10px] tabular-nums text-content-secondary hover:text-accent"
                  >
                    {r.task_short_id !== null
                      ? formatShortId(r.task_short_id)
                      : "run"}
                  </button>
                  <CancelRunButton runId={r.id} className="px-1" />
                </span>
              ))}
            </div>
          )}
          <Row
            label={`Runs ${since}`}
            value={
              settings.daily_run_cap > 0
                ? `${runs_today} / ${settings.daily_run_cap}`
                : String(runs_today)
            }
            warn={runCapped}
          />
          <Row
            label={`Tokens ${since}`}
            value={
              settings.daily_token_cap > 0
                ? `${formatTokens(total_tokens_today)} / ${formatTokenCap(settings.daily_token_cap)}`
                : formatTokens(total_tokens_today)
            }
            warn={tokenCapped}
          />
          {/* The agent-tripwire slice of the token line above — a BREAKDOWN,
              since those runs are already in the total. Shown only once a
              check has spent something, or once a sub-cap is set: a permanent
              `0 / 5.0M` on a board with no agent tripwires is the noise this
              panel's collapse exists to avoid. */}
          {(tripwire_tokens_today > 0 || settings.tripwire_daily_token_cap > 0) && (
            <Row
              label={`  of which checks`}
              value={
                settings.tripwire_daily_token_cap > 0
                  ? `${formatTokens(tripwire_tokens_today)} / ${formatTokenCap(settings.tripwire_daily_token_cap)}`
                  : formatTokens(tripwire_tokens_today)
              }
              warn={
                settings.tripwire_daily_token_cap > 0 &&
                tripwire_tokens_today >= settings.tripwire_daily_token_cap
              }
            />
          )}
          <Row label="Concurrent" value={`${running} / ${settings.max_concurrent}`} />

          {/* All three editable, because the first daily cap shipped here was
              an order of magnitude too small and there was no way to find that
              out except by being stopped mid-day, then no way to fix it except
              a rebuild. A guard whose threshold can only be changed by its
              author is one that gets switched off instead of tuned — and the
              turn cap, which had no control at all, went on to cost two whole
              runs by being 30 when an honest run needs more. */}
          <CapField
            label="Daily token cap"
            stored={settings.daily_token_cap}
            parse={parseTokens}
            format={formatTokenCap}
            onCommit={(daily_token_cap) => ({ ...settings, daily_token_cap })}
            onSaved={setStatus}
            warn={tokenCapped}
          />
          <CapField
            label="Per-run token cap"
            stored={settings.run_token_cap}
            parse={parseTokens}
            format={formatTokenCap}
            onCommit={(run_token_cap) => ({ ...settings, run_token_cap })}
            onSaved={setStatus}
          />
          <CapField
            label="Turn cap"
            stored={settings.max_turns}
            parse={parseCount}
            format={String}
            onCommit={(max_turns) => ({ ...settings, max_turns })}
            onSaved={setStatus}
          />
          {/* A sub-cap on `agent:` tripwire checks. It bounds how much of the
              day a set of scheduled checks may quietly eat before the one
              real task gets a slot; each check is admitted only if its whole
              ceiling still fits under this. */}
          <CapField
            label="Tripwire check cap"
            stored={settings.tripwire_daily_token_cap}
            parse={parseTokens}
            format={formatTokenCap}
            onCommit={(tripwire_daily_token_cap) => ({ ...settings, tripwire_daily_token_cap })}
            onSaved={setStatus}
          />

          {/* The picker IS the model readout — showing a `Model: Sonnet 5` row
              above it stated the same fact twice. Borderless so it reads as a
              value in the list rather than a form field bolted to the sidebar;
              the trigger's own hover border supplies the affordance.

              Labelled "Claude default", because it IS one: it has no effect on
              a Codex run, which uses the Codex CLI's own default unless the
              task names a model. An unqualified "Model" here would read as the
              setting for every background run and quietly be wrong for half of
              them. The counters above it, by contrast, genuinely do span both
              agents — one budget, deliberately. */}
          <div className="flex items-center justify-between gap-2">
            <span className="text-content-muted">Claude default model</span>
            <Select
              value={settings.model}
              aria-label="Claude default background model"
              onValueChange={(v) => {
                void api.budget.save({ ...settings, model: v }).then(setStatus);
              }}
              options={BG_MODELS.map((m) => ({ value: m.value, label: m.label }))}
              className="w-auto -mr-1 border-transparent bg-transparent py-0 pl-1 pr-0.5 text-[11px] text-content-secondary"
            />
          </div>
          <p className="pb-0.5 text-[10px] leading-snug text-content-faint">
            Codex uses its own CLI default unless a task sets a model. The token
            caps and the pause switch cover both agents; the turn cap is
            Claude&apos;s only, and is there to catch a loop rather than to bound
            cost.
          </p>

          <div className="mt-1 flex flex-col gap-0.5">
            {/* The escape hatch the caps are bad at: one legitimate run can
                cost most of the day's budget, and the alternatives were waiting
                for midnight or raising the cap — and raising the cap to get
                past a spend you've decided is fine permanently weakens the
                guard for every day after it. */}
            {spentSomething && (
              <button
                type="button"
                onClick={() => void resetWindow()}
                title={
                  resetAt
                    ? `Counting since ${resetAt.toLocaleTimeString(undefined, {
                        hour: "numeric",
                        minute: "2-digit",
                      })}. Reset again to start from now.`
                    : "Start the daily counters from now. Runs keep their recorded cost."
                }
                className="-mx-1 flex items-center gap-1.5 rounded px-1 py-1 text-left text-content-faint hover:bg-content/8 hover:text-content"
              >
                <RotateCcw size={10} />
                Reset usage counter
              </button>
            )}

            <button
              type="button"
              onClick={() => void togglePause()}
              title={paused ? "Resume background runs" : "Pause all background runs"}
              className={cn(
                "-mx-1 flex items-center gap-1.5 rounded px-1 py-1 text-left",
                paused
                  ? "text-warning hover:bg-warning/10"
                  : "text-content-faint hover:bg-content/8 hover:text-content"
              )}
            >
              {paused ? <Play size={10} /> : <Pause size={10} />}
              {paused ? "Resume runs" : "Pause runs"}
            </button>

            {/* The runner is a LaunchAgent that drains queued tasks unattended,
                so it stays OFF until explicitly switched on — it spends the
                Claude subscription rate limit, and that should be a deliberate
                choice. Two clicks from collapsed is the right amount of
                friction for it. */}
            {runnerInstalled !== null && (
              <button
                type="button"
                onClick={() => void toggleRunner()}
                className={cn(
                  "-mx-1 rounded px-1 py-1 text-left",
                  runnerInstalled
                    ? "text-content-faint hover:bg-content/8 hover:text-content"
                    : "text-accent hover:bg-accent/10"
                )}
              >
                {runnerInstalled ? "Disable the runner" : "Enable the background runner…"}
              </button>
            )}
          </div>
        </div>
      )}

      {/* Also outside the disclosure: being capped is the one state that should
          interrupt rather than wait to be found — and it carries its own way
          out, because this is exactly where you are looking when you need one.
          Sending you into a collapsed panel to find the same button is how a
          blocked state reads as a dead end. */}
      {capped && (
        <p className="mt-1 px-2.5 text-warning">
          {tokenCapped ? "Token budget spent" : "Daily run cap reached"} — no new runs{" "}
          {since === "today" ? "today" : "in this window"}.{" "}
          <button
            type="button"
            onClick={() => void resetWindow()}
            className="underline underline-offset-2 hover:text-warning/80"
          >
            Reset the counter
          </button>
        </p>
      )}
    </div>
  );
}

/**
 * A whole positive count, or null. The turn cap's reader.
 *
 * Deliberately not `parseInt`, which reads `"12abc"` as 12 — and rejecting
 * rather than coercing matches `parseTokens`: an unreadable value reverts, it
 * never silently becomes a number nobody typed.
 */
function parseCount(raw: string): number | null {
  const trimmed = raw.trim().replace(/,/g, "");
  return /^\d+$/.test(trimmed) && Number(trimmed) > 0 ? Number(trimmed) : null;
}

/**
 * One editable budget number, in place.
 *
 * Commit-on-blur with a draft, like the launch console's workspace field — and
 * with the same hazard, because these rows live inside a disclosure: collapsing
 * the panel **unmounts a focused input**, and an unmounted input does not
 * reliably fire blur (gotcha #24). So the commit also runs from the unmount
 * cleanup, through a ref that always holds the latest draft.
 *
 * **Generalized over label/parse/format rather than copy-pasted per field**,
 * because that unmount fix is the whole reason this component exists and a
 * second copy is the one that would be written without it.
 *
 * `parse` returning null means "unreadable" and reverts. For the token fields
 * that is load-bearing: `parseTokens` returns null instead of 0 precisely
 * because 0 is the stored value meaning *no cap*, and turning a typo into an
 * uncapped run is the one outcome worth guarding against.
 */
function CapField({
  label,
  stored,
  parse,
  format,
  onCommit,
  onSaved,
  warn,
}: {
  label: string;
  stored: number;
  parse: (raw: string) => number | null;
  format: (n: number) => string;
  /** The settings this value belongs to, with the new value written in. */
  onCommit: (value: number) => BudgetSettings;
  onSaved: (s: BudgetStatus) => void;
  warn?: boolean;
}) {
  const [draft, setDraft] = useState(() => format(stored));

  // Adopt an externally-changed value (another window, a migration) — but never
  // while it is being typed into, which would rewrite the draft mid-edit.
  const [focused, setFocused] = useState(false);
  useEffect(() => {
    if (!focused) setDraft(format(stored));
  }, [stored, focused, format]);

  const commit = () => {
    const parsed = parse(draft);
    if (parsed === null || parsed === stored) {
      setDraft(format(stored));
      return;
    }
    void api.budget
      .save(onCommit(parsed))
      .then(onSaved)
      .catch(() => setDraft(format(stored)));
  };

  // Latest closure, read only by the unmount cleanup below.
  const commitRef = useRef(commit);
  commitRef.current = commit;
  useEffect(() => () => commitRef.current(), []);

  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-content-muted">{label}</span>
      <input
        value={draft}
        aria-label={label}
        spellCheck={false}
        onChange={(e) => setDraft(e.target.value)}
        onFocus={() => setFocused(true)}
        onBlur={() => {
          setFocused(false);
          commit();
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            setDraft(format(stored));
            e.currentTarget.blur();
          }
        }}
        className={cn(
          "w-14 rounded border border-transparent bg-transparent px-1 py-0 text-right text-[11px] tabular-nums",
          "hover:border-border-subtle focus:border-accent/60 focus:outline-none",
          warn ? "text-warning" : "text-content-secondary"
        )}
      />
    </div>
  );
}

function Row({
  label,
  value,
  warn,
}: {
  label: string;
  value: string;
  warn?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-content-muted">{label}</span>
      <span className={cn("tabular-nums", warn ? "text-warning" : "text-content-secondary")}>
        {value}
      </span>
    </div>
  );
}
