import { useEffect, useRef, useState } from "react";
import {
  Archive,
  Bot,
  CalendarClock,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Folder,
  Inbox,
  LayoutGrid,
  List,
  Menu,
  Search,
  SlidersHorizontal,
  Sun,
  Tag,
  Target,
  Wand2,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — three ways to move around the PHONE app.
   Reached only via `?mock=ios-nav` (see main.tsx); data is static.

   The friction being explored: the iPhone app has five tabs
   (Today · Inbox · Review · Agents · Search) and everything else —
   Upcoming, Open, Done, Archive, the board, every area and project —
   sits behind Browse, a screen reached only from Today's toolbar.
   Inbox → Upcoming is three taps across two tabs, and nothing swipes.

   Each variant is one phone. `ALL` puts the three side by side.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C" | "D" | "ALL";

type ViewKey = "today" | "inbox" | "upcoming" | "open" | "review" | "done" | "archive";

interface ViewDef {
  key: ViewKey;
  label: string;
  icon: React.ComponentType<{ size?: number; strokeWidth?: number; className?: string }>;
  count?: number;
}

const VIEWS: ViewDef[] = [
  { key: "today", label: "Today", icon: Sun, count: 4 },
  { key: "inbox", label: "Inbox", icon: Inbox, count: 7 },
  { key: "upcoming", label: "Upcoming", icon: CalendarClock, count: 15 },
  { key: "open", label: "Open", icon: List, count: 31 },
  { key: "review", label: "Review", icon: Wand2, count: 1 },
  { key: "done", label: "Done", icon: CheckCircle2 },
  { key: "archive", label: "Archive", icon: Archive },
];

/** The views a LISTS pager pages through — Today keeps its own tab. */
const LIST_VIEWS = VIEWS.filter((v) => v.key !== "today");

type Activity = "active" | "idle" | "dormant";

interface ProjectDef {
  name: string;
  open: number;
  activity: Activity;
}

interface AreaDef {
  name: string;
  projects: ProjectDef[];
}

const AREAS: AreaDef[] = [
  {
    name: "CX Ventures",
    projects: [
      { name: "pocketbuddy", open: 9, activity: "active" },
      { name: "cxtasks", open: 12, activity: "active" },
      { name: "buildersbuddy", open: 4, activity: "idle" },
      { name: "artist advisory", open: 6, activity: "dormant" },
    ],
  },
  {
    name: "Clients",
    projects: [
      { name: "acme", open: 5, activity: "active" },
      { name: "globex", open: 3, activity: "idle" },
      { name: "initech", open: 2, activity: "idle" },
    ],
  },
  {
    name: "Personal",
    projects: [
      { name: "music", open: 2, activity: "active" },
      { name: "home", open: 1, activity: "dormant" },
    ],
  },
];

const TAGS = [
  { name: "billable", count: 6 },
  { name: "quick-win", count: 3 },
  { name: "keystone", count: 2 },
];

interface MockTask {
  t: number;
  title: string;
  project: string;
  priority: 0 | 1 | 2 | 3;
  status: string;
  due?: string;
  overdue?: boolean;
  tripwire?: boolean;
}

const TASKS: Record<ViewKey, MockTask[]> = {
  today: [
    { t: 41, title: "Wire intake webhook retry queue", project: "acme", priority: 0, status: "In progress", due: "5:00 PM" },
    { t: 31, title: "Invoice July retainer", project: "globex", priority: 0, status: "To do", due: "Overdue 1d", overdue: true },
    { t: 29, title: "Renew Developer ID certificate", project: "cxtasks", priority: 1, status: "To do", due: "Today" },
    { t: 47, title: "Fix light-theme select popup contrast", project: "cxtasks", priority: 3, status: "To do", due: "Today" },
  ],
  inbox: [
    { t: 16, title: "Cancel Apify subscription", project: "CX Ventures", priority: 1, status: "Inbox", due: "Overdue 1d", overdue: true },
    { t: 139, title: "Ask Dana about the Northwind handoff", project: "globex", priority: 2, status: "Inbox" },
    { t: 140, title: "Look at the Fulton September invite", project: "Personal", priority: 3, status: "Inbox" },
    { t: 141, title: "Draft the intake product-line pricing page", project: "acme", priority: 2, status: "Inbox" },
    { t: 142, title: "Move the resume build to the NAS", project: "resume-platform", priority: 3, status: "Inbox" },
    { t: 137, title: "Reply to Priya re: WS2 scope", project: "initech", priority: 2, status: "Inbox" },
    { t: 138, title: "Book studio time for the September session", project: "music", priority: 3, status: "Inbox" },
  ],
  upcoming: [
    { t: 118, title: "TRIPWIRE: PocketBuddy reaches 50 outside users", project: "pocketbuddy", priority: 3, status: "To do", tripwire: true },
    { t: 128, title: "TRIPWIRE: PocketBuddy reaches 200 paying users", project: "pocketbuddy", priority: 3, status: "To do", tripwire: true },
    { t: 88, title: "Send the Fulton September invite", project: "Personal", priority: 2, status: "To do", due: "Sep 10" },
    { t: 122, title: "Get Tech E&O / cyber liability insurance", project: "CX Ventures", priority: 2, status: "To do", tripwire: true },
    { t: 45, title: "Ship CXMail 1.0.1", project: "cxmail", priority: 1, status: "To do", due: "Sep 2" },
  ],
  open: [
    { t: 11, title: "Explore creatify for UGC for pocketbuddy", project: "pocketbuddy", priority: 0, status: "In progress" },
    { t: 136, title: "Audit whose name the PocketBuddy vendor accounts are in", project: "pocketbuddy", priority: 2, status: "To do" },
    { t: 129, title: "Deterministic advice guardrails for the advisor", project: "pocketbuddy", priority: 2, status: "To do" },
    { t: 132, title: "Convert the Apple Developer membership to Organization", project: "pocketbuddy", priority: 2, status: "Blocked" },
    { t: 38, title: "Proposal e-sign audit trail", project: "cxventures", priority: 1, status: "In review" },
  ],
  review: [
    { t: 12, title: "Understand legal risks of scaling", project: "pocketbuddy", priority: 0, status: "In review" },
  ],
  done: [
    { t: 35, title: "Draft Learning Hub kickoff email", project: "globex", priority: 2, status: "Done" },
    { t: 25, title: "Read the cold-outreach batch", project: "Apollo Outreach Review", priority: 3, status: "Done" },
  ],
  archive: [
    { t: 9, title: "Old Acme discovery notes", project: "acme", priority: 3, status: "Done" },
  ],
};

/** A project list, generated from the pool so every project has rows. */
function projectTasks(name: string): MockTask[] {
  const pool = Object.values(TASKS).flat();
  const own = pool.filter((t) => t.project.toLowerCase() === name.toLowerCase());
  if (own.length) return own;
  return pool.slice(0, 3).map((t, i) => ({ ...t, t: 200 + i, project: name }));
}

const PRIORITY_TEXT = ["text-[#ff6b6b]", "text-[#ffb545]", "text-[#5b9dff]", "text-white/40"];
const ACTIVITY_DOT: Record<Activity, string> = {
  active: "bg-[#34c759]",
  idle: "bg-[#ffb545]",
  dormant: "bg-white/25",
};
const STATUS_PILL: Record<string, string> = {
  "In progress": "bg-[#5b9dff]/20 text-[#8ab8ff]",
  "In review": "bg-[#bf7bff]/20 text-[#d4a8ff]",
  Blocked: "bg-[#ff6b6b]/20 text-[#ff8f8f]",
  Done: "bg-[#34c759]/20 text-[#7ee29a]",
};

/* ── Scope: what a list screen is showing ─────────────────────── */

type Scope = { kind: "view"; key: ViewKey } | { kind: "project"; area: string; name: string };

function scopeLabel(s: Scope): string {
  return s.kind === "view" ? VIEWS.find((v) => v.key === s.key)!.label : s.name;
}

/** The pages a horizontal swipe moves through: sibling views, or the
 *  sibling projects of one area. */
function siblings(s: Scope): Scope[] {
  if (s.kind === "view") return LIST_VIEWS.map((v) => ({ kind: "view", key: v.key }));
  const area = AREAS.find((a) => a.name === s.area)!;
  return area.projects.map((p) => ({ kind: "project", area: area.name, name: p.name }));
}

function scopeIndex(s: Scope): number {
  return siblings(s).findIndex((x) =>
    x.kind === "view" ? s.kind === "view" && x.key === s.key : s.kind === "project" && x.name === s.name
  );
}

function scopeTasks(s: Scope): MockTask[] {
  return s.kind === "view" ? TASKS[s.key] : projectTasks(s.name);
}

/* ── Phone chrome ─────────────────────────────────────────────── */

function Phone({ children, label }: { children: React.ReactNode; label: string }) {
  return (
    <div className="flex shrink-0 flex-col items-center gap-3">
      <div className="relative h-[760px] w-[372px] overflow-hidden rounded-[48px] border-[10px] border-black bg-[#0b0d12] shadow-[0_30px_80px_-20px_rgba(0,0,0,.7)] ring-1 ring-white/10">
        <div className="absolute left-1/2 top-3 z-30 h-[30px] w-[112px] -translate-x-1/2 rounded-full bg-black" />
        <div className="absolute inset-x-0 top-0 z-20 flex h-12 items-end justify-between px-7 pb-1.5 text-[13px] font-semibold text-white">
          <span>9:41</span>
          <span className="text-[10px] tracking-tight">●●●● ᯤ ▮</span>
        </div>
        <div className="absolute inset-0 bg-[radial-gradient(120%_80%_at_20%_0%,#1a2340_0%,#0b0d12_55%,#05060a_100%)]" />
        <div className="relative flex h-full flex-col pt-12 text-white">{children}</div>
        <div className="pointer-events-none absolute bottom-2 left-1/2 z-30 h-1.5 w-32 -translate-x-1/2 rounded-full bg-white/70" />
      </div>
      <div className="max-w-[372px] text-center text-[11px] leading-snug text-content-faint">{label}</div>
    </div>
  );
}

interface TabDef {
  key: string;
  label: string;
  icon: ViewDef["icon"];
  badge?: number;
}

function TabBar({ tabs, active, onSelect }: { tabs: TabDef[]; active: string; onSelect?: (k: string) => void }) {
  return (
    <div className="z-20 mt-auto flex items-start justify-around border-t border-white/10 bg-black/45 px-2 pb-7 pt-2 backdrop-blur-md">
      {tabs.map((t) => (
        <button
          key={t.key}
          onClick={() => onSelect?.(t.key)}
          className={cn(
            "relative flex w-16 flex-col items-center gap-1 text-[10px]",
            active === t.key ? "text-[#4f8cff]" : "text-white/55"
          )}
        >
          <t.icon size={22} strokeWidth={1.8} />
          {t.label}
          {t.badge ? (
            <span className="absolute -top-1 left-9 rounded-full bg-[#ff453a] px-1.5 text-[10px] font-semibold leading-4 text-white">
              {t.badge}
            </span>
          ) : null}
        </button>
      ))}
    </div>
  );
}

function NavBar({
  leading,
  title,
  subtitle,
  center,
  trailing,
}: {
  leading?: React.ReactNode;
  title?: string;
  subtitle?: string;
  center?: React.ReactNode;
  trailing?: React.ReactNode;
}) {
  return (
    <div className="flex h-12 shrink-0 items-center px-3">
      <div className="flex w-14 items-center">{leading}</div>
      <div className="flex min-w-0 flex-1 flex-col items-center leading-tight">
        {center ?? (
          <>
            <span className="truncate text-[16px] font-semibold">{title}</span>
            {subtitle && <span className="text-[11px] text-white/45">{subtitle}</span>}
          </>
        )}
      </div>
      <div className="flex w-14 items-center justify-end">{trailing}</div>
    </div>
  );
}

function Row({ task }: { task: MockTask }) {
  return (
    <div className="flex items-start gap-3 rounded-xl border border-white/[0.07] bg-white/[0.05] px-3 py-2.5">
      <span className="mt-0.5 size-5 shrink-0 rounded-md border-[1.5px] border-white/25" />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="shrink-0 font-mono text-[11px] text-white/35">T{task.t}</span>
          {task.priority < 3 && (
            <span className={cn("shrink-0 font-mono text-[11px] font-medium", PRIORITY_TEXT[task.priority])}>
              P{task.priority}
            </span>
          )}
          <span className="truncate text-[15px]">{task.title}</span>
        </div>
        <div className="mt-1.5 flex items-center gap-2 text-[12px]">
          <span
            className={cn(
              "rounded-full px-2 py-0.5 text-[11px] font-medium",
              STATUS_PILL[task.status] ?? "bg-white/10 text-white/75"
            )}
          >
            {task.status}
          </span>
          <span className="truncate text-white/45">#{task.project}</span>
          <span className="ml-auto" />
          {task.tripwire && (
            <span className="flex shrink-0 items-center gap-1 rounded-full bg-amber-400/15 px-2 py-0.5 text-[11px] font-medium text-amber-300">
              <Zap size={9} /> tripwire
            </span>
          )}
          {task.due && (
            <span className={cn("shrink-0", task.overdue ? "text-[#ff6b6b]" : "text-white/45")}>{task.due}</span>
          )}
        </div>
      </div>
    </div>
  );
}

function ListPage({ scope }: { scope: Scope }) {
  const tasks = scopeTasks(scope);
  return (
    <div className="flex h-full w-full shrink-0 basis-full flex-col gap-2 overflow-y-auto px-3 pb-4 pt-1 [scrollbar-width:none]">
      {tasks.length ? (
        tasks.map((t) => <Row key={t.t} task={t} />)
      ) : (
        <div className="pt-16 text-center text-[13px] text-white/40">Nothing here.</div>
      )}
      <div className="pt-2 text-center text-[10px] text-white/25">‹ swipe for the next list ›</div>
    </div>
  );
}

/* ── Swipe deck — real pointer drag, snaps to a page ──────────── */

function SwipeDeck({
  count,
  index,
  onIndex,
  children,
}: {
  count: number;
  index: number;
  onIndex: (i: number) => void;
  children: React.ReactNode;
}) {
  const [drag, setDrag] = useState<{ startX: number; dx: number; width: number } | null>(null);
  const dx = drag?.dx ?? 0;
  return (
    <div
      className="relative min-h-0 flex-1 touch-pan-y select-none overflow-hidden"
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        setDrag({ startX: e.clientX, dx: 0, width: e.currentTarget.clientWidth || 1 });
      }}
      onPointerMove={(e) => {
        if (drag) setDrag({ ...drag, dx: e.clientX - drag.startX });
      }}
      onPointerUp={() => {
        if (!drag) return;
        const threshold = drag.width * 0.2;
        if (drag.dx < -threshold && index < count - 1) onIndex(index + 1);
        else if (drag.dx > threshold && index > 0) onIndex(index - 1);
        setDrag(null);
      }}
      onPointerCancel={() => setDrag(null)}
    >
      <div
        className="flex h-full"
        style={{
          transform: `translateX(calc(${-index * 100}% + ${dx}px))`,
          transition: drag ? "none" : "transform .28s cubic-bezier(.2,.8,.2,1)",
        }}
      >
        {children}
      </div>
    </div>
  );
}

