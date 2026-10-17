/** Mirrors `src-tauri/src/db/tasks.rs::Task`. Keep the two in sync by hand. */
export interface Task {
  id: string;
  title: string;
  body: string | null;
  status: TaskStatus;
  priority: number; // 0..3
  project: string | null;
  due_at: string | null; // ISO-8601 UTC, trailing Z
  defer_until: string | null;
  recurrence: string | null; // daily | weekly:MO,TH | monthly:15
  parent_id: string | null;
  sort_order: number;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
  repo_path: string | null;
  prompt: string | null;
  /**
   * Opted into unattended runs **under `bg_executor`**.
   *
   * Computed on the Rust side, not a raw column: the flag is stored per
   * executor so a pre-v8 runner can't claim a Codex task and run it with
   * Claude (see `src-tauri/src/executor.rs`). Nothing on this side needs to
   * know that — set it and read it as one boolean.
   */
  bg_allowed: boolean;
  /** Which agent runs this in the background. Foreground is always Claude. */
  bg_executor: BackgroundExecutor;
  /**
   * What the NEXT background dispatch is for.
   *
   * A one-shot arming, **not durable user state**: `runner::finish_run` resets
   * it to `"work"` after a successful run, and the bridge repatches. So any UI
   * reading it must read it straight off the task — local state would survive
   * the server's reset and leave the button promising a review already spent.
   */
  bg_intent: BackgroundIntent;
  bg_writes_allowed: boolean;
  /**
   * Let a background run use a shell without granting `Edit`/`Write`. Ignored
   * when `bg_writes_allowed` is set, which already implies a shell. Like
   * writes, it makes the run mutation-capable — git repo required, working tree
   * checkpointed first.
   */
  bg_shell_allowed: boolean;
  /** May this task run on a host that is not the machine holding its repo?
   *  Default false, in a column no pre-v23 binary reads — standing up a remote
   *  host makes nothing newly claimable. */
  bg_remote_allowed: boolean;
  /** Comma-separated MCP server NAMES a repo-less run may reach. Empty grants
   *  nothing beyond the write-back router, which is the design: an ungranted
   *  detached run can read nothing. */
  bg_mcp_servers: string | null;
  reminded_at: string | null;
  /** Overrides the global background model. null = use the default. */
  model: string | null;
  /** Grouping within a project or view. null = ungrouped. */
  section_id: string | null;
  /** In the Trash since this instant; null = not trashed. */
  deleted_at: string | null;
  /** Archived at this instant; null = not archived. Never auto-purged. */
  archived_at: string | null;
  /**
   * The speakable reference, rendered `T42`. `null` only for a row written by a
   * pre-v6 binary and not yet healed — every code path that renders it has to
   * cope with its absence rather than printing `Tnull`.
   */
  short_id: number | null;
  /**
   * The hand-set HOLD: what this task is waiting on, outside CXTasks. Non-null
   * means held, independent of `status` (v27) — an in-progress task can be
   * waiting on a key. A held task leaves Open/Today/next_task and sits in
   * Waiting, exactly like a defer date or an armed tripwire.
   *
   * This is the manual (or runner-written) reason only. A task blocked by a
   * *dependency* is derived from `task_deps` at render time via
   * `taskStore.blockedIds`, so the two never contend for one field; the row
   * badge reads both.
   */
  blocked_reason: string | null;
  /**
   * A CONDITION the task waits for, where `defer_until` is a date —
   * `script:<name> [arg]`, `jev:<condition>`, or `agent:<condition>`. While
   * `tripwire_state` is `"armed"` the task is
   * hidden exactly like a deferred one and shows in Waiting with a badge.
   * `null` = no tripwire.
   */
  tripwire: string | null;
  /** Re-check cadence in minutes; `null` = manual (only on "check now"). */
  tripwire_every_minutes: number | null;
  /** `"complete"` = auto-complete on trip (releases `blocked_by`);
   *  `"monitor"` = a standing check, re-armed by completing it; `null` =
   *  surface only, the default. */
  tripwire_on_trip: TripwireOnTrip | null;
  /** Only `"armed"` hides. `tripped` and `error` both put the task back on the
   *  board — a broken check must never keep work out of sight. Written by the
   *  runner; the one value a client may write is `"armed"` (a re-arm). */
  tripwire_state: TripwireState | null;
  tripwire_tripped_at: string | null;
}

/** Mirrors `tripwire::TripwireState`. */
export type TripwireState = "armed" | "tripped" | "error";
/** Mirrors `tripwire::OnTrip`. `"surface"` is stored as `null`. */
export type TripwireOnTrip = "surface" | "complete" | "monitor";

