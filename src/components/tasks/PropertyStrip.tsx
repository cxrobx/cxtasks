import * as Dropdown from "@radix-ui/react-dropdown-menu";
import * as Popover from "@radix-ui/react-popover";
import { forwardRef, useEffect, useRef, useState } from "react";
import {
  Ban,
  CalendarCheck,
  CalendarClock,
  Check,
  Plus,
  Repeat,
  Rows3,
  Tag as TagIcon,
  Target,
  X,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Combobox } from "@/components/shared/Combobox";
import { TagMark } from "@/components/shared/TagMark";
import { DateField, defaultDate } from "@/components/shared/DateField";
import { HoldField } from "@/components/tasks/HoldField";
import { TripwireField } from "@/components/tasks/TripwireField";
import {
  describeRecurrence,
  describeTripwire,
  formatAge,
  formatDue,
  isDeferred,
  isOverdue,
  isTripwireArmed,
  isTripwireWatching,
  tripwireOutcomeLabel,
  PRIORITY_COLORS,
  STATUS_HINTS,
  STATUS_LABELS,
  statusClass,
  orderTagsByKind,
  tripwireUnchecked,
} from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import {
  PRIORITY_OPTIONS,
  USER_STATUSES,
  type Task,
  type TaskStatus,
  type TripwireRuntime,
} from "@/types/task";

/** The properties that can be absent — the ones the `+` menu adds. Status and
 *  priority always have a value, so they always have a chip. */
type Prop = "project" | "section" | "due" | "defer" | "repeat" | "tripwire" | "waiting";

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";
const LABEL = "text-[11px] font-medium uppercase tracking-wide text-content-faint";

/** OPAQUE, like `Menu`'s panel: these float over the glass, and a translucent
 *  panel there shows the wallpaper through it (gotcha #22). */
const PANEL = "z-50 rounded-lg border border-border bg-elevated shadow-2xl";
const MENU_ITEM =
  "flex w-full cursor-default select-none items-start gap-2 rounded-md px-2 py-1.5 text-left text-[13px] text-content-secondary outline-none data-[highlighted]:bg-accent data-[highlighted]:text-white";

const TONES = {
  neutral: "bg-content/6 text-content-secondary hover:bg-content/10",
  warning: "bg-warning/12 text-warning hover:bg-warning/18",
  error: "bg-error/12 text-error hover:bg-error/18",
  success: "bg-success/12 text-success hover:bg-success/18",
} as const;
type Tone = keyof typeof TONES;

const ADD_LABELS: Record<Prop, string> = {
  project: "Project",
  section: "Section",
  due: "Due date",
  defer: "Start date",
  repeat: "Repeat",
  tripwire: "Tripwire",
  waiting: "Waiting reason",
};

const ICONS: Record<Prop, typeof Zap> = {
  project: Target,
  section: Rows3,
  due: CalendarCheck,
  defer: CalendarClock,
  repeat: Repeat,
  tripwire: Zap,
  waiting: Ban,
};

/**
 * The task's properties as the ROW's own chips — only the ones that hold
 * something, plus a `+` for the rest.
 *
 * This replaced a two-column grid of nine labelled controls that rendered
 * whether or not they held anything. On an untriaged task seven of the nine
 * were empty, the two that weren't (status, priority) were already printed on
 * the row the task was opened from, and Notes — the thing you open a task to
 * read — started 580–710px down, below the fold of a laptop window. Here an
 * empty property costs nothing, a set one costs a chip in the vocabulary the
 * row already taught, and every editor opens in a popover so nothing below
 * the strip moves while you use it.
 *
 * **A held task must still read as held** — the contract `HoldField` exists
 * for. The start date, the tripwire and the written reason each keep the
 * row's warning/error tint, and an open blocker gets a Waiting chip of its own
 * even with no reason written, because it is the one hold nothing else in
 * this strip would show.
 *
 * The editors are the existing ones (`DateField`, `TripwireField`,
 * `HoldField`, `Combobox`), moved rather than rewritten: each already carries
 * its own draft/commit discipline and its own tests.
 *
 * **Tags ride here too** (2026-09-28), as the row prints them — `#billable`,
 * `@sam`. They were a headed section of their own below Notes, which on an
 * untagged task (most of them) was a heading over an empty dashed input. A
 * tag is metadata about the task exactly as the project is, so it sits beside
 * the project. Unlike a property it is MANY-valued, so `+ → Tag` is always on
 * offer and adds one at a time.
 */