function Dots({ count, index }: { count: number; index: number }) {
  return (
    <div className="flex h-4 items-center justify-center gap-1.5">
      {Array.from({ length: count }, (_, i) => (
        <span
          key={i}
          className={cn(
            "h-1.5 rounded-full transition-all",
            i === index ? "w-4 bg-white/80" : "w-1.5 bg-white/25"
          )}
        />
      ))}
    </div>
  );
}

/* ── The sidebar's rows, shared by B's drawer and C's sheet ────── */

function ViewRow({ v, active, onClick, compact }: { v: ViewDef; active: boolean; onClick: () => void; compact?: boolean }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex w-full items-center rounded-lg pr-2 text-left",
        // Compact is the SHEET's column, which is narrow and holds the one
        // long word in the set ("Upcoming"): the gap and the left padding are
        // what that word runs out of, so both tighten rather than the type.
        compact ? "gap-2 py-1.5 pl-2 text-[13.5px]" : "gap-2.5 py-2 pl-2.5 text-[14px]",
        active ? "bg-[#4f8cff]/20 text-[#9cc0ff]" : "text-white/85 hover:bg-white/5"
      )}
    >
      <v.icon size={16} strokeWidth={1.9} className={active ? "text-[#9cc0ff]" : "text-white/55"} />
      <span className="flex-1 truncate">{v.label}</span>
      {v.count ? <span className="font-mono text-[11px] text-white/40">{v.count}</span> : null}
    </button>
  );
}

