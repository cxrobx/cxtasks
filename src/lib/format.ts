import type {
  BackgroundExecutor,
  BackgroundIntent,
  ProjectHealth,
  Task,
  TaskRun,
  TaskStatus,
  TripwireRuntime,
} from "@/types/task";

/** `42` → `"T42"`. `null` → `null`, so callers can't print `Tnull`. */
export function formatShortId(shortId: number | null): string | null {
  return shortId === null ? null : `T${shortId}`;
}

/**
 * Parse one of the DB's **bookkeeping** timestamps — `"2026-07-29 14:03:22.481"`,
 * UTC, space-separated, no `Z`.
 *
 * `new Date("2026-07-29 14:03:22")` parses that as **local** time, which silently
 * shifts every age calculation by the timezone offset: a comment posted a minute
 * ago reads as "7h" in the US, or negative in Asia. The two formats that coexist
 * in this DB are documented in CLAUDE.md § Conventions — instants the user set
 * (`due_at`, `defer_until`) already carry a `Z` and go through `new Date`
 * directly; these do not.
 *
 * Returns `null` for anything unparseable rather than an Invalid Date, so the
 * caller has to decide what to render.
 */
export function parseDbTimestamp(raw: string | null | undefined): Date | null {
  if (!raw) return null;
  const trimmed = raw.trim();
  // Already ISO-with-Z (or with an offset)? Leave it alone.
  const iso = /[Zz]$|[+-]\d{2}:?\d{2}$/.test(trimmed)
    ? trimmed
    : `${trimmed.replace(" ", "T")}Z`;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

/** Compact elapsed time: "now", "12m", "3h", "2d", "5w". */
export function formatAge(at: Date | string | null, now = new Date()): string | null {
  const d = typeof at === "string" ? parseDbTimestamp(at) : at;
  if (!d) return null;
  const mins = Math.floor((now.getTime() - d.getTime()) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  return `${Math.floor(days / 7)}w`;
}

/*
 * `STALE_UPDATE_DAYS` / `isUpdateStale` used to live here, flagging a project
 * whose last WRITTEN update had aged out — and counting "never written" as the
 * loudest case. Both are deliberately gone: the board derives its state from
 * activity now (`BoardRow.activity`), so an unwritten project is described
 * rather than accused, and the one number that could go stale no longer exists.
 * Don't reintroduce a staleness test over `last_update` — that measures a
 * reporting habit, not the project. Idle days measure the project.
 */

/** Health dot / label colour. A project with no reported health is gray. */
export const HEALTH_COLORS: Record<ProjectHealth, string> = {
  on_track: "text-success",
  at_risk: "text-warning",
  off_track: "text-error",
  paused: "text-content-faint",
};

/** `null` health = never reported, which is deliberately NOT "on track". */
export function healthColor(health: ProjectHealth | null): string {
  return health ? HEALTH_COLORS[health] : "text-content-faint";
}

/** Relative-ish due label: "Overdue 2h", "5:00 PM", "Tomorrow", "Aug 3". */
export function formatDue(iso: string | null, now = new Date()): string | null {
  if (!iso) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;

  const startOfDay = (x: Date) =>
    new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((startOfDay(d) - startOfDay(now)) / 86_400_000);

  if (d.getTime() < now.getTime()) {
    const mins = Math.round((now.getTime() - d.getTime()) / 60_000);
    if (mins < 60) return `Overdue ${mins}m`;
    if (mins < 60 * 24) return `Overdue ${Math.round(mins / 60)}h`;
    return `Overdue ${Math.round(mins / (60 * 24))}d`;
  }
  const time = d.toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
  if (days === 0) return time;
  if (days === 1) return `Tomorrow ${time}`;
  if (days < 7)
    return d.toLocaleDateString(undefined, { weekday: "short" }) + ` ${time}`;
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

const WEEKDAY_NAMES: Record<string, string> = {
  MO: "Mon",
  TU: "Tue",
  WE: "Wed",
  TH: "Thu",
  FR: "Fri",
  SA: "Sat",
  SU: "Sun",
};

function ordinal(n: number): string {
  const tens = n % 100;
  if (tens >= 11 && tens <= 13) return `${n}th`;
  return `${n}${["th", "st", "nd", "rd"][n % 10] ?? "th"}`;
}

/**
 * RRULE-lite (`recurrence.rs`) in words: `weekly:MO,TH` → "weekly on Mon, Thu",
 * `quarterly:1` → "quarterly on the 1st". A rule it doesn't recognise comes
 * back VERBATIM — the detail pane's chip is the only place the rule is shown at
 * rest, and a chip that blanked a rule it couldn't read would hide exactly the
 * one that needs fixing.
 */
export function describeRecurrence(rule: string | null): string | null {
  if (!rule?.trim()) return null;
  const [freq, arg = ""] = rule.trim().split(":");
  const day = Number(arg);
  switch (freq) {
    case "daily":
      return arg ? rule : "daily";
    case "weekly": {
      const days = arg.split(",").filter(Boolean);
      if (!days.length || days.some((d) => !WEEKDAY_NAMES[d])) return rule;
      return `weekly on ${days.map((d) => WEEKDAY_NAMES[d]).join(", ")}`;
    }
    case "monthly":
    case "quarterly":
      return Number.isInteger(day) && day >= 1 && day <= 31
        ? `${freq} on the ${ordinal(day)}`
        : rule;
    default:
      return rule;
  }
}

export function isOverdue(task: Task, now = new Date()): boolean {
  if (!task.due_at || isClosed(task.status)) return false;
  const d = new Date(task.due_at);
  return !Number.isNaN(d.getTime()) && d.getTime() < now.getTime();
}

/**
 * Still deferred — the start date hasn't passed, so the task is hidden from
 * the actionable views (Open, Inbox, Today, next_task). A defer date in the
 * past is spent and means nothing; deferral is derived from the clock, never
 * from `status` (there is no "planned" status by design).
 */
export function isDeferred(task: Task, now = new Date()): boolean {
  if (!task.defer_until || isClosed(task.status)) return false;
  const d = new Date(task.defer_until);
  return !Number.isNaN(d.getTime()) && d.getTime() > now.getTime();
}

export function isClosed(status: TaskStatus): boolean {
  return status === "done";
}

/**
 * The tag that marks a task as a DECISION — a question only Chris can answer,
 * not work an agent can do. Mirrors `db::tasks::DECISION_TAG`. A convention on
 * tags rather than a column (invariant 29): Today shows it on a Decide strip,
 * Inbox hides it, `next_task` and the background queue refuse it.
 */
export const DECISION_TAG = "decision";

export function isDecision(tags: readonly string[] | undefined): boolean {
  return Boolean(tags?.some((t) => t.toLowerCase() === DECISION_TAG));
}

/**
 * A PERSON is a tag spelled `@name`. Mirrors `db::tags::is_person`. Same
 * table as every other tag — the `@` is the whole convention — which is why
 * it renders as `@sam` and not `#@sam`: the sigil IS the label.
 */
export function isPersonTag(name: string): boolean {
  return name.startsWith("@") && name.length > 1;
}

/** How a tag reads on a row or in a chip: `#billable`, but `@sam`. */
export function tagLabel(name: string): string {
  return isPersonTag(name) ? name : `#${name}`;
}

/**
 * Marks a filing an agent made on its own (invariant 28). Mirrors
 * `mcp::server::AGENT_FILED_TAG`.
 */
export const AGENT_FILED_TAG = "agent-filed";

/**
 * The tags code attaches for their BEHAVIOUR, not as labels. Mirrors
 * `db::tags::SYSTEM_TAGS`. They render as marks rather than `#words`, and sort
 * after the labels in the sidebar, because "this is a decision" and "an agent
 * filed this" are facts about the task, not kinds of work. Reading them as one
 * list with `#billable` is what made the tag list look like noise.
 */
export const SYSTEM_TAGS: readonly string[] = [DECISION_TAG, AGENT_FILED_TAG];

export type TagKind = "label" | "person" | "system";

export function tagKind(name: string): TagKind {
  if (isPersonTag(name)) return "person";
  return SYSTEM_TAGS.includes(name.toLowerCase()) ? "system" : "label";
}

const KIND_ORDER: Record<TagKind, number> = { label: 0, person: 1, system: 2 };

/**
 * Labels, then people, then system tags. Stable, so each group keeps the order
 * it arrived in (the server sends most-open first).
 */
export function orderTagsByKind<T extends { name: string }>(tags: readonly T[]): T[] {
  return tags
    .map((t, i) => ({ t, i }))
    .sort((a, b) => KIND_ORDER[tagKind(a.t.name)] - KIND_ORDER[tagKind(b.t.name)] || a.i - b.i)
    .map(({ t }) => t);
}

/**
 * Waiting on a CONDITION — the tripwire is set and still armed, so the task is
 * hidden from the actionable views exactly as a deferred one is. `tripped` and
 * `error` both surface it: a broken check must never keep work out of sight
 * (invariant 26). Derived from the row, never from `status`.
 */
export function isTripwireArmed(
  task: Pick<Task, "tripwire" | "tripwire_state" | "status">,
): boolean {
  return (
    task.tripwire !== null &&
    task.tripwire_state === "armed" &&
    !isClosed(task.status)
  );
}

/**
 * A STANDING tripwire (`tripwire_on_trip = "monitor"`) rather than a one-time
 * gate: completing the task means "handled" and re-arms it, and the runner
 * keeps checking it while it is done. Only archiving it stops the check.
 */
export function isMonitor(
  task: Pick<Task, "tripwire" | "tripwire_on_trip">,
): boolean {
  return task.tripwire !== null && task.tripwire_on_trip === "monitor";
}

/**
 * The runner is still checking it: armed and open, or armed and a monitor —
 * which is checked while done too. Not a HOLD (that's `isTripwireArmed`): a
 * done monitor is watched but hides nothing, because Done shows it anyway.
 */
export function isTripwireWatching(
  task: Pick<Task, "tripwire" | "tripwire_state" | "tripwire_on_trip" | "status">,
): boolean {
  return (
    task.tripwire !== null &&
    task.tripwire_state === "armed" &&
    (!isClosed(task.status) || isMonitor(task))
  );
}

/** A check's last outcome in words. A monitor that found nothing wrong is
 *  "all clear"; "not yet" is a gate's word, waiting for something good. */
export function tripwireOutcomeLabel(
  task: Pick<Task, "tripwire" | "tripwire_on_trip">,
  outcome: string | null,
): string {
  if (outcome === "not_yet") return isMonitor(task) ? "all clear" : "not yet";
  return outcome ?? "?";
}

/** `"script:pocketbuddy-first-user prod"` → `{ name, arg }`. `null` for no
 *  spec or one that is not a script (see `parseAgentTripwire`). */
export function parseTripwireSpec(
  spec: string | null,
): { name: string; arg: string | null } | null {
  if (!spec) return null;
  const rest = spec.trim().replace(/^script:/, "");
  if (rest === spec.trim()) return null;
  const [name, ...tail] = rest.split(/\s+/);
  if (!name) return null;
  const arg = tail.join(" ").trim();
  return { name, arg: arg || null };
}

/** `"agent:the README mentions 2.0"` → the condition. `null` for anything
 *  else. Mirrors `tripwire::Tripwire::parse`'s `agent:` arm. */
export function parseAgentTripwire(spec: string | null): string | null {
  if (!spec) return null;
  const trimmed = spec.trim();
  if (!trimmed.startsWith("agent:")) return null;
  const condition = trimmed.slice("agent:".length).trim();
  return condition || null;
}

/** `"jev:the customer replied"` → the condition. */
export function parseJevTripwire(spec: string | null): string | null {
  if (!spec) return null;
  const trimmed = spec.trim();
  if (!trimmed.startsWith("jev:")) return null;
  const condition = trimmed.slice("jev:".length).trim();
  return condition || null;
}

/** `360` → `"every 6h"`, `null` → `"manual"`. Mirrors `tripwire::format_every`. */
export function formatCadence(minutes: number | null): string {
  if (minutes === null) return "manual";
  if (minutes % 10080 === 0) return `every ${minutes / 10080}w`;
  if (minutes % 1440 === 0) return `every ${minutes / 1440}d`;
  if (minutes % 60 === 0) return `every ${minutes / 60}h`;
  return `every ${minutes}m`;
}

/**
 * The "is the runner running?" tell: a CADENCED tripwire that has gone more
 * than three intervals without any attempt. Measured from the last terminal
 * check, else from the runtime row's last touch (a claim, a reset), else from
 * the task itself — so a freshly armed tripwire is given its full grace.
 * Never fires for a manual tripwire, which nothing is expected to check.
 */
export function tripwireUnchecked(
  task: Task,
  runtime: TripwireRuntime | undefined,
  now = new Date(),
): boolean {
  if (!isTripwireWatching(task) || task.tripwire_every_minutes === null) return false;
  const anchor = parseDbTimestamp(
    runtime?.checked_at ?? runtime?.updated_at ?? task.updated_at,
  );
  if (!anchor) return false;
  return now.getTime() - anchor.getTime() > task.tripwire_every_minutes * 60_000 * 3;
}

/** One line for a tooltip or a status row: what the tripwire is, when it was
 *  last checked, and what came of it. */
export function describeTripwire(
  task: Task,
  runtime: TripwireRuntime | undefined,
  now = new Date(),
): string {
  const parts: string[] = [`Tripwire ${task.tripwire ?? ""} · ${formatCadence(task.tripwire_every_minutes)}`];
  if (isMonitor(task)) parts.push("monitor: completing it re-arms it");
  if (runtime?.checked_at) {
    const outcome = tripwireOutcomeLabel(task, runtime.last_outcome);
    const note = runtime.last_note?.trim() ? ` — ${runtime.last_note.trim()}` : "";
    parts.push(`last checked ${formatAge(runtime.checked_at, now) ?? "?"} ago: ${outcome}${note}`);
  } else {
    parts.push("never checked");
  }
  if (runtime?.requested_at) parts.push("check requested");
  if (tripwireUnchecked(task, runtime, now)) parts.push("unchecked — is the runner running?");
  return parts.join(" · ");
}

export const STATUS_LABELS: Record<TaskStatus, string> = {
  inbox: "Inbox",
  todo: "To do",
  queued: "Queued",
  in_progress: "In progress",
  in_review: "In review",
  done: "Done",
};

/**
 * One line each, shown under the status field. These are the whole defence
 * against the drift that produced the pre-v12 set — a status nobody can define
 * in a sentence gets used for whatever is nearby.
 */
export const STATUS_HINTS: Record<TaskStatus, string> = {
  inbox: "Captured, not yet triaged.",
  todo: "Triaged and actionable now.",
  queued: "Handed to the background runner.",
  in_progress: "Being worked — by you or an agent.",
  in_review: "Produced, waiting on you to accept it.",
  done: "Finished. To drop a task instead, archive it.",
};

/** Tailwind classes for the status pill. */
export function statusClass(status: TaskStatus): string {
  switch (status) {
    case "in_progress":
      return "bg-accent/15 text-accent";
    case "queued":
      return "bg-warning/15 text-warning";
    case "in_review":
      return "bg-success/15 text-success";
    case "done":
      return "bg-content/10 text-content-faint";
    default:
      return "bg-content/8 text-content-muted";
  }
}

/**
 * Indexed by priority, so index 0 is the MOST urgent — red. Getting this array
 * backwards is the easy mistake now that P0 is highest.
 */
export const PRIORITY_COLORS = [
  "text-error",         // P0
  "text-warning",       // P1
  "text-content-muted", // P2
  "text-content-faint", // P3
] as const;

/**
 * Background form of the same scale, for the row's left edge bar. Indexed by
 * priority like `PRIORITY_COLORS`, so index 0 is red.
 *
 * **P3 is deliberately transparent.** It's the default a task gets when nobody
 * triaged it, so painting a bar on it would put a mark on nearly every row and
 * leave the scale meaning nothing. A bar therefore means "someone decided this
 * matters more than the default" — and an untouched list stays calm.
 */
export const PRIORITY_EDGE = [
  "bg-error",           // P0
  "bg-warning",         // P1
  "bg-content-muted/50",// P2
  "bg-transparent",     // P3
] as const;

/**
 * What a background run of this task may touch. Mirrors `runner::ToolTier` —
 * the precedence (writes implies shell) lives in both, so keep them in step.
 *
 * Every surface that describes the grant reads this, so the detail pane, the
 * board and Mission Control can't end up disagreeing about what a task is
 * allowed to do. Disagreeing here isn't cosmetic: the label is the only thing
 * telling you whether an unattended agent can change your files.
 */
export type BackgroundTier = "read-only" | "shell" | "write";

export function backgroundTier(task: {
  bg_writes_allowed: boolean;
  bg_shell_allowed: boolean;
}): BackgroundTier {
  if (task.bg_writes_allowed) return "write";
  if (task.bg_shell_allowed) return "shell";
  return "read-only";
}

/**
 * **"shell, no writes" used to be here, and it was not true.** A shell writes:
 * `>` is a write, a test run drops files in `target/`, `git push` publishes.
 * That is precisely why the middle tier is checkpointed and repo-gated exactly
 * like the write tier. Calling it "no writes" described a guarantee the code
 * has never made and deliberately does not make — so the label now names the
 * *intent* ("commands and tests") rather than a boundary.
 */
export const TIER_LABELS: Record<BackgroundTier, string> = {
  "read-only": "investigation",
  shell: "commands and tests",
  write: "edit workspace",
};

/**
 * How each tier is enforced, which is genuinely different per agent: Claude by
 * a tool allow/deny list, Codex by an OS sandbox. The user picks one tier and
 * gets the same *intent* either way, but the sentence describing it has to be
 * accurate about the mechanism — that's what tells you what happens when the
 * agent tries something it shouldn't.
 */
export const TIER_HINTS: Record<
  BackgroundExecutor,
  Record<BackgroundTier, string>
> = {
  claude: {
    "read-only": "Read, Glob, Grep. Cannot run commands.",
    shell: "Adds a shell — git, tests, CLIs. Edit and Write stay denied.",
    write: "Adds Edit and Write.",
  },
  codex: {
    "read-only": "read-only sandbox — reads and read-only commands.",
    shell: "workspace-write sandbox. Told not to edit source, but commands can.",
    write: "workspace-write sandbox, editing expected.",
  },
};

export const INTENT_LABELS: Record<BackgroundIntent, string> = {
  work: "Continue",
  review: "Review",
  check: "Check",
};

/**
 * The consequence of each intent, in one line. Same convention as
 * `TIER_HINTS`: the label names the choice, the hint says what it costs you.
 *
 * "Continue" never needs to justify itself as "do the task" — the control is
 * hidden until a finished run has left a handoff, so by the time it is on
 * screen the work has already started.
 */
export const INTENT_HINTS: Record<BackgroundIntent, string> = {
  work: "Picks up where the last run left off, with its handoff in the prompt.",
  review:
    "Audits the last handoff instead of continuing. Starts read-only on the " +
    "other agent — change either above.",
  check: "A tripwire check the runner started on its own — not something you arm.",
};

/**
 * What a run's status is called on screen.
 *
 * **Shared because two surfaces render this and they did not agree.** Mission
 * Control mapped through a local `RUN_FAILURES`; `TaskDetail` printed
 * `{r.status}` raw, so anything the local map didn't cover showed up there as
 * underscored snake_case — which `budget_stopped` would have done the moment it
 * gained a producer, having sat in `RUN_STATUSES` for months with none. Same
 * argument as `db::tasks::LIVE` a layer down: one constant is what makes the
 * call sites greppable, so a third status cannot be added to one surface and
 * forgotten in the other.
 *
 * Typed against the union rather than `Record<string, string>` on purpose — a
 * new status is then a compile error at every consumer, which is the good
 * failure. Call sites still `?? r.status`, because the three binaries sharing
 * this database do not upgrade in lockstep and a leftover runner can write a
 * status this build has never heard of.
 */
export const RUN_STATUS_LABELS: Record<TaskRun["status"], string> = {
  running: "Running",
  ready_for_review: "Ready for review",
  needs_action: "Needs action",
  unverified: "Needs verification",
  succeeded: "Run finished",
  failed: "Failed",
  cancelled: "Cancelled",
  timeout: "Timed out",
  budget_stopped: "Stopped — budget",
  turns_exhausted: "Stopped — turn limit",
  dispatched: "Opened in Ghostty",
  abandoned: "Lost track",
};

/**
 * The colour that goes with it.
 *
 * The two guard states are **warning, not error**: a run we stopped on purpose
 * is not a run that broke, and painting them red makes the board read as
 * failing when it is behaving exactly as designed. `timeout` stays an error
 * because a hung agent genuinely is one.
 *
 * `dispatched` is deliberately **muted, not green**: opening a session is not
 * an accomplishment, and colouring every "Open in Claude" as a success would
 * make a task look worked the moment it was looked at. `abandoned` is a
 * warning for the same reason the guard states are — we lost the thread, which
 * is not the same as the work having broken.
 */
export const RUN_STATUS_CLASS: Record<TaskRun["status"], string> = {
  running: "text-accent",
  ready_for_review: "text-accent",
  needs_action: "text-warning",
  unverified: "text-warning",
  succeeded: "text-content-secondary",
  failed: "text-error",
  cancelled: "text-error",
  timeout: "text-error",
  budget_stopped: "text-warning",
  turns_exhausted: "text-warning",
  dispatched: "text-content-muted",
  abandoned: "text-warning",
};

/**
 * The run a review would audit: the newest FINISHED run that left a non-empty
 * handoff. Mirrors `dispatch::reviewable_run` on the Rust side, which backs
 * both the queue guard and the prompt's replay — if these two disagree, the UI
 * offers a review the backend refuses, or hides one it would have accepted.
 *
 * Deliberately not filtered to `succeeded`: a timed-out run's partial handoff
 * is genuinely reviewable, and often the one most worth a second opinion.
 *
 * `runs` arrives **newest-first** — `db::runs::list_for_task` is
 * `ORDER BY started_at DESC, rowid DESC`. Do NOT sort or reverse it here: that
 * exact mistake shipped once on the Rust side and reported the three OLDEST
 * runs as the most recent (invariant 18).
 */
export function lastHandoffRun(runs: TaskRun[]): TaskRun | null {
  return (
    runs.find(
      (r) => r.finished_at !== null && (r.result_text ?? "").trim() !== "",
    ) ?? null
  );
}

/**
 * The agent that did NOT produce this run — what arming a review preselects,
 * because a second opinion from the same agent is not much of a second opinion.
 *
 * Correct only while there are exactly TWO executors. A third makes "the other
 * one" a choice rather than a function — the same shape as the reason
 * `enabled_sql()` is generated from `ALL` on the Rust side.
 */
export function otherExecutor(e: BackgroundExecutor): BackgroundExecutor {
  return e === "claude" ? "codex" : "claude";
}

export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return String(n);
}

/**
 * The inverse of [`formatTokens`], for the editable cap: `"50M"` → 50000000.
 *
 * Accepts what [`formatTokens`] emits (`"14.6M"`, `"800k"`) plus the raw digits
 * and a stray comma, because the field is pre-filled with the formatted form
 * and the obvious edit is to retype it in either style.
 *
 * Returns `null` for anything it can't read — **not** 0. Those are opposite
 * instructions here: `0` is the stored value that means *no cap at all*, so
 * falling back to it would turn a typo into an uncapped day.
 */
export function parseTokens(raw: string): number | null {
  const m = raw.trim().toLowerCase().replace(/,/g, "").match(/^(\d+(?:\.\d+)?)([km])?$/);
  if (!m) return null;
  const mult = m[2] === "m" ? 1_000_000 : m[2] === "k" ? 1_000 : 1;
  return Math.round(parseFloat(m[1]) * mult);
}

/** `50000000` → `"50M"`. [`formatTokens`]'s `.0` is noise on a round cap. */
export function formatTokenCap(n: number): string {
  return formatTokens(n).replace(/\.0(?=[kM]$)/, "");
}

/**
 * The model to show for a run or a task.
 *
 * `claude-sonnet-5` → `sonnet-5` (the vendor prefix is noise in a chip), and an
 * absent model is rendered as **"Codex default"** rather than blank or a
 * guessed name — CXTasks genuinely does not know which model the Codex CLI
 * picked, and printing one would be inventing it.
 */
export function modelLabel(
  model: string | null,
  executor: BackgroundExecutor = "claude"
): string | null {
  if (model && model.trim()) return model.replace(/^claude-/, "");
  return executor === "codex" ? "Codex default" : null;
}

/** `/Users/x/Projects/cxventures` → `~/Projects/cxventures` */
export function shortenPath(path: string, home?: string): string {
  if (home) return path.startsWith(home) ? `~${path.slice(home.length)}` : path;
  // Whoever is running it: any macOS home, without hard-coding a user name.
  return path.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
}

/**
 * The last segment: `/Users/x/Projects/cxventures` → `cxventures`.
 *
 * For places too narrow for a path — the launch console's rail gives each node
 * ~78px, where `shortenPath` renders as `~/Project…` and names no repo at all.
 * The directory name is the recognisable part, and the full path is one click
 * away in the workspace editor.
 *
 * Deliberately NOT a project name (invariant 17): this is the folder, shown as
 * the folder, in a slot labelled Workspace.
 */
export function repoName(path: string): string {
  return path.replace(/\/+$/, "").split("/").filter(Boolean).pop() ?? path;
}