export function PropertyStrip({ task }: { task: Task }) {
  const patch = useTaskStore((s) => s.patch);
  const sections = useTaskStore((s) => s.sections);
  const moveTaskToSection = useTaskStore((s) => s.moveTaskToSection);
  const projectRecords = useTaskStore((s) => s.projectRecords);
  const runtime = useTaskStore((s) => s.tripwireByTask[task.id]);
  const blockedBy = useTaskStore((s) => s.relations.blocked_by);
  // `tags` is the SELECTED task's set — this strip only ever renders for it.
  const tags = useTaskStore((s) => s.tags);
  const allTags = useTaskStore((s) => s.allTags);
  const setTaskTags = useTaskStore((s) => s.setTaskTags);

  /** The one popover (or menu) that is open. */
  const [open, setOpen] = useState<Prop | "status" | "priority" | "tag" | null>(null);
  /**
   * A property being ADDED: its chip renders before it holds a value, so the
   * editor has somewhere to anchor. Cleared a beat after its popover closes —
   * the close just flushed a commit that is still on its way through `patch`
   * → `refresh`, and clearing at once would drop the chip for that round trip
   * and pop it back.
   */
  const [adding, setAdding] = useState<Prop | "tag" | null>(null);
  const clearTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(clearTimer.current), []);

  const [project, setProject] = useState(task.project ?? "");
  const [recurrence, setRecurrence] = useState(task.recurrence ?? "");
  const [tagDraft, setTagDraft] = useState("");
  // A live-refresh from the bridge can replace the task under us.
  useEffect(() => {
    setProject(task.project ?? "");
    setRecurrence(task.recurrence ?? "");
  }, [task.id, task.updated_at, task.project, task.recurrence]);

  const section = sections.find((s) => s.id === task.section_id) ?? null;
  const openBlockers = blockedBy.filter((b) => b.is_open);

  const holds: Record<Prop, boolean> = {
    project: Boolean(task.project),
    section: Boolean(section),
    due: Boolean(task.due_at),
    defer: Boolean(task.defer_until),
    repeat: Boolean(task.recurrence),
    tripwire: task.tripwire !== null,
    waiting: Boolean(task.blocked_reason) || openBlockers.length > 0,
  };
  const shows = (p: Prop) => holds[p] || adding === p;
  // Section is only offered where there are sections to pick from — the same
  // condition the old grid rendered the field on.
  const missing = (Object.keys(ADD_LABELS) as Prop[]).filter(
    (p) => !shows(p) && (p !== "section" || sections.length > 0)
  );

  const onOpenChange = (p: Prop | "status" | "priority" | "tag") => (next: boolean) => {
    if (next) {
      setOpen(p);
      return;
    }
    setOpen((o) => (o === p ? null : o));
    if (adding === p) {
      window.clearTimeout(clearTimer.current);
      clearTimer.current = window.setTimeout(
        () => setAdding((a) => (a === p ? null : a)),
        400
      );
    }
  };

  const startAdding = (p: Prop) => {
    // A date is filled the moment it is added, exactly as `DateField`'s own
    // "Add due date" button does — a `datetime-local` must never be mounted
    // empty in WKWebView, where the blank renders as a plausible fake date.
    if (p === "due") void patch(task.id, { due_at: defaultDate() });
    if (p === "defer") void patch(task.id, { defer_until: defaultDate() });
    window.clearTimeout(clearTimer.current);
    setAdding(p);
    setOpen(p);
  };

  /**
   * Commit the project. **The workspace is filled in by the backend**, not here.
   *
   * The two halves of the mapping resolve in opposite directions and each fills
   * the gap its own caller leaves (invariant 17): an agent knows a working
   * directory and not the project's name, so `file_task` goes path → project; a
   * human picks a project from a list and doesn't want to type a path, so
   * project → path. That second half is `db::tasks::create` / `update`'s job,
   * so every route into a project agrees; the filled path comes back on the
   * patched task.
   */
  const commitProject = (value = project) => {
    const next = value.trim() || null;
    if (next === task.project) return;
    void patch(task.id, { project: next });
  };

  const tagNames = tags.map((t) => t.name);
  /** Same normalisation the old tag editor applied: a leading `#` is the label, not
   *  the name, and names are lowercase. An empty or repeated draft is dropped. */
  const commitTag = () => {
    const next = tagDraft.trim().replace(/^#/, "").toLowerCase();
    setTagDraft("");
    if (!next || tagNames.includes(next)) return;
    void setTaskTags(task.id, [...tagNames, next]);
  };

  const commitRecurrence = () => {
    if (recurrence === (task.recurrence ?? "")) return;
    void patch(task.id, { recurrence: recurrence.trim() || null });
  };

  // A chip that stopped existing (a date cleared from its own popover, a
  // tripwire set to None) must not leave its popover armed to spring open the
  // next time the property comes back by some other route.
  useEffect(() => {
    if (open && open !== "status" && open !== "priority" && open !== "tag" && !shows(open))
      setOpen(null);
  });

  const deferred = isDeferred(task);
  const overdue = isOverdue(task);

  return (
    <div className="flex flex-wrap items-center gap-1.5 px-1" data-testid="property-strip">
      {/* ── Status: the hint moved INTO the menu. It is the whole defence
          against status drift (see `STATUS_HINTS`), and the moment of CHOOSING
          is the one moment the question is live; at rest it was one more grey
          line between the title and the notes. */}
      <Dropdown.Root open={open === "status"} onOpenChange={onOpenChange("status")}>
        <Dropdown.Trigger asChild>
          <Chip label="Status" value={STATUS_LABELS[task.status]} className={statusClass(task.status)} />
        </Dropdown.Trigger>
        <Dropdown.Portal>
          <Dropdown.Content
            align="start"
            sideOffset={6}
            collisionPadding={10}
            className={cn(PANEL, "w-64 p-1")}
          >
            <Dropdown.RadioGroup
              value={task.status}
              onValueChange={(v) => void patch(task.id, { status: v as TaskStatus })}
            >
              {/* USER_STATUSES, not TASK_STATUSES: `queued` is the runner's
                  claim key and picking it by hand would enter the drain queue
                  past every check `queue_background` runs first. */}
              {USER_STATUSES.map((s) => (
                <Dropdown.RadioItem key={s} value={s} className={cn(MENU_ITEM, "group")}>
                  <span className="mt-0.5 flex w-3 shrink-0">
                    <Dropdown.ItemIndicator>
                      <Check size={12} />
                    </Dropdown.ItemIndicator>
                  </span>
                  <span className="flex min-w-0 flex-col">
                    <span>{STATUS_LABELS[s]}</span>
                    <span className="text-[11px] leading-snug text-content-faint group-data-[highlighted]:text-white/75">
                      {STATUS_HINTS[s]}
                    </span>
                  </span>
                </Dropdown.RadioItem>
              ))}
            </Dropdown.RadioGroup>
          </Dropdown.Content>
        </Dropdown.Portal>
      </Dropdown.Root>

      <Dropdown.Root open={open === "priority"} onOpenChange={onOpenChange("priority")}>
        <Dropdown.Trigger asChild>
          <Chip
            label="Priority"
            value={`P${task.priority}`}
            className={cn("bg-content/6 font-semibold hover:bg-content/10", PRIORITY_COLORS[task.priority])}
          />
        </Dropdown.Trigger>
        <Dropdown.Portal>
          <Dropdown.Content
            align="start"
            sideOffset={6}
            collisionPadding={10}
            className={cn(PANEL, "w-44 p-1")}
          >
            <Dropdown.RadioGroup
              value={String(task.priority)}
              onValueChange={(v) => void patch(task.id, { priority: Number(v) })}
            >
              {PRIORITY_OPTIONS.map((p) => (
                <Dropdown.RadioItem key={p.value} value={String(p.value)} className={MENU_ITEM}>
                  <span className="mt-0.5 flex w-3 shrink-0">
                    <Dropdown.ItemIndicator>
                      <Check size={12} />
                    </Dropdown.ItemIndicator>
                  </span>
                  {p.label}
                </Dropdown.RadioItem>
              ))}
            </Dropdown.RadioGroup>
          </Dropdown.Content>
        </Dropdown.Portal>
      </Dropdown.Root>

      {shows("project") && (
        <ChipPopover
          open={open === "project"}
          onOpenChange={onOpenChange("project")}
          title="Project"
          chip={
            <Chip label="Project" icon={Target} value={task.project ?? "Project…"} />
          }
        >
          {/* Real project records — NOT the `~/Projects` directory listing
              that used to feed this. Offering directory names is what taught
              the app that `finance-app` was a project when it is merely where
              pocketbuddy lives, and since `tasks::create` calls
              `ensure_exists`, picking one MADE it a project. */}
          <Combobox
            autoFocus
            value={project}
            placeholder="Project"
            aria-label="Project"
            onValueChange={setProject}
            options={projectRecords.map((p) => ({ value: p.name }))}
            onSelectOption={(value) => {
              commitProject(value);
              onOpenChange("project")(false);
            }}
            onBlur={() => commitProject()}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commitProject();
                onOpenChange("project")(false);
              }
            }}
            className={FIELD}
          />
        </ChipPopover>
      )}

      {shows("section") && (
        <Dropdown.Root open={open === "section"} onOpenChange={onOpenChange("section")}>
          <Dropdown.Trigger asChild>
            <Chip label="Section" icon={Rows3} value={section?.name ?? "Section…"} />
          </Dropdown.Trigger>
          <Dropdown.Portal>
            <Dropdown.Content
              align="start"
              sideOffset={6}
              collisionPadding={10}
              className={cn(PANEL, "w-48 p-1")}
            >
              {/* Not "" — Radix reserves the empty string for "no selection",
                  so an ungrouped task rendered a BLANK picker. */}
              <Dropdown.RadioGroup
                value={task.section_id ?? UNGROUPED}
                onValueChange={(v) => void moveTaskToSection(task.id, v === UNGROUPED ? null : v)}
              >
                {[{ id: UNGROUPED, name: "Ungrouped" }, ...sections].map((s) => (
                  <Dropdown.RadioItem key={s.id} value={s.id} className={MENU_ITEM}>
                    <span className="mt-0.5 flex w-3 shrink-0">
                      <Dropdown.ItemIndicator>
                        <Check size={12} />
                      </Dropdown.ItemIndicator>
                    </span>
                    {s.name}
                  </Dropdown.RadioItem>
                ))}
              </Dropdown.RadioGroup>
            </Dropdown.Content>
          </Dropdown.Portal>
        </Dropdown.Root>
      )}

      {shows("due") && (
        <ChipPopover
          open={open === "due"}
          onOpenChange={onOpenChange("due")}
          title="Due"
          hint="A deadline. It hides nothing — the task stays on Open and Today."
          chip={
            <Chip
              label="Due"
              icon={CalendarCheck}
              tone={overdue ? "error" : "neutral"}
              // `formatDue` already says "Overdue 2d" for a past date, so the
              // "Due" prefix is only for a date still ahead.
              value={
                !task.due_at
                  ? "Due…"
                  : overdue
                    ? (formatDue(task.due_at) ?? "Overdue")
                    : `Due ${formatDue(task.due_at)}`
              }
            />
          }
        >
          <DateField
            value={task.due_at}
            onChange={(iso) => void patch(task.id, { due_at: iso })}
            addLabel="Add due date"
          />
        </ChipPopover>
      )}

      {shows("defer") && (
        <ChipPopover
          open={open === "defer"}
          onOpenChange={onOpenChange("defer")}
          title="Starts"
          hint="Hidden from Open and Today until then."
          chip={
            <Chip
              label="Starts"
              icon={CalendarClock}
              tone={deferred ? "warning" : "neutral"}
              value={
                !task.defer_until
                  ? "Starts…"
                  : deferred
                    ? `Starts ${formatDue(task.defer_until)}`
                    : `Started ${shortDate(task.defer_until)}`
              }
            />
          }
        >
          <DateField
            value={task.defer_until}
            onChange={(iso) => void patch(task.id, { defer_until: iso })}
            addLabel="Not deferred"
          />
        </ChipPopover>
      )}

      {shows("repeat") && (
        <ChipPopover
          open={open === "repeat"}
          onOpenChange={onOpenChange("repeat")}
          title="Repeats"
          hint="daily · weekly:MO,TH · monthly:15 · quarterly:1"
          chip={
            <Chip label="Repeats" icon={Repeat} value={describeRecurrence(task.recurrence) ?? "Repeats…"} />
          }
        >
          <input
            autoFocus
            value={recurrence}
            placeholder="weekly:MO,TH"
            aria-label="Repeat rule"
            onChange={(e) => setRecurrence(e.target.value)}
            onBlur={commitRecurrence}
            onKeyDown={(e) => {
              if (e.key === "Enter") onOpenChange("repeat")(false);
            }}
            className={cn(FIELD, "font-mono text-[12px]")}
          />
        </ChipPopover>
      )}

      {shows("tripwire") && (
        <ChipPopover
          open={open === "tripwire"}
          onOpenChange={onOpenChange("tripwire")}
          // No title: `TripwireField` opens on its own "Tripwire" label.
          wide
          chip={
            <Chip
              label="Tripwire"
              icon={Zap}
              tone={tripwireTone(task, runtime)}
              title={task.tripwire ? describeTripwire(task, runtime) : undefined}
              data-testid="tripwire-chip"
              value={tripwireLabel(task, runtime)}
            />
          }
        >
          <TripwireField task={task} runtime={runtime} />
        </ChipPopover>
      )}

      {shows("waiting") && (
        <ChipPopover
          open={open === "waiting"}
          onOpenChange={onOpenChange("waiting")}
          title="Waiting on"
          wide
          chip={
            <Chip
              label="Waiting on"
              icon={Ban}
              tone={holds.waiting ? "error" : "neutral"}
              value={
                task.blocked_reason ??
                (openBlockers.length === 1
                  ? `Behind ${openBlockers[0].title}`
                  : openBlockers.length > 1
                    ? `Behind ${openBlockers.length} tasks`
                    : "Waiting on…")
              }
            />
          }
        >
          <HoldField
            task={task}
            runtime={runtime}
            initialEditing={adding === "waiting"}
            onCommit={(reason) => void patch(task.id, { blocked_reason: reason })}
          />
        </ChipPopover>
      )}

      {orderTagsByKind(tags).map((t) => (
        // Not a button: a tag has nothing to edit, only to remove — and the
        // remove control can't nest inside a button.
        <span
          key={t.id}
          data-testid="tag-chip"
          className="group flex items-center gap-0.5 rounded-md px-1 py-0.5 text-[11.5px] leading-[18px] text-content-muted hover:bg-content/6"
        >
          <TagMark name={t.name} />
          <button
            type="button"
            aria-label={`Remove tag ${t.name}`}
            onClick={() => void setTaskTags(task.id, tagNames.filter((n) => n !== t.name))}
            className="hidden text-content-faint hover:text-error group-hover:inline focus-visible:inline"
          >
            <X size={10} />
          </button>
        </span>
      ))}

      {adding === "tag" && (
        <ChipPopover
          open={open === "tag"}
          onOpenChange={onOpenChange("tag")}
          title="Tag"
          hint="↩ adds it and leaves the field open for another."
          chip={<Chip label="Tag" icon={TagIcon} value="Tag…" />}
        >
          {/* Commits on blur, which is also how `ChipPopover`'s close flushes
              it (gotcha #24); Enter and comma add and keep the field open. */}
          <Combobox
            autoFocus
            value={tagDraft}
            placeholder="tag or @person"
            aria-label="Tag"
            onValueChange={setTagDraft}
            options={allTags.filter((t) => !tagNames.includes(t.name)).map((t) => ({ value: t.name }))}
            onBlur={commitTag}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === ",") {
                e.preventDefault();
                commitTag();
              }
            }}
            className={FIELD}
          />
        </ChipPopover>
      )}

      {/* Always on offer: a task can take any number of tags.
          Non-modal: a modal menu traps focus, and its trap is still live in
          the commit that mounts the popover the pick opens — so the editor's
          autofocus is pulled straight back, the input blurs, and a
          blur-committing field (HoldField) drops out of editing before it is
          ever seen. */}
      <Dropdown.Root modal={false}>
        <Dropdown.Trigger asChild>
          <button
            type="button"
            aria-label="Add a property"
            title="Add a property or tag"
            className="flex items-center rounded-md border border-dashed border-border px-1 py-[3px] text-content-faint outline-none hover:border-accent hover:text-accent focus-visible:border-accent data-[state=open]:border-accent data-[state=open]:text-accent"
          >
            <Plus size={12} />
          </button>
        </Dropdown.Trigger>
        <Dropdown.Portal>
          <Dropdown.Content
            align="start"
            sideOffset={6}
            collisionPadding={10}
            // Focus is about to move into the popover the pick opens; handing
            // it back to `+` would read as a focus-outside and close it.
            onCloseAutoFocus={(e) => e.preventDefault()}
            className={cn(PANEL, "w-48 p-1")}
          >
            {missing.map((p) => {
              const Icon = ICONS[p];
              return (
                <Dropdown.Item
                  key={p}
                  onSelect={() => startAdding(p)}
                  className={cn(MENU_ITEM, "items-center")}
                >
                  <Icon size={13} className="shrink-0" />
                  {ADD_LABELS[p]}
                </Dropdown.Item>
              );
            })}
            {missing.length > 0 && <Dropdown.Separator className="mx-1 my-1 h-px bg-border-subtle" />}
            <Dropdown.Item
              onSelect={() => {
                window.clearTimeout(clearTimer.current);
                setAdding("tag");
                setOpen("tag");
              }}
              className={cn(MENU_ITEM, "items-center")}
            >
              <TagIcon size={13} className="shrink-0" />
              Tag
            </Dropdown.Item>
          </Dropdown.Content>
        </Dropdown.Portal>
      </Dropdown.Root>
    </div>
  );
}

