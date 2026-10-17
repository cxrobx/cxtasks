import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Ban,
  CalendarCheck,
  CalendarClock,
  CalendarPlus,
  Check,
  ChevronDown,
  Copy,
  GitBranch,
  Moon,
  Plus,
  Repeat,
  Rows3,
  RotateCcw,
  Sun,
  Target,
  X,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  formatDue,
  PRIORITY_COLORS,
  STATUS_HINTS,
  STATUS_LABELS,
  statusClass,
} from "@/lib/format";
import { Select } from "@/components/shared/Select";
import {
  defaultDate,
  isoToLocalInput,
  localInputToIso,
} from "@/components/shared/DateField";
import { USER_STATUSES, type TaskStatus } from "@/types/task";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — PROGRESSIVE DISCLOSURE for the detail
   pane's info fields (Status → Tripwire).

   The complaint: nine labelled controls sit between the title and
   the notes, most of them empty, and most of the filled ones repeat
   what the row already printed. Each variant is shown beside a
   replica of the shipped pane at the production width, with a
   readout of how far down the pane Notes starts — the thing you
   opened the task to read.

   Reached only via `?mock=disclosure`; all data and actions are
   static. Edits are shared between the two panes, so adding a date
   on the right shows what it costs on the left.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C";
type SampleKey = "sparse" | "scheduled" | "condition";
type Prop =
  | "status"
  | "priority"
  | "project"
  | "section"
  | "due"
  | "defer"
  | "repeat"
  | "tripwire"
  | "waiting";

interface Tripwire {
  kind: "script" | "agent";
  spec: string;
  cadence: "hourly" | "daily" | "weekly";
  checkedAgo: string | null;
  lastNote: string | null;
}

interface MockTask {
  ref: string;
  title: string;
  status: TaskStatus;
  priority: number;
  project: string | null;
  section: string | null;
  recurrence: string | null;
  due_at: string | null;
  defer_until: string | null;
  tripwire: Tripwire | null;
  blocked_reason: string | null;
  tags: string[];
  body: string[];
  filedAgo: string;
  hasRepo: boolean;
}

const SECTIONS = ["Personal", "Backlog"];
const PROJECTS = ["CXVentures", "CXTasks", "TeacherHero", "Artist Advisory", "Globex"];

/** Relative to the real clock, so the labels read "Thu 10:00 AM" whatever day
 *  the page is opened — `formatDue` is the row's own formatter. */
function inDays(days: number, hour: number): string {
  const d = new Date();
  d.setDate(d.getDate() + days);
  d.setHours(hour, 0, 0, 0);
  return d.toISOString();
}

function samples(): Record<SampleKey, MockTask> {
  return {
    sparse: {
      ref: "T202",
      title: "Play the embed once in the Courses Plus learner view on the storefront",
      status: "inbox",
      priority: 2,
      project: null,
      section: null,
      recurrence: null,
      due_at: null,
      defer_until: null,
      tripwire: null,
      blocked_reason: null,
      tags: ["billable", "training-academy", "globex"],
      body: [
        "The last unverified path of the domain lock (T158 follow-up). The lock is CSP frame-ancestors on every /watch response, fed by EMBED_ORIGINS.",
        "It was proven live in two directions: allowed in the Courses Plus lesson editor, refused on a foreign origin.",
        "NOT yet seen: the learner portal on the storefront, which needs a customer login.",
      ],
      filedAgo: "5d",
      hasRepo: true,
    },
    scheduled: {
      ref: "T155",
      title: "Quarterly tripwire review — §8.4 script, lowkey re-vote, people rail, the mix",
      status: "todo",
      priority: 1,
      project: "CXVentures",
      section: null,
      recurrence: "quarterly:1",
      due_at: inDays(4, 10),
      defer_until: inDays(4, 0),
      tripwire: null,
      blocked_reason: null,
      tags: ["focus-map", "review"],
      body: [
        "**The ritual the instrument exists for.** Recurs on the first of each quarter. This first instance folds two quarters together — don't wait for the date if you have an hour before it.",
        "**1. Read the instrument.** Open Waiting → Until a condition: each tripwire carries its last evidence line under the title. Those are the numbers.",
        "**2. The review script** — your own questions, in order.",
      ],
      filedAgo: "4w",
      hasRepo: true,
    },
    condition: {
      ref: "T205",
      title: "Read teacherhero.org Search Console: did the 9 “Discovered” pages get crawled?",
      status: "inbox",
      priority: 2,
      project: "TeacherHero",
      section: "Backlog",
      recurrence: null,
      due_at: null,
      defer_until: inDays(9, 9),
      tripwire: {
        kind: "script",
        spec: "gsc-crawled teacherhero.org",
        cadence: "daily",
        checkedAgo: "2h",
        lastNote: "3 of 9 crawled",
      },
      blocked_reason: null,
      tags: ["seo"],
      body: [
        "Nine pages sat in “Discovered — currently not indexed” after the sitemap resubmit.",
        "When the tripwire trips, open the Pages report and note which of the nine moved. If fewer than six, file the internal-linking follow-up.",
      ],
      filedAgo: "4d",
      hasRepo: false,
    },
  };
}

/* ── derived facts, the same ones the real row derives ───────── */

const isDeferred = (t: MockTask) =>
  t.status !== "done" && !!t.defer_until && new Date(t.defer_until).getTime() > Date.now();
const isArmed = (t: MockTask) => t.status !== "done" && !!t.tripwire;
const isHeld = (t: MockTask) => isDeferred(t) || isArmed(t) || !!t.blocked_reason;

const DAYS: Record<string, string> = {
  MO: "Mon", TU: "Tue", WE: "Wed", TH: "Thu", FR: "Fri", SA: "Sat", SU: "Sun",
};
function ordinal(n: number) {
  const s = ["th", "st", "nd", "rd"];
  const v = n % 100;
  return n + (s[(v - 20) % 10] || s[v] || s[0]);
}
/** RRULE-lite → words. The rule itself stays one click away in the editor. */
function describeRepeat(rule: string | null): string | null {
  if (!rule) return null;
  const [freq, arg] = rule.split(":");
  if (freq === "daily") return "daily";
  if (freq === "weekly")
    return arg ? `weekly on ${arg.split(",").map((d) => DAYS[d] ?? d).join(", ")}` : "weekly";
  if (freq === "monthly") return arg ? `monthly on the ${ordinal(Number(arg))}` : "monthly";
  if (freq === "quarterly") return arg ? `quarterly on the ${ordinal(Number(arg))}` : "quarterly";
  return rule;
}

