import { create } from "zustand";
import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";
import { isClosed, STATUS_LABELS } from "@/lib/format";
import { PRIORITY_LOWEST } from "@/types/task";
import {
  baselineFor,
  defaultsFor,
  sameFilters,
} from "@/lib/viewScopes";
import type {
  WaitingKind,
  ActiveFilter,
  Attachment,
  CaptureAssessment,
  BoardRow,
  Comment,
  FilterSpec,
  Project,
  ProjectHealth,
  ProjectKind,
  ProjectRepo,
  ProjectUpdate,
  Section,
  ListView,
  ProjectSummary,
  RepoSuggestion,
  Tag,
  TagWithCount,
  TripwireRuntime,
  Task,
  TaskPatch,
  TaskRelations,
  TaskRun,
  ViewCounts,
  ViewName,
} from "@/types/task";

/**
 * One reversible action.
 *
 * The inverse is a closure, not a description of a diff, because the two
 * actions that most need undoing can't be expressed as one: completing a
 * recurring task also *creates* its next instance, and a bulk action is N
 * calls whose before-states differ per row. A closure captured at the moment
 * the action fired knows all of that; a generic patch-reverser would have to
 * guess.
 */
export interface UndoEntry {
  /** Monotonic. Identifies the entry to the toast timer, which must only ever
   * clear the toast it armed — a later action replaces it, and a naive timer
   * would cut that newer toast short. */
  key: number;
  /** What was acted on: a task title, or "3 tasks" for a batch. */
  label: string;
  /** What happened to it, as a past-tense phrase: "completed", "moved to
   * Trash". Rendered straight after `label`. */
  verb: string;
  /** Task ids this entry's inverse depends on. An entry naming a row that gets
   * hard-deleted is dropped, because running it would fail silently. */
  ids: string[];
  /** The inverse. Runs with undo recording suppressed. */
  run: () => Promise<void>;
}


interface TaskState {
  tasks: Task[];
  subtasks: Task[];
  runs: TaskRun[];
  projects: ProjectSummary[];
  /**
   * Directory names under ~/Projects — candidates for the WORKSPACE picker.
   *
   * Deliberately not the project autocomplete any more: feeding this to the
   * Project field is what taught the app that `finance-app` was a project, when
   * it is the directory pocketbuddy happens to live in.
   */
  repos: RepoSuggestion[];
  /** Real project records — a project can exist with zero tasks. */
  projectRecords: Project[];
  /** Project ↔ repo mappings, all of them. Small, and needed on every selection. */
  projectRepos: ProjectRepo[];
  /** Sections for the current project/view. */
  sections: Section[];
  /** The GSD board. Only fetched while the GSD view is showing — same pattern
   * as `countdowns`, which only the Trash pays for. */
  board: BoardRow[];
  /** Update log of the project whose board row is expanded, lazily loaded. */
  boardUpdates: Record<string, ProjectUpdate[]>;
  allTags: TagWithCount[];
  /** Tags of the selected task. */
  tags: Tag[];
  /** Every task's tag names, keyed by task id — what the rows render.
   *
   * Derived and fetched alongside the tasks rather than being a field on
   * `Task`, exactly like `blockedIds`: asking per row is N+1, and the list can
   * hold hundreds of rows. */
  tagsByTask: Record<string, string[]>;
  /** Every live tripwire task's volatile row, keyed by task id — what the
   * badge and the detail pane read. Same shape and reason as `tagsByTask`:
   * attempt bookkeeping is its own row (invariant 26), fetched once for the
   * whole board. */
  tripwireByTask: Record<string, TripwireRuntime>;
  relations: TaskRelations;
  comments: Comment[];
  attachments: Attachment[];
  /** Ids of tasks with an open blocker — the list marks these. */
  blockedIds: string[];
  counts: ViewCounts;
  view: ViewName;
  /** Non-null when the sidebar has a project selected rather than a view. */
  activeProject: string | null;
  selectedId: string | null;
  /**
   * Ids ticked in the list's leading checkboxes — the set the bulk bar acts on.
   *
   * Deliberately separate from `selectedId`, which is the *focused* row the
   * detail pane is showing. The two are independent: ticking six rows to act on
   * them together shouldn't drag the detail pane along, and reading one task's
   * details shouldn't disturb a set you've spent six clicks building.
   */
  checkedIds: string[];
  /** Anchor for shift-click ranges — the last row ticked by a plain click. */
  lastCheckedId: string | null;
  query: string;
  /**
   * The filter bar's chips. Non-empty = filter mode: `refresh` queries the
   * whole search scope (trashed out, archived in) and the sidebar's view stops
   * applying — the same contract text search has always had, made visible.
   * Cleared by every navigation that already clears `query`.
   */
  filters: ActiveFilter[];
  loading: boolean;
  error: string | null;
  /** Task ids to flash because something outside the app just changed them. */
  flashIds: string[];
  flash: (id: string) => void;
  /** Last runner bridge hint. Components use the monotonic key to refetch only
   * their run data instead of routing progress through a full task refresh. */
  runActivity: {
    key: number;
    kind: "run-started" | "run-progress" | "run-finished";
    taskId: string;
    runId: string | null;
  } | null;
  /** Days left before purge, per trashed task id. Only fetched for the Trash. */
  countdowns: Record<string, number>;
  /**
   * The undo history, oldest first. ⌘Z pops the last entry and runs its
   * inverse.
   *
   * This is a STACK rather than the single armed trash it replaced, and the
   * stack outliving the toast is the whole point. The old shape tied "can I
   * undo this" to "is the toast still on screen", so eight seconds after a
   * mis-click there was no way back except finding the task in the Trash — and
   * only a trash was reversible at all, which meant the most common mis-click
   * in the app (closing the wrong row) had no undo whatsoever.
   *
   * Every entry carries its own inverse as a closure over the state it
   * captured BEFORE the action ran, so undo never has to re-derive what
   * changed. `ids` is what the entry depends on, which is how a hard delete
   * prunes the entries it would otherwise strand.
   */
  undoStack: UndoEntry[];
  /** The entry the toast is currently offering, or null once it has timed out.
   * Separate from the stack so a dismissed toast doesn't discard the undo. */
  undoToastKey: number | null;
  /** Transient confirmation after a ⌘Z, so an undo fired from the keyboard with
   * no toast on screen still says something happened. */
  undoNotice: string | null;
  /** Set when a task was queued but nothing will drain the queue.
   *
   * Lives in the store rather than in `DispatchBar` because there are two ways
   * to queue — the button and the native right-click menu — and only one of
   * them has a return value to inspect. The menu route emits
   * `background-idle-warning` instead, and both end up rendering the same
   * notice from here. */
  backgroundIdle: { taskId: string; installed: boolean; paused: boolean } | null;
  setBackgroundIdle: (
    v: { taskId: string; installed: boolean; paused: boolean } | null
  ) => void;