/**
 * The volatile half of a tripwire — mirrors `db::tripwires::Runtime`. A
 * separate row, never fields on `Task`, so an attempt's bookkeeping can't
 * rewrite a task row somebody is editing (invariant 26). Loaded for the whole
 * board by `api.tripwires.runtime()`, the `tagsByTask` shape.
 */
export interface TripwireRuntime {
  task_id: string;
  /** "Check now" pending; cleared by the attempt that serves it. */
  requested_at: string | null;
  lease_id: string | null;
  lease_host: string | null;
  lease_spec: string | null;
  lease_request_at: string | null;
  lease_expires_at: string | null;
  /** The last TERMINAL attempt — bookkeeping format, use `parseDbTimestamp`. */
  checked_at: string | null;
  last_outcome: "tripped" | "not_yet" | "error" | null;
  last_note: string | null;
  /** Consecutive errors; any success resets it. */
  failures: number;
  updated_at: string;
}

/** The cadence presets the detail pane offers. `null` = manual. Mirrors the
 *  grammar `tripwire::parse_every` accepts; the value is stored in minutes. */
export const TRIPWIRE_CADENCES = [
  { value: null, label: "Manual — only when asked" },
  { value: 5, label: "Every 5 minutes" },
  { value: 15, label: "Every 15 minutes" },
  { value: 60, label: "Hourly" },
  { value: 360, label: "Every 6 hours" },
  { value: 1440, label: "Daily" },
  { value: 10080, label: "Weekly" },
] as const satisfies readonly { value: number | null; label: string }[];

/** Days a trashed task survives before it's purged. Mirrors
 * `db::tasks::RETENTION_DAYS` — keep the two in sync by hand. */
export const RETENTION_DAYS = 30;

/**
 * Which agent runs a task in the background. Mirrors
 * `executor::BackgroundExecutor` — the wire values are exactly these strings.
 */
export type BackgroundExecutor = "claude" | "codex";

export const BACKGROUND_EXECUTORS = [
  {
    value: "claude",
    label: "Claude Code",
    hint: "The default. Tool grants, per-task model, turn cap.",
  },
  {
    value: "codex",
    label: "Codex",
    hint: "OS-sandboxed. Needs `codex login`; uses the CLI's own model.",
  },
] as const satisfies readonly {
  value: BackgroundExecutor;
  label: string;
  hint: string;
}[];

export const EXECUTOR_LABELS: Record<BackgroundExecutor, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

/**
 * What the next background dispatch is FOR — do the work, or audit the last
 * run's handoff.
 *
 * Continuation is not on this axis: a second run of an already-worked task
 * reframes itself from the presence of history, with no stored state and no
 * user choice. Only a *review* is chosen.
 */
/**
 * `check` is MACHINE-ONLY: it appears on a run row when the runner evaluated
 * an `agent:` tripwire, never on a task, and never in the picker below —
 * `BACKGROUND_INTENTS` is typed against the two a person can arm.
 */
export type BackgroundIntent = "work" | "review" | "check";
export type ArmableIntent = Exclude<BackgroundIntent, "check">;

export const BACKGROUND_INTENTS = [
  {
    value: "work",
    label: "Continue",
    hint: "Picks up where the last run left off.",
  },
  {
    value: "review",
    label: "Review",
    hint: "Audits the last handoff instead of continuing.",
  },
] as const satisfies readonly {
  value: ArmableIntent;
  label: string;
  hint: string;
}[];

/**
 * **Claude's** background model choices. Sonnet 5 is the default — see
 * BudgetSettings.
 *
 * There is deliberately no Codex equivalent: pinning an OpenAI model name here
 * ships a value that goes stale on someone else's release schedule, and a stale
 * name is a run that dies at spawn for a reason unrelated to the task. A Codex
 * task with no override uses the CLI's own default.
 */
export const BG_MODELS = [
  { value: "claude-sonnet-5", label: "Sonnet 5 (default)" },
  { value: "claude-haiku-5-5", label: "Haiku 5.5 (cheapest)" },
  { value: "claude-opus-5", label: "Opus 5 (most capable, burns fastest)" },
] as const;

/**
 * Every status a task may hold, in lifecycle order — mirroring
 * `db::tasks::STATUSES`.
 *
 * **One axis: how far along is this work.** Before v12 there were eight, and
 * they were not mutually exclusive: `inbox` was about triage, `next` about
 * scheduling, `queued` about the background runner, so a task could be all
 * three at once. `blocked` was worse — it meant two unrelated things *on screen
 * simultaneously*, since the row badge comes from `task_deps` while the status
 * came from the runner refusing a preflight.
 */
