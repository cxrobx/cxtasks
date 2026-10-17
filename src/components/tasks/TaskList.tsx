import {
  useEffect,
  useRef,
  useState,
  type DragEvent,
  type ReactNode,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  Ban,
  CalendarClock,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Flag,
  GitBranch,
  GripVertical,
  Plus,
  RefreshCw,
  Repeat,
  Trash2,
  Undo2,
  X,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  formatAge,
  formatDue,
  formatShortId,
  isClosed,
  isDecision,
  isDeferred,
  isOverdue,
  isMonitor,
  isTripwireArmed,
  describeTripwire,
  orderTagsByKind,
  tripwireUnchecked,
  PRIORITY_COLORS,
  PRIORITY_EDGE,
  STATUS_LABELS,
  statusClass,
} from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import TaskContextMenu from "./TaskContextMenu";
import {
  DENSITY_ROW_ESTIMATE,
  GROUP_BY_OPTIONS,
  useUiStore,
  type GroupBy,
} from "@/stores/uiStore";
import { BulkActionBar } from "@/components/tasks/BulkActionBar";
import { DateField } from "@/components/shared/DateField";
import { ReferenceChip } from "@/components/shared/ReferenceChip";
import { TagMark } from "@/components/shared/TagMark";
import {
  PRIORITY_LOWEST,
  RETENTION_DAYS,
  type Section,
  type Task,
  TripwireRuntime,
} from "@/types/task";

/**
 * One block of the grouped board: a header, its rows, and what dropping a task
 * on it means.
 *
 * Sections and projects both render through this rather than through two
 * copies of the same 60 lines. The sticky headers, the collapse state, the
 * drop highlighting and — the one that actually bites — the RENDER ORDER a
 * shift-click range is measured against all have to behave identically in both
 * modes, and the surest way to keep two grouped boards in step is for there to
 * be one.
 */
interface Group {
  /** React key and collapse key. Section groups use the section's UUID;
   * project groups carry a `project:` prefix so a project can never collide
   * with a section id, whatever it's called. */
  key: string;
  /** Only saved sections can be reordered; derived groups stay in their order. */
  sectionId?: string;
  label: string;
  rows: Task[];
  /** Trailing header controls. Sections only — a project has no milestone. */
  controls?: ReactNode;
  /** Move what was dragged into this group — a SET, because a drag that starts
   * on a selected row carries the whole selection. Every implementation takes
   * the list, so none of them can quietly act on one row of six. */
  onDropTasks: (ids: string[]) => void;
  /** Does this task already render here? Drives the drop highlight, so it must
   * agree with the bucketing above — a highlight on the group a task is
   * already in promises a move that won't happen. */
  holds: (task: Task) => boolean;
  /** Shown when the group has no rows, and (while a drag is in flight) when it
   * is collapsed — a collapsed group shrinks to its header, which is a hard
   * thing to hit mid-drag. */
  dropHint: string;
  /** Keep rendering when empty. True for sections, which exist whether or not
   * anything is filed into them; false for a derived group, which is only
   * there because it has rows. */
  persistent?: boolean;
  /** The "everything else" bucket — quieter, and it has no name of its own. */
  dim?: boolean;
}

