/** Typed IPC wrappers. The only place `invoke` is called with a raw string. */
import { invoke } from "@tauri-apps/api/core";
import type {
  Attachment,
  BackgroundExecutor,
  BoardRow,
  BudgetSettings,
  BudgetStatus,
  Comment,
  CommentWithAttachments,
  CompletionResult,
  CaptureAssessment,
  DispatchResult,
  FilterSpec,
  ListFilters,
  JevSaveSettings,
  JevStatus,
  NewTask,
  Project,
  ProjectHealth,
  ProjectKind,
  ProjectRepo,
  ProjectSummary,
  QueueResult,
  ProjectUpdate,
  RepoSuggestion,
  Retrospective,
  RetrospectiveSpec,
  ReadinessAssessment,
  RunEvent,
  RunWithTask,
  Section,
  Tag,
  TagWithCount,
  Task,
  TaskPatch,
  TaskRelations,
  TaskRun,
  TripwireRuntime,
  ViewCounts,
} from "@/types/task";

export interface DockMenuSettings {
  showToday: boolean;
  showInbox: boolean;
  showQuickAdd: boolean;
  maxTasks: number;
}

/** One tile in the icon picker — see `commands::icons`. */
export interface IconCandidate {
  label: string;
  dataUrl: string;
  /** Its solid background was cut out. */
  cutout: boolean;
}