export const TASK_STATUSES = [
  "inbox",
  "todo",
  "queued",
  "in_progress",
  "in_review",
  "done",
] as const;

export type TaskStatus = (typeof TASK_STATUSES)[number];

/**
 * The subset a human picks from — `TASK_STATUSES` minus the machine-owned ones.
 *
 * `queued` is set by "Run in Background" and cleared by the runner claiming the
 * task. It stays a real status because it is the runner's atomic claim key, but
 * offering it in a dropdown would let a task enter the drain queue past every
 * check `queue_background` runs first. Keep this in step with
 * `db::tasks::USER_STATUSES`, which is what actually enforces it.
 */
export const USER_STATUSES: TaskStatus[] = [
  "inbox",
  "todo",
  "in_progress",
  "in_review",
  "done",
];

/** One member since v12 — dropping a task archives it, it is not a status. */
export const CLOSED_STATUSES: TaskStatus[] = ["done"];

/**
 * **P0 is the HIGHEST priority, P3 the lowest** — Meta's convention. Inverted
 * from the original schema by the v3 migration, which remapped stored values
 * `new = 3 - old`. Consequences worth remembering: lists sort priority
 * ASCENDING, and a new task defaults to P3, not P0.
 */
export const PRIORITY_HIGHEST = 0;
export const PRIORITY_LOWEST = 3;

export const PRIORITY_OPTIONS = [
  { value: 0, label: "P0 — highest" },
  { value: 1, label: "P1" },
  { value: 2, label: "P2" },
  { value: 3, label: "P3 — lowest" },
] as const;

/**
 * A project's reported health. `null` — never reported — is a distinct and more
 * honest state than `on_track`, and renders as a gray dot.
 *
 * Mirrors `db::project_updates::HEALTHS`. It can only be changed by posting a
 * written update (`api.projects.postUpdate`); there is deliberately no bare
 * setter, because a status with no explanation attached is how a board rots.
 */
export type ProjectHealth = "on_track" | "at_risk" | "off_track" | "paused";

export const HEALTH_OPTIONS = [
  { value: "on_track", label: "On track" },
  { value: "at_risk", label: "At risk" },
  { value: "off_track", label: "Off track" },
  { value: "paused", label: "Paused" },
] as const satisfies readonly { value: ProjectHealth; label: string }[];

/**
 * PARA: is this thing allowed to END?
 *
 * An **area** is a standing responsibility — CXVentures, Artist Advisory, the
 * music. No finish line, so a quiet month is a lull and nothing to fix.
 * A **project** is finite: a certification wave, a launch, a migration. It can
 * carry a target date, it gets closed when it's done, and a quiet month in one
 * is worth saying out loud.
 *
 * Nesting is exactly one level: area → project → section.
 */
export type ProjectKind = "area" | "project";

export const KIND_OPTIONS = [
  { value: "area", label: "Area", hint: "Ongoing — never finishes" },
  { value: "project", label: "Project", hint: "Finite — has an end" },
] as const satisfies readonly { value: ProjectKind; label: string; hint: string }[];

export interface Project {
  id: string;
  name: string;
  /** A hue NAME (`src/lib/hues.ts`), or null for the automatic colour. */
  color: string | null;
  /** The sidebar icon: `lucide:<name>`, a PNG `data:` URI, or null for the
   * kind's own glyph (`src/lib/nodeIcons.tsx`). */
  icon: string | null;
  sort_order: number;
  archived: boolean;
  /**
   * **Rolls up.** For an area this counts its own open tasks PLUS every open
   * task in the live projects nested under it — because the badge is a claim
   * about the list clicking it opens, and that list rolls up too. Both come
   * from `db::projects::expand_scope`; never re-derive the roll-up here.
   *
   * Open work of ANY kind — held rows included. This is what decides whether
   * the sidebar DRAWS the row; the number it prints is `badge_count`.
   */
  open_count: number;
  /**
   * **The number the sidebar prints**: how many rows clicking the node shows
   * under its own chips — saved, inherited from its area, or the baseline —
   * through the same `FilterSpec` the list runs (`count_filtered`). Filled by
   * the store after `list_all_projects`, never by the server, because saved
   * chips live in this window's localStorage. Absent when the count failed;
   * render `open_count` then.
   *
   * Kept apart from `open_count` on purpose: an area whose every task is held
   * reads "0" here yet must stay on screen, or its held rows become
   * unreachable from the sidebar.
   */
  badge_count?: number;
  /** null = never reported. */
  health: ProjectHealth | null;
  kind: ProjectKind;
  /** Name of the owning area; null = top-level. */
  parent: string | null;
  /** ISO-8601 UTC with a trailing `Z`. Always null on an area. */
  target_date: string | null;
  /** Set when the project is finished — a different claim from `archived`. */
  closed_at: string | null;
}

