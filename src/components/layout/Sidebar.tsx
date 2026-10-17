import {
  Archive,
  CalendarClock,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  CheckCheck,
  CornerDownRight,
  Folder,
  FolderGit2,
  FolderInput,
  Eye,
  Hourglass,
  Layers,
  Inbox,
  ListTodo,
  Palette,
  Pencil,
  Sun,
  Plus,
  RotateCcw,
  Tag as TagIcon,
  AtSign,
  Bot,
  Scale,
  Target,
  Trash2,
  Wand2,
} from "lucide-react";
import { Fragment, useState, type DragEventHandler } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import {
  Content as MenuContent,
  Item as MenuItem,
  Label as MenuLabel,
  ListMenu,
  Root as MenuRoot,
  Separator as MenuSeparator,
  SubMenu,
  Trigger as MenuTrigger,
} from "@/components/shared/Menu";
import { DateField } from "@/components/shared/DateField";
import { BudgetPanel } from "@/components/layout/BudgetPanel";
import { ProjectReposDialog } from "@/components/layout/ProjectReposDialog";
import { activeTagsOf, useTaskStore } from "@/stores/taskStore";
import { orderViews, useUiStore } from "@/stores/uiStore";
import type { Project, ViewName } from "@/types/task";
import { hueFor, hueStyle, type Hue } from "@/lib/hues";
import { NodeIcon } from "@/lib/nodeIcons";
import { DECISION_TAG, orderTagsByKind, tagKind } from "@/lib/format";

/**
 * The sidebar's default glyph per tag KIND, so a person and the two system
 * tags read as different things from a label before you read the word. An
 * icon set by hand ("Icon & Colour…") still wins — this is only the fallback.
 */
function tagKindIcon(name: string) {
  const kind = tagKind(name);
  if (kind === "person") return AtSign;
  if (kind === "system") return name.toLowerCase() === DECISION_TAG ? Scale : Bot;
  return TagIcon;
}
import { IconPicker, type IconTarget } from "@/components/layout/IconPicker";

/** What's currently being renamed inline — set by double-click or by the
 * native right-click menu's "Rename" item (which can't do the rename itself,
 * so it emits `sidebar-rename-start` and this listens for it). */
type Renaming = { kind: "project" | "tag"; original: string };

type ViewRow = {
  key: ViewName;
  label: string;
  icon: typeof Inbox;
  /** The icon's colour. Fixed per view, and chosen for what the view IS: blue
   * for the inbox you file into, amber for the day, sand for waiting, green for
   * done, grey for put away. */
  hue: Hue;
  countKey?: "inbox" | "today" | "waiting" | "in_review";
  /**
   * Other views this row stays lit for, because they are reached from *inside*
   * it rather than from the sidebar.
   *
   * Without this the nav goes blank the moment you switch to the Trash segment
   * — nothing highlighted, so the sidebar stops answering "where am I".
   */
  covers?: ViewName[];
};

/**
 * The views you work in, in the order they ship.
 *
 * **The default order, not the order** — this list is a sort domain that
 * `uiStore.viewOrder` permutes (see `orderViews`). Which views exist is always
 * this constant's answer; storage only ever says in what sequence. So adding a
 * row here is enough: it appears for everyone, including anyone who has already
 * dragged their sidebar into a different shape.
 */
const VIEWS: ViewRow[] = [
  // First, above Inbox: the board is the question you ask before "what's on my
  // plate today" — which projects are moving and which have gone quiet.
  { key: "gsd", label: "GSD", icon: Target, hue: "red" },
  { key: "inbox", label: "Inbox", icon: Inbox, hue: "blue", countKey: "inbox" },
  { key: "today", label: "Today", icon: Sun, hue: "amber", countKey: "today" },
  // What is held back and by what — a start date, a tripwire, another task, a
  // written block. The ONE "not yet" list: Upcoming used to sit above this row
  // and held almost exactly the same tasks (25 of its 28, measured), so the
  // sidebar was asking the same question twice and neither answer was whole.
  // Oldest first, because age is what this list exists to show.
  { key: "waiting", label: "Waiting", icon: Hourglass, hue: "sand", countKey: "waiting" },
  { key: "in_review", label: "In Review", icon: Wand2, hue: "purple", countKey: "in_review" },
  // "Open" is everything not HELD. A future due date is a deadline, not a
  // start date, so dated work belongs here from the day it is filed — hiding
  // it until the morning it falls due is how a deadline becomes a surprise.
  // `View::All` still exists backend-side for project drill-downs.
  { key: "open", label: "Open", icon: ListTodo, hue: "teal" },
  { key: "done", label: "Done", icon: CheckCircle2, hue: "green" },
];

/**
 * Views that sit below the divider, in a fixed order you cannot drag.
 *
 * The divider is a claim — work lives above this line, and below it is where
 * you go to get something back — so a row that could be dragged across it would
 * turn a boundary into a line that happens to sit above one particular row.
 * These are anchored instead, and they carry no drag handlers at all, so there
 * is no gesture that appears to work and then refuses.
 *
 * Archive and Trash are ONE destination with two segments inside it (see
 * `TaskList`'s hidden header), not two rows. Two permanent rows was a lot of
 * standing furniture for two places you only visit when something has gone
 * wrong or gone away — and the segmented control puts the distinction where
 * it's actually legible: side by side, at the moment you're looking at
 * set-aside work and deciding which kind it is.
 */
const PINNED_VIEWS: ViewRow[] = [
  { key: "archive", label: "Archive", icon: Archive, hue: "grey", covers: ["trash"] },
];

/** Sort domain for `orderViews` — the reorderable keys, in shipping order. */
const VIEW_KEYS = VIEWS.map((v) => v.key);