export const api = {
  dock: {
    settings: () => invoke<DockMenuSettings>("get_dock_menu_settings"),
    saveSettings: (settings: DockMenuSettings) =>
      invoke<DockMenuSettings>("save_dock_menu_settings", { settings }),
  },
  tasks: {
    list: (filters: ListFilters = {}) =>
      invoke<Task[]>("list_tasks", { filters }),
    get: (id: string) => invoke<Task | null>("get_task", { id }),
    subtasks: (parentId: string) =>
      invoke<Task[]>("list_subtasks", { parentId }),
    create: (task: NewTask) => invoke<Task>("create_task", { task }),
    update: (id: string, patch: TaskPatch) =>
      invoke<Task>("update_task", { id, patch }),
    complete: (id: string, result?: string) =>
      invoke<CompletionResult>("complete_task", { id, result: result ?? null }),
    /** Move to the Trash — recoverable for `RETENTION_DAYS`. This is what
     * every "delete" affordance in the UI calls. The irreversible one is
     * `destroy` below and is reachable only from inside the Trash. */
    trash: (id: string) => invoke<void>("trash_task", { id }),
    restore: (id: string) => invoke<Task>("restore_task", { id }),
    archive: (id: string) => invoke<void>("archive_task", { id }),
    unarchive: (id: string) => invoke<Task>("unarchive_task", { id }),
    /** Irreversible. Confirm before calling. */
    destroy: (id: string) => invoke<void>("destroy_task", { id }),
    emptyTrash: () => invoke<number>("empty_trash"),
    /** Days-until-purge per trashed task id. */
    countdowns: () => invoke<Record<string, number>>("trash_countdowns"),
    reorder: (ids: string[]) => invoke<void>("reorder_tasks", { ids }),
    search: (query: string, limit?: number) =>
      invoke<Task[]>("search_tasks", { query, limit: limit ?? null }),
    /** The filter bar's chips + text. Only called once at least one chip is
     * active — a plain text search stays on `search`. */
    filter: (spec: FilterSpec) => invoke<Task[]>("filter_tasks", { spec }),
    /** One count per spec, in order — the sidebar's node badges, each the
     * same WHERE clause `filter` runs so a badge and its list agree. */
    countFiltered: (specs: FilterSpec[]) =>
      invoke<number[]>("count_filtered", { specs }),
    projects: () => invoke<ProjectSummary[]>("list_projects"),
    counts: () => invoke<ViewCounts>("view_counts"),
  },
  projects: {
    list: (includeArchived = false) =>
      invoke<Project[]>("list_all_projects", { includeArchived }),
    /** `kind` defaults to `"area"` server-side — the direction that never
     * invents a deadline for something nobody classified. */
    create: (name: string, kind?: ProjectKind, parent?: string | null) =>
      invoke<Project>("create_project", {
        name,
        kind: kind ?? null,
        parent: parent ?? null,
      }),
    /** Nest a project under an area, or pass `null` to lift it to the top. */
    setParent: (name: string, parent: string | null) =>
      invoke<void>("set_project_parent", { name, parent }),
    /** Promoting to `"area"` clears the parent, target date and closed stamp. */
    setKind: (name: string, kind: ProjectKind) =>
      invoke<void>("set_project_kind", { name, kind }),
    /** ISO-8601 UTC with a trailing `Z`. Refused on an area. */
    setTargetDate: (name: string, targetDate: string | null) =>
      invoke<void>("set_project_target_date", { name, targetDate }),
    /** Closing a project archives it too; reopening undoes both. */
    setClosed: (name: string, closed: boolean) =>
      invoke<void>("set_project_closed", { name, closed }),
    /** Icon + colour (`db::appearance`). Both are sent every time — null
     * clears either back to the default. */
    setAppearance: (name: string, color: string | null, icon: string | null) =>
      invoke<void>("set_project_appearance", { name, color, icon }),
    rename: (from: string, to: string) =>
      invoke<Project>("rename_project", { from, to }),
    archive: (name: string, archived: boolean) =>
      invoke<void>("archive_project", { name, archived }),
    /** `names` in their new visual order; each takes its index as sort_order. */
    reorder: (names: string[]) => invoke<void>("reorder_projects", { names }),
    remove: (name: string, clearFromTasks = false) =>
      invoke<void>("delete_project", { name, clearFromTasks }),
    /** The GSD board — one row per project. */
    board: () => invoke<BoardRow[]>("project_board"),
    /** The ONLY way a project's health changes. There is deliberately no bare
     * setter: a status with no written explanation is how a board rots. */
    postUpdate: (project: string, body: string, health: ProjectHealth | null) =>
      invoke<ProjectUpdate>("post_project_update", { project, body, health }),
    updates: (project: string, limit?: number) =>
      invoke<ProjectUpdate[]>("list_project_updates", {
        project,
        limit: limit ?? null,
      }),
    /**
     * The project ↔ repo mapping. This is what stops a project's NAME and its
     * directory being derived from each other.
     */
    repos: {
      /** Every mapping — small table, loaded once rather than per project. */
      list: () => invoke<ProjectRepo[]>("list_project_repos"),
      add: (project: string, repoPath: string, makePrimary = false) =>
        invoke<ProjectRepo>("add_project_repo", {
          project,
          repoPath,
          makePrimary,
        }),
      remove: (id: string) => invoke<void>("remove_project_repo", { id }),
      setPrimary: (id: string) =>
        invoke<void>("set_primary_project_repo", { id }),
      /** Which project owns this directory? null = unmapped. */
      owner: (repoPath: string) =>
        invoke<string | null>("project_for_repo_path", { repoPath }),
    },
  },
  sections: {
    list: (project: string | null) =>
      invoke<Section[]>("list_sections", { project }),
    create: (project: string | null, name: string) =>
      invoke<Section>("create_section", { project, name }),
    rename: (id: string, name: string) =>
      invoke<void>("rename_section", { id, name }),
    remove: (id: string) => invoke<void>("delete_section", { id }),
    reorder: (project: string | null, ids: string[]) =>
      invoke<void>("reorder_sections", { project, ids }),
    moveTask: (taskId: string, sectionId: string | null) =>
      invoke<void>("move_task_to_section", { taskId, sectionId }),
    /** Dating a section is what turns it into a milestone. Full ISO-8601 UTC. */
    setTargetDate: (id: string, targetDate: string | null) =>
      invoke<void>("set_section_target_date", { id, targetDate }),
    setCompleted: (id: string, done: boolean) =>
      invoke<void>("set_section_completed", { id, done }),
  },
  tags: {
    list: () => invoke<TagWithCount[]>("list_tags"),
    forTask: (taskId: string) => invoke<Tag[]>("task_tags", { taskId }),
    setForTask: (taskId: string, tags: string[]) =>
      invoke<Tag[]>("set_task_tags", { taskId, tags }),
    rename: (from: string, to: string) => invoke<Tag>("rename_tag", { from, to }),
    setAppearance: (name: string, color: string | null, icon: string | null) =>
      invoke<void>("set_tag_appearance", { name, color, icon }),
    remove: (name: string) => invoke<void>("delete_tag", { name }),
    /** Every task's tags keyed by task id — one call for the whole board,
     *  rather than one per row. Same shape as `deps.blockedIds`. */
    byTask: () => invoke<Record<string, string[]>>("task_tag_map"),
    /** Tasks a delete would unlabel, closed ones included — what the confirm
     *  quotes. Deliberately not the sidebar's `open_count`. */
    usage: (name: string) => invoke<number>("tag_usage_count", { name }),
  },
  deps: {
    forTask: (taskId: string) => invoke<TaskRelations>("task_relations", { taskId }),
    add: (taskId: string, blockedById: string) =>
      invoke<void>("add_dependency", { taskId, blockedById }),
    remove: (taskId: string, blockedById: string) =>
      invoke<void>("remove_dependency", { taskId, blockedById }),
    blockedIds: () => invoke<string[]>("blocked_task_ids"),
    relate: (
      fromTaskId: string,
      toTaskId: string,
      kind: "supersedes" | "relates_to"
    ) => invoke<void>("add_relation", { fromTaskId, toTaskId, kind }),
    unrelate: (
      fromTaskId: string,
      toTaskId: string,
      kind: "supersedes" | "relates_to"
    ) => invoke<void>("remove_relation", { fromTaskId, toTaskId, kind }),
  },
  retrospective: (spec: RetrospectiveSpec) =>
    invoke<Retrospective>("retrospective", { spec }),

  jev: {
    status: () => invoke<JevStatus>("jev_status"),
    save: (settings: JevSaveSettings) =>
      invoke<JevStatus>("save_jev_settings", { settings }),
    assessCapture: (request: {
      text: string;
      projects: string[];
      tags: string[];
    }) => invoke<CaptureAssessment | null>("jev_assess_capture", { request }),
    assessReadiness: (taskId: string) =>
      invoke<ReadinessAssessment | null>("jev_assess_readiness", { taskId }),
  },
  tripwires: {
    /** Every live tripwire task's runtime row keyed by task id — one call for
     * the board, like `tags.byTask`. Setting, re-arming and "check now" are
     * ordinary `tasks.update` patches; evaluating is the runner's alone. */
    runtime: () =>
      invoke<Record<string, TripwireRuntime>>("list_tripwire_runtime"),
    /** The runnable scripts in THIS Mac's tripwires directory. Host-local. */
    scripts: () => invoke<string[]>("tripwire_scripts"),
  },
  comments: {
    list: (taskId: string) => invoke<Comment[]>("list_comments", { taskId }),
    add: (taskId: string, body: string) =>
      invoke<Comment>("add_comment", { taskId, body }),
    addWithAttachments: (taskId: string, body: string, sourcePaths: string[], clipboardImages: number[][] = []) =>
      invoke<CommentWithAttachments>("add_comment_with_attachments", {
        taskId,
        body,
        sourcePaths,
        clipboardImages,
      }),
    edit: (id: string, body: string) => invoke<Comment>("edit_comment", { id, body }),
    remove: (id: string) => invoke<void>("delete_comment", { id }),
  },
  attachments: {
    readClipboard: () => invoke<number[]>("read_clipboard_image"),
    tryReadClipboard: () => invoke<number[] | null>("try_read_clipboard_image"),
    paste: (taskId: string, quiet = false) =>
      invoke<Attachment[]>("paste_task_image", { taskId, quiet }),
    list: (taskId: string) =>
      invoke<Attachment[]>("list_task_attachments", { taskId }),
    add: (taskId: string, commentId: string | null, sourcePaths: string[]) =>
      invoke<Attachment[]>("add_task_attachments", {
        taskId,
        commentId,
        sourcePaths,
      }),
    remove: (id: string) => invoke<void>("delete_task_attachment", { id }),
  },
  runs: {
    list: (taskId: string) => invoke<TaskRun[]>("list_runs", { taskId }),
    events: (runId: string, afterSeq = 0) =>
      invoke<RunEvent[]>("list_run_events", { runId, afterSeq }),
    /** Across every task, running first — Mission Control's feed. */
    recent: (limit?: number) =>
      invoke<RunWithTask[]>("recent_runs", { limit: limit ?? null }),
    /**
     * Ask an in-flight background run to stop. `true` = the request was taken,
     * `false` = the run was already terminal (or is a foreground session, which
     * this process does not own a child for).
     *
     * A REQUEST. The runner notices at its next 250ms poll and records the run
     * as `cancelled`; nothing here waits for the kill, because the run may
     * equally be a second from finishing on its own.
     */
    cancel: (runId: string) => invoke<boolean>("cancel_run", { runId }),
  },
  budget: {
    status: () => invoke<BudgetStatus>("budget_status"),
    save: (settings: BudgetSettings) =>
      invoke<BudgetStatus>("save_budget_settings", { settings }),
    /**
     * Zero the daily counters by moving the window start to now. Keeps every
     * run's recorded cost — only the "since when" moves.
     */
    reset: () => invoke<BudgetStatus>("reset_budget_window"),
    setPaused: (paused: boolean) =>
      invoke<boolean>("set_background_paused", { paused }),
  },
  repos: {
    suggest: () => invoke<RepoSuggestion[]>("suggest_repos"),
  },
  checkpoint: {
    diff: (runId: string) => invoke<string>("run_diff", { runId }),
    restore: (runId: string) => invoke<string>("restore_checkpoint", { runId }),
  },
  runner: {
    isInstalled: () => invoke<boolean>("is_runner_installed"),
    install: () => invoke<boolean>("install_runner"),
    uninstall: () => invoke<boolean>("uninstall_runner"),
    /** Drain the queue once, now, without installing the LaunchAgent. */
    runOnce: () => invoke<string>("run_queue_once"),
  },
  dispatch: {
    foreground: (taskId: string) =>
      invoke<DispatchResult>("dispatch_foreground", { taskId }),
    prompt: (taskId: string) =>
      invoke<string>("get_dispatch_prompt", { taskId }),
    queueBackground: (taskId: string) =>
      invoke<QueueResult>("queue_background", { taskId }),
    /**
     * `null` = this agent is installed and usable; a string = why it isn't.
     *
     * Called while the executor is being *chosen*, not at queue time: a
     * decision needs to know its option is available, and finding out at
     * dispatch is finding out too late to have decided differently.
     */
    executorAvailable: (executor: BackgroundExecutor) =>
      invoke<string | null>("executor_available", { executor }),
  },
  appearance: {
    /** Pin AppKit's `NSApp.appearance`. The web layer can't reach native chrome
     * (NSMenu, sheets, scrollbars), so the theme toggle has to say it twice —
     * once in CSS, once here. See `appearance_macos` in the Rust side. */
    setNative: (dark: boolean) =>
      invoke<void>("set_native_appearance", { dark }),
  },
  vaultLook: {
    /** Ask Onyx for the Obsidian vault's palette (`commands::vault_look`).
     * Resolves `null` when Onyx is not running, the vault has never synced, or
     * Onyx's own "Match vault appearance" is off. Rejects only for an address
     * that is not this Mac. The result is `unknown` on purpose — it is always
     * run through `parseVaultPalette` before anything uses it. */
    fetch: (url: string) => invoke<unknown>("fetch_vault_look", { url }),
  },
  icons: {
    /** App icons found in a project's repos, ready to store (`commands::icons`).
     * This Mac only — it reads the repos off disk. */
    repoCandidates: (project: string) =>
      invoke<IconCandidate[]>("repo_icon_candidates", { project }),
    /** One picked image file, as-is and (when it has one) with its solid
     * background cut out. */
    fromFile: (path: string) => invoke<IconCandidate[]>("icon_from_file", { path }),
  },
  glass: {
    /** Turn the window's desktop blur on or off.
     *
     * The RGB triple is the theme's base colour, and is sent on BOTH edges:
     * turning glass off has to repaint the window opaque in something, and the
     * only place that colour is authoritative is `globals.css`. Passing it from
     * here rather than hardcoding it in Rust is what stops the two drifting the
     * next time a theme token moves. */
    set: (enabled: boolean, radius: number, rgb: [number, number, number]) =>
      invoke<void>("set_window_glass", {
        enabled,
        radius,
        r: rgb[0],
        g: rgb[1],
        b: rgb[2],
      }),
    /** Change only the radius, for the transparency slider. Separate from `set`
     * so a drag doesn't re-run the window setup on every step. */
    setRadius: (radius: number) => invoke<void>("set_blur_radius", { radius }),
    /** Is macOS's Reduce Transparency on? Read at hydrate; changes after that
     * arrive as a `reduce-transparency-changed` event. */
    reduceTransparency: () => invoke<boolean>("get_reduce_transparency"),
  },
  notifications: {
    /**
     * Collect and clear the task a notification click asked to open.
     *
     * The id is *not* on the `open-task-from-notification` event, deliberately.
     * A click on a closed app launches it and Rust emits from `.setup()`,
     * before any listener exists — so the event alone loses precisely the click
     * that mattered most. One latch, drained by both triggers: this call on
     * mount, and the same call from the event handler.
     */
    takePending: () =>
      invoke<string | null>("take_pending_notification_task"),
  },

  /**
   * Pairing, pausing and unpairing.
   *
   * Host-local on purpose: these say who THIS machine talks to. Routed through
   * a server they would configure the server, and `/rpc`'s board scope refuses
   * only three verbs by name — so a phone's token could otherwise unpair the
   * Mac it syncs with.
   */
  sync: {
    status: () => invoke<SyncStatus>("sync_status"),
    /** Returns a sentence describing what pairing did, for the panel to show. */
    pair: (url: string, token: string) => invoke<string>("sync_pair", { url, token }),
    now: () => invoke<ReconcileReport>("sync_now"),
    setEnabled: (enabled: boolean) => invoke<void>("sync_set_enabled", { enabled }),
    unpair: (force: boolean) => invoke<void>("sync_unpair", { force }),
  },

  hosts: {
    list: () => invoke<HostEntry[]>("list_hosts"),
    setPolicy: (hostId: string, policy: HostPolicy) =>
      invoke<void>("set_host_policy", { hostId, policy }),
    setMirrorRemote: (hostId: string, remote: string | null) =>
      invoke<void>("set_host_mirror_remote", { hostId, remote }),
    remove: (hostId: string) => invoke<boolean>("remove_host", { hostId }),
  },
};