/**
 * One repository owned by a project.
 *
 * A project NAME and a repo DIRECTORY are different things and neither is
 * derivable from the other — `pocketbuddy` lives at `~/Projects/finance-app`.
 * Many repos per project (the web+iOS pairs); at most one project per repo, so
 * a filed task can resolve its project from the directory it came from.
 */
export interface ProjectRepo {
  id: string;
  project: string;
  repo_path: string;
  /** Prefills a task's `repo_path` when its project is set. Exactly one per project. */
  is_primary: boolean;
  sort_order: number;
}

export interface Section {
  id: string;
  name: string;
  /** null = a section that groups a VIEW rather than a project. */
  project: string | null;
  sort_order: number;
  /**
   * A section carrying a target date IS a milestone. Full ISO-8601 UTC with a
   * trailing `Z` — a bare `YYYY-MM-DD` would render a day off once parsed as
   * local time.
   */
  target_date: string | null;
  /** Set by hand, deliberately decoupled from whether its tasks are all done. */
  completed_at: string | null;
}

/** One entry in a project's written update log. */
export interface ProjectUpdate {
  id: string;
  project: string;
  author: string;
  body: string;
  /** The health this update set, or null if it only narrated. */
  health: ProjectHealth | null;
  /**
   * Bookkeeping format: space-separated UTC with milliseconds and **no** `Z`.
   * Never `new Date()` this directly — use `parseDbTimestamp`.
   */
  created_at: string;
}

/** The next dated, uncompleted section in a project. */
export interface BoardMilestone {
  section_id: string;
  name: string;
  target_date: string | null;
  open_count: number;
  total_count: number;
}

/**
 * How alive a project is, derived from the clock by `db::projects`.
 *
 * **Resolved in Rust on purpose** — the thresholds live in exactly one place
 * (`ActivityState::from_idle_days`), so this file styles the state and never
 * re-derives it from `idle_days`. Adding a `days > 3` test here is how the app
 * and the MCP start disagreeing about the same project.
 */
export type ActivityState = "active" | "idle" | "dormant" | "empty";

/**
 * The same `ActivityState` means different things on an area than on a project,
 * so it gets different words. A finite project untouched for a month is
 * stalled; an area untouched for a month is quiet, and there is nothing to fix.
 * The STATE is still resolved in Rust — this only picks the noun for it.
 */
export function activityLabelFor(
  state: ActivityState,
  kind: ProjectKind,
  idleDays: number | null,
): string {
  const days = (word: string) => (idleDays === null ? word : `${word} ${idleDays}d`);
  if (state === "active") return "active";
  if (state === "empty") return "nothing yet";
  if (kind === "area") return days("quiet");
  return state === "idle" ? days("idle") : days("stalled");
}

/** One row of the GSD board. Mirrors `db::projects::BoardRow`. */
export interface BoardRow {
  project: string;
  /** Area or project — the board groups by it, and it changes what quiet means. */
  kind: ProjectKind;
  /** Owning area's name; null for a top-level row. */
  parent: string | null;
  target_date: string | null;
  closed_at: string | null;
  health: ProjectHealth | null;
  last_update: ProjectUpdate | null;
  next_milestone: BoardMilestone | null;
  open_count: number;
  blocked_count: number;
  done_this_week: number;
  /** Title + T-number of what `next_task` would hand you for this project. */
  next_task_title: string | null;
  next_task_short_id: number | null;
  /** Open and workable now — the Open view's count for this project. A future
   * due date does not disqualify a task; only a hold does. */
  actionable_count: number;
  /** Open but HELD by a date or a condition — deferred, or on an armed
   * tripwire. The name predates the retirement of the Upcoming view. */
  planned_count: number;
  /** Finished background runs waiting on a human. The loudest signal here. */
  in_review_count: number;
  created_this_week: number;
  last_activity: string | null;
  idle_days: number | null;
  activity: ActivityState;
  /** Events per local day, oldest first, always 14 entries. */
  activity_strip: number[];
}

export interface Tag {
  id: string;
  name: string;
  color: string | null;
}

export interface TagWithCount extends Tag {
  open_count: number;
  /** Sidebar icon — only on this type; the per-task `Tag` never carries one. */
  icon: string | null;
}

export interface DepEdge {
  task_id: string;
  title: string;
  status: TaskStatus;
  priority: number;
  project: string | null;
  /** The blocker is still open, i.e. actually blocking. */
  is_open: boolean;
}

export interface RelationEdge {
  task_id: string;
  short_id: number | null;
  title: string;
  status: TaskStatus;
  priority: number;
  project: string | null;
  created_at: string;
}