  loadRepos: () => Promise<void>;
  /** Map a directory to a project. Returns an error message on failure. */
  addProjectRepo: (
    project: string,
    repoPath: string,
    makePrimary?: boolean
  ) => Promise<string | null>;
  removeProjectRepo: (id: string) => Promise<void>;
  setPrimaryProjectRepo: (id: string) => Promise<void>;
  /**
   * Create an area or project and SCOPE THE LIST TO IT. A new node has no
   * open work, and the sidebar only draws a node with open work (or the one
   * you are standing in) — so without this the row you just typed would not
   * appear anywhere. Landing in it also means the next capture files there.
   * Returns an error message on failure.
   */
  createProject: (
    name: string,
    kind?: ProjectKind,
    parent?: string | null
  ) => Promise<string | null>;
  renameProject: (from: string, to: string) => Promise<string | null>;
  /** Icon + colour for a project/area or a tag. Returns an error message on
   * failure (the server validates every value — the row syncs everywhere). */
  setAppearance: (
    kind: "project" | "tag",
    name: string,
    color: string | null,
    icon: string | null
  ) => Promise<string | null>;
  /** Nest a project under an area, or lift it out with `null`. Returns an error
   * message on failure — every depth rule is enforced server-side, so the
   * message is the one thing the UI has to show rather than pre-guess. */
  setProjectParent: (name: string, parent: string | null) => Promise<string | null>;
  /** Switch between area and project. Promoting to an area clears the parent,
   * target date and closed stamp. */
  setProjectKind: (name: string, kind: ProjectKind) => Promise<string | null>;
  setProjectTargetDate: (
    name: string,
    targetDate: string | null
  ) => Promise<string | null>;
  /** Close a finished project (archives it too), or reopen it. */
  setProjectClosed: (name: string, closed: boolean) => Promise<string | null>;
  /** `names` in their new visual order — the whole sidebar list, not a move. */
  reorderProjects: (names: string[]) => Promise<void>;
  deleteProject: (name: string) => Promise<void>;
  createSection: (name: string) => Promise<string | null>;
  renameSection: (id: string, name: string) => Promise<void>;
  deleteSection: (id: string) => Promise<void>;
  reorderSections: (ids: string[]) => Promise<void>;
  moveTaskToSection: (taskId: string, sectionId: string | null) => Promise<void>;
  /**
   * File a SET of tasks into a section, or (with `null`) out of one.
   *
   * The board's drop targets go through this rather than through
   * `moveTaskToSection` in a loop: a drop is ONE action, so it has to leave one
   * undo entry. N entries would mean N ⌘Zs to take back a single gesture, with
   * the board half-moved in between.
   */
  moveTasksToSection: (ids: string[], sectionId: string | null) => Promise<void>;
  /** The same, for the project grouping's drop targets. Both groupings are one
   * descriptor in `TaskList`, so a drop that moved six rows into a section and
   * one into a project would be the same gesture meaning two things. */
  setTasksProject: (ids: string[], project: string | null) => Promise<void>;
  /** Returns an error message on failure, else null — same shape as
   * `createProject`, because the composer needs to show it inline. */
  postProjectUpdate: (
    project: string,
    body: string,
    health: ProjectHealth | null
  ) => Promise<string | null>;
  loadProjectUpdates: (project: string) => Promise<void>;
  setSectionTargetDate: (id: string, targetDate: string | null) => Promise<void>;
  setSectionCompleted: (id: string, done: boolean) => Promise<void>;
  renameTag: (from: string, to: string) => Promise<string | null>;
  deleteTag: (name: string) => Promise<void>;
  toggleTagFilter: (name: string) => Promise<void>;
  selectOnlyTag: (name: string) => Promise<void>;
  setTaskTags: (id: string, tags: string[]) => Promise<void>;
  addDependency: (id: string, blockedById: string) => Promise<string | null>;
  removeDependency: (id: string, blockedById: string) => Promise<void>;
  addRelation: (
    id: string,
    otherId: string,
    kind: "supersedes" | "relates_to"
  ) => Promise<string | null>;
  removeRelation: (
    id: string,
    otherId: string,
    kind: "supersedes" | "relates_to"
  ) => Promise<void>;
  addComment: (id: string, body: string, imagePaths?: string[], clipboardImages?: number[][]) => Promise<boolean>;
  removeComment: (commentId: string) => Promise<void>;
  pasteNoteImage: (id: string, quiet?: boolean) => Promise<boolean>;
  addNoteImages: (id: string, imagePaths: string[]) => Promise<boolean>;
  removeAttachment: (attachmentId: string) => Promise<void>;
  setView: (view: ViewName) => Promise<void>;
  setProject: (project: string | null) => Promise<void>;
  setQuery: (query: string) => Promise<void>;
  addFilter: (f: ActiveFilter) => Promise<void>;
  /** Replace the whole chip set — Reset, and applying a saved default. */
  setFilters: (filters: ActiveFilter[]) => Promise<void>;
  /** Remove by identity match, not index — chips re-render between the click
   * and the handler when a refresh lands in the gap. */
  removeFilter: (f: ActiveFilter) => Promise<void>;
  /** Clears chips AND the text in one refresh — the Escape/clear-button verb. */
  clearSearch: () => Promise<void>;
  select: (id: string | null) => Promise<void>;
  /**
   * Select a task AND make sure it is actually on screen.
   *
   * `select` alone is not enough for anything arriving from outside the list.
   * The detail pane resolves its task out of `tasks` — the rows the *current
   * view* loaded — so selecting one the view doesn't hold sets `selectedId` to
   * an id nothing renders and leaves the pane blank. That is what a
   * notification click used to do: a finished background run lands in
   * `in_review`, and clicking its banner while sitting on Today looked
   * completely inert.
   *
   * So this navigates first: it fetches the task by id (`get` is deliberately
   * unfiltered, so it finds trashed and archived ones too), and only if the
   * task is missing from the current list does it move to a view that holds it,
   * dropping the search and tag filters that would hide it there. Already
   * visible → nothing moves, because yanking someone out of their view to show
   * them a row they can already see is its own bug.
   */
  reveal: (id: string) => Promise<void>;
  /**
   * Bumped by `reveal`, and by nothing else.
   *
   * The list scrolls to the selected row on a change of THIS, never on a change
   * of `selectedId` — scrolling on every selection would fight the user every
   * time they clicked a row that was already partly off-screen.
   */
  revealTick: number;
  toggleChecked: (id: string) => void;
  /**
   * Shift-click: tick everything between the anchor and `id`, inclusive.
   *
   * `order` is the ids in RENDER order, supplied by the caller because the
   * grouped list doesn't render `tasks` in array order — deriving the range
   * from `tasks` would tick rows that aren't visually between the two clicks,
   * which is the one thing a range selection must never do.
   */
  checkRange: (id: string, order: string[]) => void;
  /**
   * A click on a task row, modifiers and all — the *only* way a mouse builds a
   * selection now that the leading box completes the task instead.
   *
   * Plain click focuses one row and drops whatever set was open; ⌘-click adds
   * or removes one row; ⇧-click takes the range. The seeding matters as much
   * as the ranging: the first ⌘-click has to extend the row already focused,
   * or a two-row selection takes three clicks and the first one silently
   * doesn't count.
   */
  rowClick: (
    id: string,
    mods: { meta: boolean; shift: boolean; toggleOpen?: boolean },
    order: string[]
  ) => void;
  clearChecked: () => void;
  /** Every bulk action below acts on `checkedIds`, then clears it. */
  bulkComplete: () => Promise<void>;
  bulkPriority: (priority: number) => Promise<void>;
  bulkTrash: () => Promise<void>;
  /** Put back — restore from the Trash or the Archive, per the current view. */
  bulkRestore: () => Promise<void>;
  /** Irreversible; the caller confirms first. Trash only. */
  bulkDestroy: () => Promise<void>;
  refresh: () => Promise<void>;
  quickAdd: (input: string, suggestion?: CaptureAssessment | null) => Promise<Task | null>;
  patch: (id: string, patch: TaskPatch) => Promise<void>;
  /** `answer` is the Decide strip's one line — it becomes the task's handoff
   * (`complete_task`'s `result`), so the call is on record for the next
   * session. Plain completion passes nothing. */
  complete: (id: string, answer?: string) => Promise<void>;
  /** Move to the Trash and arm the undo toast. The only "delete" the board
   * exposes; `destroy` is reachable solely from inside the Trash. */
  trash: (id: string) => Promise<void>;
  restore: (id: string) => Promise<void>;
  archive: (id: string) => Promise<void>;
  unarchive: (id: string) => Promise<void>;
  destroy: (id: string) => Promise<void>;
  emptyTrash: () => Promise<void>;
  /** Record a reversible action. No-op while an undo is itself running, which
   * is what stops ⌘Z from turning into a redo of the thing it just took back. */
  pushUndo: (entry: Omit<UndoEntry, "key">) => void;
  /** ⌘Z: pop the newest entry and run its inverse. */
  undoLast: () => Promise<void>;
  /** Hide the toast. The entry stays on the stack — ⌘Z still reaches it. */
  dismissUndo: () => void;
  reorder: (ids: string[]) => Promise<void>;
  addSubtask: (parentId: string, title: string) => Promise<void>;
}

const EMPTY_COUNTS: ViewCounts = {
  inbox: 0,
  today: 0,
  waiting: 0,
  in_review: 0,
};

/**
 * Map a sidebar selection onto a view the backend actually has.
 *
 * `gsd` renders the project board instead of a task list, so it never became a
 * `db::tasks::View` variant. `all` is the right substitute in both cases: at the
 * top level the board is what's on screen and the task list is unused, and after
 * drilling into a project from the board `view` STAYS `"gsd"` while
 * `activeProject` carries the filter — so the list wants that project's open
 * tasks, which is exactly `all` plus the project filter already being sent.
 */
function listView(view: ViewName): ListView {
  return view === "gsd" ? "all" : view;
}

/**
 * Reset fields for the multi-select set, spread into every filter change.
 *
 * A tick means "this row". Once the list underneath it changes — a different
 * view, project, tag filter or search — the set no longer refers to anything
 * the user pointed at, and a bulk bar still reading "3 selected" over a list
 * that never showed those three is a way to act on the wrong tasks.
 */
const CLEAR_CHECKED = () => ({ checkedIds: [] as string[], lastCheckedId: null });

/** The view the app opens on. Named so the initial `filters` can be derived
 * from it — the two must agree, or the first paint applies another view's
 * defaults. */
const INITIAL_VIEW: ViewName = "today";

/**
 * Run a project-shape mutation, refresh, and hand back the error message.
 *
 * Every PARA rule — the depth cap, "an area has no finish line", "move the
 * children out first" — is enforced in `db::projects` and nowhere else, so the
 * UI's job is to SHOW the refusal, never to pre-guess it. Guessing here is how
 * the menu and the database come to disagree about what is allowed.
 */
async function projectMutation(
  get: () => TaskState,
  run: () => Promise<void>
): Promise<string | null> {
  try {
    await run();
    await get().refresh();
    return null;
  } catch (e) {
    return String(e).replace(/^Invalid input: /, "");
  }
}

/**
 * Fold the chip list (plus any free text) into the backend's `FilterSpec`.
 *
 * Pure and exported for tests. The grouping rules live here and nowhere else:
 * values within a facet collect into one OR list (`tags` keeps its ALL
 * semantics — the backend narrows), and the four date slots are single-valued
 * because `upsertFilter` already guarantees at most one chip per slot.
 */
export function compileFilters(filters: ActiveFilter[], query: string): FilterSpec {
  const spec: FilterSpec = {};
  if (query.trim()) spec.query = query;
  for (const f of filters) {
    const no = f.op === "is_not";
    switch (f.facet) {
      case "tag":
        if (no) (spec.exclude_tags ??= []).push(f.value);
        else (spec.tags ??= []).push(f.value);
        break;
      case "project":
        if (no) (spec.exclude_projects ??= []).push(f.value);
        else (spec.projects ??= []).push(f.value);
        break;
      case "status":
        if (no) (spec.exclude_statuses ??= []).push(f.value as Task["status"]);
        else (spec.statuses ??= []).push(f.value as Task["status"]);
        break;
      case "priority":
        if (no) (spec.exclude_priorities ??= []).push(Number(f.value));
        else (spec.priorities ??= []).push(Number(f.value));
        break;
      case "waiting":
        if (no) (spec.exclude_waiting ??= []).push(f.value as WaitingKind);
        else (spec.waiting ??= []).push(f.value as WaitingKind);
        break;
      case "due":
        if (f.op === "before") spec.due_before = f.value;
        else spec.due_after = f.value;
        break;
      case "created":
        if (f.op === "before") spec.created_before = f.value;
        else spec.created_after = f.value;
        break;
    }
  }
  return spec;
}