function statusArticle(s: TaskStatus) {
  return s === "inbox" || s === "in_progress" || s === "in_review" ? "An" : "A";
}

/* ── shared state ────────────────────────────────────────────── */

type Update = (patch: Partial<MockTask>) => void;

function useSamples() {
  const [all, setAll] = useState(samples);
  const [key, setKey] = useState<SampleKey>("sparse");
  const update: Update = (patch) => setAll((a) => ({ ...a, [key]: { ...a[key], ...patch } }));
  const reset = () => setAll((a) => ({ ...a, [key]: samples()[key] }));
  return { task: all[key], key, setKey, update, reset };
}

/* ── pieces every pane shares ────────────────────────────────── */

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";
const LABEL = "text-[11px] font-medium uppercase tracking-wide text-content-faint";

function RefAndTitle({ task }: { task: MockTask }) {
  return (
    <>
      <div className="flex items-center gap-1.5 pt-1 text-[11px] text-content-muted">
        {task.ref}
        <Copy size={11} className="text-content-faint" />
      </div>
      <div className="px-2 text-[15px] font-semibold leading-snug text-content">{task.title}</div>
    </>
  );
}

/** Notes, rendered — clipped to the height the real box opens at. `data-notes`
 *  is what the pane's readout measures to. */
function Notes({ task }: { task: MockTask }) {
  return (
    <section data-notes className="flex flex-col gap-1">
      <span className={LABEL}>Notes</span>
      <div className="relative max-h-[178px] overflow-hidden rounded-md border border-border bg-input px-3 py-2.5 text-[12.5px] leading-relaxed text-content">
        {task.body.map((p, i) => (
          <p key={i} className={i ? "mt-2" : ""}>
            {p.split(/(\*\*[^*]+\*\*)/).map((part, j) =>
              part.startsWith("**") ? (
                <strong key={j} className="font-semibold">
                  {part.slice(2, -2)}
                </strong>
              ) : (
                <span key={j}>{part}</span>
              )
            )}
          </p>
        ))}
        <div className="pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-input to-transparent" />
      </div>
    </section>
  );
}

function Tags({ task }: { task: MockTask }) {
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
      {task.tags.map((t) => (
        <span key={t} className="rounded-md bg-content/8 px-1.5 py-0.5 text-content-muted">
          #{t}
        </span>
      ))}
      <span className="text-content-faint">+ Tag</span>
    </div>
  );
}

/* ── editors, shared by B and C ──────────────────────────────── */

function StatusEditor({ task, update }: { task: MockTask; update: Update }) {
  // The hint moves HERE — to the moment of choosing, which is the only moment
  // "what does this status mean" is a live question. On the resting pane it
  // was one more line of grey between the title and the notes.
  return (
    <div className="flex flex-col gap-0.5">
      {USER_STATUSES.map((s) => (
        <button
          key={s}
          type="button"
          onClick={() => update({ status: s })}
          className={cn(
            "flex items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-content/6",
            task.status === s && "bg-accent/10"
          )}
        >
          <Check
            size={12}
            className={cn("mt-0.5 shrink-0", task.status === s ? "text-accent" : "opacity-0")}
          />
          <span className="flex flex-col">
            <span className="text-[12px] text-content">{STATUS_LABELS[s]}</span>
            <span className="text-[10.5px] leading-snug text-content-faint">{STATUS_HINTS[s]}</span>
          </span>
        </button>
      ))}
    </div>
  );
}

function PriorityEditor({ task, update }: { task: MockTask; update: Update }) {
  return (
    <div className="grid grid-cols-4 gap-1">
      {[0, 1, 2, 3].map((p) => (
        <button
          key={p}
          type="button"
          onClick={() => update({ priority: p })}
          className={cn(
            "rounded-md border px-2 py-1.5 text-[12px] font-semibold",
            task.priority === p
              ? "border-accent bg-accent/12"
              : "border-border-subtle hover:bg-content/6",
            PRIORITY_COLORS[p]
          )}
        >
          P{p}
        </button>
      ))}
      <p className="col-span-4 mt-1 text-[10.5px] text-content-faint">P0 is the highest.</p>
    </div>
  );
}

function ProjectEditor({ task, update }: { task: MockTask; update: Update }) {
  return (
    <div className="flex flex-col gap-2">
      <input
        autoFocus
        value={task.project ?? ""}
        placeholder="Project"
        onChange={(e) => update({ project: e.target.value || null })}
        className={FIELD}
      />
      <div className="flex flex-wrap gap-1">
        {PROJECTS.map((p) => (
          <button
            key={p}
            type="button"
            onClick={() => update({ project: p })}
            className={cn(
              "rounded-md px-1.5 py-0.5 text-[11px]",
              task.project === p ? "bg-accent/15 text-accent" : "bg-content/6 text-content-muted hover:text-content"
            )}
          >
            {p}
          </button>
        ))}
      </div>
    </div>
  );
}

function SectionEditor({ task, update }: { task: MockTask; update: Update }) {
  return (
    <div className="flex flex-col gap-0.5">
      {[null, ...SECTIONS].map((s) => (
        <button
          key={s ?? "none"}
          type="button"
          onClick={() => update({ section: s })}
          className={cn(
            "flex items-center gap-2 rounded-md px-2 py-1 text-left text-[12px] hover:bg-content/6",
            task.section === s ? "text-content" : "text-content-muted"
          )}
        >
          <Check size={12} className={task.section === s ? "text-accent" : "opacity-0"} />
          {s ?? "Ungrouped"}
        </button>
      ))}
    </div>
  );
}

function DateEditor({
  value,
  onChange,
  hint,
}: {
  value: string | null;
  onChange: (iso: string | null) => void;
  hint: string;
}) {
  const preset = (days: number, hour: number) => onChange(inDays(days, hour));
  return (
    <div className="flex flex-col gap-2">
      {/* Only ever mounted WITH a value — the WKWebView empty-datetime trap
          (`DateField`) still applies inside a popover. */}
      <input
        type="datetime-local"
        value={isoToLocalInput(value ?? defaultDate())}
        onChange={(e) => onChange(localInputToIso(e.target.value))}
        className={FIELD}
      />
      <div className="flex flex-wrap gap-1">
        {(
          [
            ["Today 5 PM", 0, 17],
            ["Tomorrow", 1, 9],
            ["Next week", 7, 9],
          ] as const
        ).map(([label, d, h]) => (
          <button
            key={label}
            type="button"
            onClick={() => preset(d, h)}
            className="rounded-md bg-content/6 px-1.5 py-0.5 text-[11px] text-content-muted hover:text-content"
          >
            {label}
          </button>
        ))}
      </div>
      <p className="text-[10.5px] leading-snug text-content-faint">{hint}</p>
    </div>
  );
}