export interface TaskRelations {
  blocked_by: DepEdge[];
  blocking: DepEdge[];
  is_blocked: boolean;
  /** Older tasks this task replaces. */
  supersedes: RelationEdge[];
  /** Newer tasks that replace this one. Non-empty means this task is history. */
  superseded_by: RelationEdge[];
  related: RelationEdge[];
  is_superseded: boolean;
}

export interface RetrospectiveSpec {
  project?: string;
  from?: string;
  to?: string;
  limit?: number;
}

export interface RetrospectiveEvidence {
  task_id: string;
  task_ref: string | null;
  title: string;
  project: string | null;
  closure: "completed" | "superseded";
  closed_at: string;
  source: string;
  text: string;
}

export interface Retrospective {
  tasks_scanned: number;
  source_items_scanned: number;
  learned: RetrospectiveEvidence[];
  recurring: Array<{ term: string; task_count: number; task_refs: string[] }>;
  ruled_out: RetrospectiveEvidence[];
}

export interface JevSettings {
  enabled: boolean;
  model: string;
  capture: boolean;
  readiness: boolean;
  review: boolean;
  recall: boolean;
  /** Who answers capture. Readiness, review, tripwires and reranking stay on Jev. */
  capture_engine: CaptureEngine;
}

export type CaptureEngine = "decisions" | "jev";

export interface JevStatus {
  settings: JevSettings;
  configured: boolean;
  credential_source: string | null;
  /** The OpenAI key the Decisions capture engine reads (env or login Keychain). */
  decisions_configured: boolean;
  decisions_credential_source: string | null;
}

export interface JevSaveSettings extends JevSettings {
  /** Absent keeps the existing key; blank removes the locally stored key. */
  api_key?: string | null;
}

export interface CaptureAssessment {
  project: string | null;
  priority: number;
  status: "inbox" | "todo";
  tags: string[];
  needs_clarification: number;
  confidence: number;
  /** `needs_clarification` against the answering engine's own bar. */
  too_vague: boolean;
  /** Which engine answered; a Decisions failure that fell back says `jev`. */
  engine: CaptureEngine | null;
}

export interface ReadinessAssessment {
  label: "ready" | "needs_context";
  clear_outcome: number;
  enough_context: number;
  unresolved_decision: number;
  bounded: number;
  should_block: boolean;
  note: string;
}

export interface Comment {
  id: string;
  task_id: string;
  author: string;
  body: string;
  created_at: string;
}

/** A managed image copied into the app's data directory. `comment_id = null`
 * places it in Notes; otherwise it renders under that comment. */
export interface Attachment {
  id: string;
  task_id: string;
  comment_id: string | null;
  name: string;
  mime_type: string;
  path: string;
  created_at: string;
  /** The content address of the bytes at `path` — how a synced node knows a
   * cached file is the right one. `null` on a row written before schema v22. */
  sha256: string | null;
}

export interface CommentWithAttachments {
  comment: Comment;
  attachments: Attachment[];
}

export type ViewName =
  /** The GSD board: one row per project, not a task list. NOT a hidden view —
   * it has its own renderer in `AppLayout` rather than different row
   * affordances. */
  | "gsd"
  | "inbox"
  | "today"
  /** Held back by something that has to happen first — a start date, an armed
   * tripwire, an open blocker, or a written blocked reason. Oldest first: the
   * question is "what am I waiting on, and since when". The one "not yet"
   * list since Upcoming was retired. */
  | "waiting"
  /** Actionable right now: open and not held. The sidebar's main list. A
   * future DUE date does not hide a task from it — a deadline says when work
   * is due, not when it may begin; what says "not yet" is a defer. */
  | "open"
  /** Every open task, held rows included. No sidebar row since the Open
   * split; project drill-downs and the GSD board query it. */
  | "all"
  | "in_review"
  | "done"
  | "trash"
  | "archive";

/** Views that show tasks held back from the board. They get their own row
 * affordances (Put Back / Delete Permanently) and no quick-add. */
export const HIDDEN_VIEWS: ViewName[] = ["trash", "archive"];

/**
 * The views the **backend** knows about. `gsd` is deliberately excluded: it is a
 * frontend-only name with its own renderer, and Rust's `db::tasks::View` has no
 * such variant, so sending it makes `list_tasks` reject the whole call with
 * "unknown variant `gsd`".
 *
 * Typing `ListFilters.view` with this rather than `ViewName` is what turns that
 * into a compile error at the call site instead of a runtime one — the previous
 * shape let `view` flow straight through from the store, and because the list is
 * the first `await` in `refresh()`, the throw took the board fetch down with it
 * and rendered "No projects yet" on a board that had projects.
 */