export function Sidebar() {
  /**
   * The inline "new node" field, or null when it's closed.
   *
   * `parent` is what makes one input serve both affordances: the header's `+`
   * opens it with `null` (a top-level AREA — the sidebar's outer list is areas,
   * and an unclassified node must never start demanding a completion date),
   * while an area's "New project inside…" opens it with that area's name and
   * files a PROJECT under it. One field, one commit path, no way for the two to
   * drift.
   */
  const [newProject, setNewProject] = useState<{
    value: string;
    parent: string | null;
  } | null>(null);
  const [projectError, setProjectError] = useState<string | null>(null);
  /**
   * Names created in this session. A new node is empty by definition, so
   * without this it would vanish the moment you clicked away from it — with no
   * row left to drop anything on (2026-09-26, "Products"). Deliberately NOT
   * persisted: it is a grace period for a node you just made, and a stored
   * copy would become a setting that quietly undoes hiding.
   */
  const [justCreated, setJustCreated] = useState<string[]>([]);
  /** Project whose target date is being edited inline, or null. */
  const [datingProject, setDatingProject] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<Renaming | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [renameError, setRenameError] = useState<string | null>(null);
  const [dragProject, setDragProject] = useState<string | null>(null);
  /**
   * Where a dragged sidebar row would land: an insertion line above or below
   * the row under the pointer, or INTO it when that row is an area (see
   * `nestZone`).
   */
  const [dropProject, setDropProject] = useState<{
    name: string;
    zone: "above" | "below" | "into";
  } | null>(null);
  const [dragView, setDragView] = useState<ViewName | null>(null);
  const [dropView, setDropView] = useState<{
    key: ViewName;
    below: boolean;
  } | null>(null);
  const view = useTaskStore((s) => s.view);
  const activeProject = useTaskStore((s) => s.activeProject);
  const counts = useTaskStore((s) => s.counts);
  const setView = useTaskStore((s) => s.setView);
  const setProject = useTaskStore((s) => s.setProject);
  const projectRecords = useTaskStore((s) => s.projectRecords);
  const createProject = useTaskStore((s) => s.createProject);
  const renameProject = useTaskStore((s) => s.renameProject);
  const setProjectParent = useTaskStore((s) => s.setProjectParent);
  const setProjectKind = useTaskStore((s) => s.setProjectKind);
  const setProjectTargetDate = useTaskStore((s) => s.setProjectTargetDate);
  const setProjectClosed = useTaskStore((s) => s.setProjectClosed);
  const reorderProjects = useTaskStore((s) => s.reorderProjects);
  const deleteProject = useTaskStore((s) => s.deleteProject);
  const allTags = useTaskStore((s) => s.allTags);
  // Derived from the chips, not stored beside them: a tag picked in the
  // sidebar and one picked in the filter strip are the same filter, and while
  // they were two pieces of state each gesture silently cleared the other.
  const filters = useTaskStore((s) => s.filters);
  const activeTags = activeTagsOf(filters);
  const toggleTagFilter = useTaskStore((s) => s.toggleTagFilter);
  const selectOnlyTag = useTaskStore((s) => s.selectOnlyTag);
  const renameTag = useTaskStore((s) => s.renameTag);
  const deleteTag = useTaskStore((s) => s.deleteTag);
  const tagsCollapsed = useUiStore((s) => s.tagsCollapsed);
  const toggleTags = useUiStore((s) => s.toggleTags);
  const areasCollapsed = useUiStore((s) => s.areasCollapsed);
  const toggleAreas = useUiStore((s) => s.toggleAreas);
  const expandAreas = useUiStore((s) => s.expandAreas);
  const showHidden = useUiStore((s) => s.showHidden);
  const toggleShowHidden = useUiStore((s) => s.toggleShowHidden);
  const collapsedAreas = useUiStore((s) => s.collapsedAreas);
  const toggleArea = useUiStore((s) => s.toggleArea);
  const expandArea = useUiStore((s) => s.expandArea);
  const viewOrder = useUiStore((s) => s.viewOrder);
  const setViewOrder = useUiStore((s) => s.setViewOrder);
  /** Project whose repo mapping is open, or null. */
  const [reposFor, setReposFor] = useState<string | null>(null);
  const [iconFor, setIconFor] = useState<IconTarget | null>(null);

  /**
   * Tags worth showing: the ones with open work, plus any that are currently
   * filtering regardless of count.
   *
   * A tag nothing carries at all no longer exists — the backend collects it
   * (`db::tags::gc`), so nothing here has to hide those. What this drops is the
   * narrower case GC deliberately leaves alone: a tag whose tasks are all done
   * or archived. It is still real and still on those tasks, it just isn't open
   * work, and the sidebar is a filter over open work. It comes back by itself
   * the moment a task carrying it reopens.
   *
   * The `activeTags` exception is what stops this stranding you: filter by a
   * tag, close the last open task carrying it, and without that clause the row
   * vanishes mid-filter — leaving the board narrowed by something you can no
   * longer see or click off.
   */
  const visibleTags = orderTagsByKind(
    allTags.filter((t) => t.open_count > 0 || activeTags.includes(t.name))
  );

  /**
   * The view rows, in the user's order.
   *
   * Resolved at render from the constant rather than stored as rows, so the
   * saved order can only ever permute what this build ships — see `orderViews`.
   */
  const orderedViews = orderViews(viewOrder, VIEW_KEYS).map(
    (key) => VIEWS.find((v) => v.key === key)!
  );

  /**
   * The sidebar list, one level deep: areas with their projects underneath,
   * then any project belonging to no area.
   *
   * Standalone projects are kept VISIBLE at the bottom rather than mixed in
   * among the areas — a finite piece of work with no home is a real thing (and
   * often a thing to give a home), and sorting it in with the permanent buckets
   * is the flattening this whole split exists to undo.
   *
   * Nothing here re-derives a count: `open_count` already rolls up server-side
   * (`db::projects::expand_scope`), and a second roll-up in the renderer is how
   * the badge and the list it opens start disagreeing. The number PRINTED is
   * `badge_count` — the same roll-up under the node's own chips, counted by
   * the store through the one query the list runs — so an area that saved
   * "hide what is waiting" prints what that list will show. `open_count`
   * still decides whether the row is drawn, so a node whose every task is
   * held reads "0" rather than vanishing with its held rows unreachable.
   *
   * **Only a node with open work is drawn** — `visibleTags`' rule, one section
   * up. The sidebar is a filter over open work, and an area or project with
   * nothing open in it is a row that scopes the list to nothing: a finished
   * project, or one that has gone quiet. It comes back by itself the moment a task lands in it, and
   * the GSD board still lists every node — `empty` is one of the states it
   * exists to report. The `activeProject` exception is the same one tags
   * have: complete the last open task in the project you are standing in and
   * the row must not vanish from under the highlight.
   *
   * **Areas follow the same rule (2026-09-29, reversing 2026-09-26).** An empty
   * area is hidden like an empty project; right-click the Areas label →
   * "Show hidden" brings both back, which is also how you reach an empty area to
   * drop a project onto. Two things keep a node on screen without the toggle:
   * you are standing in it, or you made it this session (`justCreated`) — the
   * latter is what stops a new area vanishing the moment it exists. An area is
   * also drawn when anything beneath it is, because a row you are standing in
   * needs its parent to nest under.
   */
  const same = (a: string | null | undefined, b: string | null | undefined) =>
    (a ?? "").toLowerCase() === (b ?? "").toLowerCase();
  const worthShowing = (p: Project) =>
    p.open_count > 0 ||
    activeProject === p.name ||
    showHidden ||
    justCreated.some((n) => same(n, p.name));
  const areas = projectRecords.filter((p) => p.kind === "area");
  const byName = (name: string | null) =>
    name === null ? undefined : projectRecords.find((p) => same(p.name, name));
  /** Everything directly inside `area`, drawn or not — the tree's edges. */
  const allChildrenOf = (area: Project) =>
    projectRecords.filter((p) => p.parent !== null && same(p.parent, area.name));
  /**
   * Is this node drawn? A project by `worthShowing`; an area by that OR by
   * anything drawn beneath it. Bounded against a hand-edited cycle.
   */
  const isDrawn = (p: Project, guard = 0): boolean =>
    worthShowing(p) ||
    (p.kind === "area" &&
      guard < 32 &&
      allChildrenOf(p).some((c) => isDrawn(c, guard + 1)));
  /** What an area DRAWS beneath it. */
  const childrenOf = (area: Project) => allChildrenOf(area).filter((p) => isDrawn(p));
  /**
   * Is `node` inside `ancestor`, at any depth? Areas nest to any depth since
   * 2026-09-27, so a drop must never put a node inside its own subtree — that
   * is a cycle, and a cycle takes the whole loop out of the sidebar. The walk
   * is bounded, so a hand-edited cycle already in the data cannot hang it.
   */
  const isInside = (node: Project, ancestor: Project) => {
    let cur = byName(node.parent);
    for (let i = 0; cur && i < 32; i += 1) {
      if (cur.id === ancestor.id) return true;
      cur = byName(cur.parent);
    }
    return false;
  };
  /** The node's path for a menu label: "Products › Artist Advisory". */
  const pathOf = (node: Project) => {
    const parts = [node.name];
    let cur = byName(node.parent);
    for (let i = 0; cur && i < 32; i += 1) {
      parts.unshift(cur.name);
      cur = byName(cur.parent);
    }
    return parts.join(" › ");
  };
  // A parent that isn't in the list (archived, or a name that resolves to
  // nothing) must not take its children off the sidebar with it: such a node
  // is drawn at the top level instead of nowhere.
  const rootAreas = areas.filter((a) => byName(a.parent) === undefined && isDrawn(a));
  const homeless = projectRecords.filter(
    (p) => p.kind === "project" && byName(p.parent) === undefined && worthShowing(p)
  );
  const visibleAreaCount = rootAreas.length + homeless.length;

  /** Run a PARA mutation and surface the server's refusal, which is the only
   * place the rules live. */
  async function runProjectAction(action: Promise<string | null>) {
    const err = await action;
    if (err) setProjectError(err);
    else setProjectError(null);
  }

  // "Repositories…" opens a sheet, and both deletes raise a confirm — neither
  // of which an `NSMenu` item can do. The native menu therefore emitted an
  // intent (`sidebar-project-repos`, `sidebar-delete-project`, …) and a
  // `listen()` here translated it back into the real action. The menu is React
  // now, so it calls these directly and the round trip is gone.

  // The confirm spells out that the TASKS survive — that is invariant 11b, and
  // without saying so the word "delete" beside a project showing "4" reads as
  // deleting four tasks.
  async function confirmDeleteProject(name: string) {
    const ok = await confirm(
      `Delete the project "${name}"? Its tasks are kept — they just stop being grouped under it.`,
      { title: "Delete Project", kind: "warning", okLabel: "Delete" }
    );
    if (ok) await deleteProject(name);
  }

  // The opposite message, because the opposite thing happens to the tasks.
  // Deleting a project keeps them; deleting a tag reaches into every one of
  // them and takes the label off. So the confirm reads the real usage count
  // first: `open_count` from the sidebar would say "1 task" while unlabelling
  // nine finished ones, and there is no undo for this (see `deleteTag`).
  async function confirmDeleteTag(name: string) {
    let used = 0;
    try {
      used = await api.tags.usage(name);
    } catch {
      // Fall through to the vaguer wording rather than blocking the delete on a
      // count that is only there to describe it.
    }
    const scope =
      used === 0
        ? "It isn't on any tasks."
        : `It will be removed from ${used} task${used === 1 ? "" : "s"}.`;
    const ok = await confirm(
      `Delete the tag "${name}"? ${scope} The tasks themselves are kept, and this can't be undone.`,
      { title: "Delete Tag", kind: "warning", okLabel: "Delete" }
    );
    if (ok) await deleteTag(name);
  }

  function startRename(kind: "project" | "tag", original: string) {
    setRenaming({ kind, original });
    setRenameValue(original);
    setRenameError(null);
  }

  async function commitRename() {
    if (!renaming) return;
    const value = renameValue.trim();
    if (!value || value === renaming.original) {
      setRenaming(null);
      return;
    }
    const err =
      renaming.kind === "project"
        ? await renameProject(renaming.original, value)
        : await renameTag(renaming.original, value);
    if (err) setRenameError(err);
    else setRenaming(null);
  }

  function endProjectDrag() {
    setDragProject(null);
    setDropProject(null);
  }

  /**
   * Can `dragged` be nested inside `target` by dropping it there?
   *
   * The target must be an area, not the dragged row, not its current parent (a
   * drop that changes nothing), and not anywhere beneath the dragged row — that
   * would be a cycle. An area holding projects is welcome: areas nest to any
   * depth and carry everything under them (2026-09-27). The server enforces
   * all of this either way; checking it here is what stops the row offering a
   * drop that is certain to fail.
   */
  function canNest(dragged: string, target: Project) {
    if (target.kind !== "area") return false;
    const d = projectRecords.find((p) => p.name === dragged);
    if (!d || d.id === target.id) return false;
    if (same(d.parent, target.name)) return false;
    return !isInside(target, d);
  }

  /**
   * Nest `dragged` inside the area `target`, keeping what it is.
   *
   * An area stays an area — it goes in with everything under it. (Until
   * 2026-09-27 an area was demoted to a project on the way in, because areas
   * were always top-level; that is how pocketbuddy became a project.)
   */
  async function nestProject(dragged: string, target: Project) {
    const err = await setProjectParent(dragged, target.name);
    if (err) return setProjectError(err);
    setProjectError(null);
    // Land it where you can see it: a collapsed area would swallow the row
    // you just dropped, which reads as the drop having deleted it.
    expandArea(target.id);
  }

  /**
   * Apply a project drag-drop.
   *
   * Removes the dragged name FIRST and only then looks up the target's index,
   * which is what makes dragging up and dragging down both land where the
   * insertion line was drawn — computing the destination against the original
   * array is off by one in one of the two directions.
   */
  async function commitProjectDrop() {
    const order = projectRecords.map((p) => p.name);
    const drop = dropProject;
    const dragged = dragProject;
    endProjectDrag();
    if (!dragged || !drop || drop.name === dragged) return;
    const target = projectRecords.find((p) => p.name === drop.name);
    const d = projectRecords.find((p) => p.name === dragged);
    if (!target || !d) return;

    if (drop.zone === "into") {
      void nestProject(dragged, target);
      return;
    }

    // Dropping BETWEEN rows moves the node to that row's level. This is the
    // drag half of "pull it back out": drop Artist Advisory between two
    // top-level areas and it is top-level again. A row inside the dragged
    // subtree never offers a zone (see onDragOver), so this cannot loop.
    if (!same(d.parent, target.parent)) {
      const err = await setProjectParent(dragged, target.parent);
      if (err) return setProjectError(err);
      setProjectError(null);
    }

    const names = order.filter((n) => n !== dragged);
    const at = names.indexOf(drop.name);
    if (at === -1) return;
    names.splice(drop.zone === "below" ? at + 1 : at, 0, dragged);
    // A drop that changes nothing still costs a write and a re-render.
    // Compared element-wise rather than by joining: a delimiter is a bet that
    // no project name contains it, and project names are free text.
    if (names.every((n, i) => n === order[i])) return;
    await reorderProjects(names);
  }

  /**
   * Does `node`'s subtree hold the project you are standing in? A collapsed
   * area still draws the path down to it — the sidebar's job is to answer
   * "where am I", and hiding the one highlighted row leaves the whole nav with
   * nothing selected. Derived at render rather than written back into the
   * preference: a stored value that edits itself as you navigate is one you
   * stop trusting. Bounded against a hand-edited cycle.
   */
  function holdsActive(node: Project, guard = 0): boolean {
    if (activeProject === null || guard > 32) return false;
    if (node.name === activeProject) return true;
    return node.kind === "area" && allChildrenOf(node).some((c) => holdsActive(c, guard + 1));
  }

  /**
   * An area and everything drawn beneath it, one level deeper per step.
   *
   * Rendered BY the parent rather than filtered out of a flat map, so a node is
   * drawn exactly once — under its parent — at whatever depth it sits. The
   * count on each area's own row is unaffected by collapse: it already rolls up
   * every level server-side.
   */
  function renderArea(area: Project, depth: number, guard: number): React.ReactNode {
    if (guard > 32) return null;
    const children = childrenOf(area);
    const collapsed = collapsedAreas.includes(area.id);
    const shown = collapsed ? children.filter((c) => holdsActive(c)) : children;
    return (
      <Fragment key={area.id}>
        {projectRow(
          area,
          depth,
          // Nothing inside, no chevron: a disclosure over nothing is a control
          // that takes a click and changes nothing.
          children.length === 0
            ? undefined
            : { collapsed, onToggle: () => toggleArea(area.id) }
        )}
        {shown.map((child) =>
          child.kind === "area"
            ? renderArea(child, depth + 1, guard + 1)
            : <Fragment key={child.id}>{projectRow(child, depth + 1)}</Fragment>
        )}
        {newProject !== null &&
          newProject.parent !== null &&
          same(newProject.parent, area.name) && <li>{newProjectField}</li>}
      </Fragment>
    );
  }

  function endViewDrag() {
    setDragView(null);
    setDropView(null);
  }

  /**
   * Apply a view drag-drop.
   *
   * Same shape as `commitProjectDrop` — remove the dragged key FIRST, then look
   * up the target's index, which is what makes dragging up and dragging down
   * both land on the insertion line that was drawn. The one difference is where
   * it writes: there is no `views` table, so this is a `uiStore` preference
   * rather than a backend call, and there is nothing to roll back on failure.
   */
  function commitViewDrop() {
    const order = orderedViews.map((v) => v.key);
    const drop = dropView;
    const dragged = dragView;
    endViewDrag();
    if (!dragged || !drop || drop.key === dragged) return;

    const keys = order.filter((k) => k !== dragged);
    const at = keys.indexOf(drop.key);
    if (at === -1) return;
    keys.splice(drop.below ? at + 1 : at, 0, dragged);
    if (keys.every((k, i) => k === order[i])) return;
    setViewOrder(keys);
  }

  /**
   * One view row's button.
   *
   * Shared by the reorderable rows and the pinned ones so the two cannot drift
   * apart visually. A pinned row that looked even slightly different would read
   * as disabled rather than as anchored, which is the wrong claim — Archive is
   * perfectly clickable, it just doesn't move.
   */
  function viewButton(
    v: ViewRow,
    drag?: {
      draggable: true;
      onDragStart: DragEventHandler<HTMLButtonElement>;
      onDragEnd: () => void;
    }
  ) {
    const Icon = v.icon;
    const count = v.countKey ? counts[v.countKey] : 0;
    const selected =
      (view === v.key || (v.covers?.includes(view) ?? false)) && !activeProject;
    return (
      <button
        type="button"
        aria-current={selected ? "page" : undefined}
        onClick={() => void setView(v.key)}
        {...drag}
        className={cn(
          "flex w-full items-center gap-2 rounded-md px-2 py-[5px] text-left",
          "transition-colors duration-75",
          // Selection is a neutral ink lift, not the accent — `--selected` in
          // globals.css carries the reasoning. Only the FILL changed: the
          // label, icon and count keep the strengths they had on the blue
          // slab, spelled as ink so light mode isn't white-on-white.
          selected
            ? "bg-selected text-content"
            : "text-content-secondary hover:bg-content/5"
        )}
      >
        {/* Coloured whether or not the row is selected: the background lift
            already says "you are here", and an icon that drops to ink on
            selection would make the row you are on the least recognisable. */}
        <Icon size={14} className="shrink-0" style={hueStyle(v.hue)} />
        <span className="flex-1 truncate">{v.label}</span>
        {count > 0 && (
          <span
            className={cn(
              "tabular-nums text-[11px]",
              selected ? "text-content/80" : "text-content-faint"
            )}
          >
            {count}
          </span>
        )}
      </button>
    );
  }

  // Shared inline-edit row, swapped in for whichever project/tag button is
  // currently being renamed — only one can be active at a time.
  const renameField = (
    <div className="px-2 py-[3px]">
      <input
        autoFocus
        value={renameValue}
        onChange={(e) => setRenameValue(e.target.value)}
        onClick={(e) => e.stopPropagation()}
        onBlur={() => void commitRename()}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setRenaming(null);
          } else if (e.key === "Enter") {
            e.preventDefault();
            void commitRename();
          }
        }}
        className="w-full rounded border border-border bg-input px-1.5 py-1 text-[12px]"
      />
      {renameError && (
        <p className="pt-1 text-[10px] text-error">{renameError}</p>
      )}
    </div>
  );

  /**
   * The inline create field. One element for both affordances — see the
   * `newProject` state — so the header's `+` and an area's "New project
   * inside…" can never grow different validation or different commit paths.
   */
  const newProjectField = newProject === null ? null : (
    <div className={cn("py-[3px] pr-2", newProject.parent === null ? "pl-2" : "pl-6")}>
      <input
        autoFocus
        value={newProject.value}
        placeholder={
          newProject.parent === null ? "Area name…" : `Project in ${newProject.parent}…`
        }
        onChange={(e) =>
          setNewProject((n) => (n === null ? n : { ...n, value: e.target.value }))
        }
        onBlur={() => {
          if (!newProject.value.trim()) setNewProject(null);
        }}
        onKeyDown={async (e) => {
          if (e.key === "Escape") {
            setNewProject(null);
            setProjectError(null);
          } else if (e.key === "Enter" && newProject.value.trim()) {
            // The kind follows the SLOT, not a picker: the outer list is areas
            // and the nested list is projects, so where you opened the field
            // already said which you meant.
            const err = await createProject(
              newProject.value,
              newProject.parent === null ? "area" : "project",
              newProject.parent
            );
            if (err) setProjectError(err);
            else {
              // `createProject` selects what the server stored (trimmed,
              // spaces collapsed), so pin THAT, not what was typed.
              const stored = useTaskStore.getState().activeProject;
              if (stored) setJustCreated((n) => [...n, stored]);
              setNewProject(null);
              setProjectError(null);
            }
          }
        }}
        className="w-full rounded border border-border bg-input px-1.5 py-1 text-[12px]"
      />
    </div>
  );

  /**
   * One sidebar row — an area, or a project nested under one.
   *
   * The two share a renderer because they are the same affordance (click to
   * scope the list, drag to reorder, right-click to manage) and differ only in
   * what the menu can offer and how the row is marked. Splitting them into two
   * components is how the drag handling, the rename hook-up and the selected
   * styling end up subtly different for the half you touched last.
   *
   * `disclosure` is passed ONLY for an area that actually holds projects, and
   * that is the whole rule for whether a chevron appears: it replaces the folder
   * icon rather than sitting beside it, so the leading glyph answers "does this
   * hold anything" without costing a second column — and an area holding nothing
   * keeps its folder rather than growing a control that would change nothing.
   */
  function projectRow(
    p: Project,
    depth = 0,
    disclosure?: { collapsed: boolean; onToggle: () => void }
  ) {
    const selected = activeProject === p.name;
    const editingThis =
      renaming?.kind === "project" && renaming.original === p.name;
    const closed = p.closed_at !== null;
    // What the row wears when nothing is chosen: a PROJECT's target takes an
    // automatic colour, an area's folder stays ink — colouring every area made
    // the list read as confetti (Chris, 2026-09-27). A chosen icon or colour
    // (the Icon & Colour picker) overrides both; a closed project goes back to
    // ink, since colour is how the live ones are told apart.
    const customised = p.icon !== null || p.color !== null;
    const nodeIcon = (
      <NodeIcon
        icon={p.icon}
        color={p.color}
        fallback={p.kind === "area" ? Folder : Target}
        autoHue={p.kind === "project" ? hueFor(p.name) : null}
        size={p.kind === "area" ? 14 : 13}
        inkClass={selected ? "text-content" : "text-content-muted"}
        dim={closed}
      />
    );
    // Where this node may move: any area but itself and its own subtree.
    const moveTargets = areas.filter((a) => a.id !== p.id && !isInside(a, p));
    const moveMenu = (
      <SubMenu icon={FolderInput} label="Move to Area">
        {moveTargets.length === 0 && <MenuLabel>No areas yet</MenuLabel>}
        {moveTargets.map((a) => (
          <MenuItem
            key={a.id}
            label={pathOf(a)}
            disabled={same(p.parent, a.name)}
            onSelect={() => {
              expandArea(a.id);
              void runProjectAction(setProjectParent(p.name, a.name));
            }}
          />
        ))}
        <MenuSeparator />
        <MenuItem
          label="Top level"
          disabled={p.parent === null}
          onSelect={() => void runProjectAction(setProjectParent(p.name, null))}
        />
      </SubMenu>
    );
    /** The disclosure control, for an area that holds projects. */
    const Chevron = disclosure?.collapsed ? ChevronRight : ChevronDown;

    return (
      <li
        key={p.id}
        onDragOver={(e) => {
          if (!dragProject) return;
          // Nowhere inside the dragged subtree is a destination — every zone
          // there would be a cycle. No preventDefault, so the drop is refused
          // by the platform and the cursor says so.
          const dragged = projectRecords.find((x) => x.name === dragProject);
          if (dragged && dragged.id !== p.id && isInside(p, dragged)) {
            setDropProject(null);
            return;
          }
          e.preventDefault();
          // Where in the row the pointer is decides the drop. On a row that
          // can take the dragged one, the middle half nests it and the outer
          // quarters still reorder; everywhere else it is the half the
          // pointer is in, above or below. Without the edges, the last slot
          // in the list is unreachable — every drop would be "before the row
          // you're over" — and without the middle there is no way to put a
          // project into an area by hand at all.
          const box = e.currentTarget.getBoundingClientRect();
          const y = (e.clientY - box.top) / box.height;
          const nest = canNest(dragProject, p) && y > 0.25 && y < 0.75;
          setDropProject({
            name: p.name,
            zone: nest ? "into" : y > 0.5 ? "below" : "above",
          });
        }}
        onDrop={(e) => {
          e.preventDefault();
          void commitProjectDrop();
        }}
        className={cn(
          dragProject === p.name && "cx-dragging",
          dropProject?.name === p.name &&
            dragProject !== null &&
            dragProject !== p.name &&
            `cx-drop-${dropProject.zone}`
        )}
      >
        {editingThis ? (
          renameField
        ) : (
          <MenuRoot>
            {/* The row's PAINT lives on the trigger, not on the button inside
                it. The marker is its own element — a chevron button nested in
                the row button would be invalid HTML and swallow the row's own
                click — so the selected background has to belong to the thing
                that spans both, or the highlight stops short of the icon. */}
            <MenuTrigger
              className={cn(
                "flex items-center rounded-md transition-colors duration-75",
                selected
                  ? "bg-selected text-content"
                  : "text-content-secondary hover:bg-content/5"
              )}
              // One 16px step per level. A margin rather than padding, so the
              // selected highlight indents with the row and the tree reads as a
              // tree rather than as one slab with offset text.
              style={depth > 0 ? { marginLeft: depth * 16 } : undefined}
            >
              {/* The chevron is lifted OUT of the row button only when there is
                  one to draw. Everywhere else the marker stays inside it, so the
                  common row keeps a click target that runs the full width — a
                  30px dead zone in front of every project name is the price of
                  hoisting an icon that isn't a control. Its padding is the row's
                  left inset plus the 8px that is otherwise the flex gap, so the
                  label lands in the same column either way. */}
              {disclosure && (
                <button
                  type="button"
                  aria-expanded={!disclosure.collapsed}
                  aria-label={`${disclosure.collapsed ? "Show" : "Hide"} projects in ${p.name}`}
                  title={
                    disclosure.collapsed
                      ? `Show the projects in ${p.name}`
                      : `Hide the projects in ${p.name}`
                  }
                  onClick={disclosure.onToggle}
                  className={cn(
                    "shrink-0 py-[5px] pl-2 pr-2",
                    selected
                      ? "text-content/70 hover:text-content"
                      : "text-content-muted hover:text-content"
                  )}
                >
                  <Chevron size={14} />
                </button>
              )}
              <button
                type="button"
                draggable
                aria-current={selected ? "page" : undefined}
                onClick={() => void setProject(p.name)}
                onDragStart={(e) => {
                  // WebKit will not start a drag session carrying no data —
                  // see the same note on TaskRow. The payload is unused.
                  e.dataTransfer.setData("text/plain", p.name);
                  e.dataTransfer.effectAllowed = "move";
                  setDragProject(p.name);
                }}
                onDragEnd={endProjectDrag}
                onDoubleClick={(e) => {
                  e.stopPropagation();
                  startRename("project", p.name);
                }}
                className={cn(
                  "flex min-w-0 flex-1 items-center gap-2 py-[5px] pr-2 text-left",
                  disclosure ? "pl-0" : "pl-2",
                  // A closed project stays listed while it is still around, but
                  // reads as finished rather than as one more thing on the plate.
                  closed && !selected && "text-content-faint line-through"
                )}
              >
                {/* The icon is the kind, at a glance: a folder holds things, a
                    target is something you finish. An area with projects trades
                    the plain folder for the chevron above — which says the same
                    thing and can also put them away — but a CHOSEN icon is
                    drawn beside the chevron: it names the thing, not its kind,
                    and hiding it on exactly the areas that hold the most would
                    defeat choosing one. */}
                {(!disclosure || customised) && nodeIcon}
                <span className="flex-1 truncate">{p.name}</span>
                <span
                  className={cn(
                    "tabular-nums text-[11px]",
                    selected ? "text-content/80" : "text-content-faint"
                  )}
                >
                  {p.badge_count ?? p.open_count}
                </span>
              </button>
            </MenuTrigger>
            <MenuContent>
              <MenuItem
                icon={Pencil}
                label="Rename…"
                onSelect={() => startRename("project", p.name)}
              />
              {p.kind === "area" ? (
                <>
                  <MenuItem
                    icon={CornerDownRight}
                    label="New Project Inside…"
                    onSelect={() => {
                      // The field renders where the projects render, so a
                      // collapsed area has to open first — otherwise you type
                      // into a region that isn't on screen and the project you
                      // just made looks like it was never created.
                      expandArea(p.id);
                      setNewProject({ value: "", parent: p.name });
                      setProjectError(null);
                    }}
                  />
                  {moveMenu}
                  <MenuItem
                    icon={Target}
                    label="Make It a Project"
                    onSelect={() =>
                      void runProjectAction(setProjectKind(p.name, "project"))
                    }
                  />
                </>
              ) : (
                <>
                  {moveMenu}
                  <MenuItem
                    icon={CalendarClock}
                    label={p.target_date ? "Change Target Date…" : "Set Target Date…"}
                    onSelect={() => setDatingProject(p.name)}
                  />
                  <MenuItem
                    icon={closed ? RotateCcw : CheckCheck}
                    label={closed ? "Reopen Project" : "Close Project"}
                    onSelect={() =>
                      void runProjectAction(setProjectClosed(p.name, !closed))
                    }
                  />
                  <MenuItem
                    icon={Layers}
                    label="Make It an Area"
                    onSelect={() =>
                      void runProjectAction(setProjectKind(p.name, "area"))
                    }
                  />
                </>
              )}
              <MenuSeparator />
              <MenuItem
                icon={Palette}
                label="Icon & Colour…"
                onSelect={() =>
                  setIconFor({ kind: "project", name: p.name, icon: p.icon, color: p.color })
                }
              />
              <MenuItem
                icon={FolderGit2}
                label="Repositories…"
                onSelect={() => setReposFor(p.name)}
              />
              <MenuSeparator />
              <MenuItem
                icon={Trash2}
                danger
                label="Delete…"
                onSelect={() => void confirmDeleteProject(p.name)}
              />
            </MenuContent>
          </MenuRoot>
        )}

        {/* The date editor lives under the row rather than in a dialog: it is
            one field, and a modal for one field is how a target date stops
            being worth setting. */}
        {datingProject === p.name && (
          <div className="py-1 pr-2" style={{ paddingLeft: 8 + depth * 16 }}>
            <DateField
              value={p.target_date}
              addLabel="Target date"
              onChange={(iso) => {
                void runProjectAction(setProjectTargetDate(p.name, iso));
                if (!iso) setDatingProject(null);
              }}
            />
          </div>
        )}
      </li>
    );
  }

  return (
    <nav
      aria-label="Views"
      // The right edge belongs to this pane, and it has to be on the same
      // element as `bg-sidebar`: `--border-subtle` is 5% white, so drawn on the
      // unpainted wrapper in `AppLayout` it composited over raw blurred desktop
      // and read as a hairline slit through the window.
      className="flex h-full min-h-0 flex-col border-r border-border-subtle bg-sidebar"
    >
      {/* The traffic-light headroom, as its own non-scrolling strip rather than
          padding on the scroller below. Two reasons, both load-bearing: a drag
          region that lives inside an `overflow-y-auto` scrolls away with the
          content, so the window would only be draggable here at scroll-top; and
          rows passing under the floating traffic lights on scroll looks broken.
          `shrink-0` because it's fixed chrome (gotcha #10). */}
      <div
        data-tauri-drag-region="deep"
        className="shrink-0"
        style={{ height: "var(--titlebar-height)" }}
      />
      {/* Only the lists scroll. The budget panel below is pinned: it exists to
          make background spend impossible to miss, so it must not be the thing
          that scrolls off the bottom.

          It no longer dims for search or chips. It used to, because a chip
          REPLACED the view and the nav genuinely wasn't applying; chips refine
          the view now, so the selected row is exactly as true as it looks. */}
      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto px-3 pb-3">
      <ul className="flex flex-col gap-px">
        {orderedViews.map((v) => (
          <li
            key={v.key}
            onDragOver={(e) => {
              // Guarded on OUR drag state, not on the event: the sidebar is a
              // drop target for projects and for task rows too, and without this
              // a project dragged past the view list would paint insertion lines
              // through it and then drop into nothing.
              if (!dragView) return;
              e.preventDefault();
              // Which HALF of the row the pointer is in decides above vs below —
              // same as the project list. Without it the last slot is
              // unreachable, since every drop would mean "before this row".
              const box = e.currentTarget.getBoundingClientRect();
              setDropView({
                key: v.key,
                below: e.clientY > box.top + box.height / 2,
              });
            }}
            onDrop={(e) => {
              e.preventDefault();
              commitViewDrop();
            }}
            className={cn(
              dragView === v.key && "cx-dragging",
              dropView?.key === v.key &&
                dragView !== null &&
                dragView !== v.key &&
                (dropView.below ? "cx-drop-below" : "cx-drop-above")
            )}
          >
            {viewButton(v, {
              draggable: true,
              onDragStart: (e) => {
                // WebKit will not start a drag session carrying no data — see
                // the same note on TaskRow and the project rows. Payload unused.
                e.dataTransfer.setData("text/plain", v.key);
                e.dataTransfer.effectAllowed = "move";
                setDragView(v.key);
              },
              onDragEnd: endViewDrag,
            })}
          </li>
        ))}
        {PINNED_VIEWS.map((v, i) => (
          // No drag handlers at all — see `PINNED_VIEWS`. The divider is the
          // boundary these sit below, so they don't move and nothing moves past
          // them.
          <li
            key={v.key}
            className={cn(i === 0 && "mt-2 border-t border-border-subtle pt-2")}
          >
            {viewButton(v)}
          </li>
        ))}
      </ul>

      <div className="flex flex-col gap-px">
        <div className="flex items-center pb-1">
          <MenuRoot>
            <MenuTrigger>
              <button
                type="button"
                onClick={toggleAreas}
                aria-expanded={!areasCollapsed}
                title={areasCollapsed ? "Show areas" : "Hide areas"}
                className={cn(
                  "group flex min-w-0 flex-1 items-center gap-1 rounded-md px-2 pb-1 pt-px text-left",
                  "text-[11px] font-semibold uppercase tracking-wider text-content-faint",
                  "transition-colors duration-75 hover:text-content-secondary"
                )}
              >
                {areasCollapsed ? (
                  <ChevronRight size={11} className="shrink-0" />
                ) : (
                  <ChevronDown size={11} className="shrink-0" />
                )}
                <span>Areas</span>
                {areasCollapsed && (
                  <span className="ml-auto tabular-nums font-normal normal-case tracking-normal">
                    {activeProject ? "1 selected" : visibleAreaCount}
                  </span>
                )}
              </button>
            </MenuTrigger>
            <MenuContent>
              <MenuItem
                icon={Eye}
                label="Show hidden"
                checked={showHidden}
                onSelect={toggleShowHidden}
              />
            </MenuContent>
          </MenuRoot>
          <button
            type="button"
            aria-label="New area"
            title="New area — an ongoing responsibility. Use an area's menu to put a finite project inside it."
            onClick={() => {
              expandAreas();
              setNewProject({ value: "", parent: null });
              setProjectError(null);
            }}
            className="mr-2 rounded p-0.5 text-content-faint hover:bg-content/10 hover:text-content"
          >
            <Plus size={12} />
          </button>
        </div>

        {!areasCollapsed && (
          <>
        {newProject !== null && newProject.parent === null && newProjectField}


        {projectRecords.length === 0 && newProject === null && (
          <p className="px-2 py-1 text-content-faint">None yet — use +</p>
        )}
        <ul className="flex flex-col gap-px">
          {rootAreas.map((area) => renderArea(area, 0, 0))}
          {homeless.length > 0 && (
            <>
              {/* Not folded in among the areas: a finite project with no home
                  is a real state, usually a temporary one, and it should look
                  like an outstanding question rather than a sixth area. */}
              <li
                aria-hidden
                className="px-2 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint"
              >
                Projects
              </li>
              {homeless.map((p) => projectRow(p))}
            </>
          )}
        </ul>
        {projectError && (
          <p className="px-2 pt-1 text-[10px] text-error">{projectError}</p>
        )}
          </>
        )}
      </div>

        {visibleTags.length > 0 && (
          <div className="flex flex-col gap-px">
            {/* Tags can grow as agents file them. Keep their section toggle
                independent of Areas so each can be put away on its own. */}
            <button
              type="button"
              onClick={toggleTags}
              aria-expanded={!tagsCollapsed}
              title={tagsCollapsed ? "Show tags" : "Hide tags"}
              className={cn(
                "group flex w-full items-center gap-1 rounded-md px-2 pb-1 pt-px text-left",
                "text-[11px] font-semibold uppercase tracking-wider text-content-faint",
                "transition-colors duration-75 hover:text-content-secondary"
              )}
            >
              {tagsCollapsed ? (
                <ChevronRight size={11} className="shrink-0" />
              ) : (
                <ChevronDown size={11} className="shrink-0" />
              )}
              <span>Tags</span>
              {/* Collapsed, the count is the only thing left saying how many
                  there are — and it is also what tells you a filter is still
                  on when the row applying it is hidden. */}
              {tagsCollapsed && (
                <span className="ml-auto tabular-nums font-normal normal-case tracking-normal">
                  {activeTags.length > 0
                    ? `${activeTags.length} filtering`
                    : visibleTags.length}
                </span>
              )}
            </button>
            {/* Tags scroll INSIDE their own box rather than lengthening the
                sidebar's. The list is unbounded — it grows with every tag ever
                used — so without this it pushes Projects, and the budget panel
                below it, off the bottom of a short window, and the only way to
                reach either is to scroll past every tag. A nested scroller
                costs the outer one nothing here because the sidebar's own
                scroll is still what moves Views and Projects.
                `overscroll-contain` stops hitting the end of the tag list from
                handing the wheel to the sidebar and jumping the whole column. */}
            <ul
              className={cn(
                "max-h-56 flex-col gap-px overflow-y-auto overscroll-contain",
                tagsCollapsed ? "hidden" : "flex"
              )}
            >
              {visibleTags.map((t) => {
                const on = activeTags.includes(t.name);
                const editingThis =
                  renaming?.kind === "tag" && renaming.original === t.name;
                return (
                  <li key={t.id}>
                    {editingThis ? (
                      renameField
                    ) : (
                      <ListMenu
                        items={[
                          { label: "Rename Tag", icon: Pencil, onSelect: () => startRename("tag", t.name) },
                          {
                            label: "Icon & Colour…",
                            icon: Palette,
                            onSelect: () => setIconFor({ kind: "tag", name: t.name, icon: t.icon, color: t.color }),
                          },
                          { separator: true },
                          { label: "Delete Tag…", icon: Trash2, danger: true, onSelect: () => void confirmDeleteTag(t.name) },
                        ]}
                      >
                      <button
                        type="button"
                        aria-pressed={on}
                        title="Click to filter by this tag alone; Option-click to add it to the filter"
                        onClick={(e) => {
                          if (e.altKey) void toggleTagFilter(t.name);
                          else void selectOnlyTag(t.name);
                        }}
                        onDoubleClick={(e) => {
                          e.stopPropagation();
                          startRename("tag", t.name);
                        }}
                        className={cn(
                          "flex w-full items-center gap-2 rounded-md px-2 py-[5px] text-left",
                          "transition-colors duration-75",
                          on
                            ? "bg-selected text-content"
                            : "text-content-secondary hover:bg-content/5"
                        )}
                      >
                        <NodeIcon
                          icon={t.icon}
                          color={t.color}
                          fallback={tagKindIcon(t.name)}
                          autoHue={hueFor(t.name)}
                          size={13}
                        />
                        <span className="flex-1 truncate">{t.name}</span>
                        <span
                          className={cn(
                            "tabular-nums text-[11px]",
                            on ? "text-content/80" : "text-content-faint"
                          )}
                        >
                          {t.open_count}
                        </span>
                      </button>
                      </ListMenu>
                    )}
                  </li>
                );
              })}
            </ul>
            {activeTags.length > 1 && !tagsCollapsed && (
              <p className="px-2 pt-1 text-[10px] text-content-faint">
                Showing tasks with ALL {activeTags.length} tags
              </p>
            )}
          </div>
        )}
      </div>

      <BudgetPanel />

      <IconPicker target={iconFor} onClose={() => setIconFor(null)} />
      <ProjectReposDialog
        project={reposFor}
        onClose={() => setReposFor(null)}
      />
    </nav>
  );
}