function RepeatEditor({ task, update }: { task: MockTask; update: Update }) {
  const presets = ["daily", "weekly:MO", "monthly:1", "quarterly:1"];
  return (
    <div className="flex flex-col gap-2">
      <div className="grid grid-cols-2 gap-1">
        {presets.map((p) => (
          <button
            key={p}
            type="button"
            onClick={() => update({ recurrence: p })}
            className={cn(
              "rounded-md border px-2 py-1 text-left text-[11.5px]",
              task.recurrence === p
                ? "border-accent bg-accent/10 text-content"
                : "border-border-subtle text-content-muted hover:bg-content/6"
            )}
          >
            {describeRepeat(p)}
          </button>
        ))}
      </div>
      <input
        value={task.recurrence ?? ""}
        placeholder="daily · weekly:MO,TH · monthly:15 · quarterly:1"
        onChange={(e) => update({ recurrence: e.target.value || null })}
        className={cn(FIELD, "font-mono text-[11.5px]")}
      />
    </div>
  );
}

function TripwireEditor({ task, update }: { task: MockTask; update: Update }) {
  const tw = task.tripwire ?? {
    kind: "script" as const,
    spec: "",
    cadence: "daily" as const,
    checkedAgo: null,
    lastNote: null,
  };
  const set = (p: Partial<Tripwire>) => update({ tripwire: { ...tw, ...p } });
  return (
    <div className="flex flex-col gap-2">
      <div className="grid grid-cols-2 gap-1">
        {(["script", "agent"] as const).map((k) => (
          <button
            key={k}
            type="button"
            onClick={() => set({ kind: k })}
            className={cn(
              "rounded-md border px-2 py-1 text-[11.5px]",
              tw.kind === k
                ? "border-accent bg-accent/10 text-content"
                : "border-border-subtle text-content-muted hover:bg-content/6"
            )}
          >
            {k === "script" ? "Script" : "Agent check"}
          </button>
        ))}
      </div>
      <input
        autoFocus={!task.tripwire}
        value={tw.spec}
        placeholder={tw.kind === "script" ? "script name + one argument" : "the condition, in words"}
        onChange={(e) => set({ spec: e.target.value })}
        className={cn(FIELD, tw.kind === "script" && "font-mono text-[11.5px]")}
      />
      <div className="flex items-center gap-2 text-[11px] text-content-muted">
        Check
        {(["hourly", "daily", "weekly"] as const).map((c) => (
          <button
            key={c}
            type="button"
            onClick={() => set({ cadence: c })}
            className={cn(
              "rounded px-1.5 py-0.5",
              tw.cadence === c ? "bg-accent/15 text-accent" : "hover:text-content"
            )}
          >
            {c}
          </button>
        ))}
      </div>
      {tw.checkedAgo && (
        <p className="text-[10.5px] text-content-faint">
          Checked {tw.checkedAgo} ago: <span className="text-content-muted">{tw.lastNote}</span>
        </p>
      )}
    </div>
  );
}

function WaitingEditor({ task, update }: { task: MockTask; update: Update }) {
  return (
    <div className="flex flex-col gap-1.5">
      <input
        autoFocus
        value={task.blocked_reason ?? ""}
        placeholder="e.g. Dana's sign-off on the Learning Hub scope"
        onChange={(e) => update({ blocked_reason: e.target.value || null })}
        className={FIELD}
      />
      <p className="text-[10.5px] leading-snug text-content-faint">
        Holds it off Open and Today until you clear it.
      </p>
    </div>
  );
}

const PROP_META: Record<
  Prop,
  { label: string; addLabel: string; icon: typeof Zap; clear?: Partial<MockTask> }
> = {
  status: { label: "Status", addLabel: "Status", icon: Check },
  priority: { label: "Priority", addLabel: "Priority", icon: Check },
  project: { label: "Project", addLabel: "Project", icon: Target, clear: { project: null } },
  section: { label: "Section", addLabel: "Section", icon: Rows3, clear: { section: null } },
  due: { label: "Due", addLabel: "Due date", icon: CalendarCheck, clear: { due_at: null } },
  defer: { label: "Starts", addLabel: "Start date", icon: CalendarClock, clear: { defer_until: null } },
  repeat: { label: "Repeats", addLabel: "Repeat", icon: Repeat, clear: { recurrence: null } },
  tripwire: { label: "Tripwire", addLabel: "Condition", icon: Zap, clear: { tripwire: null } },
  waiting: { label: "Waiting on", addLabel: "Waiting reason", icon: Ban, clear: { blocked_reason: null } },
};

function Editor({ prop, task, update }: { prop: Prop; task: MockTask; update: Update }) {
  switch (prop) {
    case "status":
      return <StatusEditor task={task} update={update} />;
    case "priority":
      return <PriorityEditor task={task} update={update} />;
    case "project":
      return <ProjectEditor task={task} update={update} />;
    case "section":
      return <SectionEditor task={task} update={update} />;
    case "due":
      return (
        <DateEditor
          value={task.due_at}
          onChange={(v) => update({ due_at: v })}
          hint="A deadline. It hides nothing — the task stays on Open and Today."
        />
      );
    case "defer":
      return (
        <DateEditor
          value={task.defer_until}
          onChange={(v) => update({ defer_until: v })}
          hint="Hidden from Open and Today until then."
        />
      );
    case "repeat":
      return <RepeatEditor task={task} update={update} />;
    case "tripwire":
      return <TripwireEditor task={task} update={update} />;
    case "waiting":
      return <WaitingEditor task={task} update={update} />;
  }
}

/** Is this property set? Status and priority always are. */
function isSet(prop: Prop, t: MockTask): boolean {
  switch (prop) {
    case "status":
    case "priority":
      return true;
    case "project":
      return !!t.project;
    case "section":
      return !!t.section;
    case "due":
      return !!t.due_at;
    case "defer":
      return !!t.defer_until;
    case "repeat":
      return !!t.recurrence;
    case "tripwire":
      return !!t.tripwire;
    case "waiting":
      return !!t.blocked_reason;
  }
}

const OPTIONAL: Prop[] = ["project", "section", "due", "defer", "repeat", "tripwire", "waiting"];

/** First-time value when a property is added, so a date control is never
 *  mounted empty (the WKWebView trap) and the chip has something to say. */
