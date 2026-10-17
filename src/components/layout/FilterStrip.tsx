/**
 * The filter strip: everything the list is applying, said on screen.
 *
 * Its own row under the header rather than chips inside the search box, which
 * is what this used to be. A view's locked chip is a sentence ("Due later,
 * deferred, or on a tripwire"), and a sentence plus two filters plus the
 * controls does not fit a 30rem field — it wrapped to a second row at the
 * third chip, in an area, which is the common case.
 *
 * Three kinds of thing live here, and the distinction is the design:
 *
 * - **Locked chips** are the view's identity. No ×, because taking "Held back"
 *   off Waiting leaves a title making a claim the list no longer supports.
 * - **Filter pills** are one per (facet, operator), values joined — `Status
 *   is not Done, In review` rather than two pills saying half of it each.
 *   Clicking one edits it; × removes the whole pill.
 * - **Two icon buttons**, save and sort. Icons because a text button is what
 *   forced the second row; state is carried by colour and named in the title.
 */
import { Bookmark, ArrowUpDown, ChevronLeft, ChevronRight, Check, Lock } from "lucide-react";
import {
  CalendarClock,
  CalendarPlus,
  Flag,
  Folder,
  Hourglass,
  ListFilter,
  Tag as TagIcon,
  Wand2,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { STATUS_LABELS, tagLabel } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import { baselineFor, lockedChips, sameFilters, scopeKey } from "@/lib/viewScopes";
import { SORT_OPTIONS } from "@/types/task";
import type {
  ActiveFilter,
  FilterFacet,
  FilterOp,
  SortKey,
  TaskStatus,
  WaitingKind,
} from "@/types/task";

/**
 * Statuses the strip offers — all seven, `queued` included.
 *
 * Deliberately NOT `USER_STATUSES`, which exists to keep `queued` from being
 * *set* by hand (it's the runner's atomic claim key — invariant 16). Filtering
 * by it is a read: "what's sitting in the drain queue" is a legitimate
 * question, and hiding the option here would make it unanswerable for safety
 * reasons that only apply to writes.
 */
const FILTER_STATUSES: TaskStatus[] = [
  "inbox",
  "todo",
  "queued",
  "in_progress",
  "in_review",
  "done",
];

/** P0 first — highest priority (invariant 9), same order every list sorts by. */
const PRIORITIES = ["0", "1", "2", "3"] as const;

/** One name per row badge, so "Waiting is not Tripwire" takes off exactly the
 * rows wearing the tripwire badge. */
const WAITING_KINDS: { value: WaitingKind; label: string }[] = [
  { value: "deferred", label: "Deferred" },
  { value: "tripwire", label: "Tripwire" },
  { value: "blocked", label: "Blocked" },
  { value: "*", label: "Held back (any)" },
];

const FACET_LABELS: Record<FilterFacet, string> = {
  tag: "Tag",
  project: "Project",
  status: "Status",
  priority: "Priority",
  waiting: "Waiting",
  due: "Due date",
  created: "Created",
};

/** Today as a LOCAL `YYYY-MM-DD`. Not `toISOString().slice(0,10)`, which is
 * the UTC day and is yesterday/tomorrow for part of every local day. */
function todayLocal(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** `2026-08-03` → `Aug 3`. The `T00:00:00` suffix forces LOCAL parsing — a
 * bare date string parses as UTC midnight and renders as the previous day in
 * any western timezone (the `sections.target_date` lesson). */
function formatDay(value: string): string {
  const d = new Date(`${value}T00:00:00`);
  if (Number.isNaN(d.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(d);
}

function valueLabel(f: ActiveFilter): string {
  switch (f.facet) {
    case "tag":
      return tagLabel(f.value);
    case "status":
      return STATUS_LABELS[f.value as TaskStatus] ?? f.value;
    case "priority":
      return `P${f.value}`;
    case "waiting":
      return WAITING_KINDS.find((k) => k.value === f.value)?.label ?? f.value;
    case "due":
    case "created":
      return formatDay(f.value);
    default:
      return f.value;
  }
}

const OP_LABELS: Record<FilterOp, string> = {
  is: "is",
  is_not: "is not",
  before: "before",
  after: "after",
};

/** One pill per (facet, op), in the order the first of its chips was added. */
interface Pill {
  facet: FilterFacet;
  op: FilterOp;
  chips: ActiveFilter[];
}

function groupPills(filters: ActiveFilter[]): Pill[] {
  const out: Pill[] = [];
  for (const f of filters) {
    const found = out.find((p) => p.facet === f.facet && p.op === f.op);
    if (found) found.chips.push(f);
    else out.push({ facet: f.facet, op: f.op, chips: [f] });
  }
  return out;
}

type Pane = "menu" | FilterFacet;

export function FilterStrip() {
  const filters = useTaskStore((s) => s.filters);
  const view = useTaskStore((s) => s.view);
  const activeProject = useTaskStore((s) => s.activeProject);
  const projectRecords = useTaskStore((s) => s.projectRecords);
  const allTags = useTaskStore((s) => s.allTags);
  const addFilter = useTaskStore((s) => s.addFilter);
  const removeFilter = useTaskStore((s) => s.removeFilter);
  const setFilters = useTaskStore((s) => s.setFilters);
  const refresh = useTaskStore((s) => s.refresh);

  const sort = useUiStore((s) => s.sort);
  const setSort = useUiStore((s) => s.setSort);
  const viewFilters = useUiStore((s) => s.viewFilters);
  const saveViewFilters = useUiStore((s) => s.saveViewFilters);
  const forgetViewFilters = useUiStore((s) => s.forgetViewFilters);

  const [pane, setPane] = useState<Pane | null>(null);
  const [op, setOp] = useState<FilterOp>("is");
  const [sortOpen, setSortOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Click-outside closes both popovers. **Capture phase**, because Tauri's
  // injected `drag.js` registers a BUBBLE-phase `mousedown` on `document` and
  // calls `stopImmediatePropagation()` for anything landing in a drag region
  // (gotcha #15) — which is the whole header above this strip. A bubble
  // listener never sees those clicks, so the popover stayed open over the top
  // of the app with nothing on screen to say why.
  useEffect(() => {
    if (!pane && !sortOpen) return;
    const onDown = (e: MouseEvent) => {
      if (!containerRef.current?.contains(e.target as Node)) {
        setPane(null);
        setSortOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown, true);
    return () => document.removeEventListener("mousedown", onDown, true);
  }, [pane, sortOpen]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && (pane || sortOpen)) {
        e.stopPropagation();
        setPane(null);
        setSortOpen(false);
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [pane, sortOpen]);

  const listViewName = view === "gsd" ? "all" : view;
  const locked = lockedChips(listViewName, activeProject);
  const pills = groupPills(filters);
  const key = scopeKey(listViewName, activeProject, projectRecords);
  const saved = key === null ? undefined : viewFilters[key];
  const baseline = baselineFor(listViewName, activeProject);
  const effective = saved ?? baseline;
  const dirty = key !== null && !sameFilters(filters, effective);
  const hasSaved = saved !== undefined;

  const scopeName = activeProject ?? SCOPE_TITLES[listViewName] ?? listViewName;
  const saveTitle = dirty
    ? `Save these filters as the default for ${scopeName}`
    : hasSaved
      ? `${scopeName} opens with these filters — click to forget them`
      : `${scopeName} is on its baseline filters`;

  const active = (f: ActiveFilter) =>
    filters.some((x) => x.facet === f.facet && x.op === f.op && x.value === f.value);
  const toggle = (f: ActiveFilter) => {
    if (active(f)) void removeFilter(f);
    else void addFilter(f);
  };
  const openPane = (facet: FilterFacet, at: FilterOp = "is") => {
    setOp(facet === "due" || facet === "created" ? "before" : at);
    setPane(facet);
    setSortOpen(false);
  };

  return (
    <div
      ref={containerRef}
      className={cn(
        "relative flex shrink-0 flex-wrap items-center gap-1.5",
        "border-b border-border-subtle px-3 py-1.5"
      )}
    >
      {locked.map((l) => (
        <span
          key={l.label}
          title={l.hint ?? "Part of this view — it can't be removed"}
          className={cn(
            "flex shrink-0 cursor-default items-center gap-1 rounded-full px-2 py-px",
            "bg-content/8 text-[11px] text-content-secondary"
          )}
        >
          <Lock size={9} className="shrink-0 opacity-70" />
          {l.label}
        </span>
      ))}

      {pills.map((p) => {
        const isSaved =
          saved?.some((f) => f.facet === p.facet && f.op === p.op) ?? false;
        return (
          <span
            key={`${p.facet}:${p.op}`}
            className={cn(
              "flex shrink-0 items-center gap-1 rounded-full bg-accent/15 px-2 py-px",
              "text-[11px] text-accent"
            )}
          >
            <button
              type="button"
              onClick={() => openPane(p.facet, p.op)}
              title={`Edit this ${FACET_LABELS[p.facet].toLowerCase()} filter`}
              className="flex items-center gap-1"
            >
              <span className="opacity-85">{FACET_LABELS[p.facet]}</span>
              <span className="italic opacity-70">{OP_LABELS[p.op]}</span>
              <span className="font-medium">
                {p.chips.map((c) => valueLabel(c)).join(", ")}
              </span>
              {isSaved && (
                <span
                  title="Part of this view's saved default"
                  className="h-1 w-1 shrink-0 rounded-full bg-accent/70"
                />
              )}
            </button>
            <button
              type="button"
              aria-label={`Remove filter ${FACET_LABELS[p.facet]} ${OP_LABELS[p.op]} ${p.chips
                .map((c) => valueLabel(c))
                .join(", ")}`}
              onClick={() => {
                for (const c of p.chips) void removeFilter(c);
              }}
              className="rounded-full hover:bg-accent/20"
            >
              <X size={10} />
            </button>
          </span>
        );
      })}

      <button
        type="button"
        onClick={() => (pane ? setPane(null) : openPane("status"))}
        aria-expanded={pane !== null}
        className={cn(
          "flex shrink-0 items-center gap-1 rounded-full border border-dashed border-border",
          "px-2 py-px text-[11px] text-content-secondary hover:border-content-faint hover:text-content"
        )}
      >
        <ListFilter size={10} />
        Filter
      </button>

      {/* Save sits HERE, beside the control that changes what it saves —
          not out at the right edge with sort, which is about reading order. */}
      <button
        type="button"
        disabled={key === null || (!dirty && !hasSaved)}
        title={saveTitle}
        aria-label={saveTitle}
        onClick={() => {
          if (key === null) return;
          if (dirty) saveViewFilters(key, filters);
          else if (hasSaved) forgetViewFilters(key);
          // The sidebar badges count under the SAVED chips, so a save or a
          // forget changes a number the list itself did not — re-count.
          void refresh();
        }}
        className={cn(
          "flex shrink-0 items-center rounded p-0.5 transition-colors",
          dirty
            ? "text-accent hover:bg-accent/15"
            : hasSaved
              ? "text-content-secondary hover:bg-content/8"
              : "text-content-faint",
          key === null || (!dirty && !hasSaved) ? "cursor-default" : "cursor-pointer"
        )}
      >
        <Bookmark size={13} fill={!dirty && hasSaved ? "currentColor" : "none"} />
      </button>

      <div className="relative ml-auto flex shrink-0 items-center gap-1">
        {hasSaved && !dirty && (
          <button
            type="button"
            onClick={() => void setFilters(baseline)}
            className="rounded px-1.5 py-0.5 text-[11px] text-content-faint hover:bg-content/8 hover:text-content"
          >
            Reset
          </button>
        )}
        <button
          type="button"
          aria-expanded={sortOpen}
          title={
            sort.key === "default"
              ? "Sorted the way this view sorts — click to change"
              : `Sorted by ${SORT_OPTIONS.find((o) => o.value === sort.key)?.label}${
                  sort.dir === "asc" ? " ↑" : " ↓"
                } — click to change`
          }
          aria-label="Sort"
          onClick={() => {
            setSortOpen((v) => !v);
            setPane(null);
          }}
          className={cn(
            "flex items-center rounded p-0.5 transition-colors",
            sort.key === "default"
              ? "text-content-faint hover:bg-content/8 hover:text-content"
              : "text-accent hover:bg-accent/15"
          )}
        >
          <ArrowUpDown size={13} />
        </button>
        {sortOpen && (
          <div
            className={cn(
              "absolute right-0 top-full z-50 mt-1 w-56 rounded-md border border-border",
              "bg-elevated p-1 shadow-lg"
            )}
          >
            <p className="px-2 pb-1 pt-0.5 text-[11px] uppercase tracking-wider text-content-faint">
              Sort by
            </p>
            {SORT_OPTIONS.map((o) => (
              <MenuRow
                key={o.value}
                label={o.value === "default" ? "Default (this view's order)" : o.label}
                on={sort.key === o.value}
                onClick={() => {
                  void setSort({ ...sort, key: o.value as SortKey });
                  setSortOpen(false);
                  void useTaskStore.getState().refresh();
                }}
              />
            ))}
            <div className="my-1 border-t border-border-subtle" />
            {(["asc", "desc"] as const).map((d) => (
              <MenuRow
                key={d}
                label={d === "asc" ? "Ascending" : "Descending"}
                on={sort.dir === d}
                disabled={sort.key === "default"}
                onClick={() => {
                  void setSort({ ...sort, dir: d });
                  setSortOpen(false);
                  void useTaskStore.getState().refresh();
                }}
              />
            ))}
            <p className="px-2 pb-0.5 pt-1.5 text-[11px] leading-snug text-content-faint">
              Deferred and tripwire tasks stay at the bottom of their section
              under every sort. Dragging to reorder needs Default.
            </p>
          </div>
        )}
      </div>

      {pane && (
        <div
          className={cn(
            "absolute left-3 top-full z-50 mt-1 w-60 rounded-md border border-border",
            "bg-elevated p-1 shadow-lg"
          )}
        >
          {pane === "menu" ? (
            <FacetMenu
              onPick={(f) => openPane(f)}
              hasTags={allTags.length > 0}
              hasProjects={projectRecords.length > 0}
            />
          ) : (
            <>
              <button
                type="button"
                onClick={() => setPane("menu")}
                className={cn(
                  "mb-1 flex w-full items-center gap-1 rounded px-2 py-1 text-left",
                  "text-[11px] uppercase tracking-wider text-content-faint hover:bg-content/5"
                )}
              >
                <ChevronLeft size={11} />
                Filters
                <span className="ml-1 normal-case tracking-normal text-content-secondary">
                  {FACET_LABELS[pane]}
                </span>
              </button>
              <OpToggle
                facet={pane}
                op={op}
                onChange={setOp}
              />
              {pane === "tag" && (
                <ValueList
                  values={allTags.map((t) => ({ value: t.name, label: tagLabel(t.name) }))}
                  isActive={(v) => active({ facet: "tag", op, value: v })}
                  onToggle={(v) => toggle({ facet: "tag", op, value: v })}
                  empty="No tags yet"
                />
              )}
              {pane === "project" && (
                <ValueList
                  values={projectRecords.map((p) => ({ value: p.name, label: p.name }))}
                  isActive={(v) => active({ facet: "project", op, value: v })}
                  onToggle={(v) => toggle({ facet: "project", op, value: v })}
                  empty="No projects yet"
                />
              )}
              {pane === "status" && (
                <ValueList
                  values={FILTER_STATUSES.map((s) => ({ value: s, label: STATUS_LABELS[s] }))}
                  isActive={(v) => active({ facet: "status", op, value: v })}
                  onToggle={(v) => toggle({ facet: "status", op, value: v })}
                />
              )}
              {pane === "priority" && (
                <ValueList
                  values={PRIORITIES.map((p) => ({
                    value: p,
                    label: p === "0" ? "P0 — highest" : p === "3" ? "P3 — lowest" : `P${p}`,
                  }))}
                  isActive={(v) => active({ facet: "priority", op, value: v })}
                  onToggle={(v) => toggle({ facet: "priority", op, value: v })}
                />
              )}
              {pane === "waiting" && (
                <ValueList
                  values={WAITING_KINDS.map((k) => ({ value: k.value, label: k.label }))}
                  isActive={(v) => active({ facet: "waiting", op, value: v })}
                  onToggle={(v) => toggle({ facet: "waiting", op, value: v })}
                />
              )}
              {(pane === "due" || pane === "created") && (
                <DatePane
                  facet={pane}
                  op={op === "after" ? "after" : "before"}
                  onAdd={(f) => {
                    void addFilter(f);
                    setPane(null);
                  }}
                />
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

const SCOPE_TITLES: Record<string, string> = {
  inbox: "Inbox",
  today: "Today",
  waiting: "Waiting",
  in_review: "In Review",
  open: "Open",
  all: "All Open",
  done: "Done",
  archive: "Archive",
  trash: "Trash",
};

function MenuRow({
  label,
  on,
  disabled,
  onClick,
}: {
  label: string;
  on: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      aria-checked={on}
      role="menuitemradio"
      onClick={onClick}
      className={cn(
        "flex w-full items-center gap-2 rounded px-2 py-1 text-left",
        on ? "text-accent" : "text-content-secondary hover:bg-content/5",
        disabled && "cursor-default opacity-40 hover:bg-transparent"
      )}
    >
      <span className="flex-1">{label}</span>
      {on && <Check size={13} className="shrink-0" />}
    </button>
  );
}

/**
 * `is` / `is not`, on top of every value pane.
 *
 * The date facets keep `before`/`after` instead — the same control saying the
 * thing a date bound actually means, which is why `FilterOp` has four members
 * rather than a separate negation flag.
 */
function OpToggle({
  facet,
  op,
  onChange,
}: {
  facet: FilterFacet;
  op: FilterOp;
  onChange: (op: FilterOp) => void;
}) {
  const options: FilterOp[] =
    facet === "due" || facet === "created" ? ["before", "after"] : ["is", "is_not"];
  return (
    <div className="flex gap-1 px-1 pb-1.5">
      {options.map((o) => (
        <button
          key={o}
          type="button"
          aria-pressed={op === o}
          onClick={() => onChange(o)}
          className={cn(
            "flex-1 rounded border px-2 py-0.5 text-[11px]",
            op === o
              ? "border-accent bg-accent/15 text-accent"
              : "border-border text-content-secondary hover:bg-content/5"
          )}
        >
          {OP_LABELS[o]}
        </button>
      ))}
    </div>
  );
}

function FacetMenu({
  onPick,
  hasTags,
  hasProjects,
}: {
  onPick: (facet: FilterFacet) => void;
  hasTags: boolean;
  hasProjects: boolean;
}) {
  const rows: { facet: FilterFacet; icon: typeof TagIcon; show: boolean }[] = [
    { facet: "status", icon: Wand2, show: true },
    { facet: "priority", icon: Flag, show: true },
    { facet: "waiting", icon: Hourglass, show: true },
    { facet: "tag", icon: TagIcon, show: hasTags },
    { facet: "project", icon: Folder, show: hasProjects },
    { facet: "due", icon: CalendarClock, show: true },
    { facet: "created", icon: CalendarPlus, show: true },
  ];
  return (
    <>
      <p
        className={cn(
          "flex items-center gap-1 px-2 pb-1 pt-0.5 text-[11px] uppercase",
          "tracking-wider text-content-faint"
        )}
      >
        <ListFilter size={11} />
        Filter by
      </p>
      {rows
        .filter((r) => r.show)
        .map((r) => {
          const Icon = r.icon;
          return (
            <button
              key={r.facet}
              type="button"
              onClick={() => onPick(r.facet)}
              className={cn(
                "flex w-full items-center gap-2 rounded px-2 py-1 text-left",
                "text-content-secondary hover:bg-content/5"
              )}
            >
              <Icon size={13} className="shrink-0 text-content-muted" />
              <span className="flex-1">{FACET_LABELS[r.facet]}</span>
              <ChevronRight size={12} className="text-content-faint" />
            </button>
          );
        })}
    </>
  );
}

function ValueList({
  values,
  isActive,
  onToggle,
  empty,
}: {
  values: { value: string; label: string }[];
  isActive: (value: string) => boolean;
  onToggle: (value: string) => void;
  empty?: string;
}) {
  if (values.length === 0) {
    return <p className="px-2 py-1 text-content-faint">{empty ?? "Nothing here"}</p>;
  }
  return (
    <div className="max-h-64 overflow-y-auto">
      {values.map((v) => {
        const on = isActive(v.value);
        return (
          <button
            key={v.value}
            type="button"
            aria-pressed={on}
            onClick={() => onToggle(v.value)}
            className={cn(
              "flex w-full items-center gap-2 rounded px-2 py-1 text-left",
              on ? "text-accent" : "text-content-secondary hover:bg-content/5"
            )}
          >
            <span className="flex-1 truncate">{v.label}</span>
            {on && <Check size={13} className="shrink-0" />}
          </button>
        );
      })}
    </div>
  );
}

/**
 * Before/after date entry for one facet.
 *
 * The input is SEEDED with today rather than starting empty, because an empty
 * `type="date"` in WKWebView paints a plausible grey sample date instead of a
 * placeholder — the exact looks-like-data trap `DateField` documents. Seeding
 * means the field always shows a value that IS the value.
 */
function DatePane({
  facet,
  op,
  onAdd,
}: {
  facet: "due" | "created";
  op: "before" | "after";
  onAdd: (f: ActiveFilter) => void;
}) {
  const [date, setDate] = useState(todayLocal);
  return (
    <div className="flex flex-col gap-1.5 p-1">
      <input
        type="date"
        value={date}
        onChange={(e) => setDate(e.target.value)}
        className="rounded border border-border bg-input px-2 py-1 text-content"
      />
      <button
        type="button"
        disabled={!date}
        onClick={() => onAdd({ facet, op, value: date })}
        className={cn(
          "rounded bg-accent px-2 py-1 text-white disabled:opacity-50",
          "hover:bg-accent/90"
        )}
      >
        Add filter
      </button>
    </div>
  );
}