export type ListView = Exclude<ViewName, "gsd">;

export interface ListFilters {
  view?: ListView;
  project?: string | null;
  status?: TaskStatus | null;
  /** Narrow to tasks carrying ALL of these tags. */
  tags?: string[] | null;
  hide_blocked?: boolean;
  include_subtasks?: boolean;
  limit?: number;
}

/** Mirrors `db::search::FilterSpec` — the ONE query the app runs. Facets AND
 * together; values within a facet OR, except `tags`, which narrows. A `view`
 * scopes it (chips REFINE that view — invariant 21); without one the scope is
 * search's: trashed out, archived in. */
export interface FilterSpec {
  query?: string;
  /** The sidebar view. Omit for the search scope. */
  view?: ListView;
  /** The selected area or project, rolled up through `expand_scope`.
   * Deliberately NOT the `projects` facet: folding the two would turn "in area
   * Y" plus a `project is X` chip into "Y or X", widening a narrowing. */
  project?: string | null;
  projects?: string[];
  statuses?: TaskStatus[];
  /** 0–3, P0 highest — invariant 9. */
  priorities?: number[];
  tags?: string[];
  /** Local calendar days, `YYYY-MM-DD`. Both bounds EXCLUDE the named day. */
  due_before?: string;
  due_after?: string;
  created_before?: string;
  created_after?: string;
  /** Held back in ANY of these ways. `"*"` is the whole Waiting predicate. */
  waiting?: WaitingKind[];
  /** Tasks involving ANY of these people (`@name` tags, `@` optional; `"*"` =
   * anyone). ORs, unlike `tags`. */
  people?: string[];
  hide_blocked?: boolean;
  include_subtasks?: boolean;
  sort?: Sort;
  /** The negatives. Every one is NULL-safe server-side — see `filtered`. */
  exclude_statuses?: TaskStatus[];
  exclude_priorities?: number[];
  exclude_projects?: string[];
  exclude_tags?: string[];
  exclude_people?: string[];
  exclude_waiting?: WaitingKind[];
  /** Leave finished work out. Defaults to `view === undefined` server-side, so
   * a view-scoped query states its own rule and the "not Done" chip can come
   * off. The app always sends `false`. */
  exclude_closed?: boolean;
  limit?: number;
}

/** The ways a task can be held back — one name per row badge, so "Waiting is
 * not Tripwire" takes off exactly the rows wearing that badge. */
export type WaitingKind = "deferred" | "tripwire" | "blocked" | "*";

export type SortKey = "default" | "due" | "priority" | "created" | "updated" | "title";
export type SortDir = "asc" | "desc";

/** How the list is ordered. `default` is each view's own order — board order,
 * Waiting oldest-first, Trash/Archive by when things were set aside — and is
 * the only sort under which a manual drag means anything, since `sort_order`
 * is merely a tiebreaker. */
export interface Sort {
  key: SortKey;
  dir: SortDir;
}

export const DEFAULT_SORT: Sort = { key: "default", dir: "asc" };

export const SORT_OPTIONS = [
  { value: "default", label: "Default" },
  { value: "due", label: "Due date" },
  { value: "priority", label: "Priority" },
  { value: "created", label: "Created" },
  { value: "updated", label: "Updated" },
  { value: "title", label: "Title" },
] as const satisfies readonly { value: SortKey; label: string }[];

/** The facets a chip can carry. `due`/`created` use `before`/`after`; every
 * other facet is `is` or `is_not`. */
export type FilterFacet =
  | "tag"
  | "project"
  | "status"
  | "priority"
  | "waiting"
  | "due"
  | "created";
export type FilterOp = "is" | "is_not" | "before" | "after";

/**
 * One bubble in the filter strip.
 *
 * Stored as a flat list rather than pre-grouped into a `FilterSpec` so the UI
 * can render and remove chips one at a time; `compileFilters` folds the list
 * into the spec at query time.
 */
export interface ActiveFilter {
  facet: FilterFacet;
  op: FilterOp;
  /** Tag/project name, status key, priority digit, waiting kind, or
   * `YYYY-MM-DD`. */
  value: string;
}

/** Mirrors `NewTask`. */
export interface NewTask {
  title: string;
  body?: string | null;
  status?: TaskStatus;
  priority?: number;
  project?: string | null;
  due_at?: string | null;
  defer_until?: string | null;
  recurrence?: string | null;
  parent_id?: string | null;
  repo_path?: string | null;
  prompt?: string | null;
  bg_allowed?: boolean;
  bg_executor?: BackgroundExecutor;
  bg_writes_allowed?: boolean;
  bg_shell_allowed?: boolean;
  bg_remote_allowed?: boolean;
  bg_mcp_servers?: string | null;
  model?: string | null;
  section_id?: string | null;
  /** Filed ARMED — hidden until the check trips. */
  tripwire?: string | null;
  tripwire_every_minutes?: number | null;
  tripwire_on_trip?: TripwireOnTrip | null;
}