function seed(prop: Prop): Partial<MockTask> {
  if (prop === "due") return { due_at: defaultDate() };
  if (prop === "defer") return { defer_until: inDays(1, 9) };
  return {};
}

/* ── NOW · the shipped field grid, replicated ────────────────── */

function RowTag({ kind }: { kind: "row" | "empty" }) {
  return (
    <span
      className={cn(
        "ml-1.5 rounded px-1 py-px text-[8.5px] font-semibold normal-case tracking-normal",
        kind === "row" ? "bg-warning/15 text-warning" : "bg-content/8 text-content-faint"
      )}
    >
      {kind === "row" ? "on the row" : "empty"}
    </span>
  );
}

function ShippedField({
  label,
  tag,
  children,
  wide,
}: {
  label: string;
  tag?: "row" | "empty" | null;
  children: React.ReactNode;
  wide?: boolean;
}) {
  return (
    <div className={cn("flex flex-col gap-1", wide && "col-span-2")}>
      <span className={cn(LABEL, "flex items-center")}>
        {label}
        {tag && <RowTag kind={tag} />}
      </span>
      {children}
    </div>
  );
}

function Dashed({ icon: Icon, children }: { icon: typeof Zap; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-1.5 rounded-md border border-dashed border-border px-2 py-1 text-content-faint">
      <Icon size={13} />
      {children}
    </div>
  );
}

function DateValue({ iso, onClear }: { iso: string; onClear: () => void }) {
  return (
    <div className="flex items-center gap-1">
      <input
        type="datetime-local"
        value={isoToLocalInput(iso)}
        readOnly
        className="w-full rounded-md border border-border bg-input px-2 py-1 text-content"
      />
      <button type="button" onClick={onClear} className="rounded p-1 text-content-faint hover:text-content">
        <X size={12} />
      </button>
    </div>
  );
}

/** The grid as it ships — `TaskDetail.tsx`'s field block, every control in its
 *  resting state. `mark` tags each label with whether the row already says it. */