function ProjectRow({
  p,
  active,
  onClick,
  indent = true,
}: {
  p: ProjectDef;
  active: boolean;
  onClick: () => void;
  indent?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex w-full items-center gap-2.5 rounded-lg py-1.5 pr-2.5 text-left text-[14px]",
        indent ? "pl-8" : "pl-2.5",
        active ? "bg-[#4f8cff]/20 text-[#9cc0ff]" : "text-white/85 hover:bg-white/5"
      )}
    >
      <Target size={14} strokeWidth={1.9} className="text-white/45" />
      <span className="flex-1 truncate">{p.name}</span>
      <span className={cn("size-1.5 rounded-full", ACTIVITY_DOT[p.activity])} />
      <span className="font-mono text-[11px] text-white/40">{p.open}</span>
    </button>
  );
}

/* ── The two surfaces, shared ──────────────────────────────────
   B opens the drawer, C opens the sheet, D opens both — so they
   live here rather than inside a variant. If they were copied per
   variant the switcher would be comparing three drifting drawers
   instead of three navigations. */

function SidebarDrawer({
  open,
  onClose,
  scope,
  onChoose,
}: {
  open: boolean;
  onClose: () => void;
  scope: Scope;
  onChoose: (s: Scope) => void;
}) {
  const active = (s: Scope) =>
    s.kind === "view"
      ? scope.kind === "view" && scope.key === s.key
      : scope.kind === "project" && scope.name === s.name;
  return (
    <div className={cn("absolute inset-0 z-40", open ? "" : "pointer-events-none")}>
      <div
        className={cn("absolute inset-0 bg-black/55 transition-opacity", open ? "opacity-100" : "opacity-0")}
        onClick={onClose}
      />
      <aside
        className={cn(
          "absolute inset-y-0 left-0 flex w-[78%] flex-col gap-3 overflow-y-auto border-r border-white/10 bg-[#111520]/95 px-2.5 pb-8 pt-14 backdrop-blur-xl transition-transform duration-300 [scrollbar-width:none]",
          open ? "translate-x-0" : "-translate-x-full"
        )}
      >
        <div className="px-2.5 text-[11px] font-medium uppercase tracking-wide text-white/40">Views</div>
        <div>
          {VIEWS.map((v) => (
            <ViewRow
              key={v.key}
              v={v}
              compact
              active={active({ kind: "view", key: v.key })}
              onClick={() => onChoose({ kind: "view", key: v.key })}
            />
          ))}
        </div>
        <div className="px-2.5 text-[11px] font-medium uppercase tracking-wide text-white/40">Areas</div>
        <div>
          {AREAS.map((a) => (
            <div key={a.name}>
              <div className="flex items-center gap-2.5 px-2.5 py-1.5 text-[13px] font-semibold text-white/80">
                <Folder size={15} className="text-white/50" />
                {a.name}
              </div>
              {a.projects.map((p) => (
                <ProjectRow
                  key={p.name}
                  p={p}
                  active={active({ kind: "project", area: a.name, name: p.name })}
                  onClick={() => onChoose({ kind: "project", area: a.name, name: p.name })}
                />
              ))}
            </div>
          ))}
        </div>
        <div className="px-2.5 text-[11px] font-medium uppercase tracking-wide text-white/40">Tags</div>
        <div>
          {TAGS.map((t) => (
            <button
              key={t.name}
              className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-[13px] text-white/85"
            >
              <Tag size={13} className="text-white/45" />
              <span className="flex-1">#{t.name}</span>
              <span className="font-mono text-[11px] text-white/40">{t.count}</span>
            </button>
          ))}
        </div>
      </aside>
    </div>
  );
}