/** Section picker value for "no section". */
const UNGROUPED = "__ungrouped__";

function shortDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** What the tripwire chip says — the runner's own last words while armed,
 *  which beat any badge (the row's Waiting line makes the same choice). */
function tripwireLabel(task: Task, runtime: TripwireRuntime | undefined): string {
  if (!task.tripwire) return "Tripwire…";
  if (task.tripwire_state === "tripped") {
    const age = task.tripwire_tripped_at ? formatAge(task.tripwire_tripped_at) : null;
    return age ? `Tripped ${age} ago` : "Tripped";
  }
  if (task.tripwire_state === "error") return "Check failed";
  if (!isTripwireWatching(task)) return "Tripwire";
  if (!runtime?.checked_at) return "Not checked yet";
  const said =
    runtime.last_note?.trim() ||
    (runtime.last_outcome ? tripwireOutcomeLabel(task, runtime.last_outcome) : "checked");
  return `${said} · ${formatAge(runtime.checked_at) ?? "?"}`;
}

/** Red when it failed or has gone unchecked for three intervals — the
 *  runner-not-running tell, which a calm chip would hide. */
function tripwireTone(task: Task, runtime: TripwireRuntime | undefined): Tone {
  if (!task.tripwire) return "neutral";
  if (task.tripwire_state === "error" || tripwireUnchecked(task, runtime)) return "error";
  if (task.tripwire_state === "tripped") return "success";
  // A watching monitor is the calm, normal state — nothing is held by it.
  return isTripwireArmed(task) && task.tripwire_on_trip !== "monitor" ? "warning" : "neutral";
}

type ChipProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** The property's name — the accessible name leads with it, and so does
   *  the tooltip, because a chip shows only the value. */
  label: string;
  value: string;
  icon?: typeof Zap;
  tone?: Tone;
};