/** The tag names the chips are narrowing by — what the sidebar highlights.
 *
 * Derived rather than stored: `activeTags` used to be a second place a tag
 * filter could live, so a tag picked in the sidebar and one picked in the
 * filter strip were two different things that cleared each other. */
export function activeTagsOf(filters: ActiveFilter[]): string[] {
  return filters.filter((f) => f.facet === "tag" && f.op === "is").map((f) => f.value);
}

/**
 * Add a chip to the list, resolving collisions by facet.
 *
 * Value facets treat an exact duplicate as a no-op — two `Status: Done` chips
 * would be one filter rendered twice. A date facet REPLACES the chip holding
 * the same slot (`due before`, `created after`, …): two "due before" bounds
 * can only mean the tighter one, and keeping both would render a filter half
 * of which does nothing.
 */
export function upsertFilter(filters: ActiveFilter[], f: ActiveFilter): ActiveFilter[] {
  if (f.facet === "due" || f.facet === "created") {
    return [...filters.filter((x) => !(x.facet === f.facet && x.op === f.op)), f];
  }
  // An exact duplicate is one filter rendered twice.
  if (filters.some((x) => x.facet === f.facet && x.op === f.op && x.value === f.value)) {
    return filters;
  }
  // Adding `status is done` must take off `status is not done`, and the other
  // way round: one facet cannot say both about one value, and the pair
  // compiles to an empty list with nothing on screen to explain it.
  const opposite = f.op === "is" ? "is_not" : "is";
  return [
    ...filters.filter((x) => !(x.facet === f.facet && x.op === opposite && x.value === f.value)),
    f,
  ];
}

/**
 * The query clicking `node` runs — the node scoped through `expand_scope`,
 * under the chips it opens with. Exported so the badge and `refresh` cannot
 * compile the node's scope two different ways.
 */
export function nodeSpec(
  name: string,
  saved: Record<string, ActiveFilter[]>,
  projectRecords: Project[]
): FilterSpec {
  return {
    view: "all",
    project: name,
    exclude_closed: false,
    ...compileFilters(defaultsFor("all", name, saved, projectRecords), ""),
  };
}

/**
 * Attach `badge_count` to every node: the length of the list clicking it
 * opens, counted server-side under the node's own chips.
 *
 * `open_count` alone was the badge until 2026-09-22, and it counted "open
 * and top-level" while the list applied the area's saved `Waiting is not …`
 * chip — so pocketbuddy read "8" over "Nothing here". One `count_filtered`
 * call for all nodes, in order, because the saved chips are this window's and
 * the server cannot know them.
 *
 * A failed count leaves the field absent rather than failing the refresh: a
 * wrong badge is a smaller defect than an empty list.
 */
async function withBadgeCounts(projectRecords: Project[]): Promise<Project[]> {
  if (projectRecords.length === 0) return projectRecords;
  const saved = useUiStore.getState().viewFilters;
  try {
    const counts = await api.tasks.countFiltered(
      projectRecords.map((p) => nodeSpec(p.name, saved, projectRecords))
    );
    return projectRecords.map((p, i) =>
      typeof counts[i] === "number" ? { ...p, badge_count: counts[i] } : p
    );
  } catch {
    return projectRecords;
  }
}

/**
 * The state every navigation lands in: a new scope, its own filters, nothing
 * carried over.
 *
 * Chips used to be cleared outright here, because a chip REPLACED the view and
 * a stale one would sit above a list it no longer described. Now that they
 * refine it, "no chips" is not a neutral state — every open view applies
 * `not Done`, and arriving with none would show a year of finished work under
 * a title that says Today. So navigation resets to what this scope means,
 * which is its saved filters, or its baseline when it has none.
 */
function enterScope(
  view: ViewName,
  project: string | null,
  get: () => TaskState
): Partial<TaskState> {
  const { projectRecords } = get();
  return {
    view,
    activeProject: project,
    query: "",
    filters: defaultsFor(
      listView(view),
      project,
      useUiStore.getState().viewFilters,
      projectRecords
    ),
    ...CLEAR_CHECKED(),
  };
}

/**
 * Go to a scope and make sure ONE task is actually on screen there.
 *
 * Every "we took you to it" path — reveal, a capture that lands outside the
 * current list — promises a row the user can see. Saved default filters can
 * break that promise silently: land in an area that remembers "not blocked",
 * reveal a blocked task, and the app scrolls to nothing. So the scope's
 * filters are tried first (they are what you asked that view to mean) and
 * dropped to the baseline only when the row is genuinely missing. Falling
 * straight to the baseline would be worse in the common case: it would throw
 * away a preference every time you clicked a notification.
 */
async function landOn(
  view: ViewName,
  project: string | null,
  taskId: string,
  set: (partial: Partial<TaskState>) => void,
  get: () => TaskState
): Promise<void> {
  set(enterScope(view, project, get));
  await get().refresh();
  if (get().tasks.some((t) => t.id === taskId)) return;
  const baseline = baselineFor(listView(view), project);
  if (sameFilters(get().filters, baseline)) return;
  set({ filters: baseline });
  await get().refresh();
}

/** The selected task stays selected across refreshes when it's still present. */
function keepSelection(tasks: Task[], selectedId: string | null): string | null {
  if (selectedId && tasks.some((t) => t.id === selectedId)) return selectedId;
  return tasks[0]?.id ?? null;
}