/** The left edge, as its own 22px strip — so a drag that starts at the
 *  bezel opens the drawer and the pager underneath never sees it. */
function EdgeGrab({ onOpen }: { onOpen: () => void }) {
  const [from, setFrom] = useState<number | null>(null);
  return (
    <div
      className="absolute inset-y-14 left-0 z-30 w-[22px]"
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        setFrom(e.clientX);
      }}
      onPointerMove={(e) => {
        if (from !== null && e.clientX - from > 40) {
          onOpen();
          setFrom(null);
        }
      }}
      onPointerUp={() => setFrom(null)}
    />
  );
}

function JumpSheet({
  open,
  onClose,
  scope,
  onChoose,
}: {
  open: boolean;
  onClose: () => void;
  scope: Scope;
  onChoose: (s: Scope) => void;
}) {
  const [expanded, setExpanded] = useState<string | null>(
    scope.kind === "project" ? scope.area : AREAS[0].name
  );
  // Re-open on the area you are in each time the sheet is raised: with a
  // stale expansion the one project list you are looking at is the one the
  // sheet makes you go hunting for.
  useEffect(() => {
    if (open && scope.kind === "project") setExpanded(scope.area);
  }, [open, scope]);
  const active = (s: Scope) =>
    s.kind === "view"
      ? scope.kind === "view" && scope.key === s.key
      : scope.kind === "project" && scope.name === s.name;
  return (
    <div className={cn("absolute inset-0 z-40", open ? "" : "pointer-events-none")}>
      <div
        className={cn("absolute inset-0 bg-black/55 transition-opacity", open ? "opacity-100" : "opacity-0")}
        onClick={onClose}
      />
      <div
        className={cn(
          "absolute inset-x-0 bottom-0 flex h-[72%] flex-col rounded-t-3xl border-t border-white/10 bg-[#111520]/95 backdrop-blur-xl transition-transform duration-300",
          open ? "translate-y-0" : "translate-y-full"
        )}
      >
        <div className="mx-auto mt-2 h-1 w-9 rounded-full bg-white/30" />
        <div className="px-4 pb-2 pt-3">
          <div className="flex items-center gap-2 rounded-xl bg-white/10 px-3 py-2 text-[14px] text-white/45">
            <Search size={15} />
            Jump to a view, area or project
          </div>
        </div>
        <div className="flex min-h-0 flex-1 gap-2 px-2.5 pb-6">
          <div className="flex w-[38%] shrink-0 flex-col">
            <div className="px-2.5 pb-1 text-[11px] font-medium uppercase tracking-wide text-white/40">Views</div>
            <div className="overflow-y-auto [scrollbar-width:none]">
              {VIEWS.map((v) => (
                <ViewRow
                  key={v.key}
                  v={v}
                  compact
                  active={active({ kind: "view", key: v.key })}
                  onClick={() => onChoose({ kind: "view", key: v.key })}
                />
              ))}
            </div>
          </div>
          <div className="w-px bg-white/10" />
          <div className="flex min-w-0 flex-1 flex-col">
            <div className="px-2.5 pb-1 text-[11px] font-medium uppercase tracking-wide text-white/40">Areas</div>
            <div className="overflow-y-auto [scrollbar-width:none]">
              {AREAS.map((a) => {
                const isOpen = expanded === a.name;
                return (
                  <div key={a.name}>
                    <button
                      onClick={() => setExpanded(isOpen ? null : a.name)}
                      className="flex w-full items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-[13px] font-semibold text-white/85"
                    >
                      <ChevronRight size={13} className={cn("text-white/45 transition-transform", isOpen && "rotate-90")} />
                      <span className="flex-1 truncate">{a.name}</span>
                      <span className="font-mono text-[11px] font-normal text-white/40">
                        {a.projects.reduce((n, p) => n + p.open, 0)}
                      </span>
                    </button>
                    {isOpen &&
                      a.projects.map((p) => (
                        <ProjectRow
                          key={p.name}
                          p={p}
                          indent
                          active={active({ kind: "project", area: a.name, name: p.name })}
                          onClick={() => onChoose({ kind: "project", area: a.name, name: p.name })}
                        />
                      ))}
                  </div>
                );
              })}
              <div className="mt-2 px-2.5 pb-1 text-[11px] font-medium uppercase tracking-wide text-white/40">Tags</div>
              <div className="flex flex-wrap gap-1.5 px-2.5">
                {TAGS.map((t) => (
                  <span key={t.name} className="rounded-full bg-white/10 px-2 py-0.5 text-[12px] text-white/80">
                    #{t.name}
                  </span>
                ))}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/** The scope pill — the title IS the control (variant C's move). */
function ScopePill({ scope, onOpen }: { scope: Scope; onOpen: () => void }) {
  return (
    <button
      onClick={onOpen}
      className="flex max-w-full items-center gap-1.5 rounded-full border border-white/15 bg-white/10 py-1 pl-3 pr-2 text-[14px] font-semibold"
    >
      {scope.kind === "project" && <span className="font-normal text-white/45">{scope.area} /</span>}
      <span className="truncate">{scopeLabel(scope)}</span>
      <ChevronDown size={14} className="text-white/60" />
    </button>
  );
}

/** The tab whose label FOLLOWS the swipe — B's move, kept in one place.
 *  The badge is the Inbox's, so it appears only when that is the scope. */
function scopeTab(scope: Scope): TabDef {
  return {
    key: "lists",
    label: scopeLabel(scope),
    icon: scope.kind === "view" ? VIEWS.find((v) => v.key === scope.key)!.icon : Target,
    badge: scope.kind === "view" && scope.key === "inbox" ? 7 : undefined,
  };
}

/* ── Variant A — "Lists pager" ─────────────────────────────────
   The safe one. Inbox becomes LISTS: a scrollable segmented strip
   of every view that has no tab, with a real horizontal swipe
   between them and the strip following along. Areas and projects
   stop hiding behind Today's toolbar: Browse gets the tab Review
   used to hold, and Review becomes a segment in the strip — its
   badge rides on the Lists tab. Every screen is one the app has
   already; only where they sit changes. */
function MockA() {
  const [tab, setTab] = useState("lists");
  const [page, setPage] = useState(0);
  const strip = useRef<HTMLDivElement>(null);
  useEffect(() => {
    strip.current
      ?.querySelector<HTMLElement>(`[data-page="${page}"]`)
      ?.scrollIntoView({ inline: "center", block: "nearest", behavior: "smooth" });
  }, [page]);

  const tabs: TabDef[] = [
    { key: "today", label: "Today", icon: Sun },
    { key: "lists", label: "Lists", icon: Inbox, badge: 7 },
    { key: "browse", label: "Browse", icon: LayoutGrid },
    { key: "agents", label: "Agents", icon: Bot, badge: 1 },
    { key: "search", label: "Search", icon: Search },
  ];

  return (
    <Phone label="A · Lists pager — a segmented strip over a swipeable pager; Browse becomes a tab, Review becomes a segment. Nothing new to learn: iOS already reads this as “swipe between sections”.">
      {tab === "lists" ? (
        <>
          <NavBar
            title="Lists"
            trailing={<SlidersHorizontal size={18} className="text-[#4f8cff]" />}
          />
          <div
            ref={strip}
            className="flex shrink-0 gap-1.5 overflow-x-auto px-3 pb-2 pt-0.5 [scrollbar-width:none]"
          >
            {LIST_VIEWS.map((v, i) => (
              <button
                key={v.key}
                data-page={i}
                onClick={() => setPage(i)}
                className={cn(
                  "flex shrink-0 items-center gap-1.5 rounded-full px-3 py-1.5 text-[13px] font-medium transition-colors",
                  i === page ? "bg-white text-black" : "bg-white/10 text-white/75"
                )}
              >
                <v.icon size={13} strokeWidth={2} />
                {v.label}
                {v.count ? (
                  <span className={cn("font-mono text-[11px]", i === page ? "text-black/50" : "text-white/40")}>
                    {v.count}
                  </span>
                ) : null}
              </button>
            ))}
          </div>
          <SwipeDeck count={LIST_VIEWS.length} index={page} onIndex={setPage}>
            {LIST_VIEWS.map((v) => (
              <ListPage key={v.key} scope={{ kind: "view", key: v.key }} />
            ))}
          </SwipeDeck>
        </>
      ) : tab === "browse" ? (
        <>
          <NavBar title="Browse" />
          <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 pb-3 [scrollbar-width:none]">
            <div className="rounded-xl border border-white/[0.07] bg-white/[0.05] p-1.5">
              <button className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[14px] text-white/85">
                <LayoutGrid size={16} className="text-white/55" />
                <span className="flex-1">Board</span>
                <ChevronRight size={14} className="text-white/30" />
              </button>
            </div>
            <div className="px-1 text-[11px] font-medium uppercase tracking-wide text-white/40">Areas & projects</div>
            <div className="rounded-xl border border-white/[0.07] bg-white/[0.05] p-1.5">
              {AREAS.map((a) => (
                <div key={a.name}>
                  <button className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[14px] font-semibold text-white/90">
                    <Folder size={16} className="text-white/55" />
                    <span className="flex-1">{a.name}</span>
                    <span className="font-mono text-[11px] font-normal text-white/40">
                      {a.projects.reduce((n, p) => n + p.open, 0)}
                    </span>
                  </button>
                  {a.projects.map((p) => (
                    <ProjectRow key={p.name} p={p} active={false} onClick={() => {}} />
                  ))}
                </div>
              ))}
            </div>
            <div className="px-1 text-[11px] font-medium uppercase tracking-wide text-white/40">Tags</div>
            <div className="rounded-xl border border-white/[0.07] bg-white/[0.05] p-1.5">
              {TAGS.map((t) => (
                <button key={t.name} className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-[14px] text-white/85">
                  <Tag size={14} className="text-white/45" />
                  <span className="flex-1">#{t.name}</span>
                  <span className="font-mono text-[11px] text-white/40">{t.count}</span>
                </button>
              ))}
            </div>
          </div>
        </>
      ) : (
        <>
          <NavBar title={tabs.find((t) => t.key === tab)?.label} />
          <div className="flex flex-1 items-center justify-center text-[13px] text-white/35">unchanged</div>
        </>
      )}
      <TabBar tabs={tabs} active={tab} onSelect={setTab} />
    </Phone>
  );
}

/* ── Variant B — "Edge drawer" ─────────────────────────────────
   The desktop sidebar comes to the phone as a drawer: swipe in
   from the left edge (or tap ☰) on ANY list, and the whole map is
   there. The list screen names its scope in the title, a
   horizontal swipe moves between that scope's siblings, and the
   second TAB follows along — so the toolbar always says where you
   are without anyone having to look up. */
function MockB() {
  const [scope, setScope] = useState<Scope>({ kind: "view", key: "inbox" });
  const [open, setOpen] = useState(false);
  const sibs = siblings(scope);
  const idx = scopeIndex(scope);
  const count = scopeTasks(scope).length;

  const tabs: TabDef[] = [
    { key: "today", label: "Today", icon: Sun },
    scopeTab(scope),
    { key: "agents", label: "Agents", icon: Bot, badge: 1 },
    { key: "search", label: "Search", icon: Search },
  ];

  return (
    <Phone label="B · Edge drawer — the desktop sidebar, one swipe from the left edge of any list. The title names the scope, and the second tab's label follows the swipe.">
      <NavBar
        leading={
          <button onClick={() => setOpen(true)} className="rounded-md p-1 text-[#4f8cff]" aria-label="Open drawer">
            <Menu size={22} />
          </button>
        }
        title={scopeLabel(scope)}
        subtitle={scope.kind === "project" ? `${scope.area} · ${count} open` : `${count} tasks`}
        trailing={<SlidersHorizontal size={18} className="text-[#4f8cff]" />}
      />
      <Dots count={sibs.length} index={idx} />
      <SwipeDeck count={sibs.length} index={idx} onIndex={(i) => setScope(sibs[i])}>
        {sibs.map((s) => (
          <ListPage key={scopeLabel(s)} scope={s} />
        ))}
      </SwipeDeck>
      <TabBar tabs={tabs} active="lists" />
      <EdgeGrab onOpen={() => setOpen(true)} />
      <SidebarDrawer
        open={open}
        onClose={() => setOpen(false)}
        scope={scope}
        onChoose={(s) => {
          setScope(s);
          setOpen(false);
        }}
      />
    </Phone>
  );
}

/* ── Variant C — "Scope pill + jump sheet" ─────────────────────
   The title itself is the control: a pill that names the scope and
   opens a half-height JUMP sheet with two columns — views on the
   left, areas on the right expanding in place to their projects —
   plus a search field, so any list is a tap and a half from any
   other. Under the pill, page dots: the list is a deck, and a
   swipe moves through the scope's siblings. */
function MockC() {
  const [scope, setScope] = useState<Scope>({ kind: "view", key: "inbox" });
  const [sheet, setSheet] = useState(false);
  const sibs = siblings(scope);
  const idx = scopeIndex(scope);

  const tabs: TabDef[] = [
    { key: "today", label: "Today", icon: Sun },
    { key: "lists", label: "Lists", icon: Inbox, badge: 7 },
    { key: "agents", label: "Agents", icon: Bot, badge: 1 },
    { key: "search", label: "Search", icon: Search },
  ];

  return (
    <Phone label="C · Scope pill + jump sheet — the title is the control. Tap it for a half-height sheet: views on the left, areas expanding in place on the right, a search field on top. Page dots say the list is a deck; swipe walks it.">
      <NavBar
        center={<ScopePill scope={scope} onOpen={() => setSheet(true)} />}
        trailing={<SlidersHorizontal size={18} className="text-[#4f8cff]" />}
      />
      <Dots count={sibs.length} index={idx} />
      <SwipeDeck count={sibs.length} index={idx} onIndex={(i) => setScope(sibs[i])}>
        {sibs.map((s) => (
          <ListPage key={scopeLabel(s)} scope={s} />
        ))}
      </SwipeDeck>
      <TabBar tabs={tabs} active="lists" />
      <JumpSheet
        open={sheet}
        onClose={() => setSheet(false)}
        scope={scope}
        onChoose={(s) => {
          setScope(s);
          setSheet(false);
        }}
      />
    </Phone>
  );
}

/* ── Variant D — "The build" ───────────────────────────────────
   B's hamburger and C's pill on one screen, which is what was
   asked for, and they are not redundant once you say what each is
   FOR:

   * the **pill** is where you ARE — it names the scope, and opens
     the two-column sheet for a fast jump between the handful of
     places you actually live in;
   * the **hamburger** is the whole MAP — every view, every area
     and project, tags — and it keeps the left-edge swipe, so
     reaching it costs no tap at all;
   * the **swipe** walks the scope's siblings, and the second tab's
     label follows it, so the toolbar always answers "which list is
     this" without a glance upward.

   Three ways in, each cheaper than the last, and the tab bar stops
   being the only way to change what you are looking at. */
function MockD() {
  const [scope, setScope] = useState<Scope>({ kind: "view", key: "inbox" });
  const [drawer, setDrawer] = useState(false);
  const [sheet, setSheet] = useState(false);
  const sibs = siblings(scope);
  const idx = scopeIndex(scope);

  const tabs: TabDef[] = [
    { key: "today", label: "Today", icon: Sun },
    scopeTab(scope),
    { key: "agents", label: "Agents", icon: Bot, badge: 1 },
    { key: "search", label: "Search", icon: Search },
  ];

  const choose = (s: Scope) => {
    setScope(s);
    setDrawer(false);
    setSheet(false);
  };

  return (
    <Phone label="D · The build — hamburger (the whole map, still swipeable from the edge) + scope pill (where you are, and a fast jump) + a swipe that walks the siblings and renames the second tab.">
      <NavBar
        leading={
          <button onClick={() => setDrawer(true)} className="rounded-md p-1 text-[#4f8cff]" aria-label="Open drawer">
            <Menu size={22} />
          </button>
        }
        center={<ScopePill scope={scope} onOpen={() => setSheet(true)} />}
        trailing={<SlidersHorizontal size={18} className="text-[#4f8cff]" />}
      />
      <Dots count={sibs.length} index={idx} />
      <SwipeDeck count={sibs.length} index={idx} onIndex={(i) => setScope(sibs[i])}>
        {sibs.map((s) => (
          <ListPage key={scopeLabel(s)} scope={s} />
        ))}
      </SwipeDeck>
      <TabBar tabs={tabs} active="lists" />
      <EdgeGrab onOpen={() => setDrawer(true)} />
      <SidebarDrawer open={drawer} onClose={() => setDrawer(false)} scope={scope} onChoose={choose} />
      <JumpSheet open={sheet} onClose={() => setSheet(false)} scope={scope} onChoose={choose} />
    </Phone>
  );
}

/* ── Page ──────────────────────────────────────────────────────── */

const VARIANTS: Record<
  MockVariant,
  { name: string; blurb: string; el: React.ComponentType }
> = {
  A: {
    name: "Lists pager",
    blurb: "Segmented strip + swipe; Browse becomes a tab. The conventional move.",
    el: MockA,
  },
  B: {
    name: "Edge drawer",
    blurb: "The desktop sidebar as a left-edge drawer on every list; the title names the scope.",
    el: MockB,
  },
  C: {
    name: "Scope pill",
    blurb: "The title opens a two-column jump sheet; the list is a deck you swipe through.",
    el: MockC,
  },
  D: {
    name: "The build",
    blurb: "Hamburger + scope pill + a swipe that renames the tab. B and C combined — the one to build.",
    el: MockD,
  },
  ALL: {
    name: "Side by side",
    blurb: "All four, each with its own state. Swipe the lists; open the drawer and the sheet.",
    el: () => (
      <div className="flex items-start justify-center gap-8">
        <MockA />
        <MockB />
        <MockC />
        <MockD />
      </div>
    ),
  },
};

export default function IosNavMock() {
  const [variant, setVariant] = useState<MockVariant>("D");
  // The phones are always dark, like the screenshots they mimic; the page
  // chrome follows, or the switcher sits in a light bar over a dark study.
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", "dark");
  }, []);
  const Active = VARIANTS[variant].el;

  return (
    <div className="iosnav flex h-full flex-col bg-base text-content">
      <div className="flex items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <span className="text-[12px] font-semibold">CXTasks iOS — navigation exploration</span>
        <div className="flex overflow-hidden rounded-lg border border-border">
          {(Object.keys(VARIANTS) as MockVariant[]).map((v) => (
            <button
              key={v}
              onClick={() => setVariant(v)}
              className={cn(
                "px-3 py-1 text-[11px] font-medium",
                variant === v ? "bg-accent text-white" : "bg-surface text-content-secondary hover:bg-elevated"
              )}
            >
              {v === "ALL" ? "All" : `${v} · ${VARIANTS[v].name}`}
            </button>
          ))}
        </div>
        <span className="min-w-0 truncate text-[11px] text-content-muted">{VARIANTS[variant].blurb}</span>
      </div>

      <div className="flex flex-wrap items-center gap-x-5 gap-y-1 border-b border-border-subtle bg-sidebar px-5 py-1.5 text-[10.5px] text-content-faint">
        <span>
          Today on the phone: five tabs (Today · Inbox · Review · Agents · Search); Upcoming, Open, Done,
          Archive, the board and every area/project sit behind Browse, reached only from Today’s toolbar.
          Inbox → Upcoming is three taps across two tabs and nothing swipes.
        </span>
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-8">
        <Active />
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