/** A tier is a POLICY CEILING, not a probe — a host is read-only because that
 *  is the decision, not because anything detected what it can do. */
export type ToolTier = "read-only" | "shell" | "write";
export type WorkspaceKind = "live" | "mirror" | "detached";

/** What a host is allowed to do. Declared, and never overwritten by a restart:
 *  a container comes back twice a day and must not re-assert its image's
 *  defaults over a decision made here. */
export interface HostCapability {
  id: string;
  label: string;
  max_tier: ToolTier;
  executors: string[];
  workspaces: WorkspaceKind[];
  can_checkpoint: boolean;
  max_concurrent: number;
  /** Percent of the daily token pool this host must leave unspent. A ceiling,
   *  never a second budget — it reserves headroom and never creates any. */
  headroom_pct: number;
  priority: number;
  enabled: boolean;
  /** Observed, not declared. There is deliberately no way to edit these: a
   *  settable "Claude is installed" checkbox is a lie waiting to be believed
   *  by the router. */
  last_seen_at: string | null;
  paused: boolean;
  gate: string | null;
  executors_ready: string[];
}

export interface HostEntry {
  capability: HostCapability;
  /** Repos this host holds a mirror of. Written by the machine that PUSHED
   *  them, which is the only side that knows — a bare repo is named by a hash
   *  of its source path. */
  mirrors: string[];
  mirror_remote: string | null;
  is_this_host: boolean;
  alive: boolean;
}