/** forwardRef + spread: a Radix `Trigger asChild` passes its ref and its
 *  handlers through whatever it wraps. */
const Chip = forwardRef<HTMLButtonElement, ChipProps>(function Chip(
  { label, value, icon: Icon, tone = "neutral", className, title, ...rest },
  ref
) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={`${label}: ${value}`}
      title={title ?? label}
      {...rest}
      className={cn(
        "flex max-w-full items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] leading-[18px] outline-none transition-shadow",
        "focus-visible:ring-1 focus-visible:ring-accent data-[state=open]:ring-1 data-[state=open]:ring-accent",
        className ?? TONES[tone]
      )}
    >
      {Icon && <Icon size={11} className="shrink-0 opacity-80" />}
      <span className="truncate">{value}</span>
    </button>
  );
});

/**
 * A chip whose editor floats beneath it.
 *
 * **Closing FLUSHES the draft first** (gotcha #24). Every editor in here
 * commits on blur, and a focused input that is unmounted does not reliably
 * fire blur — so a click-away that just unmounted the popover would drop what
 * you typed, and the next live refresh would reset the draft over it. Blurring
 * the focused element inside the panel BEFORE the close runs the editor's own
 * commit path. Escape skips the flush: in a popover it means "cancel", the
 * same thing it means in `HoldField`'s reason input.
 */
function ChipPopover({
  open,
  onOpenChange,
  title,
  hint,
  wide,
  chip,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title?: string;
  hint?: string;
  /** Room for a small form (tripwire, hold) rather than one control. */
  wide?: boolean;
  chip: React.ReactElement;
  children: React.ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const escaping = useRef(false);

  const change = (next: boolean) => {
    if (!next && !escaping.current) {
      const active = document.activeElement;
      if (active instanceof HTMLElement && panel.current?.contains(active)) active.blur();
    }
    escaping.current = false;
    onOpenChange(next);
  };

  return (
    <Popover.Root open={open} onOpenChange={change}>
      <Popover.Trigger asChild>{chip}</Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          ref={panel}
          side="bottom"
          align="start"
          sideOffset={6}
          collisionPadding={10}
          onEscapeKeyDown={() => {
            escaping.current = true;
          }}
          className={cn(PANEL, "flex flex-col gap-2 p-2.5", wide ? "w-[340px]" : "w-64")}
        >
          {title && <span className={LABEL}>{title}</span>}
          {children}
          {hint && <p className="text-[11px] leading-snug text-content-faint">{hint}</p>}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