/**
 * Mirrors `TaskPatch`. An **absent** key leaves the column alone; an explicit
 * `null` clears it. Don't spread a whole Task in here — you'd rewrite fields
 * you didn't mean to touch.
 */
export interface TaskPatch {
  title?: string;
  body?: string | null;
  status?: TaskStatus;
  priority?: number;
  project?: string | null;
  due_at?: string | null;
  defer_until?: string | null;
  recurrence?: string | null;
  sort_order?: number;
  repo_path?: string | null;
  prompt?: string | null;
  bg_allowed?: boolean;
  /**
   * Switching this clears any existing `model` server-side, unless a
   * replacement `model` is sent in the SAME patch — a model name only means
   * something to one vendor.
   */
  bg_executor?: BackgroundExecutor;
  bg_writes_allowed?: boolean;
  bg_shell_allowed?: boolean;
  bg_remote_allowed?: boolean;
  bg_mcp_servers?: string | null;
  model?: string | null;
  section_id?: string | null;
  /**
   * The hold. A string holds the task, `null` (or an empty string) releases
   * it, absent leaves it alone. Independent of `status`; the backend releases
   * it on `done` and on `queued` (re-queueing is the un-park gesture).
   */
  blocked_reason?: string | null;
  /**
   * Resolved on its own server-side, deliberately NOT alongside
   * `bg_executor` — arming a review preselects the *other* agent, so sweeping
   * intent into that resolution would disarm the review as it was set up.
   */
  bg_intent?: BackgroundIntent;
  /**
   * The five tripwire fields resolve TOGETHER server-side, like status +
   * blocked_reason. `tripwire: null` clears the tripwire and every column
   * beside it; a new or changed spec re-arms and resets the attempt history.
   */
  tripwire?: string | null;
  /** Minutes; `null` = manual. Floor-checked against the spec's kind. */
  tripwire_every_minutes?: number | null;
  tripwire_on_trip?: TripwireOnTrip | null;
  /** The ONLY value a client may send is `"armed"` — a re-arm from `tripped`
   *  or `error`. The runner owns the other two. */
  tripwire_state?: "armed";
  /** "Check now": stamps a request the runner honours on its next tick,
   *  whatever the cadence. Refused while the task is deferred. */
  tripwire_check_requested?: true;
}

export interface TaskRun {
  id: string;
  task_id: string;
  mode: "foreground" | "background";
  /**
   * `budget_stopped`/`turns_exhausted`/`timeout` are all "one of OUR guards
   * stopped a run that was working" rather than "the run broke" — see
   * `RUN_STATUS_LABELS` in `lib/format.ts`, which is the one place any of these
   * get a name and a colour.
   *
   * The last two are about what the app can HONESTLY claim to know.
   * `dispatched` is every foreground run: a Ghostty session was opened and the
   * app has no visibility into it after that, so it never claims one is
   * running. `abandoned` is a background run whose process vanished without
   * recording an outcome — distinct from `failed`, which asserts the work
   * broke, and from `timeout`, which asserts our own guard fired.
   */
  status:
    | "running"
    | "ready_for_review"
    | "needs_action"
    | "unverified"
    | "succeeded"
    | "failed"
    | "cancelled"
    | "timeout"
    | "budget_stopped"
    | "turns_exhausted"
    | "dispatched"
    | "abandoned";
  started_at: string;
  finished_at: string | null;
  session_id: string | null;
  exit_code: number | null;
  result_text: string | null;
  output_tokens: number | null;
  scratch_dir: string | null;
  input_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  model: string | null;
  /**
   * Which agent produced this run. Recorded on the run, so it keeps saying the
   * truth after the task is re-pointed at a different agent.
   */
  executor: BackgroundExecutor;
  /**
   * What this run was for. Recorded on the run for a sharper reason than
   * `executor`'s: the task's own `bg_intent` is reset when a run finishes, so
   * this is the only durable record that a review ever happened.
   */
  intent: BackgroundIntent;
}

/** A provider-neutral, safe-to-render execution trace row. */
export interface RunEvent {
  id: number;
  run_id: string;
  seq: number;
  occurred_at: string;
  kind: "tool" | "notice";
  call_id: string | null;
  tool_name: string | null;
  phase: "started" | "succeeded" | "failed";
  summary: string;
  detail_json: string | null;
}