/** Only the declared half. Absent fields are left alone. */
export interface HostPolicy {
  label?: string;
  max_tier?: ToolTier;
  executors?: string[];
  workspaces?: WorkspaceKind[];
  can_checkpoint?: boolean;
  max_concurrent?: number;
  headroom_pct?: number;
  priority?: number;
  enabled?: boolean;
}

export interface SyncStatus {
  enabled: boolean;
  /** Configured but switched off is PAUSED — a state you chose. Not the same
   *  as never configured, which is a setup prompt. */
  paired: boolean;
  peer_id: string | null;
  peer_url: string | null;
  last_pull_at: string | null;
  /** Deletions waiting to go. The only thing that cannot travel as state, which
   *  is why unpairing with these queued loses work. */
  pending: number;
  lease_remaining: number | null;
  next_short_id: number | null;
  host: string;
  /** When a reconcile was last ATTEMPTED — by the five-minute loop or by hand.
   *  Separate from `last_pull_at`, which only moves when a pull lands: a stale
   *  pull time alone cannot tell "nothing is trying" from "every attempt is
   *  refused", and that ambiguity is what hid four days of silent staleness. */
  last_attempt_at: string | null;
  /** Why the last attempt failed, or null when it worked. */
  last_error: string | null;
  /** Seconds between automatic passes — shown, so nobody has to guess whether
   *  an automatic pass exists at all. */
  auto_interval_seconds: number;
}

export interface ReconcileReport {
  swept: number;
  pushed: number;
  failed: { local_seq: number; err: string }[];
  pulled: { inserted_or_updated: number; deleted: number };
  /** The pull was skipped because something is still un-pushed. Staying behind
   *  is recoverable; being silently rolled back is not. */
  held_back: boolean;
  /** Attachment BYTES moved this pass. A failure here is one missing image and
   *  never a failed reconcile, so it is reported rather than thrown. */
  blobs: {
    uploaded: number;
    downloaded: number;
    failed: { stored_name: string; err: string }[];
    /** A cap stopped the pass early; the next one continues. */
    more: boolean;
  };
  lease_renewed: { lo: number; hi: number } | null;
}