function ShippedFields({
  task,
  update,
  mark,
}: {
  task: MockTask;
  update: Update;
  mark: boolean;
}) {
  const tag = (onRow: boolean, empty: boolean): "row" | "empty" | null =>
    !mark ? null : empty ? "empty" : onRow ? "row" : null;
  const deferred = isDeferred(task);
  const armed = isArmed(task);
  return (
    <div className="grid grid-cols-2 gap-3">
      <ShippedField label="Status" tag={tag(true, false)}>
        <Select
          value={task.status}
          onValueChange={(v) => update({ status: v as TaskStatus })}
          options={USER_STATUSES.map((s) => ({ value: s, label: STATUS_LABELS[s] }))}
        />
        <p className="mt-1 text-[11px] leading-snug text-content-faint">{STATUS_HINTS[task.status]}</p>
      </ShippedField>
      <ShippedField label="Priority" tag={tag(task.priority < 3, false)}>
        <Select
          value={task.priority}
          onValueChange={(v) => update({ priority: Number(v) })}
          options={[0, 1, 2, 3].map((p) => ({ value: p, label: `P${p}` }))}
        />
      </ShippedField>

      <ShippedField label="Waiting on" wide tag={tag(true, !isHeld(task))}>
        <div className="flex flex-col gap-1">
          {deferred && (
            <div className="flex items-center gap-1.5 rounded-md border border-border bg-input px-2 py-1 text-content-secondary">
              <CalendarClock size={12} className="text-content-faint" />
              Starts {formatDue(task.defer_until)}
            </div>
          )}
          {armed && (
            <div className="flex items-center gap-1.5 rounded-md border border-border bg-input px-2 py-1 text-content-secondary">
              <Zap size={12} className="text-content-faint" />
              <span className="truncate">
                Until the tripwire trips — checked {task.tripwire?.checkedAgo} ago: {task.tripwire?.lastNote}
              </span>
            </div>
          )}
          {task.blocked_reason ? (
            <input
              value={task.blocked_reason}
              onChange={(e) => update({ blocked_reason: e.target.value || null })}
              className={cn(FIELD, "border-error/40")}
            />
          ) : (
            <Dashed icon={Ban}>{deferred || armed ? "Add a reason" : "Not waiting"}</Dashed>
          )}
        </div>
      </ShippedField>

      <ShippedField label="Project" tag={tag(true, !task.project)}>
        <input
          value={task.project ?? ""}
          onChange={(e) => update({ project: e.target.value || null })}
          className={FIELD}
        />
      </ShippedField>
      <ShippedField label="Section" tag={tag(false, !task.section)}>
        <Select
          value={task.section ?? "__none__"}
          onValueChange={(v) => update({ section: v === "__none__" ? null : v })}
          options={[
            { value: "__none__", label: "Ungrouped" },
            ...SECTIONS.map((s) => ({ value: s, label: s })),
          ]}
        />
      </ShippedField>

      <ShippedField label="Repeats" tag={tag(true, !task.recurrence)}>
        <input
          value={task.recurrence ?? ""}
          placeholder="daily · weekly:MO,TH · monthly:15 · quarterly:1"
          onChange={(e) => update({ recurrence: e.target.value || null })}
          className={FIELD}
        />
      </ShippedField>
      <ShippedField label="Due" tag={tag(true, !task.due_at)}>
        {task.due_at ? (
          <DateValue iso={task.due_at} onClear={() => update({ due_at: null })} />
        ) : (
          <Dashed icon={CalendarPlus}>Add due date</Dashed>
        )}
      </ShippedField>

      <ShippedField label="Defer until" tag={tag(true, !task.defer_until)}>
        {task.defer_until ? (
          <DateValue iso={task.defer_until} onClear={() => update({ defer_until: null })} />
        ) : (
          <Dashed icon={CalendarPlus}>Not deferred</Dashed>
        )}
      </ShippedField>
      <div />

      <div className="col-span-2 rounded-lg border border-border-subtle p-3">
        <span className={cn(LABEL, "flex items-center")}>
          Tripwire
          {mark && <RowTag kind={task.tripwire ? "row" : "empty"} />}
        </span>
        {task.tripwire ? (
          <div className="mt-2 flex flex-col gap-2">
            <Select value="script" onValueChange={() => {}} options={[{ value: "script", label: "Script" }]} />
            <input readOnly value={task.tripwire.spec} className={cn(FIELD, "font-mono text-[11.5px]")} />
            <div className="grid grid-cols-2 gap-2">
              <Select value="daily" onValueChange={() => {}} options={[{ value: "daily", label: "Daily" }]} />
              <div className="rounded-md border border-border px-2 py-1 text-content-muted">Check now</div>
            </div>
            <p className="text-[11px] text-content-faint">
              Armed · checked {task.tripwire.checkedAgo} ago: {task.tripwire.lastNote}
            </p>
          </div>
        ) : (
          <div className="mt-2 w-1/2">
            <Select value="none" onValueChange={() => {}} options={[{ value: "none", label: "None" }]} />
            <p className="mt-1 text-[11px] leading-snug text-content-faint">
              Wait for a condition instead of a date.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

function ShippedPane({ task, update, mark }: { task: MockTask; update: Update; mark: boolean }) {
  return (
    <div className="flex flex-col gap-5">
      <RefAndTitle task={task} />
      <ShippedFields task={task} update={update} mark={mark} />
      <Notes task={task} />
      <Tags task={task} />
    </div>
  );
}

/* ── one-line summary of the properties (A's fold, and the row) ─ */

function summaryParts(t: MockTask): { text: string; tone?: string }[] {
  const parts: { text: string; tone?: string }[] = [
    { text: STATUS_LABELS[t.status] },
    { text: `P${t.priority}`, tone: PRIORITY_COLORS[t.priority] },
  ];
  if (t.project) parts.push({ text: t.section ? `${t.project} › ${t.section}` : t.project });
  if (t.due_at) parts.push({ text: `Due ${formatDue(t.due_at)}` });
  if (isDeferred(t)) parts.push({ text: `Starts ${formatDue(t.defer_until)}`, tone: "text-warning" });
  if (t.recurrence) parts.push({ text: `↻ ${describeRepeat(t.recurrence)}` });
  if (t.tripwire) parts.push({ text: "⚡ tripwire", tone: "text-warning" });
  return parts;
}

/* ── A · Folded ──────────────────────────────────────────────── */

/**
 * The whole grid behind ONE fold, collapsed on every task open — the rule the
 * launch console already follows (`launchCollapsed`). The fold's header is the
 * row's own meta line, so a closed fold still answers "what is this".
 *
 * The one thing that never folds is the HOLD: HoldField's contract is that the
 * pane never looks un-held when the task is held, so its sentence rides above
 * the fold.
 */
function MockA({ task, update, mark }: { task: MockTask; update: Update; mark: boolean }) {
  const [open, setOpen] = useState(false);
  const deferred = isDeferred(task);
  const armed = isArmed(task);
  return (
    <div className="flex flex-col gap-5">
      <RefAndTitle task={task} />
      <div className="flex flex-col gap-2">
        {(deferred || armed || task.blocked_reason) && (
          <div className="flex flex-col gap-1 text-[12px] text-content-secondary">
            {deferred && (
              <HoldLine icon={CalendarClock}>Hidden until {formatDue(task.defer_until)}</HoldLine>
            )}
            {armed && (
              <HoldLine icon={Zap}>
                Until the tripwire trips — {task.tripwire?.lastNote} · {task.tripwire?.checkedAgo} ago
              </HoldLine>
            )}
            {task.blocked_reason && <HoldLine icon={Ban} tone="text-error">{task.blocked_reason}</HoldLine>}
          </div>
        )}
        <div className="rounded-lg border border-border-subtle">
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            className="flex w-full items-start gap-2 px-2.5 py-2 text-left hover:bg-content/4"
          >
            {/* Inline text, not a flex-wrap row: the separator trails its item,
                so a wrapped line ends on a dot instead of starting with one. */}
            <span className="min-w-0 flex-1 text-[11.5px] leading-relaxed text-content-muted">
              {summaryParts(task).map((p, i, all) => (
                <span key={i}>
                  <span className={cn("whitespace-nowrap", p.tone)}>{p.text}</span>
                  {i < all.length - 1 && <span className="text-content-faint"> · </span>}
                </span>
              ))}
            </span>
            <span className="mt-px flex shrink-0 items-center gap-0.5 text-[10.5px] text-content-faint">
              {open ? "Less" : "Edit"}
              <ChevronDown
                size={13}
                className={cn("transition-transform", open && "rotate-180")}
              />
            </span>
          </button>
          {open && (
            <div className="mock-expand border-t border-border-subtle p-3">
              <ShippedFields task={task} update={update} mark={mark} />
            </div>
          )}
        </div>
      </div>
      <Notes task={task} />
      <Tags task={task} />
    </div>
  );
}

function HoldLine({
  icon: Icon,
  tone = "text-warning",
  children,
}: {
  icon: typeof Zap;
  tone?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-1.5">
      <Icon size={12} className={cn("shrink-0", tone)} />
      <span className="truncate">{children}</span>
    </div>
  );
}

/* ── B · Chips + popover ─────────────────────────────────────── */

function chipLabel(prop: Prop, t: MockTask): string {
  switch (prop) {
    case "status":
      return STATUS_LABELS[t.status];
    case "priority":
      return `P${t.priority}`;
    case "project":
      return t.project ?? "Project…";
    case "section":
      return t.section ?? "Section…";
    case "due":
      return t.due_at ? `Due ${formatDue(t.due_at)}` : "Due…";
    case "defer":
      return t.defer_until ? `Starts ${formatDue(t.defer_until)}` : "Starts…";
    case "repeat":
      return t.recurrence ? describeRepeat(t.recurrence)! : "Repeats…";
    case "tripwire":
      return t.tripwire?.lastNote
        ? `${t.tripwire.lastNote} · ${t.tripwire.checkedAgo}`
        : t.tripwire?.spec || "Condition…";
    case "waiting":
      return t.blocked_reason ?? "Waiting on…";
  }
}

function chipTone(prop: Prop, t: MockTask): string {
  if (prop === "status") return statusClass(t.status);
  if (prop === "priority") return cn("bg-content/6 font-semibold", PRIORITY_COLORS[t.priority]);
  if (prop === "defer" && isDeferred(t)) return "bg-warning/12 text-warning";
  if (prop === "tripwire" && t.tripwire) return "bg-warning/12 text-warning";
  if (prop === "waiting" && t.blocked_reason) return "bg-error/12 text-error";
  return "bg-content/6 text-content-secondary";
}

/** Click-away in the CAPTURE phase — the house rule for popovers near a drag
 *  region (gotcha #15). */
function useClickAway(ref: React.RefObject<HTMLElement | null>, onAway: () => void, active: boolean) {
  useEffect(() => {
    if (!active) return;
    const h = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onAway();
    };
    document.addEventListener("mousedown", h, true);
    return () => document.removeEventListener("mousedown", h, true);
  }, [active, onAway, ref]);
}

/**
 * Only what is SET renders, as the row's own chips; one `+` adds the rest.
 * Each chip opens its editor in a popover, so nothing below the strip moves
 * while you edit — Notes stays exactly where it was.
 */
function MockB({ task, update }: { task: MockTask; update: Update; mark: boolean }) {
  const [openProp, setOpenProp] = useState<Prop | "add" | null>(null);
  const [anchor, setAnchor] = useState(0);
  const wrap = useRef<HTMLDivElement>(null);
  const close = () => setOpenProp(null);
  useClickAway(wrap, close, openProp !== null);

  const shown: Prop[] = (
    ["status", "priority", "project", "section", "due", "defer", "repeat", "tripwire", "waiting"] as Prop[]
  ).filter((p) => isSet(p, task) || openProp === p);
  const missing = OPTIONAL.filter((p) => !isSet(p, task));

  const open = (p: Prop | "add", el: HTMLElement) => {
    if (openProp === p) return close();
    const box = wrap.current!.getBoundingClientRect();
    const left = el.getBoundingClientRect().left - box.left;
    setAnchor(Math.max(0, Math.min(left, box.width - 264)));
    setOpenProp(p);
  };

  return (
    <div className="flex flex-col gap-5">
      <RefAndTitle task={task} />
      <div ref={wrap} className="relative -mt-2">
        <div className="flex flex-wrap items-center gap-1.5 px-1">
          {shown.map((p) => {
            const Icon = PROP_META[p].icon;
            const showIcon = p !== "status" && p !== "priority";
            return (
              <button
                key={p}
                type="button"
                title={PROP_META[p].label}
                onClick={(e) => open(p, e.currentTarget)}
                className={cn(
                  "flex max-w-full items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] transition-shadow",
                  chipTone(p, task),
                  openProp === p && "ring-1 ring-accent"
                )}
              >
                {showIcon && <Icon size={11} className="shrink-0 opacity-80" />}
                <span className="truncate">{chipLabel(p, task)}</span>
              </button>
            );
          })}
          {missing.length > 0 && (
            <button
              type="button"
              aria-label="Add a property"
              onClick={(e) => open("add", e.currentTarget)}
              className={cn(
                "flex items-center rounded-md border border-dashed border-border px-1 py-0.5 text-content-faint hover:border-accent hover:text-accent",
                openProp === "add" && "border-accent text-accent"
              )}
            >
              <Plus size={12} />
            </button>
          )}
        </div>

        {openProp && (
          <div
            className="mock-pop absolute top-full z-20 mt-1.5 w-[264px] rounded-lg border border-border bg-elevated p-2 shadow-xl"
            style={{ left: anchor }}
          >
            {openProp === "add" ? (
              <div className="flex flex-col gap-0.5">
                {missing.map((p) => {
                  const Icon = PROP_META[p].icon;
                  return (
                    <button
                      key={p}
                      type="button"
                      onClick={() => {
                        update(seed(p));
                        setOpenProp(p);
                      }}
                      className="flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px] text-content-secondary hover:bg-content/6 hover:text-content"
                    >
                      <Icon size={12} className="text-content-faint" />
                      {PROP_META[p].addLabel}
                    </button>
                  );
                })}
              </div>
            ) : (
              <>
                <div className="mb-1.5 flex items-center px-0.5">
                  <span className={LABEL}>{PROP_META[openProp].label}</span>
                  {PROP_META[openProp].clear && isSet(openProp, task) && (
                    <button
                      type="button"
                      onClick={() => {
                        update(PROP_META[openProp].clear!);
                        close();
                      }}
                      className="ml-auto text-[10.5px] text-content-faint hover:text-error"
                    >
                      Remove
                    </button>
                  )}
                </div>
                <Editor prop={openProp} task={task} update={update} />
              </>
            )}
          </div>
        )}
      </div>
      <Notes task={task} />
      <Tags task={task} />
    </div>
  );
}

/* ── C · Sentence ────────────────────────────────────────────── */

/**
 * The properties as ONE sentence — the house voice already argues for it (a
 * view's identity is a sentence; a blocker is a sentence). Every bold word is
 * the control for itself; clicking it opens that editor in place, under the
 * sentence, and the sentence rewrites as you edit. Empty properties are not
 * boxes but the faint words at the end.
 */
function MockC({ task, update }: { task: MockTask; update: Update; mark: boolean }) {
  const [active, setActive] = useState<Prop | null>(null);
  const toggle = (p: Prop) => setActive((a) => (a === p ? null : p));

  const Tok = ({ p, children, tone }: { p: Prop; children: React.ReactNode; tone?: string }) => (
    <button
      type="button"
      onClick={() => toggle(p)}
      className={cn(
        "rounded-sm font-medium underline decoration-dotted decoration-1 underline-offset-[3px] hover:text-accent",
        active === p ? "bg-accent/12 text-accent decoration-accent" : cn("text-content decoration-content-faint/70", tone)
      )}
    >
      {children}
    </button>
  );

  const when: React.ReactNode[] = [];
  if (task.due_at || active === "due")
    when.push(
      <span key="due">
        due <Tok p="due">{formatDue(task.due_at) ?? "…"}</Tok>
      </span>
    );
  if (task.defer_until || active === "defer")
    when.push(
      <span key="defer">
        {isDeferred(task) ? "hidden until " : "started "}
        <Tok p="defer" tone={isDeferred(task) ? "text-warning" : undefined}>
          {formatDue(task.defer_until) ?? "…"}
        </Tok>
      </span>
    );
  if (task.recurrence || active === "repeat")
    when.push(
      <span key="repeat">
        repeats <Tok p="repeat">{describeRepeat(task.recurrence) ?? "…"}</Tok>
      </span>
    );

  const missing = OPTIONAL.filter((p) => !isSet(p, task) && p !== "project" && p !== active);

  return (
    <div className="flex flex-col gap-5">
      <RefAndTitle task={task} />
      <div className="-mt-2 flex flex-col gap-2 px-2">
        <p className="text-[12.5px] leading-[1.75] text-content-muted">
          {statusArticle(task.status)}{" "}
          <Tok p="status">{STATUS_LABELS[task.status].toLowerCase()}</Tok> task at{" "}
          <Tok p="priority" tone={PRIORITY_COLORS[task.priority]}>
            P{task.priority}
          </Tok>
          , in <Tok p="project">{task.project ?? "no project"}</Tok>
          {(task.section || active === "section") && (
            <>
              {" "}› <Tok p="section">{task.section ?? "…"}</Tok>
            </>
          )}
          .
          {when.length > 0 && (
            <>
              {" "}
              {when.map((w, i) => (
                <span key={i}>
                  {i === 0 ? <Cap>{w}</Cap> : w}
                  {i < when.length - 1 ? ", " : "."}
                </span>
              ))}
            </>
          )}
          {(task.tripwire || active === "tripwire") && (
            <>
              {" "}Waits for{" "}
              <Tok p="tripwire" tone="text-warning">
                {task.tripwire?.spec ? "its tripwire" : "a condition…"}
              </Tok>
              {task.tripwire?.lastNote && (
                <span className="text-content-faint">
                  {" "}— last check {task.tripwire.checkedAgo} ago: {task.tripwire.lastNote}
                </span>
              )}
              .
            </>
          )}
          {(task.blocked_reason || active === "waiting") && (
            <>
              {" "}Waiting on{" "}
              <Tok p="waiting" tone="text-error">
                {task.blocked_reason ?? "…"}
              </Tok>
              .
            </>
          )}
          {missing.length > 0 && (
            <span className="text-[11px] text-content-faint">
              {"  "}
              <span className="mx-1">+</span>
              {missing.map((p, i) => (
                <span key={p}>
                  <button
                    type="button"
                    onClick={() => {
                      update(seed(p));
                      setActive(p);
                    }}
                    className="hover:text-accent hover:underline"
                  >
                    {PROP_META[p].addLabel.toLowerCase()}
                  </button>
                  {i < missing.length - 1 && <span className="mx-1">·</span>}
                </span>
              ))}
            </span>
          )}
        </p>

        {active && (
          <div className="mock-expand rounded-lg border border-border-subtle bg-content/[0.03] p-2">
            <div className="mb-1.5 flex items-center px-0.5">
              <span className={LABEL}>{PROP_META[active].label}</span>
              {PROP_META[active].clear && isSet(active, task) && (
                <button
                  type="button"
                  onClick={() => {
                    update(PROP_META[active].clear!);
                    setActive(null);
                  }}
                  className="ml-auto mr-2 text-[10.5px] text-content-faint hover:text-error"
                >
                  Remove
                </button>
              )}
              <button
                type="button"
                onClick={() => setActive(null)}
                className={cn(
                  "rounded px-1.5 py-0.5 text-[10.5px] text-accent hover:bg-accent/10",
                  !(PROP_META[active].clear && isSet(active, task)) && "ml-auto"
                )}
              >
                Done
              </button>
            </div>
            <Editor prop={active} task={task} update={update} />
          </div>
        )}
      </div>
      <Notes task={task} />
      <Tags task={task} />
    </div>
  );
}

function Cap({ children }: { children: React.ReactNode }) {
  return <span className="mock-cap">{children}</span>;
}

/* ── the row, for reference ──────────────────────────────────── */

/** What the list row already prints for this task — the argument for moving
 *  the matching fields a level down. Mirrors `TaskList`'s row. */
function RowReplica({ task }: { task: MockTask }) {
  const deferred = isDeferred(task);
  const armed = isArmed(task);
  return (
    <div className="flex items-start gap-3 rounded-lg border border-border-subtle bg-content/[0.04] px-3 py-2.5">
      <span className="mt-0.5 size-[15px] shrink-0 rounded-[4px] border border-content-muted/60" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex min-w-0 items-baseline gap-2">
          <span className="rounded border border-border-subtle bg-content/5 px-1 text-[10px] text-content-muted">
            {task.ref}
          </span>
          {task.priority < 3 && (
            <span className={cn("text-[11px] font-semibold", PRIORITY_COLORS[task.priority])}>
              P{task.priority}
            </span>
          )}
          <span className="truncate text-[13px] text-content">{task.title}</span>
        </div>
        {(deferred || armed || task.blocked_reason) && (
          <div className="truncate text-[11px] text-content-faint">
            {deferred
              ? `Starts ${formatDue(task.defer_until)}`
              : armed
                ? `Last check: ${task.tripwire?.lastNote}`
                : task.blocked_reason}
            {` · filed ${task.filedAgo} ago`}
          </div>
        )}
        <div className="flex items-center gap-2 text-[11px] text-content-faint">
          <span className={cn("rounded px-1.5 py-px font-medium", statusClass(task.status))}>
            {STATUS_LABELS[task.status]}
          </span>
          {task.project && <span>#{task.project}</span>}
          {task.tags.map((t) => (
            <span key={t} className="text-content-faint/70">
              #{t}
            </span>
          ))}
          {task.blocked_reason && (
            <span className="flex items-center gap-0.5 rounded bg-error/15 px-1 py-px font-medium text-error">
              <Ban size={9} />
              blocked
            </span>
          )}
          {deferred && (
            <span className="flex items-center gap-0.5 rounded bg-warning/15 px-1 py-px font-medium text-warning">
              <CalendarClock size={9} />
              {formatDue(task.defer_until)}
            </span>
          )}
          {armed && (
            <span className="flex items-center gap-0.5 rounded bg-warning/15 px-1 py-px font-medium text-warning">
              <Zap size={9} />
              tripwire
            </span>
          )}
          {task.recurrence && <Repeat size={11} />}
          {task.hasRepo && <GitBranch size={11} className="text-accent" />}
          {task.due_at && <span className="ml-auto shrink-0">{formatDue(task.due_at)}</span>}
        </div>
      </div>
    </div>
  );
}

/* ── harness ─────────────────────────────────────────────────── */

/** Distance from the pane's top to the Notes label — the number this mock is
 *  about. Re-measured on any size change inside the pane. */
function useNotesOffset<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [y, setY] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => {
      const notes = el.querySelector("[data-notes]");
      if (notes)
        setY(Math.round(notes.getBoundingClientRect().top - el.getBoundingClientRect().top));
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    el.querySelectorAll("*").forEach((c) => ro.observe(c));
    const mo = new MutationObserver(() => {
      measure();
      el.querySelectorAll("*").forEach((c) => ro.observe(c));
    });
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    measure();
    return () => {
      ro.disconnect();
      mo.disconnect();
    };
  }, []);
  return [ref, y] as const;
}

/** The detail column at production width (`DETAIL_WIDTH` in `AppLayout`) and a
 *  laptop window's height, scrolling like the real one. */
function Pane({
  caption,
  note,
  children,
}: {
  caption: string;
  note?: string;
  children: React.ReactNode;
}) {
  const [ref, y] = useNotesOffset<HTMLDivElement>();
  return (
    <div className="flex min-w-0 flex-col" style={{ width: "clamp(340px, 30vw, 420px)" }}>
      <div className="mb-2 flex items-baseline gap-2 px-1">
        <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">
          {caption}
        </span>
        <span className="h-px flex-1 bg-border-subtle" />
        <span className="font-mono text-[10px] tabular-nums text-accent" title="How far down the pane Notes starts">
          Notes at {y}px
        </span>
      </div>
      <div className="h-[600px] overflow-y-auto rounded-xl bg-surface">
        <div ref={ref} className="px-4 pb-8 pt-2">
          {children}
        </div>
      </div>
      {note && <p className="mt-2 px-1 text-[10px] leading-relaxed text-content-faint">{note}</p>}
    </div>
  );
}

type VariantProps = { task: MockTask; update: Update; mark: boolean };

const VARIANTS: Record<
  MockVariant,
  { name: string; blurb: string; note: string; component: (p: VariantProps) => React.ReactElement }
> = {
  A: {
    name: "Folded",
    blurb: "The whole grid behind one fold; its header is the row's meta line",
    note: "Cheapest to ship — the grid is untouched, just wrapped, and collapsed on every open like the launch console. The hold rides above the fold so a held task never looks free. Cost: opening the fold brings back all nine fields at once, empties included.",
    component: MockA,
  },
  B: {
    name: "Chips + popover",
    blurb: "Only set properties show, as the row's chips; + adds the rest",
    note: "Disclosure per property: an empty field costs nothing, a set one costs a chip. Editors float, so Notes never moves while you edit. Cost: a popover over the glass must paint opaque (gotcha #22) and commit its draft before it unmounts (gotcha #24).",
    component: MockB,
  },
  C: {
    name: "Sentence",
    blurb: "One editable sentence; each bold word opens its editor in place",
    note: "Reads as an answer, not a form — and empty properties shrink to faint words at the end. Editors open under the sentence, pushing Notes down only while you edit. Cost: a property has no fixed position to scan for, and every combination needs grammar.",
    component: MockC,
  },
};

const SAMPLE_LABELS: Record<SampleKey, string> = {
  sparse: "T202 · sparse inbox",
  scheduled: "T155 · dated + repeating",
  condition: "T205 · on a tripwire",
};

export default function DetailDisclosureMock() {
  const [variant, setVariant] = useState<MockVariant>("B");
  const [mark, setMark] = useState(true);
  const [dark, setDark] = useState(
    () => document.documentElement.getAttribute("data-theme") !== "light"
  );
  const { task, key, setKey, update, reset } = useSamples();

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }, [dark]);

  const Active = VARIANTS[variant].component;

  return (
    <div className="flex h-full flex-col bg-base text-content">
      <style>{`
        .mock-expand { animation: expandIn .16s ease-out; }
        @keyframes expandIn { from { opacity: 0; transform: translateY(-4px); } }
        .mock-pop { animation: popIn .14s cubic-bezier(.22,.9,.28,1); }
        @keyframes popIn { from { opacity: 0; transform: translateY(-6px) scale(.98); } }
        .mock-cap::first-letter { text-transform: uppercase; }
        .mock-cap { display: inline-block; }
      `}</style>

      <div className="flex flex-wrap items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <div>
          <div className="text-[12px] font-semibold">Task detail — progressive disclosure</div>
          <div className="text-[9px] text-content-faint">
            Nine fields sit between the title and the notes; most are empty or already on the row
          </div>
        </div>
        <div className="ml-3 flex overflow-hidden rounded-lg border border-border">
          {(Object.keys(VARIANTS) as MockVariant[]).map((item) => (
            <button
              key={item}
              onClick={() => setVariant(item)}
              aria-pressed={variant === item}
              className={cn(
                "px-3 py-1.5 text-[10.5px] font-medium transition-colors",
                variant === item
                  ? "bg-accent text-white"
                  : "bg-surface text-content-secondary hover:bg-elevated"
              )}
            >
              {item} · {VARIANTS[item].name}
            </button>
          ))}
        </div>
        <span className="text-[10px] text-content-muted">{VARIANTS[variant].blurb}</span>
        <button
          onClick={() => setDark((v) => !v)}
          aria-label="Toggle theme"
          className="ml-auto rounded-md border border-border p-1.5 text-content-muted hover:text-content"
        >
          {dark ? <Sun size={13} /> : <Moon size={13} />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-6">
        <div className="mx-auto flex max-w-[880px] flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">
              Sample task
            </span>
            <div className="flex overflow-hidden rounded-md border border-border">
              {(Object.keys(SAMPLE_LABELS) as SampleKey[]).map((k) => (
                <button
                  key={k}
                  onClick={() => setKey(k)}
                  aria-pressed={key === k}
                  className={cn(
                    "px-2.5 py-1 text-[10.5px]",
                    key === k ? "bg-content/12 text-content" : "text-content-muted hover:bg-content/6"
                  )}
                >
                  {SAMPLE_LABELS[k]}
                </button>
              ))}
            </div>
            <button
              onClick={reset}
              className="flex items-center gap-1 rounded-md px-1.5 py-1 text-[10.5px] text-content-faint hover:text-content"
            >
              <RotateCcw size={11} />
              Reset edits
            </button>
            <label className="ml-auto flex items-center gap-1.5 text-[10.5px] text-content-muted">
              <input type="checkbox" checked={mark} onChange={(e) => setMark(e.target.checked)} />
              Tag fields the row already shows
            </label>
          </div>
          <span className="mt-2 font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">
            The row already says
          </span>
          <RowReplica task={task} />
        </div>

        <div className="mx-auto mt-6 flex flex-wrap items-start justify-center gap-8">
          <Pane
            caption="Now · shipping"
            note="Every field renders whether or not it holds anything — on T202 seven of nine are empty, and the filled ones repeat the row above."
          >
            <ShippedPane task={task} update={update} mark={mark} />
          </Pane>
          <Pane caption={`${variant} · ${VARIANTS[variant].name}`} note={VARIANTS[variant].note}>
            <Active task={task} update={update} mark={mark} />
          </Pane>
        </div>
        <p className="mx-auto mt-6 max-w-[620px] text-center text-[10.5px] leading-relaxed text-content-faint">
          Edits are shared between the two panes — add a date or a tripwire on the right and
          watch what it costs on the left. Switch sample tasks to see the sparse and the dense
          case.
        </p>
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