export const useTaskStore = create<TaskState>((set, get) => ({
  tasks: [],
  subtasks: [],
  runs: [],
  projects: [],
  projectRecords: [],
  projectRepos: [],
  sections: [],
  board: [],
  boardUpdates: {},
  repos: [],
  allTags: [],
  tags: [],
  tagsByTask: {},
  tripwireByTask: {},
  relations: {
    blocked_by: [],
    blocking: [],
    is_blocked: false,
    supersedes: [],
    superseded_by: [],
    related: [],
    is_superseded: false,
  },
  comments: [],
  attachments: [],
  blockedIds: [],
  counts: EMPTY_COUNTS,
  view: INITIAL_VIEW,
  activeProject: null,
  selectedId: null,
  revealTick: 0,
  checkedIds: [],
  lastCheckedId: null,
  query: "",
  // The launch view's own filters, NOT an empty list. Every navigation resets
  // to a scope's defaults, and the first paint is a navigation too — starting
  // empty meant Today opened with a year of finished work in it, under a
  // header count that disagreed with the sidebar badge beside it.
  filters: defaultsFor(
    listView(INITIAL_VIEW),
    null,
    useUiStore.getState().viewFilters
  ),
  loading: false,
  error: null,
  flashIds: [],
  runActivity: null,
  countdowns: {},
  undoStack: [],
  undoToastKey: null,
  undoNotice: null,
  backgroundIdle: null,
  setBackgroundIdle: (v) => set({ backgroundIdle: v }),

  flash(id) {
    set({ flashIds: [...new Set([...get().flashIds, id])] });
    // Drop it after the animation so a later change can flash the same row again.
    setTimeout(
      () => set({ flashIds: get().flashIds.filter((x) => x !== id) }),
      1600
    );
  },

  async refresh() {
    const { view, activeProject, query, filters, selectedId } = get();
    set({ loading: true, error: null });
    try {
      // ONE query, always (invariant 21). The view, the node, the chips and
      // the text all ride on one `FilterSpec`, because chips REFINE the view
      // now rather than replacing it — and the moment they compose, two query
      // builders would have to agree about every facet forever.
      //
      // `exclude_closed: false` because this caller states its own closure
      // rule as a chip; the server's default-true is for view-less callers.
      const tasks = await api.tasks.filter({
        view: listView(view),
        project: activeProject,
        sort: useUiStore.getState().sort,
        exclude_closed: false,
        ...compileFilters(filters, query),
      });
      const [
        projects,
        counts,
        allTags,
        tagsByTask,
        blockedIds,
        projectRecords,
        sections,
        projectRepos,
        tripwireByTask,
      ] = await Promise.all([
        api.tasks.projects(),
        api.tasks.counts(),
        api.tags.list(),
        api.tags.byTask(),
        api.deps.blockedIds(),
        api.projects.list(),
        api.sections.list(activeProject),
        api.projects.repos.list(),
        api.tripwires.runtime(),
      ]);
      set({
        allTags,
        tagsByTask,
        blockedIds,
        projectRecords: await withBadgeCounts(projectRecords),
        sections,
        projectRepos,
        tripwireByTask,
      });
      // Only the Trash renders a countdown, so only the Trash pays for it.
      set({ countdowns: view === "trash" ? await api.tasks.countdowns() : {} });
      // Same deal: the board is several queries per project, and every other
      // view ignores it.
      set({ board: view === "gsd" ? await api.projects.board() : [] });
      const nextSelected = keepSelection(tasks, selectedId);
      // Prune the checked set to what's actually on screen. Without this a
      // bulk action can fire at a task the list no longer shows — completed
      // out of Today, filtered away by a tag, trashed by the runner — and the
      // user's only evidence of what they hit is the row count in a bar that
      // was already stale when they read it.
      const visible = new Set(tasks.map((t) => t.id));
      const checkedIds = get().checkedIds.filter((id) => visible.has(id));
      const lastCheckedId = get().lastCheckedId;
      set({
        tasks,
        projects,
        counts,
        loading: false,
        checkedIds,
        lastCheckedId:
          lastCheckedId && visible.has(lastCheckedId) ? lastCheckedId : null,
      });
      if (nextSelected !== selectedId) await get().select(nextSelected);
      else if (nextSelected) await get().select(nextSelected);
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  async loadRepos() {
    try {
      set({ repos: await api.repos.suggest() });
    } catch {
      // Autocomplete is a nicety; a missing ~/Projects must not block the app.
    }
  },

  /** Returns an error message on failure (e.g. the repo has another owner). */
  async addProjectRepo(project, repoPath, makePrimary = false) {
    try {
      await api.projects.repos.add(project, repoPath, makePrimary);
      await get().refresh();
      return null;
    } catch (e) {
      return String(e);
    }
  },

  async removeProjectRepo(id) {
    try {
      await api.projects.repos.remove(id);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async setPrimaryProjectRepo(id) {
    try {
      await api.projects.repos.setPrimary(id);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async setView(view) {
    set(enterScope(view, null, get));
    await get().refresh();
  },

  /** Returns an error message on failure (e.g. duplicate name), else null. */
  async createProject(name, kind, parent) {
    try {
      const created = await api.projects.create(name, kind, parent ?? null);
      // The server normalizes the name (trims, collapses spaces); select what
      // it stored, not what was typed, or the row never matches `activeProject`.
      // `setProject` refreshes, which is what brings the new row in.
      await get().setProject(created.name);
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  async setProjectParent(name, parent) {
    return projectMutation(get, () => api.projects.setParent(name, parent));
  },

  async setProjectKind(name, kind) {
    return projectMutation(get, () => api.projects.setKind(name, kind));
  },

  async setProjectTargetDate(name, targetDate) {
    return projectMutation(get, () =>
      api.projects.setTargetDate(name, targetDate)
    );
  },

  async setProjectClosed(name, closed) {
    return projectMutation(get, () => api.projects.setClosed(name, closed));
  },

  async renameProject(from, to) {
    try {
      await api.projects.rename(from, to);
      if (get().activeProject === from) set({ activeProject: to });
      await get().refresh();
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  async setAppearance(kind, name, color, icon) {
    try {
      if (kind === "project") await api.projects.setAppearance(name, color, icon);
      else await api.tags.setAppearance(name, color, icon);
      await get().refresh();
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  async reorderProjects(names) {
    // Optimistic, same as `reorder` for tasks: the drop must not visibly snap
    // back to the old order for the length of a round-trip. `refresh` isn't
    // called on success either — nothing else about the projects changed, and
    // re-reading would only reintroduce the snap it just avoided.
    const byName = new Map(get().projectRecords.map((p) => [p.name, p]));
    const reordered = names.map((n) => byName.get(n)).filter(Boolean) as Project[];
    set({ projectRecords: reordered });
    try {
      await api.projects.reorder(names);
    } catch (e) {
      set({ error: String(e) });
      await get().refresh();
    }
  },

  async deleteProject(name) {
    try {
      await api.projects.remove(name, false);
      // "open" not "all": this is a landing choice, not a visibility
      // guarantee — the sidebar has a row for Open, and none for All since
      // the actionable split.
      if (get().activeProject === name) set({ activeProject: null, view: "open" });
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async createSection(name) {
    try {
      await api.sections.create(get().activeProject, name);
      await get().refresh();
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  async renameSection(id, name) {
    try {
      await api.sections.rename(id, name);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async deleteSection(id) {
    try {
      await api.sections.remove(id);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async reorderSections(ids) {
    const { sections: previous, activeProject } = get();
    const byId = new Map(previous.map((s) => [s.id, s]));
    // A reorder carries the whole scope; never hide sections on a stale drop.
    if (
      ids.length !== previous.length ||
      new Set(ids).size !== ids.length ||
      ids.some((id) => !byId.has(id))
    ) return;
    if (ids.every((id, i) => previous[i]?.id === id)) return;
    const reordered = ids.map((id, sort_order) => ({ ...byId.get(id)!, sort_order }));
    set({ sections: reordered });
    try {
      await api.sections.reorder(activeProject, ids);
      get().pushUndo({
        label: "Sections",
        verb: "reordered",
        ids: [],
        run: async () => {
          await api.sections.reorder(activeProject, previous.map((s) => s.id));
          await get().refresh();
        },
      });
    } catch (e) {
      // A failed request from a previous view must not restore its sections
      // into the project the user has navigated to since.
      set({
        error: String(e),
        ...(get().sections === reordered ? { sections: previous } : {}),
      });
      await get().refresh();
    }
  },

  async moveTaskToSection(taskId, sectionId) {
    const before = findLoaded(get, taskId);
    try {
      await api.sections.moveTask(taskId, sectionId);
      await get().refresh();
      if (before && before.section_id !== sectionId) {
        get().pushUndo({
          label: before.title,
          verb: "regrouped",
          ids: [taskId],
          run: async () => {
            await api.sections.moveTask(taskId, before.section_id);
            await get().refresh();
          },
        });
      }
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async moveTasksToSection(ids, sectionId) {
    // Per-row before-states, for `bulkPriority`'s reason inverted: a set dragged
    // out of three different sections has three different origins, and filing
    // them all back into whichever one came first would look like the undo
    // worked.
    const before = new Map(get().tasks.map((t) => [t.id, t.section_id]));
    // Only the rows that actually change. A set dropped onto a group that
    // already holds some of them would otherwise re-file those too — a write
    // that changes nothing, bumps `updated_at`, and resets every draft in the
    // detail pane for it (gotcha #24).
    const moving = ids.filter((id) => before.get(id) !== sectionId);
    if (moving.length === 0) return;
    const label = bulkLabel(get, moving);
    await runOnMany(get, set, moving, (id) => api.sections.moveTask(id, sectionId));
    get().pushUndo({
      label,
      verb: "regrouped",
      ids: moving,
      run: async () => {
        for (const id of moving) {
          await api.sections.moveTask(id, before.get(id) ?? null);
        }
        await get().refresh();
      },
    });
  },

  async setTasksProject(ids, project) {
    const before = new Map(get().tasks.map((t) => [t.id, t]));
    const moving = ids.filter((id) => before.get(id)?.project !== project);
    if (moving.length === 0) return;
    const label = bulkLabel(get, moving);
    // Diffed against what the server RETURNED, never against the patch we sent:
    // moving a task between projects also re-points `repo_path` (invariant 17's
    // prefill), and an inverse blind to that restores the project name while
    // leaving the workspace pointing into the project you just left. Same trap
    // `patch` documents; this is the multi-row copy of it.
    const inverses = new Map<string, TaskPatch>();
    await runOnMany(get, set, moving, async (id) => {
      const after = await api.tasks.update(id, { project });
      const prior = before.get(id);
      const inverse = prior ? inversePatch(prior, after) : null;
      if (inverse) inverses.set(id, inverse);
    });
    if (inverses.size === 0) return;
    get().pushUndo({
      label,
      verb: describePatch({ project }),
      ids: [...inverses.keys()],
      run: async () => {
        for (const [id, inverse] of inverses) await api.tasks.update(id, inverse);
        await get().refresh();
      },
    });
  },

  async postProjectUpdate(project, body, health) {
    try {
      await api.projects.postUpdate(project, body, health);
      // Refresh the log too, or the composer clears and the entry it just
      // wrote doesn't appear until the row is collapsed and reopened.
      await get().loadProjectUpdates(project);
      await get().refresh();
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  async loadProjectUpdates(project) {
    try {
      const updates = await api.projects.updates(project);
      set({ boardUpdates: { ...get().boardUpdates, [project]: updates } });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async setSectionTargetDate(id, targetDate) {
    try {
      await api.sections.setTargetDate(id, targetDate);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async setSectionCompleted(id, done) {
    try {
      await api.sections.setCompleted(id, done);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  /** Returns an error message on failure, else null. Renaming onto an
   * existing tag merges the two (backend behaviour) rather than failing. */
  async renameTag(from, to) {
    try {
      await api.tags.rename(from, to);
      // The chip carries the tag NAME, so a rename has to move it or the
      // strip goes on filtering by a tag that no longer exists.
      set({
        filters: get().filters.map((f) =>
          f.facet === "tag" && f.value === from ? { ...f, value: to } : f
        ),
      });
      await get().refresh();
      return null;
    } catch (e) {
      return String(e).replace(/^Invalid input: /, "");
    }
  },

  /** Delete a tag from every task carrying it.
   *
   * Only ever reached for a tag that IS in use — an unused one is collected by
   * the backend and never appears in the sidebar to be right-clicked. The
   * confirm naming the task count is raised by the caller, before this runs.
   *
   * No undo entry, matching `deleteProject`: the undo stack is task-patch
   * shaped and this spans an arbitrary set of tasks the caller never
   * enumerated — the same reason `empty_trash` clears the stack outright. */
  async deleteTag(name) {
    try {
      await api.tags.remove(name);
      // Drop it from the filter first, or the board stays narrowed by a tag
      // that no longer exists and reads as an empty view with no cause.
      set({ filters: get().filters.filter((f) => !(f.facet === "tag" && f.value === name)) });
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  /** Option-click: add or remove ONE tag chip, leaving the rest of the strip
   * alone. A tag filter is a chip like any other now, so this is the same
   * gesture as picking one from the filter strip's Tag pane. */
  async toggleTagFilter(name) {
    const chip: ActiveFilter = { facet: "tag", op: "is", value: name };
    const on = activeTagsOf(get().filters).includes(name);
    if (on) await get().removeFilter(chip);
    else await get().addFilter(chip);
  },

  /** Default (non-multi) tag click: select just this tag, or clear the
   * filter if it's already the sole selection. `toggleTagFilter` (Option-
   * click) stays available for building an AND filter across several tags. */
  async selectOnlyTag(name) {
    const { view, activeProject, filters } = get();
    const current = activeTagsOf(filters);
    const isOnlySelected = current.length === 1 && current[0] === name;
    // Back to the scope's own filters, then this one tag on top — so clicking
    // a tag doesn't quietly drop the view's `not Done` along with the other
    // tags it is replacing.
    const base = filters.filter((f) => !(f.facet === "tag" && f.op === "is"));
    set({
      filters: isOnlySelected ? base : [...base, { facet: "tag", op: "is", value: name }],
      query: "",
      ...CLEAR_CHECKED(),
    });
    void view;
    void activeProject;
    await get().refresh();
  },

  async setTaskTags(id, tags) {
    // `tags` in the store is the SELECTED task's set, which is this task's
    // whenever the editor is open — and the editor is the only caller.
    const before = get().selectedId === id ? get().tags.map((t) => t.name) : null;
    try {
      await api.tags.setForTask(id, tags);
      await get().refresh();
      await get().select(id);
      if (before) {
        get().pushUndo({
          label: findLoaded(get, id)?.title ?? "Task",
          verb: "re-tagged",
          ids: [id],
          run: async () => {
            await api.tags.setForTask(id, before);
            await get().refresh();
            await get().select(id);
          },
        });
      }
    } catch (e) {
      set({ error: String(e) });
    }
  },

  /** Returns an error message on failure (e.g. a cycle), else null. */
  async addDependency(id, blockedById) {
    try {
      await api.deps.add(id, blockedById);
      await get().refresh();
      await get().select(id);
      return null;
    } catch (e) {
      return String(e);
    }
  },

  async removeDependency(id, blockedById) {
    try {
      await api.deps.remove(id, blockedById);
      await get().refresh();
      await get().select(id);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async addRelation(id, otherId, kind) {
    try {
      await api.deps.relate(id, otherId, kind);
      await get().refresh();
      await get().select(id);
      return null;
    } catch (e) {
      return String(e);
    }
  },

  async removeRelation(id, otherId, kind) {
    try {
      await api.deps.unrelate(id, otherId, kind);
      await get().refresh();
      await get().select(id);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async addComment(id, body, imagePaths = [], clipboardImages = []) {
    if (!body.trim() && imagePaths.length === 0 && clipboardImages.length === 0) return false;
    try {
      if (imagePaths.length > 0 || clipboardImages.length > 0) {
        await api.comments.addWithAttachments(id, body, imagePaths, clipboardImages);
      } else {
        await api.comments.add(id, body);
      }
      await get().select(id);
      return true;
    } catch (e) {
      set({ error: String(e) });
      return false;
    }
  },

  async removeComment(commentId) {
    const id = get().selectedId;
    try {
      await api.comments.remove(commentId);
      if (id) await get().select(id);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async pasteNoteImage(id, quiet = false) {
    try {
      const images = await api.attachments.paste(id, quiet);
      if (images.length === 0) return false;
      // Do not switch back if the user changed tasks while importing.
      if (get().selectedId === id) await get().select(id);
      return true;
    } catch (e) {
      set({ error: String(e) });
      return false;
    }
  },

  async addNoteImages(id, imagePaths) {
    if (imagePaths.length === 0) return false;
    try {
      await api.attachments.add(id, null, imagePaths);
      await get().select(id);
      return true;
    } catch (e) {
      set({ error: String(e) });
      return false;
    }
  },

  async removeAttachment(attachmentId) {
    const id = get().selectedId;
    try {
      await api.attachments.remove(attachmentId);
      if (id) await get().select(id);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async setProject(project) {
    set(enterScope("all", project, get));
    await get().refresh();
  },

  async setQuery(query) {
    set({ query, ...CLEAR_CHECKED() });
    await get().refresh();
  },

  async addFilter(f) {
    const next = upsertFilter(get().filters, f);
    if (next === get().filters) return; // exact duplicate — nothing to do
    set({ filters: next, ...CLEAR_CHECKED() });
    await get().refresh();
  },

  async setFilters(filters) {
    set({ filters: filters.map((f) => ({ ...f })), ...CLEAR_CHECKED() });
    await get().refresh();
  },

  async removeFilter(f) {
    set({
      filters: get().filters.filter(
        (x) => !(x.facet === f.facet && x.op === f.op && x.value === f.value)
      ),
      ...CLEAR_CHECKED(),
    });
    await get().refresh();
  },

  async clearSearch() {
    const { view, activeProject, query, filters } = get();
    const defaults = defaultsFor(
      listView(view),
      activeProject,
      useUiStore.getState().viewFilters,
      get().projectRecords
    );
    // Back to the scope's own filters, not to none: an empty strip is not the
    // neutral state — every open view applies `not Done`, so clearing to
    // nothing would answer "stop filtering" with a year of finished work.
    if (!query && sameFilters(filters, defaults)) return;
    set({ query: "", filters: defaults, ...CLEAR_CHECKED() });
    await get().refresh();
  },

  async select(id) {
    set({ selectedId: id });
    if (!id) {
      set({
        subtasks: [],
        runs: [],
        tags: [],
        comments: [],
        attachments: [],
        relations: {
          blocked_by: [],
          blocking: [],
          is_blocked: false,
          supersedes: [],
          superseded_by: [],
          related: [],
          is_superseded: false,
        },
      });
      return;
    }
    try {
      const [subtasks, runs, tags, relations, comments, attachments] = await Promise.all([
        api.tasks.subtasks(id),
        api.runs.list(id),
        api.tags.forTask(id),
        api.deps.forTask(id),
        api.comments.list(id),
        api.attachments.list(id),
      ]);
      // Guard against an out-of-order response overwriting a newer selection.
      if (get().selectedId === id)
        set({ subtasks, runs, tags, relations, comments, attachments });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async reveal(id) {
    // Prefer the loaded row, fall back to a direct fetch. `get` looks past the
    // `LIVE` filter on purpose (restore depends on it), which is exactly what
    // makes a trashed or archived task revealable at all.
    const lookup = async (taskId: string): Promise<Task | null> =>
      get().tasks.find((t) => t.id === taskId) ??
      (await api.tasks.get(taskId).catch(() => null));

    let target = await lookup(id);
    // Hard-deleted, or an id from a stale notification. Nothing to show, and
    // guessing something to show instead would be worse than doing nothing.
    if (!target) return;

    // A subtask is never a top-level row: `list_tasks` leaves subtasks out
    // (`include_subtasks` defaults false) because they render nested in the
    // parent's pane. `list_due_for_reminder` has no such filter, though, so a
    // subtask with a due date really does fire a banner — and selecting it
    // would render nothing at all in any view. Reveal the parent instead;
    // that's the row that exists, and the subtask is listed inside it.
    if (target.parent_id) {
      target = (await lookup(target.parent_id)) ?? target;
    }

    if (!get().tasks.some((t) => t.id === target!.id)) {
      // Trash before Archive, matching the backend: something both archived
      // and trashed is *in the Trash*, on the purge clock. `all` is the
      // catch-all for anything open — it is the only view with no date or
      // status narrowing beyond "not closed".
      const home: ViewName = target.deleted_at
        ? "trash"
        : target.archived_at
          ? "archive"
          : isClosed(target.status)
            ? "done"
            : "all";
      // Entering a scope resets the project, the query and the chips to what
      // that scope means — and `landOn` drops even those to the baseline if
      // they would hide the very task we are revealing.
      await landOn(home, null, target.id, set, get);
    }

    await get().select(target.id);
    useUiStore.getState().showDetail();
    get().flash(target.id);
    set({ revealTick: get().revealTick + 1 });
  },

  toggleChecked(id) {
    const checked = get().checkedIds;
    const on = checked.includes(id);
    set({
      checkedIds: on ? checked.filter((x) => x !== id) : [...checked, id],
      // The anchor follows the last plain click even when that click was an
      // *un*tick — shift-clicking after untying a row should range from there,
      // not from wherever the previous tick happened to be.
      lastCheckedId: id,
    });
  },

  checkRange(id, order) {
    const anchor = get().lastCheckedId;
    const from = anchor ? order.indexOf(anchor) : -1;
    const to = order.indexOf(id);
    // No anchor, or either end no longer rendered: a shift-click with nothing
    // to range from is just a click. Falling back to "select everything" here
    // would be a very expensive misread of an ambiguous gesture.
    if (from === -1 || to === -1) {
      get().toggleChecked(id);
      return;
    }
    const [lo, hi] = from <= to ? [from, to] : [to, from];
    const span = order.slice(lo, hi + 1);
    // Additive union, never a replacement: shift-click extends a selection in
    // every list that has one, and ranges built in two passes have to survive
    // the second pass.
    set({
      checkedIds: [...new Set([...get().checkedIds, ...span])],
      lastCheckedId: id,
    });
  },

  rowClick(id, mods, order) {
    // A plain click is a fresh selection of one, exactly as in Finder. It also
    // sets the anchor, so the very next ⇧-click ranges from the row you just
    // clicked rather than from whatever was ticked several gestures ago.
    if (!mods.meta && !mods.shift) {
      // Captured BEFORE `select` runs, because `select` is precisely what makes
      // the answer yes. The question is "was I already reading this row" — both
      // halves of it, since a click on the selected row while the pane is shut
      // has to OPEN rather than toggle back to shut.
      const wasReading =
        get().selectedId === id && !useUiStore.getState().detailCollapsed;
      set({ checkedIds: [], lastCheckedId: id });
      void get().select(id);
      // Opening a task IS the point of clicking one, so a collapsed pane gets
      // out of the way rather than making you reach for ⌘\ first — and clicking
      // the row you are already reading puts it back. One gesture, both
      // directions, which is what makes the pane feel like part of the row
      // instead of a separate thing to manage.
      //
      // Deliberately only on the PLAIN branch. ⌘- and ⇧-click build a set, and
      // a pane that sprang open on the first row of a six-row selection would
      // take 300px away from the list you are still picking from.
      //
      // `toggleOpen` is what keeps this to a real CLICK. `TaskContextMenu`'s
      // `onOpen` routes through here too — it takes the selection so the menu
      // acts on the row you right-clicked (gotcha #9) — and without the flag,
      // right-clicking the row you are reading would close the pane underneath
      // the menu you just opened. `showDetail` is idempotent, so the menu path
      // still lands on "open" and never fights it.
      const ui = useUiStore.getState();
      if (mods.toggleOpen && wasReading) ui.toggleDetail();
      else ui.showDetail();
      return;
    }

    // Seed from the focused row. Without this the first ⌘-click starts from an
    // empty set, so clicking row A then ⌘-clicking row B selects only B — the
    // row already highlighted quietly drops out of a gesture whose entire
    // meaning is "and this one too".
    const { checkedIds, selectedId, lastCheckedId } = get();
    if (checkedIds.length === 0 && selectedId && selectedId !== id) {
      set({ checkedIds: [selectedId], lastCheckedId: lastCheckedId ?? selectedId });
    }

    if (mods.shift) get().checkRange(id, order);
    else get().toggleChecked(id);
    // The detail pane follows the last row touched, the way every multi-select
    // list on this machine does. The set is still the thing the bulk bar acts
    // on — this only decides which one of them is being read.
    void get().select(id);
  },

  clearChecked() {
    set(CLEAR_CHECKED());
  },

  async bulkComplete() {
    const ids = get().checkedIds;
    if (ids.length === 0) return;
    const label = bulkLabel(get, ids);
    // Snapshot before the sweep: `refresh` inside `runOnMany` drops the rows
    // out of the list, taking the statuses the inverse needs with them.
    const before = new Map(get().tasks.map((t) => [t.id, t.status]));
    const spawned: string[] = [];
    await runOnMany(get, set, ids, async (id) => {
      const result = await api.tasks.complete(id);
      if (result.next) spawned.push(result.next.id);
    });
    get().pushUndo({
      label,
      verb: "completed",
      ids: [...ids, ...spawned],
      run: async () => {
        for (const id of spawned) await api.tasks.trash(id);
        for (const id of ids) {
          await api.tasks.update(id, { status: before.get(id) ?? "todo" });
        }
        await get().refresh();
      },
    });
  },

  async bulkPriority(priority) {
    const ids = get().checkedIds;
    if (ids.length === 0) return;
    const label = bulkLabel(get, ids);
    const before = new Map(get().tasks.map((t) => [t.id, t.priority]));
    await runOnMany(get, set, ids, (id) => api.tasks.update(id, { priority }));
    get().pushUndo({
      label,
      verb: `set to P${priority}`,
      ids,
      run: async () => {
        // Per-row, not one shared value: a bulk priority is the one action
        // where every row's previous state genuinely differs, and restoring
        // them all to the first one's would look like it worked.
        for (const id of ids) {
          await api.tasks.update(id, { priority: before.get(id) ?? PRIORITY_LOWEST });
        }
        await get().refresh();
      },
    });
  },

  async bulkTrash() {
    const ids = get().checkedIds;
    if (ids.length === 0) return;
    // Capture the label BEFORE the refresh drops the rows, same reason as the
    // single-task path: the toast has to name what went.
    const label = bulkLabel(get, ids);
    await runOnMany(get, set, ids, (id) => api.tasks.trash(id), {
      // Selection is gone the moment the rows are, so the detail pane can't be
      // left showing one of them.
      afterEach: (id) => {
        if (get().selectedId === id) set({ selectedId: null });
      },
    });
    get().pushUndo({
      label,
      verb: "moved to Trash",
      ids,
      run: async () => {
        // Sequential and independent: each id was trashed by its own call and
        // carries its own batch key, so each restore brings back exactly the
        // subtree that went with it (invariant 13).
        for (const id of ids) await api.tasks.restore(id);
        await get().refresh();
        if (ids[0]) await get().select(ids[0]);
      },
    });
  },

  async bulkRestore() {
    const ids = get().checkedIds;
    if (ids.length === 0) return;
    const hidden = get().view;
    const label = bulkLabel(get, ids);
    await runOnMany(get, set, ids, (id) =>
      hidden === "trash" ? api.tasks.restore(id) : api.tasks.unarchive(id)
    );
    get().pushUndo({
      label,
      verb: "put back",
      ids,
      run: async () => {
        for (const id of ids) {
          await (hidden === "trash" ? api.tasks.trash(id) : api.tasks.archive(id));
        }
        await get().refresh();
      },
    });
  },

  async bulkDestroy() {
    await runOnMany(get, set, get().checkedIds, (id) => api.tasks.destroy(id), {
      afterEach: (id) => {
        if (get().selectedId === id) set({ selectedId: null });
        // An undo pointing at rows that no longer exist would fail silently.
        dropUndoFor(get, set, [id]);
      },
    });
  },

  async quickAdd(input, suggestion) {
    // Capture is contextual: from a project, a task belongs to that project
    // unless the user explicitly types another #project. Filing it into Inbox
    // while immediately refreshing the project made a successful create look
    // exactly like data loss.
    const parsed = parseQuickAdd(input, get().activeProject);
    if (!parsed) return null;
    try {
      const explicitProject = /(^|\s)#[^\s]+/.test(input) || get().activeProject !== null;
      const explicitPriority = /(^|\s)![0-3](?=\s|$)/.test(input);
      const { tags: parsedTags, ...parsedFields } = parsed;
      const tags = Array.from(new Set([...parsedTags, ...(suggestion?.tags ?? [])]));
      const fields = {
        ...parsedFields,
        ...(!explicitProject && suggestion?.project ? { project: suggestion.project } : {}),
        ...(!explicitPriority && suggestion ? { priority: suggestion.priority } : {}),
        ...(suggestion ? { status: suggestion.status } : {}),
      };
      const task = await api.tasks.create(fields);
      // People typed as `@name` become tags — a second write, because tags
      // are many-to-many and `create_task` takes the row alone.
      if (tags.length > 0) await api.tags.setForTask(task.id, tags);
      await get().refresh();
      // A modifier can deliberately send the task somewhere other than the
      // current list (and capture is also available from views such as Today).
      // In that case, take the user to the destination instead of claiming the
      // details opened while the new row is still invisible.
      if (!get().tasks.some((candidate) => candidate.id === task.id)) {
        // `all` rather than `inbox`, because Inbox is now every UNSCHEDULED
        // task — so a project-less capture carrying an `@today` token isn't in
        // it, and sending the user there would reproduce the exact
        // looks-like-data-loss bug this block exists to prevent. `all` is the
        // only view that holds any open task whatever its date.
        await landOn("all", task.project ?? null, task.id, set, get);
      }
      await get().select(task.id);
      get().pushUndo({
        label: task.title,
        verb: "added",
        ids: [task.id],
        // Trash rather than destroy: undoing a capture must not be the one
        // gesture in the app that permanently loses text you just typed.
        run: async () => {
          await api.tasks.trash(task.id);
          await get().refresh();
        },
      });
      return task;
    } catch (e) {
      set({ error: String(e) });
      return null;
    }
  },

  async patch(id, patch) {
    const before = findLoaded(get, id);
    try {
      const after = await api.tasks.update(id, patch);
      // `after`, not `patch` — the server writes fields the caller never sent,
      // and only a before/after diff sees them. See `inversePatch`.
      const inverse = before ? inversePatch(before, after) : null;
      // No inverse means nothing measurably changed — a detail-pane field
      // committed on blur with the value it already had. Recording those would
      // fill the stack with entries whose undo is a no-op, so ⌘Z would appear
      // to do nothing several times before reaching the edit you meant.
      if (inverse) {
        get().pushUndo({
          label: after.title,
          verb: describePatch(patch),
          ids: [id],
          run: async () => {
            await api.tasks.update(id, inverse);
            await get().refresh();
          },
        });
      }
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async complete(id, answer) {
    const before = findLoaded(get, id);
    try {
      // `answer` is the Decide strip's one line. It rides `complete_task`'s
      // `result`, which is the same handoff a session leaves (invariant 18):
      // full text onto a run row, a derived line into the comment log — so
      // the call Chris made is recorded where the next session will read it.
      const result = await api.tasks.complete(id, answer);
      const previous = before?.status ?? "todo";
      get().pushUndo({
        label: result.completed.title,
        verb: "completed",
        ids: [id, ...(result.next ? [result.next.id] : [])],
        run: async () => {
          // Completing a recurring task also MATERIALIZES its next instance,
          // so putting the completion back has to take that instance away —
          // otherwise every undo of a recurring task leaves a duplicate behind
          // and the list grows by one each time.
          if (result.next) await api.tasks.trash(result.next.id);
          await api.tasks.update(id, { status: previous });
          await get().refresh();
          await get().select(id);
        },
      });
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async trash(id) {
    // Capture the title BEFORE the refresh drops the row, so the toast can name
    // what just went ("Weekly repo sweep moved to Trash" beats "Task deleted"
    // when you're trying to work out whether you hit the wrong thing).
    const title = findLoaded(get, id)?.title ?? "Task";
    try {
      await api.tasks.trash(id);
      if (get().selectedId === id) set({ selectedId: null });
      await get().refresh();
      get().pushUndo({
        label: title,
        verb: "moved to Trash",
        ids: [id],
        run: async () => {
          await api.tasks.restore(id);
          await get().refresh();
          await get().select(id);
        },
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  pushUndo(entry) {
    // Suppressed while an inverse is running. Without this the undo's own
    // `restore`/`update` calls would push an entry of their own, and a second
    // ⌘Z would redo the thing the first one just took back — which is not what
    // ⌘Z means anywhere, and is indistinguishable from the undo having failed.
    if (undoSuppressed) return;
    const armed: UndoEntry = { ...entry, key: ++undoKeySeq };
    set({
      undoStack: [...get().undoStack, armed].slice(-UNDO_DEPTH),
      undoToastKey: armed.key,
      undoNotice: null,
    });
    setTimeout(() => {
      // Only ever clear the toast this call armed. A later action replaces it,
      // and without the identity check this timer would cut that newer toast
      // short.
      if (get().undoToastKey === armed.key) set({ undoToastKey: null });
    }, UNDO_TOAST_MS);
  },

  async undoLast() {
    const stack = get().undoStack;
    const entry = stack[stack.length - 1];
    if (!entry) return;
    // Popped before it runs, so a failing inverse can't be retried forever by
    // hammering ⌘Z against whatever is wrong.
    set({ undoStack: stack.slice(0, -1), undoToastKey: null });
    undoSuppressed = true;
    try {
      await entry.run();
      showNotice(set, `Undone — ${entry.label} ${entry.verb}`);
    } catch (e) {
      set({ error: String(e) });
    } finally {
      undoSuppressed = false;
    }
  },

  dismissUndo() {
    set({ undoToastKey: null });
  },

  async restore(id) {
    try {
      await api.tasks.restore(id);
      await get().refresh();
      get().pushUndo({
        label: findLoaded(get, id)?.title ?? "Task",
        verb: "put back",
        ids: [id],
        run: async () => {
          await api.tasks.trash(id);
          await get().refresh();
        },
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async archive(id) {
    const title = findLoaded(get, id)?.title ?? "Task";
    try {
      await api.tasks.archive(id);
      if (get().selectedId === id) set({ selectedId: null });
      await get().refresh();
      get().pushUndo({
        label: title,
        verb: "archived",
        ids: [id],
        run: async () => {
          await api.tasks.unarchive(id);
          await get().refresh();
          await get().select(id);
        },
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async unarchive(id) {
    try {
      await api.tasks.unarchive(id);
      await get().refresh();
      get().pushUndo({
        label: findLoaded(get, id)?.title ?? "Task",
        verb: "put back",
        ids: [id],
        run: async () => {
          await api.tasks.archive(id);
          await get().refresh();
        },
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  /** Irreversible — callers must confirm first. */
  async destroy(id) {
    try {
      await api.tasks.destroy(id);
      if (get().selectedId === id) set({ selectedId: null });
      // An undo pointing at a row that no longer exists would fail silently.
      dropUndoFor(get, set, [id]);
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async emptyTrash() {
    try {
      await api.tasks.emptyTrash();
      // The whole stack, not the entries naming trashed rows — this destroys an
      // unbounded set the caller never enumerated, so there is no honest way to
      // say which entries survived. Clearing all of them costs an undo that
      // might still have worked; keeping any of them offers one that silently
      // won't.
      set({ selectedId: null, undoStack: [], undoToastKey: null });
      await get().refresh();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async reorder(ids) {
    // Optimistic: reorder locally first so the drop doesn't visibly snap back.
    const previous = get().tasks.map((t) => t.id);
    const byId = new Map(get().tasks.map((t) => [t.id, t]));
    const reordered = ids.map((id) => byId.get(id)).filter(Boolean) as Task[];
    set({ tasks: reordered });
    try {
      await api.tasks.reorder(ids);
      get().pushUndo({
        label: byId.get(ids[0] ?? "")?.title ?? "Task",
        verb: "reordered",
        ids,
        run: async () => {
          await api.tasks.reorder(previous);
          await get().refresh();
        },
      });
    } catch (e) {
      set({ error: String(e) });
      await get().refresh();
    }
  },

  async addSubtask(parentId, title) {
    if (!title.trim()) return;
    try {
      const created = await api.tasks.create({ title, parent_id: parentId });
      await get().select(parentId);
      get().pushUndo({
        label: created.title,
        verb: "added",
        ids: [created.id],
        run: async () => {
          await api.tasks.trash(created.id);
          await get().select(parentId);
          await get().refresh();
        },
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));

// ── multi-select helpers ───────────────────────────────────────────────────

type StoreGet = () => TaskState;
type StoreSet = (partial: Partial<TaskState>) => void;

/**
 * Run a per-task IPC call across a set of tasks, then refresh ONCE.
 *
 * Sequential rather than `Promise.all`: the three binaries share one SQLite
 * file behind a mutex, so twenty concurrent writes buy lock contention and
 * nothing else. It also means a failure stops the sweep with the remainder
 * untouched instead of scattering half of it.
 *
 * There is deliberately NO bulk command behind this. Every action calls the
 * same command the single-task path already uses, so recurrence, subtree
 * stamping and batch keys behave identically whether you acted on one task or
 * nine — a bulk endpoint would be a second implementation of invariant 13's
 * rules, sitting exactly where it would be least likely to be kept in step.
 *
 * `ids` is a PARAMETER rather than `get().checkedIds` read in here, because the
 * checked set is not the only set the app acts on: a drag carries whatever the
 * grabbed row carries, which is the checked set when the row is part of it and
 * that row alone when it isn't. Reading the store here would have silently made
 * every drag act on the checkboxes instead — moving a set nobody dragged, or
 * nothing at all.
 */
async function runOnMany(
  get: StoreGet,
  set: StoreSet,
  ids: string[],
  call: (id: string) => Promise<unknown>,
  opts: { afterEach?: (id: string) => void } = {}
): Promise<void> {
  if (ids.length === 0) return;
  try {
    for (const id of ids) {
      await call(id);
      opts.afterEach?.(id);
    }
  } catch (e) {
    set({ error: String(e) });
  }
  // The set this action CONSUMED leaves the selection — everything else stays
  // exactly as the user built it. For every bulk-bar verb that is the whole
  // checked set and the outcome is unchanged, but a drag of an UNCHECKED row
  // acts on one task that was never in the set, and clearing there would throw
  // away a selection the gesture never touched.
  //
  // Subtracting `ids` rather than the ids that actually landed is deliberate:
  // on a failure part-way through, the remainder is still gone. A half-applied
  // sweep that left the rest ticked would sit there looking like a selection
  // the user made, one click away from being run a second time.
  const { checkedIds, lastCheckedId } = get();
  const consumed = new Set(ids);
  const remaining = checkedIds.filter((id) => !consumed.has(id));
  set(
    remaining.length === 0
      ? CLEAR_CHECKED()
      : {
          checkedIds: remaining,
          // A ⇧-anchor that was itself acted on can't survive: the next range
          // would be measured from a row that has just moved or gone.
          lastCheckedId:
            lastCheckedId && consumed.has(lastCheckedId) ? null : lastCheckedId,
        }
  );
  await get().refresh();
}

/** How deep ⌘Z reaches. Deliberately finite — every entry pins the closure it
 * captured, and an unbounded history of a long session would keep a snapshot of
 * every task ever touched alive for no benefit past the first few. */
const UNDO_DEPTH = 25;
/** How long the toast offers itself. The ENTRY outlives this; only the toast
 * goes. */
const UNDO_TOAST_MS = 8000;
/** How long the post-⌘Z confirmation stays up. Short: it reports something that
 * already happened and needs no decision. */
const NOTICE_MS = 2600;

let undoKeySeq = 0;
let noticeSeq = 0;
/** True while an inverse is running — see `pushUndo`. Module-level rather than
 * state because it must be readable synchronously by every action the inverse
 * calls, and nothing renders from it. */
let undoSuppressed = false;

/** Name a batch: the task itself when it's one, a count when it's several. */
function bulkLabel(get: StoreGet, ids: string[]): string {
  if (ids.length !== 1) return `${ids.length} tasks`;
  return get().tasks.find((t) => t.id === ids[0])?.title ?? "Task";
}

/** The loaded row for `id`, from either list. Subtasks never appear in `tasks`
 * — they render inside the parent's pane — so a detail-pane action on one would
 * otherwise capture nothing and record no undo at all. */
function findLoaded(get: StoreGet, id: string): Task | null {
  return (
    get().tasks.find((t) => t.id === id) ??
    get().subtasks.find((t) => t.id === id) ??
    null
  );
}

/** Drop every entry that depends on a hard-deleted row, and the toast with it
 * if that's what it was offering. Running such an entry would fail against a
 * row that no longer exists, which reads as "undo is broken" rather than as
 * "that task is gone for good". */
function dropUndoFor(get: StoreGet, set: StoreSet, ids: string[]): void {
  const gone = new Set(ids);
  const stack = get().undoStack;
  const kept = stack.filter((e) => !e.ids.some((id) => gone.has(id)));
  if (kept.length === stack.length) return;
  const toastKey = get().undoToastKey;
  set({
    undoStack: kept,
    undoToastKey: kept.some((e) => e.key === toastKey) ? toastKey : null,
  });
}

/** Flash a transient line, cancelling any earlier one still counting down. */
function showNotice(set: StoreSet, text: string): void {
  const token = ++noticeSeq;
  set({ undoNotice: text });
  setTimeout(() => {
    if (noticeSeq === token) set({ undoNotice: null });
  }, NOTICE_MS);
}

/**
 * Every field a `TaskPatch` can carry — and therefore everything an undo is
 * allowed to put back.
 *
 * `satisfies` is doing real work: it fails to compile if a name here is not on
 * BOTH `Task` and `TaskPatch`. The reverse direction — a patchable field
 * *missing* from this list — is caught by the assertion below.
 *
 * Deliberately excludes the read-only fields, `updated_at` above all: it moves
 * on EVERY write, so diffing it would make every patch look like a change and
 * fill the stack with entries whose undo does nothing.
 */
const UNDOABLE_FIELDS = [
  "title",
  "body",
  "status",
  "priority",
  "project",
  "due_at",
  "defer_until",
  "recurrence",
  "sort_order",
  "repo_path",
  "prompt",
  "bg_allowed",
  "bg_executor",
  "bg_writes_allowed",
  "bg_shell_allowed",
  "bg_remote_allowed",
  "bg_mcp_servers",
  "model",
  "section_id",
  "blocked_reason",
  "bg_intent",
  // The three tripwire FIELDS. Restored together, and the server resolves
  // them jointly, so undoing a cleared tripwire puts spec, cadence and policy
  // back in one write and re-arms it.
  "tripwire",
  "tripwire_every_minutes",
  "tripwire_on_trip",
] as const satisfies readonly (keyof TaskPatch & keyof Task)[];

/**
 * `TaskPatch` keys that are ACTIONS rather than fields, and so have no
 * before-state to put back. Named here explicitly — an omission from
 * `UNDOABLE_FIELDS` must be a decision with a reason, never a forgotten key.
 *
 * - `tripwire_check_requested` stamps a request the runner consumes; there is
 *   nothing on the task to restore.
 * - `tripwire_state` accepts exactly one value (`"armed"`, a re-arm) and the
 *   server refuses the others, so an inverse of `tripped → armed` could not be
 *   written. A re-arm is not undoable; the task simply trips again.
 */
type PatchActions = "tripwire_check_requested" | "tripwire_state";

/**
 * Compile-time proof that the list above covers **every** `TaskPatch` key.
 *
 * `satisfies` only checks the keys that ARE listed; it cannot notice one that
 * is missing, and a missing key is silent — that field simply stops being
 * undoable, with every test still green. This errors instead, naming the key.
 */
type MissingFromUndoable = Exclude<
  keyof TaskPatch,
  (typeof UNDOABLE_FIELDS)[number] | PatchActions
>;
const _undoableCoversEveryPatchField: [MissingFromUndoable] extends [never]
  ? true
  : ["UNDOABLE_FIELDS is missing", MissingFromUndoable] = true;
void _undoableCoversEveryPatchField;

/**
 * The patch that puts `before` back — derived by diffing the task **as it came
 * back from the server**, not by walking the keys the caller sent.
 *
 * **That distinction is the whole point.** `db::tasks::update` writes fields
 * the caller never mentioned, and an inverse built from the applied patch is
 * blind to every one of them:
 *
 * - Changing `project` rewrites `repo_path` (invariant 17's prefill). Undoing
 *   from the applied keys restored the project name and left the workspace
 *   pointing into the project you just left — which is the stale-workspace
 *   dispatch footgun that prefill exists to PREVENT, recreated by the undo,
 *   with the launch console showing the wrong repo as if you had chosen it.
 * - Switching `bg_executor` clears `model` (invariant 15).
 *
 * `reminded_at` is a side effect too (a `due_at` change re-arms it) but is not
 * patchable, so it stays un-restored — re-arming a reminder is the safe
 * direction to fail in.
 *
 * Returns null when nothing in the list moved, which is the common case for a
 * commit-on-blur field re-committing its own value — recording those would make
 * ⌘Z appear dead for several presses before reaching the edit you meant.
 *
 * `blocked_reason` is an ordinary field here: a HOLD independent of `status`
 * (v27), so the diff sends it on its own and undoing a release re-holds.
 */
function inversePatch(before: Task, after: Task): TaskPatch | null {
  const inverse: Record<string, unknown> = {};
  const prior = before as unknown as Record<string, unknown>;
  const current = after as unknown as Record<string, unknown>;
  for (const key of UNDOABLE_FIELDS) {
    if (prior[key] !== current[key]) inverse[key] = prior[key];
  }
  if (Object.keys(inverse).length === 0) return null;
  return inverse as TaskPatch;
}

/**
 * A past-tense phrase for what a patch did, for the toast.
 *
 * Only the single-key cases get a real sentence, because those are the ones a
 * click produces — a priority key, a status dropdown, a drag onto a project.
 * A multi-key patch comes from a form and "edited" is all that can honestly be
 * said about it.
 */
function describePatch(patch: TaskPatch): string {
  const keys = Object.keys(patch);
  if (keys.length !== 1) return "edited";
  switch (keys[0]) {
    case "blocked_reason":
      return patch.blocked_reason ? "held" : "released";
    case "priority":
      return `set to P${patch.priority}`;
    case "status":
      return `moved to ${patch.status ? STATUS_LABELS[patch.status] : "a new status"}`;
    case "project":
      return patch.project ? `moved to #${patch.project}` : "taken out of its project";
    case "due_at":
      return patch.due_at ? "rescheduled" : "un-scheduled";
    case "defer_until":
      return patch.defer_until ? "deferred" : "un-deferred";
    case "recurrence":
      return patch.recurrence ? "set to repeat" : "stopped repeating";
    case "title":
      return "renamed";
    case "body":
      return "notes edited";
    case "section_id":
      return "regrouped";
    default:
      return "edited";
  }
}

// ── quick-add parsing ──────────────────────────────────────────────────────

/**
 * Minimal inline syntax for the quick-add bar:
 *   `#project`  `!0..!3` priority  `@today` `@tomorrow` `@fri` due date
 *   `@name` — any other `@word` is a PERSON tag (`@sam`), see `isPersonTag`.
 *   The date words win the `@`: nobody is called Today, and a person who is
 *   is one more `@` away (`@@fri` is not supported; use the tag editor).
 *
 * Deliberately small. Full natural-language date parsing was explicitly cut
 * from v1 — it's a bottomless pit of ambiguity ("next friday", "in 2 weeks",
 * "the 3rd") and the detail pane has a real date field.
 */
export function parseQuickAdd(input: string, defaultProject: string | null = null) {
  const raw = input.trim();
  if (!raw) return null;

  let project: string | null = defaultProject;
  // P0 is the HIGHEST priority (invariant 9). An untyped capture is the
  // lowest, matching `create_task`'s own default — otherwise every capture
  // whose enrichment times out or fails lands as drop-everything urgent.
  let priority = PRIORITY_LOWEST;
  let dueAt: string | null = null;
  const tags: string[] = [];

  const words = raw.split(/\s+/);
  const kept: string[] = [];

  for (const word of words) {
    if (word.startsWith("#") && word.length > 1) {
      project = word.slice(1);
    } else if (/^![0-3]$/.test(word)) {
      priority = Number(word.slice(1));
    } else if (word.startsWith("@") && word.length > 1) {
      const due = parseDueToken(word.slice(1));
      if (due) dueAt = due;
      else {
        // Not a date word: a person. Trailing punctuation is prose, not
        // part of the name ("ask @dana," is Dana).
        const name = word.replace(/[,.;:!?]+$/, "").toLowerCase();
        if (name.length > 1 && !tags.includes(name)) tags.push(name);
      }
    } else {
      kept.push(word);
    }
  }

  const title = kept.join(" ").trim();
  if (!title) return null;

  return {
    title,
    project,
    priority,
    due_at: dueAt,
    tags,
  };
}

const WEEKDAYS: Record<string, number> = {
  sun: 0, sunday: 0,
  mon: 1, monday: 1,
  tue: 2, tuesday: 2,
  wed: 3, wednesday: 3,
  thu: 4, thursday: 4,
  fri: 5, friday: 5,
  sat: 6, saturday: 6,
};

/** `today` | `tomorrow` | a weekday name → 5pm local that day, as ISO UTC. */
export function parseDueToken(token: string, now = new Date()): string | null {
  const t = token.toLowerCase();
  const at5pm = (d: Date) => {
    const out = new Date(d);
    out.setHours(17, 0, 0, 0);
    return out.toISOString();
  };

  if (t === "today") return at5pm(now);
  if (t === "tomorrow") {
    const d = new Date(now);
    d.setDate(d.getDate() + 1);
    return at5pm(d);
  }
  const weekday = WEEKDAYS[t];
  if (weekday === undefined) return null;

  // The next occurrence of that weekday, strictly in the future.
  const d = new Date(now);
  let delta = (weekday - d.getDay() + 7) % 7;
  if (delta === 0) delta = 7;
  d.setDate(d.getDate() + delta);
  return at5pm(d);
}