/**
 * A run plus the task it belongs to — what Mission Control renders. Mirrors
 * `db::runs::RunWithTask`, whose run fields are `#[serde(flatten)]`ed, so this
 * is a genuine superset of `TaskRun` on the wire.
 *
 * Already filtered to live tasks by the query: a trashed or archived task's runs
 * never appear here (invariant 13).
 */
export interface RunWithTask extends TaskRun {
  task_title: string;
  task_short_id: number | null;
  task_project: string | null;
  task_status: TaskStatus;
  latest_event_summary: string | null;
  latest_event_phase: RunEvent["phase"] | null;
  latest_event_seq: number | null;
}

export interface ProjectSummary {
  project: string;
  open_count: number;
}

export interface ViewCounts {
  /**
   * **The Inbox view's length** — what is left to file: no project, no
   * section, no due date, not held. It used to count only the project-less
   * subset of a view that held every undated task, so the badge and the
   * header disagreed; the view now holds exactly what the badge counted.
   */
  inbox: number;
  today: number;
  /** **Legacy.** The Upcoming view is gone and the backend now sends a copy of
   * `waiting` under this key, because the iOS client decodes it
   * non-optionally. Nothing here reads it. */
  upcoming?: number;
  waiting: number;
  in_review: number;
}

export interface BudgetSettings {
  max_concurrent: number;
  timeout_minutes: number;
  daily_run_cap: number;
  daily_token_cap: number;
  /**
   * Ceiling on what ONE run may touch, polled while it is alive and enforced
   * by killing it. Covers **both** agents — unlike `max_turns`, because it
   * lives in the provider-neutral poll loop.
   *
   * This is the real per-run cost bound. `0` disables it.
   */
  run_token_cap: number;
  /**
   * **Claude only** — its per-run turn ceiling, and since the ceiling above
   * exists, a *loop detector* rather than a cost guard: turns correlate with
   * spend far too loosely to be one (every turn re-sends the whole accumulated
   * conversation, so late turns cost several times what early ones do). Codex
   * has no equivalent.
   */
  max_turns: number;
  /**
   * **Claude's** default background model. A Codex run ignores this and uses
   * the Codex CLI's own default unless the task sets `model`.
   */
  model: string;
  /**
   * Daily sub-cap on `agent:` tripwire CHECKS, in total tokens. A slice of
   * `daily_token_cap`, not a second budget — check tokens count against both.
   * `0` switches the sub-cap off.
   */
  tripwire_daily_token_cap: number;
}

export interface BudgetStatus {
  settings: BudgetSettings;
  running: number;
  runs_today: number;
  output_tokens_today: number;
  /** input + cache-read + cache-write + output — the real rate-limit signal. */
  total_tokens_today: number;
  /** The slice of `total_tokens_today` spent by agent tripwire checks. */
  tripwire_tokens_today: number;
  paused: boolean;
  /** Where the `_today` counters start measuring (UTC, space-separated). */
  window_started_at: string;
  /**
   * True when a **manual reset**, not midnight, set that boundary — in which
   * case the counters above are not today's spend and must not be labelled as
   * such. A day that has burned 15M and been reset reads 0 here.
   */
  window_reset: boolean;
}

export interface CompletionResult {
  completed: Task;
  next: Task | null;
}

export interface DispatchResult {
  run_id: string;
  repo_path: string;
  scratch_dir: string;
}

/**
 * Mirrors `commands::dispatch::QueueResult`. Queueing succeeds regardless, but
 * "Queued" with nothing draining the queue looks identical on screen to
 * "Queued, running shortly" — and stays that way forever. These two say which.
 */
export interface QueueResult {
  task: Task;
  runner_installed: boolean;
  paused: boolean;
  /** Which agent will pick it up, so the confirmation can name it. */
  executor: BackgroundExecutor;
}

export interface RepoSuggestion {
  name: string;
  path: string;
}

/** Payload of the `mcp-activity` event — a hint to refetch, never content. */
export interface McpActivity {
  v: number;
  kind:
    | "task-created"
    | "task-updated"
    | "task-completed"
    | "task-deleted"
    | "run-started"
    | "run-progress"
    | "run-finished"
    /** A project update was posted or a milestone changed. `task_id` is empty:
     * refresh without flashing a row. */
    | "project-updated"
    /** A tripwire check came back true / crossed its failure threshold. The
     * task has already moved (state, comment, maybe completion); this is the
     * cue to refetch and flash it. */
    | "tripwire-tripped"
    | "tripwire-failed";
  task_id: string;
  tool?: string;
  run_id?: string;
  summary?: string;
}