export function TaskList() {
  const tasks = useTaskStore((s) => s.tasks);
  const selectedId = useTaskStore((s) => s.selectedId);
  const loading = useTaskStore((s) => s.loading);
  const flashIds = useTaskStore((s) => s.flashIds);
  const blockedIds = useTaskStore((s) => s.blockedIds);
  const tagsByTask = useTaskStore((s) => s.tagsByTask);
  const tripwireByTask = useTaskStore((s) => s.tripwireByTask);
  const complete = useTaskStore((s) => s.complete);
  const reorder = useTaskStore((s) => s.reorder);
  const sections = useTaskStore((s) => s.sections);
  const createSection = useTaskStore((s) => s.createSection);
  const deleteSection = useTaskStore((s) => s.deleteSection);
  const reorderSections = useTaskStore((s) => s.reorderSections);
  const moveTasksToSection = useTaskStore((s) => s.moveTasksToSection);
  const setTasksProject = useTaskStore((s) => s.setTasksProject);
  const view = useTaskStore((s) => s.view);
  const setView = useTaskStore((s) => s.setView);
  const countdowns = useTaskStore((s) => s.countdowns);
  const restore = useTaskStore((s) => s.restore);
  const unarchive = useTaskStore((s) => s.unarchive);
  const destroy = useTaskStore((s) => s.destroy);
  const emptyTrash = useTaskStore((s) => s.emptyTrash);
  const checkedIds = useTaskStore((s) => s.checkedIds);
  const rowClick = useTaskStore((s) => s.rowClick);
  const patch = useTaskStore((s) => s.patch);
  const activeProject = useTaskStore((s) => s.activeProject);
  const projectRecords = useTaskStore((s) => s.projectRecords);
  const revealTick = useTaskStore((s) => s.revealTick);
  const groupBy = useUiStore((s) => s.groupBy);
  /**
   * Manual order only means something under the default sort.
   *
   * `tasks.sort_order` is the SIXTH key in the board order — a drag only ever
   * broke ties — so under an explicit sort a drop would write positions
   * derived from a sorted sequence, scrambling the manual order you built and
   * snapping back on the next refresh. Disabled and *said*, the same treatment
   * the dead group-by segment gets, rather than accepted and quietly ignored.
   */
  const sortKey = useUiStore((s) => s.sort.key);
  const reorderable = sortKey === "default";
  const density = useUiStore((s) => s.density);
  const setGroupBy = useUiStore((s) => s.setGroupBy);
  const hidden = view === "trash" || view === "archive";
  const [newSection, setNewSection] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<string[]>([]);

  const parentRef = useRef<HTMLDivElement>(null);
  /** The grouped board's scroller. Separate from `parentRef` (the
   * virtualizer's) so a reveal can only ever find a row in the list that is
   * actually rendered. */
  const groupedRef = useRef<HTMLDivElement>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropId, setDropId] = useState<string | null>(null);
  /** Which group the pointer is currently over, in the grouped view.
   * `undefined` = none. The "everything else" bucket is a key like any other:
   * it's a real target, not the absence of one — it's how a task comes back
   * out of a section, or out of a project. */
  const [dropGroup, setDropGroup] = useState<string | undefined>(undefined);
  const [dragSectionId, setDragSectionId] = useState<string | null>(null);
  const [dropSection, setDropSection] = useState<{
    id: string;
    edge: "before" | "after";
  } | null>(null);

  /**
   * What the drag in flight is carrying.
   *
   * A row that is part of the checked set drags the WHOLE set. The selection is
   * something the user deliberately built, so a gesture that acted on one row
   * of the six they'd just picked would be the app ignoring the only thing
   * they'd said. A row OUTSIDE the set drags alone and does not join it —
   * grabbing an unchecked row is how you move one task without first tearing
   * down a selection you still want.
   */
  const moving: string[] = !dragId
    ? []
    : checkedIds.includes(dragId)
      ? checkedIds
      : [dragId];

  const endDrag = () => {
    setDragId(null);
    setDropId(null);
    setDropGroup(undefined);
    setDragSectionId(null);
    setDropSection(null);
  };

  const moveSection = (source: string, target: string, edge: "before" | "after") => {
    if (source === target) return;
    const ids = sections.map((s) => s.id);
    const from = ids.indexOf(source);
    if (from === -1 || !ids.includes(target)) return;
    ids.splice(from, 1);
    ids.splice(ids.indexOf(target) + (edge === "after" ? 1 : 0), 0, source);
    void reorderSections(ids);
  };

  const handleDrop = (targetId: string) => {
    if (!reorderable) {
      endDrag();
      return;
    }
    if (!dragId || dragId === targetId) {
      endDrag();
      return;
    }
    const ids = tasks.map((t) => t.id);
    const from = ids.indexOf(dragId);
    const to = ids.indexOf(targetId);
    if (from === -1 || to === -1) return;
    ids.splice(to, 0, ids.splice(from, 1)[0]!);
    endDrag();
    void reorder(ids);
  };

  const virtualizer = useVirtualizer({
    count: tasks.length,
    getScrollElement: () => parentRef.current,
    // Row + the container's `pb-1.5` gap, PER DENSITY. A stale estimate is
    // not cosmetic: the scrollbar is sized from it until each row is measured,
    // so it jumps under the thumb during the first scroll (gotcha #33) — which
    // is why this has to move whenever the padding does.
    estimateSize: () => DENSITY_ROW_ESTIMATE[density],
    overscan: 8,
    // Stable keys + measureElement — without both, virtualized rows recycle
    // onto the wrong task and the list visibly scrambles (cxmail hit this).
    getItemKey: (index) => tasks[index]?.id ?? index,
  });

  /**
   * Live projects nested under whatever the sidebar has selected.
   *
   * This is what the area/project split bought the grouping control. Selecting
   * an AREA lists its own tasks plus every live project under it
   * (`db::projects::expand_scope`), so grouping by project inside one carves the
   * roll-up back into the piles it was made of — which is the only way to read
   * an area whose work is spread across three projects. Selecting a project, or
   * an area holding nothing, is still one pile with one name on it.
   *
   * Archived children are already absent — `projectRecords` is loaded without
   * them and `expand_scope` drops them too — so the two agree by construction,
   * and closing a project takes it out of both in the same breath.
   */
  const nested =
    activeProject === null
      ? []
      : projectRecords.filter(
          (p) =>
            p.parent !== null &&
            p.parent.toLowerCase() === activeProject.toLowerCase()
        );

  /**
   * The grouping actually applied, which is not always the one stored.
   *
   * Two overrides, both resolved HERE rather than by writing a different value
   * back into the store. The Trash and the Archive are never grouped — they
   * have their own header, and carving up deleted work by the project it used
   * to belong to is filing you'd only ever do to things you're not doing. And
   * grouping by project inside a scope that holds no projects would draw a
   * single header over the whole list. Storing the fallback would be the real
   * bug: the mode has to come back the moment you leave, and a preference that
   * silently rewrites itself as you navigate is one you stop trusting.
   *
   * `nested.length === 0` and not `activeProject !== null`, which is what this
   * used to say — before areas existed, being inside one meant every row shared
   * a project. An area with projects under it is the case that broke that.
   */
  const mode: GroupBy = hidden
    ? "none"
    : groupBy === "project" && activeProject !== null && nested.length === 0
      ? "section"
      : groupBy;

  /**
   * Sections: the hand-curated grouping. Unchanged from what this list has
   * always done — every section renders whether or not it holds anything, and
   * the Ungrouped bucket is how a task gets back out of one.
   */
  const sectionGroups = (rows: Task[]): Group[] => {
    if (sections.length === 0) return [];
    const bySection = new Map<string, Task[]>();
    for (const s of sections) bySection.set(s.id, []);
    const loose: Task[] = [];
    // A `section_id` pointing at a section this view doesn't list (another
    // project's, or one deleted by the MCP mid-session) falls to Ungrouped
    // rather than vanishing — the same `has` check the bucketing has always
    // made, and `keyOf` repeats it so the drop highlight can't disagree.
    const keyOf = (t: Task) =>
      t.section_id && bySection.has(t.section_id) ? t.section_id : "section:none";
    for (const t of rows) (bySection.get(keyOf(t)) ?? loose).push(t);

    return [
      ...sections.map((sec) => ({
        key: sec.id,
        sectionId: sec.id,
        label: sec.name,
        rows: bySection.get(sec.id) ?? [],
        controls: (
          <>
            <MilestoneControls section={sec} />
            <button
              type="button"
              aria-label={`Delete section ${sec.name}`}
              title="Delete section (tasks are kept, just ungrouped)"
              onClick={() => void deleteSection(sec.id)}
              className="opacity-0 transition-opacity group-hover:opacity-100"
            >
              <X size={11} className="text-content-faint hover:text-error" />
            </button>
          </>
        ),
        onDropTasks: (ids: string[]) => void moveTasksToSection(ids, sec.id),
        holds: (t: Task) => keyOf(t) === sec.id,
        dropHint: `Drop here to file into ${sec.name}.`,
        persistent: true,
      })),
      {
        key: "section:none",
        label: "Ungrouped",
        rows: loose,
        onDropTasks: (ids: string[]) => void moveTasksToSection(ids, null),
        holds: (t: Task) => keyOf(t) === "section:none",
        dropHint: "Drop here to take a task out of its section.",
        dim: true,
      },
    ];
  };

  /**
   * Projects: the derived grouping, and the only one that means anything in
   * Inbox / Today / Open. Those views cut across the whole portfolio, so
   * until this existed the only way to read "what's due today, by project" was
   * to visit each project in turn and hold the dates in your head.
   */
  const projectGroups = (rows: Task[]): Group[] => {
    const byProject = new Map<string, Task[]>();
    const loose: Task[] = [];
    for (const t of rows) {
      if (!t.project) {
        loose.push(t);
        continue;
      }
      const rows = byProject.get(t.project);
      if (rows) rows.push(t);
      else byProject.set(t.project, [t]);
    }
    // Nothing here carries a project, so there is nothing to group BY. Fall
    // through to the flat list rather than draw one "No project" header over
    // the entire view, which says nothing and costs a row.
    if (byProject.size === 0) return [];

    // Sidebar order first, so the board reads in the order the projects are
    // listed two panes away — that order is hand-set (`reorderProjects`) and
    // re-sorting alphabetically here would quietly ignore it. A project with
    // tasks but no `projects` row can only come from a pre-v4 binary or a
    // hand-edited DB; alphabetical after the known ones is a deterministic
    // place to put it, and putting it somewhere beats dropping its rows.
    const rank = new Map(projectRecords.map((p, i) => [p.name, i]));
    const names = [...byProject.keys()].sort((a, b) => {
      const ra = rank.get(a);
      const rb = rank.get(b);
      if (ra !== undefined && rb !== undefined) return ra - rb;
      if (ra !== undefined) return -1;
      if (rb !== undefined) return 1;
      return a.localeCompare(b);
    });

    return [
      ...names.map((name) => ({
        key: `project:${name}`,
        label: name,
        rows: byProject.get(name) ?? [],
        // Dropping re-files the task, exactly as dropping on a section does.
        // It writes `tasks.project`, which is a real move rather than a view
        // change — but it's the same gesture, in the same list, with the same
        // highlight, and a board where dragging works in one mode and dies in
        // the other reads as broken.
        onDropTasks: (ids: string[]) => void setTasksProject(ids, name),
        holds: (t: Task) => t.project === name,
        dropHint: `Drop here to move a task into ${name}.`,
      })),
      {
        key: "project:none",
        label: "No project",
        rows: loose,
        onDropTasks: (ids: string[]) => void setTasksProject(ids, null),
        holds: (t: Task) => !t.project,
        dropHint: "Drop here to take a task out of its project.",
        dim: true,
      },
    ];
  };

  /**
   * Waiting: grouped by WHY, never by project or section — the reason is the
   * only axis that answers "what am I waiting on". The date gate is judged
   * first because the runner judges it first (invariant 26): a deferred task
   * with an armed tripwire is waiting for the date, and only then for the
   * check. A reason is derived from the row, so nothing can be dragged into
   * one; the drop hint says so instead of the drop silently doing nothing.
   */
  const waitingGroups = (): Group[] => {
    const kinds = [
      {
        key: "waiting:date",
        label: "Until a date",
        hint: "Deferred — a reason is derived from the row, not somewhere a task can be dropped.",
      },
      {
        key: "waiting:condition",
        label: "Until a condition",
        hint: "An armed tripwire — the runner checks it; the last answer is under the title.",
      },
      {
        key: "waiting:task",
        label: "Behind another task",
        hint: "Blocked by an open task — released the moment that task closes.",
      },
      {
        key: "waiting:blocked",
        label: "Waiting on something",
        hint: "A written reason — outside CXTasks — that has to clear before this can move. The reason is under the title.",
      },
    ];
    const kindOf = (t: Task): string =>
      isDeferred(t)
        ? "waiting:date"
        : isTripwireArmed(t)
          ? "waiting:condition"
          : blockedIds.includes(t.id)
            ? "waiting:task"
            : "waiting:blocked";
    return kinds
      .map((k) => ({
        key: k.key,
        label: k.label,
        rows: tasks.filter((t) => kindOf(t) === k.key),
        onDropTasks: () => {},
        holds: (t: Task) => kindOf(t) === k.key,
        dropHint: k.hint,
      }))
      .filter((g) => g.rows.length > 0);
  };

  /**
   * Today's Decide strip: the open decisions (`DECISION_TAG`), first, above
   * whatever grouping the rest of the day uses. A decision is on Today because
   * nothing else can move it — not an agent, not a date — and it sits at the
   * top because it is usually the cheapest row on the board to close: one
   * line, typed into the row itself (`DecideLine`).
   */
  const decisionRows =
    view === "today" && !hidden ? tasks.filter((t) => isDecision(tagsByTask[t.id])) : [];
  const decisionIds = new Set(decisionRows.map((t) => t.id));
  const undecided = decisionRows.length ? tasks.filter((t) => !decisionIds.has(t.id)) : tasks;

  const base: Group[] =
    mode === "project"
      ? projectGroups(undecided)
      : mode === "section"
        ? sectionGroups(undecided)
        : [];
  const groups: Group[] =
    view === "waiting" && !hidden
      ? waitingGroups()
      : decisionRows.length === 0
        ? base
        : [
            {
              key: "today:decide",
              label: "Decide",
              rows: decisionRows,
              onDropTasks: () => {},
              holds: (t: Task) => decisionIds.has(t.id),
              dropHint:
                "A decision is a question for you, not a drop target — tag a task `decision` to put it here.",
            },
            // With no grouping preference the rest of the day still needs a
            // group to live in, or the strip would be the whole list.
            ...(base.length
              ? base
              : undecided.length
                ? [
                    {
                      key: "today:rest",
                      label: "Today",
                      rows: undecided,
                      onDropTasks: () => {},
                      holds: (t: Task) => !decisionIds.has(t.id),
                      dropHint: "",
                      dim: true,
                    },
                  ]
                : []),
          ];

  /**
   * Scroll a REVEALED row into view, opening whatever was hiding it.
   *
   * Keyed on `revealTick` and never on `selectedId`. Scrolling on every
   * selection change would drag the list around under someone who just clicked
   * a row that happened to sit half off-screen; only an arrival from outside
   * the list — a notification click — has any business moving the viewport.
   *
   * Split across two commits deliberately. Expanding a collapsed group is a
   * state change, so the row does not exist in the DOM until React has
   * re-rendered; scrolling in the same pass would look for an element that
   * isn't there yet and silently do nothing.
   */
  const [pendingReveal, setPendingReveal] = useState<string | null>(null);

  useEffect(() => {
    if (revealTick === 0 || !selectedId) return;
    const holder = groups.find((g) => g.rows.some((t) => t.id === selectedId));
    if (holder) setCollapsed((c) => c.filter((k) => k !== holder.key));
    setPendingReveal(selectedId);
    // Intentionally keyed on the tick alone. `groups` is rebuilt every render,
    // so depending on it would re-fire this forever; `selectedId` and `groups`
    // are read at the moment the reveal lands, which is the only moment that
    // matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revealTick]);

  useEffect(() => {
    if (!pendingReveal) return;
    setPendingReveal(null);
    // Two render paths, two ways to scroll. The grouped board is plain-scrolled
    // so the element itself is the handle; the ungrouped list is virtualized,
    // where the row may not be mounted at all and only the virtualizer can put
    // it on screen. Ids are UUIDs, so they are always valid in a selector.
    if (groups.length > 0) {
      groupedRef.current
        ?.querySelector(`[data-task-id="${pendingReveal}"]`)
        ?.scrollIntoView({ block: "center" });
      return;
    }
    const index = tasks.findIndex((t) => t.id === pendingReveal);
    if (index !== -1) virtualizer.scrollToIndex(index, { align: "center" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingReveal]);

  /** Row props that only exist in the Trash/Archive, shared by both render
   * paths so the two can't drift. */
  const hiddenRowProps = (task: Task) =>
    hidden
      ? {
          onRestore: () =>
            void (view === "trash" ? restore(task.id) : unarchive(task.id)),
          daysLeft: countdowns[task.id],
          onDestroy:
            view === "trash"
              ? async () => {
                  // The one destructive action left in the app, so it's the one
                  // place a confirm earns its interruption — a native dialog,
                  // naming the task, with the irreversibility spelled out.
                  const ok = await confirm(
                    `Permanently delete "${task.title}"? This cannot be undone.`,
                    { title: "Delete Task", kind: "warning", okLabel: "Delete" }
                  );
                  if (ok) await destroy(task.id);
                }
              : undefined,
        }
      : {};

  /**
   * Selection + completion wiring, shared by both render paths.
   *
   * `order` is the ids in the order they are RENDERED, which is not `tasks`
   * order in the grouped view — a shift-click range has to mean "the rows
   * between these two on screen", not "the entries between them in an array
   * the user cannot see".
   */
  const rowProps = (task: Task, order: string[]) => ({
    checked: checkedIds.includes(task.id),
    onSelect: (mods: { meta: boolean; shift: boolean; toggleOpen?: boolean }) =>
      rowClick(task.id, mods, order),
    onComplete: () => void complete(task.id),
    onReopen: () => void patch(task.id, { status: "todo" }),
  });

  /**
   * The Archive/Trash segmented control.
   *
   * These are one sidebar destination now, not two rows, so this is the only
   * way to reach the Trash — which is exactly why it can't be subtle. Trashed
   * tasks are the one set `search_tasks` deliberately excludes (archived ones
   * still match), so this control is their sole door; an archived task you
   * can't find here is still findable by typing its name.
   */
  const hiddenSegments = (
    <div
      role="tablist"
      aria-label="Set-aside tasks"
      className="flex shrink-0 items-center gap-px rounded-md border border-border-subtle p-px"
    >
      {(["archive", "trash"] as const).map((k) => (
        <button
          key={k}
          type="button"
          role="tab"
          aria-selected={view === k}
          onClick={() => void setView(k)}
          className={cn(
            "rounded-[5px] px-2 py-0.5 transition-colors duration-75",
            // `bg-accent` + white is the app's active-state pairing everywhere
            // else, but it is only 3.64:1 — below the 4.5:1 floor — and the
            // sidebar gets away with it because those rows are 13px. Inside
            // this header the label inherits 11px, where thin white strokes on
            // a light blue visibly turn to mud. `accent-hover` is the same hue
            // two steps darker and measures 5.45:1, and the medium weight
            // thickens the strokes rather than relying on colour alone.
            view === k
              ? "bg-accent-hover font-medium text-white"
              : "text-content-secondary hover:bg-content/8 hover:text-content"
          )}
        >
          {k === "archive" ? "Archived" : "Trash"}
        </button>
      ))}
    </div>
  );

  // Trash and Archive get their own header. Sections are a way of organising
  // work you're doing, which neither of these is — offering "+ Section" in the
  // Trash invites you to file things you've deleted.
  const hiddenHeader = (
    <div className="flex items-center gap-2 border-b border-border-subtle px-3 py-1.5 text-[11px] text-content-faint">
      {hiddenSegments}
      {view === "trash" ? (
        <>
          <span className="truncate">Removed after {RETENTION_DAYS} days.</span>
          {tasks.length > 0 && (
            <button
              type="button"
              onClick={async () => {
                // `countdowns` covers EVERY trashed row; `tasks` is only the
                // batch roots the list shows. Counting the visible rows would
                // under-report a parent that took subtasks down with it — and
                // an irreversible confirm is the last place to be approximate.
                const total = Object.keys(countdowns).length || tasks.length;
                const ok = await confirm(
                  `Permanently delete ${total} task${total === 1 ? "" : "s"}? This cannot be undone.`,
                  { title: "Empty Trash", kind: "warning", okLabel: "Empty Trash" }
                );
                if (ok) await emptyTrash();
              }}
              className="ml-auto rounded px-1.5 py-0.5 text-error hover:bg-error/10"
            >
              Empty Trash
            </button>
          )}
        </>
      ) : (
        <span className="truncate">
          Never deleted. Still turns up in search.
        </span>
      )}
    </div>
  );

  /**
   * The grouping control.
   *
   * `radiogroup`, not `tablist`: these three don't swap what's on screen the
   * way Archive/Trash do, they re-carve the same rows. It reflects the mode
   * actually in force rather than the stored one, so the highlighted segment is
   * always a true statement about the list underneath it — see `mode`.
   */
  const groupSegments = (
    <div
      role="radiogroup"
      aria-label="Group tasks by"
      className="ml-auto flex shrink-0 items-center gap-px rounded-md border border-border-subtle p-px text-[11px]"
    >
      {GROUP_BY_OPTIONS.map((opt) => {
        // Every task here is already in this project, so the option would draw
        // one header over everything. Disabled and *said*, rather than
        // accepted and quietly ignored — a control that takes a click and
        // changes nothing is what teaches you to distrust the other two.
        //
        // The exception is an AREA holding projects: its list rolls them up, so
        // there is more than one project in it and the grouping means something
        // again. Same condition as `mode`'s fallback, deliberately — read from
        // one place, because the two disagreeing is a live-looking control that
        // silently renders sections.
        const dead =
          opt.value === "project" && activeProject !== null && nested.length === 0;
        return (
          <button
            key={opt.value}
            type="button"
            role="radio"
            aria-checked={mode === opt.value}
            disabled={dead}
            title={
              dead
                ? `Everything here is already in ${activeProject}.`
                : undefined
            }
            onClick={() => setGroupBy(opt.value)}
            className={cn(
              "rounded-[5px] px-2 py-0.5 transition-colors duration-75",
              // `accent-hover` rather than `accent`, for the same reason the
              // Archive/Trash segments use it: at 11px the 3.64:1 pairing turns
              // to mud, and this control inherits that size.
              mode === opt.value
                ? "bg-accent-hover font-medium text-white"
                : "text-content-secondary hover:bg-content/8 hover:text-content",
              dead && "cursor-default opacity-40 hover:bg-transparent hover:text-content-secondary"
            )}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );

  const sectionHeader = hidden ? (
    hiddenHeader
  ) : (
    <div className="flex items-center gap-2 border-b border-border-subtle px-3 py-1.5">
      {/* "+ Section" only while sections are what's on screen. Offering it
          under any other grouping invites you to create something the list
          won't show you, and the section you made looks like it wasn't
          created at all. */}
      {mode !== "section" ? null : newSection !== null ? (
        <input
          autoFocus
          value={newSection}
          placeholder="Section name…"
          onChange={(e) => setNewSection(e.target.value)}
          onBlur={() => {
            if (!newSection.trim()) setNewSection(null);
          }}
          onKeyDown={async (e) => {
            if (e.key === "Escape") setNewSection(null);
            else if (e.key === "Enter" && newSection.trim()) {
              await createSection(newSection);
              setNewSection(null);
            }
          }}
          className="w-48 rounded border border-border bg-input px-1.5 py-0.5 text-[11px]"
        />
      ) : (
        <button
          type="button"
          onClick={() => setNewSection("")}
          className="flex items-center gap-1 rounded px-1 py-0.5 text-[11px] text-content-faint hover:bg-content/8 hover:text-content"
        >
          <Plus size={11} />
          Section
        </button>
      )}
      {/* Waiting is grouped by reason and by nothing else — the control would
          be a live-looking switch that changes nothing (the same argument as
          the disabled project segment above). */}
      {view === "waiting" ? null : groupSegments}
    </div>
  );

  if (!loading && tasks.length === 0 && groups.length === 0) {
    return (
      <div className="flex h-full flex-col">
        {sectionHeader}
        <div className="flex flex-1 items-center justify-center text-content-faint">
          Nothing here.
        </div>
      </div>
    );
  }

  // Grouped rendering: plain scroll (not virtualized), because a grouped view
  // is a board you read — tens of rows, not thousands — and interleaving
  // headers into a virtualizer's flat index space is a bug factory. The
  // ungrouped path below keeps the virtualizer for the long lists that need it.
  if (groups.length > 0) {
    // A derived group is only on screen because it has rows; a section is on
    // screen either way. The one exception is a drag in flight, when the empty
    // "everything else" bucket has to appear — hide it and the last task to
    // leave a project takes the only way back out with it.
    const visible = groups.filter(
      (g) => g.persistent || g.rows.length > 0 || dragId !== null
    );

    // The rendered order, group by group. This is what a shift-click range is
    // measured against — a range has to mean the rows between two clicks ON
    // SCREEN, which in a grouped list is nothing like `tasks` order.
    const renderOrder = visible.flatMap((g) => g.rows.map((t) => t.id));

    const renderRows = (rows: Task[]) =>
      rows.map((task) => (
        <div key={task.id} className="px-2 pb-1.5">
          <TaskRow
            task={task}
            selected={task.id === selectedId}
            flashing={flashIds.includes(task.id)}
            blocked={blockedIds.includes(task.id)}
            tags={tagsByTask[task.id]}
            tripwire={tripwireByTask[task.id]}
            // Two view-specific props the row otherwise ignores: Waiting reads
            // what the last check said and how long the row has sat; Decide
            // takes the one-line call.
            waitingSince={view === "waiting" ? task.created_at : undefined}
            onDecide={
              decisionIds.has(task.id)
                ? (answer: string) => void complete(task.id, answer)
                : undefined
            }
            // EVERY row that's coming along dims, not just the one under the
            // cursor. It's the only thing on screen that says the drop will move
            // six tasks rather than one, and a gesture that quietly does more
            // than it looks like it does is the version of this feature that
            // costs someone an afternoon of filing.
            dragging={moving.includes(task.id)}
            onDragStart={() => {
              endDrag();
              setDragId(task.id);
            }}
            onDragEnd={endDrag}
            // The header a few pixels above already says the project; printing
            // it on every row as well turns a real signal into wallpaper.
            hideProject={mode === "project"}
            {...rowProps(task, renderOrder)}
          />
      </div>
      ));

    /** Drop-target wiring, shared by every group so no two can drift into
     * behaving differently. Rows inside a group don't handle the drop
     * themselves — it bubbles up to here, which is why a drop anywhere in the
     * block (header, a row, the empty placeholder) means the same thing: move
     * what's being dragged into this group. */
    const dropZone = (group: Group) => ({
      onDragOver: (e: DragEvent<HTMLDivElement>) => {
        if (dragSectionId) {
          if (!group.sectionId || group.sectionId === dragSectionId) {
            setDropSection(null);
            return;
          }
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          const rect = e.currentTarget.getBoundingClientRect();
          setDropSection({
            id: group.sectionId,
            edge: e.clientY < rect.top + rect.height / 2 ? "before" : "after",
          });
          return;
        }
        if (!dragId) return;
        e.preventDefault();
        setDropGroup(group.key);
      },
      onDrop: (e: DragEvent) => {
        e.preventDefault();
        if (dragSectionId) {
          if (group.sectionId && dropSection?.id === group.sectionId) {
            moveSection(dragSectionId, group.sectionId, dropSection.edge);
          }
        } else if (moving.length > 0) group.onDropTasks(moving);
        endDrag();
      },
      onDragLeave: (e: DragEvent<HTMLDivElement>) => {
        if (e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget)) return;
        setDropSection((current) => current?.id === group.sectionId ? null : current);
      },
    });

    /** Only light up a group you'd actually be moving something INTO — a
     * highlight on a group that already holds every row you're carrying
     * promises a change that won't happen. `some`, not the dragged row alone:
     * a set can straddle two groups, and dropping it is still a move for the
     * rows that aren't here yet. */
    const movingTasks = moving
      .map((id) => tasks.find((t) => t.id === id))
      .filter((t): t is Task => t !== undefined);
    const activeDrop = (group: Group) =>
      dropGroup === group.key && movingTasks.some((t) => !group.holds(t));

    return (
      <div className="flex h-full min-h-0 flex-col">
        {sectionHeader}
        <div ref={groupedRef} className="min-h-0 flex-1 overflow-y-auto">
          {visible.map((group) => {
            const isCollapsed = collapsed.includes(group.key);
            const sectionId = group.sectionId;
            return (
              <div
                key={group.key}
                data-section-id={group.sectionId}
                {...dropZone(group)}
                className={cn(
                  activeDrop(group) && "cx-drop-into",
                  dragSectionId === group.sectionId && "cx-dragging",
                  dropSection?.id === sectionId && dropSection?.edge === "after" && "cx-drop-below"
                )}
              >
                <div className={cn(
                  "group sticky top-0 z-10 flex items-center gap-1.5 bg-base/95 px-3 py-1 backdrop-blur",
                  dropSection?.id === sectionId && dropSection?.edge === "before" && "cx-drop-above"
                )}>
                  {sectionId && sections.length > 1 && (
                    <button
                      type="button"
                      draggable
                      aria-label={`Reorder section ${group.label}`}
                      aria-keyshortcuts="ArrowUp ArrowDown"
                      title="Drag to reorder sections, or focus and use ↑ / ↓"
                      onDragStart={(e) => {
                        e.stopPropagation();
                        // WebKit requires a payload even though local state
                        // identifies the section throughout this drag.
                        e.dataTransfer.setData("text/plain", sectionId);
                        e.dataTransfer.effectAllowed = "move";
                        endDrag();
                        setDragSectionId(sectionId);
                      }}
                      onDragEnd={endDrag}
                      onKeyDown={(e) => {
                        if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
                        e.preventDefault();
                        e.stopPropagation();
                        const index = sections.findIndex((s) => s.id === sectionId);
                        const target = sections[index + (e.key === "ArrowUp" ? -1 : 1)];
                        if (target) {
                          moveSection(sectionId, target.id, e.key === "ArrowUp" ? "before" : "after");
                        }
                      }}
                      className="flex size-4 shrink-0 cursor-grab items-center justify-center text-content-faint hover:text-content focus-visible:text-content active:cursor-grabbing"
                    >
                      <GripVertical size={12} />
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() =>
                      setCollapsed((c) =>
                        c.includes(group.key)
                          ? c.filter((x) => x !== group.key)
                          : [...c, group.key]
                      )
                    }
                    className={cn(
                      "flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wider hover:text-content",
                      group.dim ? "text-content-faint" : "text-content-muted"
                    )}
                  >
                    {isCollapsed ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
                    {group.label}
                  </button>
                  <span className="tabular-nums text-[10px] text-content-faint">
                    {group.rows.length}
                  </span>
                  {group.controls}
                </div>
                {/* A collapsed group is still a drop target, but with its rows
                    hidden it shrinks to the header alone, which is a hard thing
                    to hit mid-drag. Give it a landing strip — but only while
                    something is actually being dragged, so a collapsed group is
                    still collapsed the rest of the time. */}
                {(group.rows.length === 0 || (isCollapsed && dragId !== null)) && (
                  <p className="px-3 py-2 text-[11px] text-content-faint">
                    {group.rows.length === 0 && dragId === null
                      ? "Drag a task here."
                      : group.dropHint}
                  </p>
                )}
                {!isCollapsed && renderRows(group.rows)}
              </div>
            );
          })}
        </div>
        <BulkActionBar />
      </div>
    );
  }

  const renderOrder = tasks.map((t) => t.id);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {sectionHeader}
      <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto">
        <div
          style={{ height: virtualizer.getTotalSize() }}
          className="relative w-full"
        >
          {virtualizer.getVirtualItems().map((row) => {
            const task = tasks[row.index];
            if (!task) return null;
            return (
              <div
                key={row.key}
                data-index={row.index}
                ref={virtualizer.measureElement}
                className="absolute left-0 top-0 w-full px-2 pb-1.5"
                style={{ transform: `translateY(${row.start}px)` }}
              >
                <TaskRow
                  task={task}
                  selected={task.id === selectedId}
                  flashing={flashIds.includes(task.id)}
                  blocked={blockedIds.includes(task.id)}
                  tags={tagsByTask[task.id]}
                  tripwire={tripwireByTask[task.id]}
                  // Single-row on purpose, unlike the grouped path above. A
                  // drag here is a REORDER, and a reorder of a set that isn't
                  // contiguous has no honest answer to "where do the other five
                  // land" — so it moves the row you grabbed, and dims only that
                  // row to say so.
                  dragging={dragId === task.id}
                  dropTarget={dropId === task.id}
                  reorderable={reorderable}
                  onDragStart={() => setDragId(task.id)}
                  onDragOver={() => dragId && setDropId(task.id)}
                  onDragEnd={endDrag}
                  onDrop={() => handleDrop(task.id)}
                  {...hiddenRowProps(task)}
                  {...rowProps(task, renderOrder)}
                />
              </div>
            );
          })}
        </div>
      </div>
      <BulkActionBar />
    </div>
  );
}

interface TaskRowProps {
  task: Task;
  selected: boolean;
  flashing?: boolean;
  blocked?: boolean;
  dragging?: boolean;
  dropTarget?: boolean;
  /** A click on the row, modifiers included — this is BOTH "show me this one"
   * and "add this to the set", told apart by what was held down. */
  onSelect: (mods: {
    meta: boolean;
    shift: boolean;
    /** True only for a real click on the row. See `rowClick` — it gates the
     * click-the-same-row-again-to-close behaviour, which the context menu's
     * selection-taking must not trigger. */
    toggleOpen?: boolean;
  }) => void;
  onComplete: () => void;
  /** False turns the row's own drag off — see `reorderable` in `TaskList`.
   * Only the flat path passes it; in a grouped list a drag is a re-file, not
   * a reorder, so the sort has no bearing on it. */
  reorderable?: boolean;
  onDragStart?: () => void;
  onDragOver?: () => void;
  onDragEnd?: () => void;
  onDrop?: () => void;
  /** Set in the Trash/Archive views: replaces the complete check in the
   * trailing cluster with a restore button and, in the Trash, exposes
   * permanent deletion. */
  onRestore?: () => void;
  onDestroy?: () => void;
  /** Days until this task is purged. Trash only. */
  daysLeft?: number;
  /** In the multi-select set — the row is highlighted for it. Nothing in the
   * row is a selection *control* any more; the whole row is. */
  checked?: boolean;
  /** Put a closed task back on the board. Distinct from `onComplete`, which is
   * one-way — see the note on the leading box. */
  onReopen?: () => void;
  /** Drop the `#project` chip because a header directly above already says it.
   * Set only when the list is grouped BY project — the chip is load-bearing
   * everywhere else, since Today and Inbox otherwise give a row no way to say
   * where it belongs. */
  hideProject?: boolean;
  /** This task's tag names.
   *
   * Passed in rather than read off `task`, because tags aren't a column on it —
   * they're many-to-many, and `taskStore.tagsByTask` fetches the whole board's
   * in one query. Exactly how `blocked` arrives, and for the same reason. */
  tags?: string[];
  /** This task's tripwire attempt row, if it has a tripwire. Same delivery as
   * `tags`: a separate table, fetched for the whole board at once. */
  tripwire?: TripwireRuntime;
  /** Set in the Waiting view: print how long the row has sat and, for an armed
   * tripwire, what its last check said — the runtime's `last_note`, the one
   * line that answers "waiting on WHAT, exactly". */
  waitingSince?: string;
  /** Set on a Decide-strip row: a one-line answer closes the task with that
   * line as its handoff. See `DecideLine`. */
  onDecide?: (answer: string) => void;
}

export function TaskRow({
  task,
  selected,
  flashing,
  blocked,
  dragging,
  dropTarget,
  onSelect,
  onComplete,
  reorderable = true,
  onDragStart,
  onDragOver,
  onDragEnd,
  onDrop,
  onRestore,
  onDestroy,
  daysLeft,
  checked = false,
  onReopen,
  hideProject = false,
  tags,
  tripwire,
  waitingSince,
  onDecide,
}: TaskRowProps) {
  const closed = isClosed(task.status);
  const overdue = isOverdue(task);
  const due = formatDue(task.due_at);
  const deferred = isDeferred(task);
  const armed = isTripwireArmed(task);
  const monitor = isMonitor(task);
  // Completing something you've deleted is meaningless, so in the hidden views
  // the trailing action becomes "put it back" instead.
  const restorable = Boolean(onRestore);

  return (
    // The right-click menu wraps the row rather than the row calling out to
    // one: Radix needs the trigger to be an ancestor of what you click.
    // `contents` keeps it out of the layout entirely — a real box here would
    // sit between the list's container and the row and break the gap.
    <TaskContextMenu
      task={task}
      triggerClassName="contents"
      onOpen={() => onSelect({ meta: false, shift: false })}
    >
      <div
        role="option"
        // Both count as selected to assistive tech: a ⌘-clicked row is in the set
        // the bulk bar acts on, and reporting only the focused one would describe
        // a selection of six as a selection of one.
        aria-selected={selected || checked}
        // How `reveal` finds the row to scroll to. The grouped path isn't
        // virtualized and has no index to scroll by, so the id on the element is
        // the only handle the two render paths share.
        data-task-id={task.id}
        draggable={reorderable}
        title={
          reorderable
            ? undefined
            : "Reordering is off while a sort is applied — set Sort back to Default to drag rows."
        }
        // The ROW is the selection control now — ⌘-click to add one, ⇧-click for
        // a range, plain click for just this one. The modifiers have to be read
        // here rather than in the store, since this is the only place that sees
        // the event.
        onClick={(e) =>
          onSelect({
            meta: e.metaKey || e.ctrlKey,
            shift: e.shiftKey,
            // Only here. The context-menu `onOpen` above deliberately omits it.
            toggleOpen: true,
          })
        }
        onDragStart={(e) => {
          // WebKit refuses to start a drag session that carries no data — with no
          // `setData` the row lifts, then every dragover/drop that follows is
          // silently dead. The payload itself is unused (React state carries the
          // id); its mere presence is what makes the drag real. `move`, not the
          // default `all`, so the cursor promises the right thing.
          e.dataTransfer.setData("text/plain", task.id);
          e.dataTransfer.effectAllowed = "move";
          onDragStart?.();
        }}
        onDragOver={(e) => {
          e.preventDefault();
          onDragOver?.();
        }}
        onDragEnd={onDragEnd}
        onDrop={(e) => {
          e.preventDefault();
          onDrop?.();
        }}
        // `select-none` is not cosmetic: ⇧-click is BOTH this row's range gesture
        // and WebKit's "extend the document selection to here", so picking six
        // rows also painted ::selection across every title, tag and chip between
        // the two — a second, brighter highlight fighting the real one, and the
        // row backgrounds became unreadable underneath it. Nothing here is text
        // worth copying; the title is an editable field in the detail pane.
        // The tilt and the specular sweep are written STRAIGHT TO THE NODE as CSS
        // variables rather than held in state. A pointer-rate `setState` re-renders
        // the row ~60 times a second to move a decoration, and this row is the one
        // component in the app that can be on screen fifty times at once.
        onMouseMove={(e) => {
          const el = e.currentTarget;
          const r = el.getBoundingClientRect();
          const mx = ((e.clientX - r.left) / r.width) * 100;
          const my = ((e.clientY - r.top) / r.height) * 100;
          // /46 caps the tilt near one degree. A list is scanned DOWN a column, so
          // a row that visibly swings as the pointer crosses it fights the scan it
          // is meant to serve — felt, not seen.
          el.style.setProperty(
            "--card-tilt",
            `perspective(900px) rotateX(${(50 - my) / 46}deg) rotateY(${(mx - 50) / 46}deg) translateZ(4px)`
          );
          el.style.setProperty("--card-sheen-x", `${mx}%`);
          el.style.setProperty("--card-sheen-y", `${my}%`);
        }}
        onMouseLeave={(e) => e.currentTarget.style.removeProperty("--card-tilt")}
        // ONE attribute for the row's state, because `.cx-task-card`'s rules key
        // off it and a row is only ever in one of these. Checked outranks
        // selected, matching the old tint order: a ⌘/⇧-picked row reads a step
        // stronger than the merely focused one, or "one row is selected" and "one
        // row of this set is the one you're reading" look identical and the bulk
        // bar is the only thing left saying which you're in.
        //
        // Selection only. A running task is NOT a card state — the status pill
        // already says "In progress" in accent, and marking the row as well says
        // one thing twice, louder.
        data-card-state={checked ? "checked" : selected ? "selected" : undefined}
        className={cn(
          "group relative flex cursor-default select-none items-start gap-2.5 py-2.5 pl-4 pr-3",
          "cx-task-card",
          flashing && "cx-row-flash",
          dragging && "cx-dragging",
          dropTarget && "cx-drop-above"
        )}
      >
        {/* Priority as a left edge bar: the whole column reads as one scale in a
            single glance, which a chip buried mid-row cannot do. Redundant with
            the P chip on purpose — the bar is the scannable form, the chip the
            one that survives being colour-blind or read aloud. Transparent at P3,
            so an untriaged list carries no marks at all. */}
        <span
          aria-hidden
          className={cn(
            "pointer-events-none absolute inset-y-2 left-0 w-[3px] rounded-full",
            PRIORITY_EDGE[task.priority]
          )}
        />
        {/* The leading SQUARE closes the task — Things 3's convention, and the
            reason the shape matters rather than being decoration.

            A circle in the leading slot is what Finder, Photos and Mail use for
            multi-select, so a circle that completed work was reading as "pick a
            few of these" and quietly closing rows instead. Moving completion out
            to a hover-only trailing button fixed the misfire and cost the thing
            a todo list is for: closing a task is THE verb, and it had ended up
            behind a hover on the far edge of the row.

            A square is a checkbox everywhere on this platform, including in the
            two todo apps this one is measured against. It reads as "tick when
            done" without a caption, which frees the whole row for the selection
            gesture the circle was being mistaken for — ⌘-click and ⇧-click,
            handled on the row itself below. One shape, one meaning, and neither
            of them hidden behind a hover. */}
        <button
          type="button"
          role="checkbox"
          aria-checked={closed}
          disabled={restorable}
          aria-label={
            closed ? `Reopen ${task.title}` : `Mark ${task.title} done`
          }
          title={
            restorable
              ? "Put this back before closing it"
              : closed
                ? "Reopen"
                : "Mark done"
          }
          onClick={(e) => {
            // Never let the row's own click handler see this — it would rewrite
            // the multi-select set on the way to closing a task, so ticking off
            // one row of six you had selected would silently drop the other five.
            e.stopPropagation();
            if (restorable) return;
            // Reopen is NOT `onComplete` again. `complete_task` is one-way — it
            // sets `done` unconditionally — so routing both through it means a
            // click on a closed recurring task spawns a fresh instance every
            // time, which looks like the list breeding rows on its own.
            if (closed) onReopen?.();
            else onComplete();
          }}
          className={cn(
            "mt-px flex size-[15px] shrink-0 items-center justify-center rounded-[4px] border transition-colors duration-75",
            restorable && "opacity-40",
            closed
              ? "border-success bg-success text-white"
              : "border-content-muted/60 hover:border-success hover:bg-success/10",
            restorable && "cursor-default hover:border-content-muted/60 hover:bg-transparent"
          )}
        >
          {closed && <Check size={10} strokeWidth={3} />}
        </button>

        <div className="cx-row-body flex min-w-0 flex-1 flex-col gap-1">
          <div className="cx-row-titleline flex min-w-0 items-baseline gap-2">
            {/* The speakable reference, next to the priority chip. Mono and
                boxed because it's an address, not a status — the border makes it
                read as a citable token you can say out loud, while staying quiet
                enough to ignore the rest of the time. Clicking it copies it:
                an address you can't take with you is half an address. */}
            {task.short_id !== null && (
              <ReferenceChip
                reference={formatShortId(task.short_id)!}
                className="cx-row-ref rounded border border-border-subtle bg-content/5 px-1 text-[10px] data-[copied]:border-success/50 data-[copied]:bg-success/10"
              />
            )}
            {/* P0..P2 show; P3 does not. This was `> 0`, which is exactly
                inverted under invariant 9 — it hid the chip on P0, the "drop
                everything" priority, and printed one on every untriaged P3. The
                tell was that `PRIORITY_COLORS[0]` (red) was unreachable dead
                code: index 0 could never render. */}
            {task.priority < PRIORITY_LOWEST && (
              <span
                className={cn(
                  "shrink-0 text-[11px] font-semibold tabular-nums",
                  PRIORITY_COLORS[task.priority]
                )}
              >
                P{task.priority}
              </span>
            )}
            <span
              className={cn(
                "truncate",
                closed ? "text-content-faint line-through" : "text-content"
              )}
            >
              {task.title}
            </span>
          </div>

          {/* Waiting on WHAT, and since when. The tripwire's last note is the
              runner's own words ("0/50 outside users — 4 signups, 0 paying"),
              which beats any badge; the others say the one fact the reason
              group can't: the date, the blocker, the written reason. */}
          {waitingSince && (
            <div
              className="cx-row-evidence truncate text-[11px] text-content-faint"
              data-testid="waiting-evidence"
            >
              {deferred
                ? `Starts ${formatDue(task.defer_until) ?? "later"}`
                : armed && tripwire?.last_note?.trim()
                  ? `Last check: ${tripwire.last_note.trim()}`
                  : armed
                    ? "Not checked yet"
                    : blocked
                      ? "Waiting on another task"
                      : (task.blocked_reason ?? "Blocked")}
              {` · filed ${formatAge(waitingSince) ?? "?"} ago`}
            </div>
          )}

          <div className="cx-row-meta flex items-center gap-2 text-[11px] text-content-faint">
            <span
              className={cn(
                "rounded px-1.5 py-px font-medium",
                statusClass(task.status)
              )}
            >
              {STATUS_LABELS[task.status]}
            </span>
            {task.project && !hideProject && (
              <span className="cx-row-tags truncate">#{task.project}</span>
            )}
            {/* Tags read like the project — same `#name` shape, one notch fainter.
                Two different axes ("where does this live" vs "what kind of thing
                is this"), so the weight difference is what keeps them from
                scanning as one list, while the shared `#` keeps both legible as
                labels. Deliberately after the project: a task has at most one, and
                tags are unbounded, so this order stops a heavily-tagged row from
                pushing its project off the line. */}
            {orderTagsByKind((tags ?? []).map((name) => ({ name }))).map(({ name: tag }) => (
              <TagMark key={tag} name={tag} className="cx-row-tags shrink-0 text-content-faint/70" />
            ))}
            {/* A HOLD, never a status (v27): dependency-blocking is DERIVED from
                `task_deps`, and a written reason is `blocked_reason`. Both draw
                the one badge, because on screen they are one fact — "this can't
                move yet" — and the hover says which. The reason is what stops
                "blocked" from being a dead end, so it is one hover away. */}
            {(blocked || task.blocked_reason) && (
              <span
                className="flex items-center gap-0.5 rounded bg-error/15 px-1 py-px font-medium text-error"
                title={task.blocked_reason ?? "Waiting on another task"}
              >
                <Ban size={9} />
                blocked
              </span>
            )}
            {/* Deferral is DERIVED from the clock, exactly like the blocked
                badge above — a date, never a status. Without this a deferred
                task in a project list or the Archive-adjacent views is
                indistinguishable from an actionable one, which is how planned
                work gets started early. Only a still-pending defer prints; a
                spent one means nothing. */}
            {deferred && (
              <span
                className="flex items-center gap-0.5 rounded bg-warning/15 px-1 py-px font-medium text-warning"
                title="Deferred — hidden from Open and Today until its start date"
              >
                <CalendarClock size={9} />
                {formatDue(task.defer_until)}
              </span>
            )}
            {/* A tripwire hides the task exactly as a deferral does, so it gets
                the deferral's badge treatment — derived from the row, only
                while ARMED. Tripped and errored tasks are back on the board and
                print nothing here; the detail pane says what happened. Red
                when a cadenced check has gone three intervals without an
                attempt: that is the runner-not-running tell, and a badge that
                looked calm through it would be lying. */}
            {/* A MONITOR is badged in every state, done included — it is
                the one thing that tells a standing check from a gate at a
                glance, and a done monitor is still being checked. */}
            {monitor ? (
              <span
                className={cn(
                  "flex items-center gap-0.5 rounded px-1 py-px font-medium",
                  tripwireUnchecked(task, tripwire)
                    ? "bg-error/15 text-error"
                    : "bg-accent/15 text-accent"
                )}
                title={describeTripwire(task, tripwire)}
                data-testid="monitor-badge"
              >
                <RefreshCw size={9} />
                monitor
              </span>
            ) : armed && (
              <span
                className={cn(
                  "flex items-center gap-0.5 rounded px-1 py-px font-medium",
                  tripwireUnchecked(task, tripwire)
                    ? "bg-error/15 text-error"
                    : "bg-warning/15 text-warning"
                )}
                title={describeTripwire(task, tripwire)}
                data-testid="tripwire-badge"
              >
                <Zap size={9} />
                tripwire
              </span>
            )}
            {task.recurrence && (
              <Repeat size={11} aria-label={`Repeats ${task.recurrence}`} />
            )}
            {task.repo_path && (
              <GitBranch
                size={11}
                aria-label="Has execution context"
                className="text-accent"
              />
            )}
            {due && !restorable && (
              <span
                className={cn("ml-auto shrink-0", overdue && "text-error font-medium")}
              >
                {due}
              </span>
            )}

            {/* The countdown is the whole point of a Trash with a clock on it —
                without it you can't tell what's about to go. It goes urgent in
                the last week so a task on its way out is visibly different from
                one deleted this morning. */}
            {daysLeft !== undefined && (
              <span
                className={cn(
                  "ml-auto shrink-0 tabular-nums",
                  daysLeft <= 7 && "font-medium text-error"
                )}
                title={`Permanently deleted in ${daysLeft} day${daysLeft === 1 ? "" : "s"}`}
              >
                {daysLeft === 0 ? "today" : `${daysLeft}d left`}
              </span>
            )}
          </div>
          {onDecide && (
            <div className="cx-row-decide min-w-0">
              <DecideLine title={task.title} onDecide={onDecide} />
            </div>
          )}
        </div>

        {/* Trailing cluster — the Trash and Archive verbs, and nothing else.
            The completion check that used to live here is gone: it was the
            trailing half of a swap that put selection on the leading circle, and
            with the leading control a square checkbox again there is no reason
            for the app's most-used verb to sit behind a hover on the far edge of
            the row. Two controls for one action would also mean a hover target
            that closes a task sitting a few pixels from the one that deletes it.

            Restore and permanent-delete stay hover-gated (and shown on the
            selected row, so they're reachable without a mouse) — they only exist
            in views you have to navigate to on purpose.
            `pointer-events-none` while hidden matters: an opacity-0 button is
            still clickable. */}
        {restorable && (
          <div
            className={cn(
              "flex shrink-0 items-start gap-0.5 pt-px transition-opacity duration-75",
              "pointer-events-none opacity-0 group-hover:pointer-events-auto group-hover:opacity-100",
              "focus-within:pointer-events-auto focus-within:opacity-100",
              selected && "pointer-events-auto opacity-100"
            )}
          >
            <button
              type="button"
              aria-label={`Put back ${task.title}`}
              title="Put back"
              onClick={(e) => {
                e.stopPropagation();
                onRestore?.();
              }}
              className="rounded p-0.5 text-content-faint hover:bg-accent/10 hover:text-accent"
            >
              <Undo2 size={12} />
            </button>

            {onDestroy && (
              <button
                type="button"
                aria-label={`Delete ${task.title} permanently`}
                title="Delete permanently"
                onClick={(e) => {
                  e.stopPropagation();
                  onDestroy();
                }}
                className="rounded p-0.5 text-content-faint hover:bg-error/15 hover:text-error"
              >
                <Trash2 size={11} />
              </button>
            )}
          </div>
        )}
      </div>
    </TaskContextMenu>
  );
}

/**
 * Milestone affordances on a section header.
 *
 * **A section with a target date IS a milestone** — there is no separate
 * milestone object to create, so the whole control surface is "give this phase a
 * date" and "mark it met". The date field only appears once the section has a
 * date or you press the flag, which keeps an ordinary grouping visually
 * identical to what it was before milestones existed.
 *
 * "Met" is a manual toggle rather than something derived from the task counts:
 * deriving it would silently un-complete a milestone the moment a follow-up task
 * is filed into the phase.
 */
function MilestoneControls({ section }: { section: Section }) {
  const setSectionTargetDate = useTaskStore((s) => s.setSectionTargetDate);
  const setSectionCompleted = useTaskStore((s) => s.setSectionCompleted);
  const [editing, setEditing] = useState(false);

  const met = section.completed_at !== null;
  const showDate = editing || section.target_date !== null;

  return (
    <div className="ml-auto flex items-center gap-1">
      {showDate && (
        <div className="w-52">
          <DateField
            value={section.target_date}
            onChange={(iso) => {
              void setSectionTargetDate(section.id, iso);
              if (!iso) setEditing(false);
            }}
            addLabel="Target date"
          />
        </div>
      )}

      {!showDate && (
        <button
          type="button"
          aria-label={`Make ${section.name} a milestone`}
          title="Give this phase a target date (that's what makes it a milestone)"
          onClick={() => setEditing(true)}
          className="opacity-0 transition-opacity group-hover:opacity-100"
        >
          <Flag size={11} className="text-content-faint hover:text-accent" />
        </button>
      )}

      {section.target_date !== null && (
        <button
          type="button"
          aria-label={met ? `Reopen milestone ${section.name}` : `Mark ${section.name} met`}
          title={met ? "Reopen this milestone" : "Mark this milestone met"}
          onClick={() => void setSectionCompleted(section.id, !met)}
          className={cn(
            "rounded p-0.5",
            met ? "text-success" : "text-content-faint hover:text-success"
          )}
        >
          <CheckCircle2 size={11} />
        </button>
      )}
    </div>
  );
}

/**
 * The Decide strip's answer line. One input, Enter to close: the call becomes
 * the task's handoff (`complete_task` with `result`, invariant 18), so the
 * next session reads what was decided rather than only that it was.
 *
 * Every pointer and key event stops here. The row above it selects on click
 * and drags on mousedown, and the list's shortcuts read keys — a line you
 * type into must not open the detail pane, start a drag, or fire a shortcut
 * mid-word (gotcha #16).
 */
function DecideLine({
  title,
  onDecide,
}: {
  title: string;
  onDecide: (answer: string) => void;
}) {
  const [answer, setAnswer] = useState("");
  return (
    <form
      className="mt-1 flex items-center gap-1.5"
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onSubmit={(e) => {
        e.preventDefault();
        const line = answer.trim();
        if (!line) return;
        onDecide(line);
        setAnswer("");
      }}
    >
      <input
        value={answer}
        onChange={(e) => setAnswer(e.target.value)}
        onKeyDown={(e) => e.stopPropagation()}
        placeholder="Your call, one line — Enter closes it and records the reason"
        aria-label={`Decide: ${title}`}
        className="min-w-0 flex-1 rounded border border-border bg-input px-1.5 py-0.5 text-[11px] placeholder:text-content-faint focus:outline-none"
      />
      <button
        type="submit"
        disabled={!answer.trim()}
        className="shrink-0 rounded bg-accent px-2 py-0.5 text-[11px] font-medium text-white hover:bg-accent-hover disabled:cursor-default disabled:opacity-40"
      >
        Decide
      </button>
    </form>
  );
}
