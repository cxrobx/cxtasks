import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Archive,
  Ban,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Code2,
  Copy,
  Eye,
  FileText,
  FolderGit2,
  GitMerge,
  ImagePlus,
  Link2,
  ListChecks,
  Moon,
  MoreHorizontal,
  Plus,
  RotateCcw,
  Send,
  Sparkles,
  Sun,
  Tag as TagIcon,
  Trash2,
  User,
  X,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  PRIORITY_COLORS,
  RUN_STATUS_CLASS,
  RUN_STATUS_LABELS,
  STATUS_LABELS,
  statusClass,
} from "@/lib/format";
import type { TaskRun, TaskStatus } from "@/types/task";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — EVERYTHING BELOW NOTES in the detail pane
   (Tags → Archive/Trash). The sequel to `?mock=disclosure`.

   The complaint: on a bare task — the common case — seven headed
   sections render to say there is nothing in them ("Nothing blocking
   this.", "No continuity links.", "Never dispatched."), plus two
   always-open inputs and a 90px launch card. Each variant is shown
   beside a replica of the shipped half-pane with a readout of how
   tall it is.

   Reached only via `?mock=below-notes`; all data and actions are
   static. Edits are shared between the two panes.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C";
type SampleKey = "bare" | "busy" | "errand";
type LinkKind = "blocked_by" | "blocking" | "supersedes" | "superseded_by" | "relates_to";

interface Sub {
  title: string;
  done: boolean;
}
interface LinkRef {
  ref: string;
  title: string;
  kind: LinkKind;
  done?: boolean;
}
interface Run {
  id: string;
  mode: "background" | "foreground";
  intent: "work" | "review";
  status: TaskRun["status"];
  executor: "Claude" | "Codex";
  ago: string;
  /** Minutes ago — orders runs and comments on one timeline (B, C). */
  at: number;
  result?: string;
}
interface Comment {
  id: string;
  author: string;
  ago: string;
  at: number;
  body: string;
}
interface MockTask {
  ref: string;
  title: string;
  status: TaskStatus;
  priority: number;
  project: string | null;
  body: string;
  tags: string[];
  subtasks: Sub[];
  links: LinkRef[];
  runs: Run[];
  comments: Comment[];
  repo: string | null;
  runner: "Claude" | "Codex";
  access: "Inspect" | "Run" | "Build";
}

function samples(): Record<SampleKey, MockTask> {
  return {
    bare: {
      ref: "T211",
      title: "Coach panel: empty state when a unit has no lessons yet",
      status: "inbox",
      priority: 2,
      project: "TeacherHero",
      body: "The coach panel renders a blank card when the unit is new. Say what to do next instead — \"Add a week to start planning\" — and link the action.",
      tags: [],
      subtasks: [],
      links: [],
      runs: [],
      comments: [],
      repo: "teacher-helper",
      runner: "Claude",
      access: "Inspect",
    },
    busy: {
      ref: "T158",
      title: "Learning Hub: lock /watch embeds to the storefront domain",
      status: "in_progress",
      priority: 1,
      project: "Globex",
      body: "CSP frame-ancestors on every /watch response, fed by EMBED_ORIGINS. Proven live in two directions; the learner portal is the last unseen path.",
      tags: ["billable", "training-academy", "globex"],
      subtasks: [
        { title: "Set EMBED_ORIGINS on the NAS env", done: true },
        { title: "Refuse a foreign origin (curl + browser)", done: true },
        { title: "See it in the learner portal", done: false },
      ],
      links: [
        { ref: "T202", title: "Play the embed once in the Courses Plus learner view", kind: "blocked_by" },
        { ref: "T131", title: "Signed URLs for the HLS keys", kind: "supersedes", done: true },
        { ref: "T205", title: "Read teacherhero.org Search Console", kind: "relates_to" },
      ],
      runs: [
        {
          id: "r2",
          mode: "background",
          intent: "review",
          status: "succeeded",
          executor: "Codex",
          ago: "1d",
          at: 1400,
          result: "**Review:** the header is set on /watch but NOT on the 302 from /watch/:id → the signed URL. A redirect carries its own CSP; add it there too or the lock has a hole.",
        },
        {
          id: "r1",
          mode: "background",
          intent: "work",
          status: "succeeded",
          executor: "Claude",
          ago: "2d",
          at: 2900,
          result: "Added `frame-ancestors` from EMBED_ORIGINS to every /watch response; tests cover allowed, refused and unset. Verified in the Courses Plus lesson editor.",
        },
      ],
      comments: [
        { id: "c1", author: "claude", ago: "2d", at: 2880, body: "Handoff: lock is live; the storefront learner view still needs a customer login to see." },
        { id: "c2", author: "codex", ago: "1d", at: 1390, body: "Background review · Codex — one hole: the 302 to the signed URL has no CSP." },
        { id: "c3", author: "me", ago: "3h", at: 180, body: "Fixed the 302. Asked Dana for a test learner account." },
      ],
      repo: "cxventures",
      runner: "Claude",
      access: "Build",
    },
    errand: {
      ref: "T214",
      title: "Renew the passport before the Lisbon trip",
      status: "todo",
      priority: 1,
      project: null,
      body: "Expires in March. Photos from the CVS on Main; the form is DS-82.",
      tags: ["personal"],
      subtasks: [],
      links: [],
      runs: [],
      comments: [{ id: "c1", author: "me", ago: "5d", at: 7200, body: "Photos done." }],
      repo: null,
      runner: "Claude",
      access: "Inspect",
    },
  };
}

/** What the "link a task" pickers offer — stands in for the board. */
const CANDIDATES = [
  { ref: "T188", title: "Cut the Studio export over to HLS" },
  { ref: "T197", title: "Learning Hub onboarding email" },
  { ref: "T140", title: "Globex quarterly SEO read" },
];

const KIND_LABEL: Record<LinkKind, string> = {
  blocked_by: "blocked by",
  blocking: "blocks",
  supersedes: "supersedes",
  superseded_by: "superseded by",
  relates_to: "relates to",
};

/* ── shared state ────────────────────────────────────────────── */

type Update = (fn: (t: MockTask) => MockTask) => void;

function useSamples() {
  const [all, setAll] = useState(samples);
  const [key, setKey] = useState<SampleKey>("bare");
  const update: Update = (fn) => setAll((a) => ({ ...a, [key]: fn(a[key]) }));
  const reset = () => setAll((a) => ({ ...a, [key]: samples()[key] }));
  return { task: all[key], key, setKey, update, reset };
}

const addTag = (u: Update, name: string) =>
  name.trim() && u((t) => (t.tags.includes(name.trim()) ? t : { ...t, tags: [...t.tags, name.trim()] }));
const removeTag = (u: Update, name: string) => u((t) => ({ ...t, tags: t.tags.filter((x) => x !== name) }));
const addSub = (u: Update, title: string) =>
  title.trim() && u((t) => ({ ...t, subtasks: [...t.subtasks, { title: title.trim(), done: false }] }));
const toggleSub = (u: Update, i: number) =>
  u((t) => ({ ...t, subtasks: t.subtasks.map((s, j) => (j === i ? { ...s, done: !s.done } : s)) }));
const addLink = (u: Update, ref: string, kind: LinkKind) => {
  const c = CANDIDATES.find((x) => x.ref === ref)!;
  u((t) => ({ ...t, links: [...t.links.filter((l) => l.ref !== ref), { ...c, kind }] }));
};
const removeLink = (u: Update, ref: string) => u((t) => ({ ...t, links: t.links.filter((l) => l.ref !== ref) }));
const postComment = (u: Update, body: string) =>
  body.trim() &&
  u((t) => ({
    ...t,
    comments: [...t.comments, { id: `c${Date.now()}`, author: "me", ago: "now", at: 0, body: body.trim() }],
  }));

/* ── pieces every pane shares ────────────────────────────────── */

const H3 = "text-[11px] font-semibold uppercase tracking-wider text-content-faint";
const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";

/** A tiny markdown: **bold** and `code`. */
function Prose({ text, className }: { text: string; className?: string }) {
  return (
    <p className={cn("text-[12px] leading-relaxed text-content-secondary", className)}>
      {text.split(/(\*\*[^*]+\*\*|`[^`]+`)/).map((part, i) =>
        part.startsWith("**") ? (
          <strong key={i} className="font-semibold text-content">
            {part.slice(2, -2)}
          </strong>
        ) : part.startsWith("`") ? (
          <code key={i} className="rounded bg-content/8 px-1 font-mono text-[11px]">
            {part.slice(1, -1)}
          </code>
        ) : (
          <span key={i}>{part}</span>
        )
      )}
    </p>
  );
}

function RefChip({ ref_, done }: { ref_: string; done?: boolean }) {
  return (
    <span
      className={cn(
        "shrink-0 rounded bg-content/10 px-1 py-px font-mono text-[10px] text-content-faint",
        done && "line-through opacity-60"
      )}
    >
      {ref_}
    </span>
  );
}

function useClickAway(ref: React.RefObject<HTMLElement | null>, onAway: () => void, active: boolean) {
  useEffect(() => {
    if (!active) return;
    // CAPTURE phase — the house rule near a drag region (gotcha #15).
    const h = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onAway();
    };
    document.addEventListener("mousedown", h, true);
    return () => document.removeEventListener("mousedown", h, true);
  }, [active, onAway, ref]);
}

/** The top of the pane, condensed: what sits ABOVE the half under study. When
 *  `tagsUp`, tags ride the property strip as `#chips` (B, C); when `menu`, the
 *  set-aside actions live behind ⋯ beside the reference. */
function Top({
  task,
  update,
  tagsUp,
  menu,
}: {
  task: MockTask;
  update: Update;
  tagsUp?: boolean;
  menu?: boolean;
}) {
  const [addingTag, setAddingTag] = useState(false);
  const [draft, setDraft] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  useClickAway(menuRef, () => setMenuOpen(false), menuOpen);
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-1.5 pt-1 text-[11px] text-content-muted">
        {task.ref}
        <Copy size={11} className="text-content-faint" />
        {menu && (
          <div ref={menuRef} className="relative ml-auto">
            <button
              type="button"
              aria-label="More"
              onClick={() => setMenuOpen((v) => !v)}
              className={cn(
                "rounded p-1 text-content-faint hover:bg-content/8 hover:text-content",
                menuOpen && "bg-content/8 text-content"
              )}
            >
              <MoreHorizontal size={14} />
            </button>
            {menuOpen && (
              <div className="mock-pop absolute right-0 top-full z-20 mt-1 w-44 rounded-lg border border-border bg-elevated p-1 shadow-xl">
                <MenuItem icon={Archive} label="Archive" hint="Off the board, kept" />
                <MenuItem icon={Trash2} label="Move to Trash" hint="⌘⌫" danger />
              </div>
            )}
          </div>
        )}
      </div>
      <div className="-mt-1 px-2 text-[15px] font-semibold leading-snug text-content">{task.title}</div>
      <div className="-mt-1 flex flex-wrap items-center gap-1.5 px-1 text-[11.5px]">
        <span className={cn("rounded-md px-1.5 py-0.5", statusClass(task.status))}>
          {STATUS_LABELS[task.status]}
        </span>
        <span className={cn("rounded-md bg-content/6 px-1.5 py-0.5 font-semibold", PRIORITY_COLORS[task.priority])}>
          P{task.priority}
        </span>
        {task.project && (
          <span className="rounded-md bg-content/6 px-1.5 py-0.5 text-content-secondary">{task.project}</span>
        )}
        {tagsUp &&
          task.tags.map((t) => (
            <span
              key={t}
              className="group flex items-center gap-0.5 rounded-md px-1 py-0.5 text-content-muted hover:bg-content/6"
            >
              #{t}
              <button
                type="button"
                aria-label={`Remove tag ${t}`}
                onClick={() => removeTag(update, t)}
                className="hidden group-hover:inline"
              >
                <X size={9} className="hover:text-error" />
              </button>
            </span>
          ))}
        {tagsUp &&
          (addingTag ? (
            <input
              autoFocus
              value={draft}
              placeholder="tag"
              onChange={(e) => setDraft(e.target.value)}
              onBlur={() => {
                addTag(update, draft);
                setDraft("");
                setAddingTag(false);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  addTag(update, draft);
                  setDraft("");
                }
                if (e.key === "Escape") setAddingTag(false);
              }}
              className="w-20 rounded-md border border-border bg-input px-1.5 py-0.5 text-[11px]"
            />
          ) : (
            <button
              type="button"
              aria-label="Add a property or tag"
              onClick={() => setAddingTag(true)}
              title="Add a property or tag"
              className="flex items-center rounded-md border border-dashed border-border px-1 py-0.5 text-content-faint hover:border-accent hover:text-accent"
            >
              <Plus size={12} />
            </button>
          ))}
        {!tagsUp && (
          <span className="flex items-center rounded-md border border-dashed border-border px-1 py-0.5 text-content-faint">
            <Plus size={12} />
          </span>
        )}
      </div>
      <section className="flex flex-col gap-1">
        <span className="text-[11px] font-medium uppercase tracking-wide text-content-faint">Notes</span>
        <div className="rounded-md border border-border bg-input px-3 py-2.5 text-[12.5px] leading-relaxed text-content">
          {task.body}
        </div>
      </section>
    </div>
  );
}

function MenuItem({
  icon: Icon,
  label,
  hint,
  danger,
}: {
  icon: typeof Zap;
  label: string;
  hint: string;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      className={cn(
        "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px] text-content-secondary",
        danger ? "hover:bg-error/10 hover:text-error" : "hover:bg-content/6 hover:text-content"
      )}
    >
      <Icon size={13} />
      {label}
      <span className="ml-auto text-[10px] text-content-faint">{hint}</span>
    </button>
  );
}

/* ── the launch console: shipped card, slim bar, expanded rail ─ */

function LaunchRail({ task }: { task: MockTask }) {
  const nodes: [typeof Zap, string, string, boolean][] = [
    [FolderGit2, "Workspace", task.repo ?? "Not set", !task.repo],
    [task.runner === "Claude" ? Sparkles : Code2, "Runner", task.runner, false],
    [Eye, "Capability", task.access, false],
    [FileText, "Brief", "Auto", false],
  ];
  return (
    <div className="mock-expand space-y-3 p-3">
      <div className="grid grid-cols-4 gap-1">
        {nodes.map(([Icon, label, value, unset]) => (
          <div key={label} className="flex flex-col items-center gap-1 text-center">
            <span
              className={cn(
                "flex size-9 items-center justify-center rounded-full border bg-surface",
                unset ? "border-dashed border-warning/60 text-warning" : "border-border text-content-secondary"
              )}
            >
              <Icon size={14} />
            </span>
            <span className="text-[9.5px] uppercase tracking-wide text-content-faint">{label}</span>
            <span className="max-w-full truncate text-[11px] text-content">{value}</span>
          </div>
        ))}
      </div>
      <div className="flex gap-2">
        <button
          type="button"
          disabled={!task.repo}
          className="flex-1 rounded-md bg-accent px-2 py-1.5 text-[11.5px] font-medium text-white disabled:opacity-40"
        >
          Queue for {task.runner}
        </button>
        <button type="button" className="rounded-md border border-border px-2 py-1.5 text-[11.5px] text-content-secondary">
          Open in Claude
        </button>
      </div>
    </div>
  );
}

/** As it ships: a 36px tile, a title, a subtitle, a pill and a chevron. */
function LaunchCard({ task }: { task: MockTask }) {
  const [open, setOpen] = useState(false);
  const ready = !!task.repo;
  return (
    <section className="overflow-hidden rounded-xl border border-accent/25 bg-elevated shadow-macos">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "flex w-full items-center gap-3 px-4 py-3.5 text-left hover:bg-content/4",
          open && "border-b border-border-subtle"
        )}
      >
        <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-accent/14 text-accent">
          <Zap size={16} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[14px] font-semibold tracking-[-0.01em]">Launch console</span>
          {!open && (
            <span className="mt-0.5 block truncate text-[11px] text-content-muted">
              {[task.repo ?? "No workspace", task.runner, task.access].join(" · ")}
            </span>
          )}
        </span>
        <ReadyPill ready={ready} />
        <ChevronDown size={15} className={cn("shrink-0 text-content-faint transition-transform", !open && "-rotate-90")} />
      </button>
      {open && <LaunchRail task={task} />}
    </section>
  );
}

function ReadyPill({ ready, small }: { ready: boolean; small?: boolean }) {
  return (
    <span
      className={cn(
        "flex shrink-0 items-center gap-1.5 rounded-full font-medium",
        small ? "px-1.5 py-0.5 text-[10px]" : "px-2.5 py-1.5 text-[10.5px]",
        ready ? "bg-success/10 text-success" : "bg-warning/10 text-warning"
      )}
    >
      <span className={cn("size-1.5 rounded-full", ready ? "bg-success" : "bg-warning")} />
      {ready ? "Ready" : "Needs repo"}
    </span>
  );
}

/**
 * One line: the plan IS the label. A task with no workspace — an errand — gets
 * a single faint "Hand to an agent…" instead of a warning-tinted card, because
 * for most repo-less tasks "needs repo" is not a problem to flag, it is the
 * normal state.
 */
function LaunchBar({ task, flat }: { task: MockTask; flat?: boolean }) {
  const [open, setOpen] = useState(false);
  if (!task.repo && !open)
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="flex items-center gap-1.5 self-start rounded-md px-1.5 py-1 text-[11.5px] text-content-faint hover:bg-content/6 hover:text-content"
      >
        <Zap size={12} />
        Hand to an agent…
      </button>
    );
  return (
    <section
      className={cn(
        "overflow-hidden rounded-lg border",
        flat ? "border-border-subtle" : "border-accent/25 bg-elevated"
      )}
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "flex w-full items-center gap-2 px-2.5 py-1.5 text-left hover:bg-content/4",
          open && "border-b border-border-subtle"
        )}
      >
        <Zap size={13} className="shrink-0 text-accent" />
        <span className="min-w-0 flex-1 truncate text-[11.5px] text-content-secondary">
          {[task.repo ?? "No workspace", task.runner, task.access].join(" · ")}
        </span>
        <ReadyPill ready={!!task.repo} small />
        <ChevronRight size={13} className={cn("shrink-0 text-content-faint transition-transform", open && "rotate-90")} />
      </button>
      {open && <LaunchRail task={task} />}
    </section>
  );
}

/* ── runs and comments ───────────────────────────────────────── */

function runHead(r: Run) {
  return r.intent === "review" ? `review · ${r.executor}` : `${r.mode} · ${r.executor}`;
}

function RunCard({ r }: { r: Run }) {
  return (
    <li className="rounded-md border border-border-subtle bg-elevated p-2">
      <div className="flex items-center gap-2 text-[11px] text-content-muted">
        <span className="font-medium">{r.intent === "review" ? "background review" : r.mode}</span>
        <span>·</span>
        <span className={RUN_STATUS_CLASS[r.status]}>{RUN_STATUS_LABELS[r.status]}</span>
        <span className="ml-auto">{r.ago} ago</span>
      </div>
      <div className="mt-1.5 flex gap-1">
        {["claimed", "started", "tools", "done"].map((s) => (
          <span key={s} className="h-1 flex-1 rounded-full bg-success/40" />
        ))}
      </div>
      {r.result && <Prose text={r.result} className="mt-1.5" />}
    </li>
  );
}

/** One line per run; the result opens in place. */
function RunLine({ r }: { r: Run }) {
  const [open, setOpen] = useState(false);
  return (
    <li>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left text-[11.5px] hover:bg-content/5"
      >
        <ChevronRight size={11} className={cn("shrink-0 text-content-faint transition-transform", open && "rotate-90")} />
        {r.intent === "review" ? (
          <GitMerge size={11} className="shrink-0 text-content-faint" />
        ) : (
          <Bot size={11} className="shrink-0 text-content-faint" />
        )}
        <span className="text-content-secondary">{runHead(r)}</span>
        <span className={cn("ml-auto", RUN_STATUS_CLASS[r.status])}>{RUN_STATUS_LABELS[r.status]}</span>
        <span className="w-6 text-right text-content-faint">{r.ago}</span>
      </button>
      {open && r.result && <Prose text={r.result} className="mock-expand ml-6 mt-0.5 pb-1" />}
    </li>
  );
}

function CommentCard({ c }: { c: Comment }) {
  const isAgent = c.author !== "me";
  return (
    <li
      className={cn(
        "rounded-md border p-2",
        isAgent ? "border-accent/25 bg-accent/8" : "border-border-subtle bg-elevated"
      )}
    >
      <div className="mb-1 flex items-center gap-1.5 text-[10px] text-content-faint">
        {isAgent ? <Bot size={10} className="text-accent" /> : <User size={10} />}
        <span className={cn("font-medium", isAgent && "text-accent")}>{c.author}</span>
        <span>·</span>
        <span>{c.ago} ago</span>
      </div>
      <Prose text={c.body} />
    </li>
  );
}

/** Feed entry: no card, just an author line — a transcript, not a stack. */
function FeedComment({ c }: { c: Comment }) {
  const isAgent = c.author !== "me";
  return (
    <li className="flex gap-2">
      <span
        className={cn(
          "mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full",
          isAgent ? "bg-accent/14 text-accent" : "bg-content/10 text-content-muted"
        )}
      >
        {isAgent ? <Bot size={11} /> : <User size={11} />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 text-[10.5px] text-content-faint">
          <span className={cn("font-medium", isAgent ? "text-accent" : "text-content-secondary")}>{c.author}</span>
          {c.ago === "now" ? "just now" : `${c.ago} ago`}
        </div>
        <Prose text={c.body} />
      </div>
    </li>
  );
}

/** A run, as an event in the same feed as the comments. */
function FeedRun({ r }: { r: Run }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="flex gap-2">
      <span className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border border-border-subtle text-content-faint">
        {r.intent === "review" ? <GitMerge size={10} /> : <Zap size={10} />}
      </span>
      <div className="min-w-0 flex-1">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="flex w-full items-baseline gap-1.5 text-left text-[10.5px] text-content-faint hover:text-content-muted"
        >
          {/* Sizes on the spans: the global button reset pins a button's own
              font-size, so a size class on the button is inert here. */}
          <span className="text-[10.5px] font-medium text-content-secondary">{runHead(r)}</span>
          <span className={cn("text-[10.5px]", RUN_STATUS_CLASS[r.status])}>
            {RUN_STATUS_LABELS[r.status].toLowerCase()}
          </span>
          <span className="text-[10.5px]">{r.ago} ago</span>
          <ChevronRight size={10} className={cn("ml-auto self-center transition-transform", open && "rotate-90")} />
        </button>
        {r.result && (
          <Prose text={r.result} className={cn(!open && "line-clamp-2")} />
        )}
      </div>
    </li>
  );
}

type Event = { at: number } & ({ kind: "run"; r: Run } | { kind: "comment"; c: Comment });
function timeline(t: MockTask): Event[] {
  return [
    ...t.runs.map((r) => ({ kind: "run" as const, r, at: r.at })),
    ...t.comments.map((c) => ({ kind: "comment" as const, c, at: c.at })),
  ].sort((a, b) => b.at - a.at); // `at` is minutes AGO, so this is oldest-first
}

/* ── composers ───────────────────────────────────────────────── */

/** As it ships: textarea, then an Add images / Post row. */
function ShippedComposer({ update }: { update: Update }) {
  const [draft, setDraft] = useState("");
  return (
    <>
      <textarea
        value={draft}
        rows={2}
        placeholder="Add a comment…  (⌘↩ to post)"
        onChange={(e) => setDraft(e.target.value)}
        className="w-full resize-y rounded-md border border-border bg-input px-2 py-1 leading-relaxed text-content placeholder:text-content-faint"
      />
      <div className="mt-1.5 flex items-center justify-between">
        <span className="flex items-center gap-1.5 px-1.5 py-1 text-[11px] text-content-faint">
          <ImagePlus size={13} />
          Add images
        </span>
        <button
          type="button"
          disabled={!draft.trim()}
          onClick={() => {
            postComment(update, draft);
            setDraft("");
          }}
          className="flex items-center gap-1.5 rounded-md bg-accent px-2 py-1 text-[11px] font-medium text-white disabled:opacity-40"
        >
          <Send size={11} />
          Post
        </button>
      </div>
    </>
  );
}

/**
 * One line at rest. It grows while it holds text, and Send appears only then —
 * a disabled Post button is a control that says nothing until you have typed.
 * `leading` is the slot C uses for its + menu.
 */
function CompactComposer({
  update,
  placeholder = "Comment…",
  leading,
}: {
  update: Update;
  placeholder?: string;
  leading?: React.ReactNode;
}) {
  const [draft, setDraft] = useState("");
  const post = () => {
    postComment(update, draft);
    setDraft("");
  };
  return (
    <div className="flex items-end gap-1 rounded-lg border border-border bg-input px-1.5 py-1 focus-within:border-accent/60">
      {leading}
      <textarea
        value={draft}
        rows={draft.includes("\n") || draft.length > 42 ? 3 : 1}
        placeholder={placeholder}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            post();
          }
        }}
        className="min-w-0 flex-1 resize-none bg-transparent px-1 py-0.5 leading-relaxed text-content outline-none placeholder:text-content-faint"
      />
      <button type="button" aria-label="Add images" title="Add images" className="rounded p-1 text-content-faint hover:bg-content/8 hover:text-content">
        <ImagePlus size={13} />
      </button>
      {draft.trim() && (
        <button
          type="button"
          aria-label="Post"
          title="Post (⌘↩)"
          onClick={post}
          className="mock-pop rounded bg-accent p-1 text-white"
        >
          <Send size={12} />
        </button>
      )}
    </div>
  );
}

/* ── adders shared by the variants ───────────────────────────── */

function InlineInput({
  placeholder,
  onSubmit,
  onDone,
}: {
  placeholder: string;
  onSubmit: (v: string) => void;
  onDone?: () => void;
}) {
  const [v, setV] = useState("");
  return (
    <input
      autoFocus
      value={v}
      placeholder={placeholder}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => {
        onSubmit(v);
        onDone?.();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          onSubmit(v);
          setV("");
        }
        if (e.key === "Escape") onDone?.();
      }}
      className={cn(FIELD, "text-[12px]")}
    />
  );
}

/** Type-ahead stand-in: the candidates as rows. */
function TaskPicker({ onPick }: { onPick: (ref: string) => void }) {
  return (
    <div className="flex flex-col gap-0.5">
      <input autoFocus placeholder="Find a task…" className={cn(FIELD, "mb-1 text-[12px]")} />
      {CANDIDATES.map((c) => (
        <button
          key={c.ref}
          type="button"
          onClick={() => onPick(c.ref)}
          className="flex items-center gap-2 rounded-md px-1.5 py-1 text-left text-[12px] text-content-secondary hover:bg-content/6 hover:text-content"
        >
          <RefChip ref_={c.ref} />
          <span className="truncate">{c.title}</span>
        </button>
      ))}
    </div>
  );
}

function SubRow({ s, onToggle }: { s: Sub; onToggle: () => void }) {
  return (
    <li className="flex items-center gap-2 py-0.5">
      <button
        type="button"
        aria-label={s.done ? "Reopen" : "Complete"}
        onClick={onToggle}
        className={cn(
          "flex size-[13px] shrink-0 items-center justify-center rounded-[3px] border",
          s.done ? "border-accent bg-accent text-white" : "border-content-muted/60"
        )}
      >
        {s.done && <Check size={9} strokeWidth={3} />}
      </button>
      <span className={cn("truncate text-[12px]", s.done ? "text-content-faint line-through" : "text-content-secondary")}>
        {s.title}
      </span>
    </li>
  );
}

function LinkRow({ l, badge, onRemove }: { l: LinkRef; badge?: boolean; onRemove: () => void }) {
  const tone =
    l.kind === "blocked_by" && !l.done
      ? "bg-error/12 text-error"
      : l.kind === "supersedes" || l.kind === "superseded_by"
        ? "bg-warning/12 text-warning"
        : "bg-content/8 text-content-muted";
  return (
    <li className="group flex items-center gap-2 py-0.5">
      {badge && (
        <span className={cn("w-[74px] shrink-0 rounded px-1 py-px text-center text-[9.5px] font-medium", tone)}>
          {KIND_LABEL[l.kind]}
        </span>
      )}
      <RefChip ref_={l.ref} done={l.done} />
      <span className="min-w-0 flex-1 truncate text-[12px] text-content-secondary">{l.title}</span>
      <button type="button" aria-label="Remove link" onClick={onRemove} className="opacity-0 group-hover:opacity-100">
        <X size={11} className="text-content-faint hover:text-error" />
      </button>
    </li>
  );
}

/* ── NOW · the shipped lower half, replicated ────────────────── */

function ShippedLower({ task, update }: { task: MockTask; update: Update }) {
  const blockers = task.links.filter((l) => l.kind === "blocked_by");
  const blocking = task.links.filter((l) => l.kind === "blocking");
  const continuity = task.links.filter((l) => l.kind !== "blocked_by" && l.kind !== "blocking");
  const [subDraft, setSubDraft] = useState("");
  return (
    <div data-lower className="flex flex-col gap-5">
      <section>
        <h3 className={cn(H3, "mb-2")}>Tags</h3>
        <div className="flex flex-wrap items-center gap-1.5">
          {task.tags.map((t) => (
            <span key={t} className="flex items-center gap-1 rounded-full bg-content/10 px-2 py-0.5 text-[11px] text-content-secondary">
              <TagIcon size={9} className="text-content-faint" />
              {t}
            </span>
          ))}
          <span className="w-24 rounded-full border border-dashed border-border px-2 py-0.5 text-[11px] text-content-faint">
            {task.tags.length ? "add…" : "add a tag…"}
          </span>
        </div>
      </section>

      <section>
        <h3 className={cn(H3, "mb-2")}>Subtasks</h3>
        <ul className="mb-2 flex flex-col gap-1">
          {task.subtasks.map((s, i) => (
            <li key={i} className="flex items-center gap-2 text-content-secondary">
              <span className="text-[11px] text-content-faint">{s.done ? "Done" : "To do"}</span>
              <span className="truncate">{s.title}</span>
            </li>
          ))}
        </ul>
        <div className="flex items-center gap-1.5">
          <Plus size={13} className="text-content-faint" />
          <input
            value={subDraft}
            placeholder="Add subtask"
            onChange={(e) => setSubDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                addSub(update, subDraft);
                setSubDraft("");
              }
            }}
            className={FIELD}
          />
        </div>
      </section>

      <section>
        <div className="mb-2 flex items-center gap-2">
          <h3 className={H3}>Blocked by</h3>
          {blockers.some((b) => !b.done) && (
            <span className="flex items-center gap-1 rounded bg-error/15 px-1.5 py-px text-[10px] font-medium text-error">
              <Ban size={9} />
              blocked
            </span>
          )}
          <span className="ml-auto flex items-center gap-1 px-1 text-[11px] text-content-faint">
            <Plus size={11} />
            Add
          </span>
        </div>
        {blockers.length === 0 ? (
          <p className="text-content-faint">Nothing blocking this.</p>
        ) : (
          <ul className="flex flex-col gap-1">
            {blockers.map((l) => (
              <LinkRow key={l.ref} l={l} onRemove={() => removeLink(update, l.ref)} />
            ))}
          </ul>
        )}
        {blocking.length > 0 && (
          <>
            <h3 className={cn(H3, "mb-2 mt-4")}>Blocking</h3>
            <ul className="flex flex-col gap-1">
              {blocking.map((l) => (
                <LinkRow key={l.ref} l={l} onRemove={() => removeLink(update, l.ref)} />
              ))}
            </ul>
          </>
        )}
        <div className="mt-4 border-t border-border pt-4">
          <div className="mb-2 flex items-center gap-2">
            <h3 className={H3}>Continuity</h3>
            <span className="ml-auto flex items-center gap-1 px-1 text-[11px] text-content-faint">
              <Plus size={11} />
              Add relation
            </span>
          </div>
          {continuity.length === 0 ? (
            <p className="text-content-faint">No continuity links.</p>
          ) : (
            <div className="flex flex-col gap-3">
              {(["supersedes", "superseded_by", "relates_to"] as LinkKind[]).map((k) => {
                const ls = continuity.filter((l) => l.kind === k);
                if (!ls.length) return null;
                return (
                  <div key={k}>
                    <p className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-content-faint">
                      {k === "supersedes" ? "Supersedes" : k === "superseded_by" ? "Superseded by" : "Related"}
                    </p>
                    <ul className="flex flex-col gap-1">
                      {ls.map((l) => (
                        <LinkRow key={l.ref} l={l} onRemove={() => removeLink(update, l.ref)} />
                      ))}
                    </ul>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </section>

      <LaunchCard task={task} />

      <section>
        <h3 className={cn(H3, "mb-2")}>Runs</h3>
        {task.runs.length === 0 ? (
          <p className="text-content-faint">Never dispatched.</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {task.runs.map((r) => (
              <RunCard key={r.id} r={r} />
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3 className={cn(H3, "mb-2")}>Comments</h3>
        {task.comments.length > 0 && (
          <ul className="mb-2 flex flex-col gap-2">
            {task.comments.map((c) => (
              <CommentCard key={c.id} c={c} />
            ))}
          </ul>
        )}
        <ShippedComposer update={update} />
      </section>

      <div className="flex items-center gap-1">
        <span className="flex items-center gap-1.5 rounded-md px-2 py-1 text-content-faint">
          <Archive size={13} />
          Archive
        </span>
        <span className="flex items-center gap-1.5 rounded-md px-2 py-1 text-content-faint">
          <Trash2 size={13} />
          Move to Trash
        </span>
      </div>
    </div>
  );
}

/* ── A · Quiet empties ───────────────────────────────────────── */

type AddKind = "tag" | "subtask" | "blocker" | "link";
const ADD_META: Record<AddKind, { label: string; icon: typeof Zap }> = {
  tag: { label: "Tag", icon: TagIcon },
  subtask: { label: "Subtask", icon: ListChecks },
  blocker: { label: "Blocker", icon: Ban },
  link: { label: "Link", icon: Link2 },
};

function AdderBody({
  kind,
  update,
  done,
}: {
  kind: AddKind;
  update: Update;
  done: () => void;
}) {
  if (kind === "tag") return <InlineInput placeholder="Tag name" onSubmit={(v) => addTag(update, v)} onDone={done} />;
  if (kind === "subtask")
    return <InlineInput placeholder="Subtask — ↩ adds another" onSubmit={(v) => addSub(update, v)} onDone={done} />;
  return (
    <TaskPicker
      onPick={(ref) => {
        addLink(update, ref, kind === "blocker" ? "blocked_by" : "relates_to");
        done();
      }}
    />
  );
}

/** A small header that carries its own + — used for sections that HAVE something. */
function MiniHead({
  label,
  extra,
  onAdd,
}: {
  label: string;
  extra?: React.ReactNode;
  onAdd?: () => void;
}) {
  return (
    <div className="mb-1 flex items-center gap-2">
      <h3 className={H3}>{label}</h3>
      {extra}
      {onAdd && (
        <button
          type="button"
          aria-label={`Add to ${label}`}
          onClick={onAdd}
          className="ml-auto rounded p-0.5 text-content-faint hover:bg-content/8 hover:text-content"
        >
          <Plus size={12} />
        </button>
      )}
    </div>
  );
}

/**
 * Same order, same sections — but a section renders only when it holds
 * something, and every empty one collapses into ONE line of faint adders. The
 * launch card shrinks to a bar, the composer to a line, the footer to two
 * icons. The shipped pane's mental model survives untouched.
 */
function MockA({ task, update }: { task: MockTask; update: Update }) {
  const [adding, setAdding] = useState<AddKind | null>(null);
  const blockers = task.links.filter((l) => l.kind === "blocked_by");
  const links = task.links.filter((l) => l.kind !== "blocked_by");
  const has: Record<AddKind, boolean> = {
    tag: task.tags.length > 0,
    subtask: task.subtasks.length > 0,
    blocker: blockers.length > 0,
    link: links.length > 0,
  };
  const missing = (Object.keys(ADD_META) as AddKind[]).filter((k) => !has[k]);
  const adder = (k: AddKind) =>
    adding === k && (
      <div className="mock-expand mt-1.5">
        <AdderBody kind={k} update={update} done={() => setAdding(null)} />
      </div>
    );
  const done = task.subtasks.filter((s) => s.done).length;

  return (
    <div data-lower className="flex flex-col gap-4">
      {has.tag && (
        <section>
          <MiniHead label="Tags" onAdd={() => setAdding("tag")} />
          <div className="flex flex-wrap gap-1.5">
            {task.tags.map((t) => (
              <span key={t} className="group flex items-center gap-1 rounded-full bg-content/10 px-2 py-0.5 text-[11px] text-content-secondary">
                {t}
                <button type="button" onClick={() => removeTag(update, t)} className="opacity-0 group-hover:opacity-100">
                  <X size={9} className="hover:text-error" />
                </button>
              </span>
            ))}
          </div>
          {adder("tag")}
        </section>
      )}
      {has.subtask && (
        <section>
          <MiniHead
            label="Subtasks"
            extra={<span className="text-[10.5px] tabular-nums text-content-faint">{done}/{task.subtasks.length}</span>}
            onAdd={() => setAdding("subtask")}
          />
          <ul>
            {task.subtasks.map((s, i) => (
              <SubRow key={i} s={s} onToggle={() => toggleSub(update, i)} />
            ))}
          </ul>
          {adder("subtask")}
        </section>
      )}
      {has.blocker && (
        <section>
          <MiniHead
            label="Blocked by"
            extra={
              blockers.some((b) => !b.done) && (
                <span className="rounded bg-error/15 px-1.5 py-px text-[10px] font-medium text-error">blocked</span>
              )
            }
            onAdd={() => setAdding("blocker")}
          />
          <ul>
            {blockers.map((l) => (
              <LinkRow key={l.ref} l={l} onRemove={() => removeLink(update, l.ref)} />
            ))}
          </ul>
          {adder("blocker")}
        </section>
      )}
      {has.link && (
        <section>
          <MiniHead label="Linked" onAdd={() => setAdding("link")} />
          <ul>
            {links.map((l) => (
              <LinkRow key={l.ref} l={l} badge onRemove={() => removeLink(update, l.ref)} />
            ))}
          </ul>
          {adder("link")}
        </section>
      )}

      {missing.length > 0 && (
        <div>
          <div className="flex flex-wrap items-center gap-x-1 gap-y-0.5 text-[11.5px]">
            {missing.map((k) => {
              const Icon = ADD_META[k].icon;
              return (
                <button
                  key={k}
                  type="button"
                  onClick={() => setAdding((a) => (a === k ? null : k))}
                  className={cn(
                    "flex items-center gap-1 rounded-md px-1.5 py-0.5 hover:bg-content/6 hover:text-content",
                    adding === k ? "bg-accent/10 text-accent" : "text-content-faint"
                  )}
                >
                  <Plus size={11} />
                  <Icon size={11} />
                  {ADD_META[k].label}
                </button>
              );
            })}
          </div>
          {adding && missing.includes(adding) && adder(adding)}
        </div>
      )}

      <LaunchBar task={task} />

      {task.runs.length > 0 && (
        <section>
          <MiniHead label="Runs" />
          <ul>
            {task.runs.map((r) => (
              <RunLine key={r.id} r={r} />
            ))}
          </ul>
        </section>
      )}

      <section>
        {task.comments.length > 0 && (
          <>
            <MiniHead label="Comments" />
            <ul className="mb-2 flex flex-col gap-2">
              {task.comments.map((c) => (
                <CommentCard key={c.id} c={c} />
              ))}
            </ul>
          </>
        )}
        <CompactComposer update={update} placeholder="Comment…  ⌘↩" />
      </section>

      <div className="-mt-1 flex justify-end gap-0.5">
        <button type="button" title="Archive — off the board, kept" className="rounded p-1 text-content-faint hover:bg-content/8 hover:text-content">
          <Archive size={13} />
        </button>
        <button type="button" title="Move to Trash (⌘⌫)" className="rounded p-1 text-content-faint hover:bg-error/10 hover:text-error">
          <Trash2 size={13} />
        </button>
      </div>
    </div>
  );
}

/* ── B · Tabs ────────────────────────────────────────────────── */

type Tab = "activity" | "subtasks" | "links";

/**
 * Tags join the property strip (they ARE row metadata — the row prints them
 * beside the project), Archive/Trash go behind ⋯, and what's left becomes
 * three tabs under a slim launch bar. The half is only ever as tall as ONE
 * tab, and a tab's count answers "is anything in there" without opening it.
 */
function MockB({ task, update }: { task: MockTask; update: Update }) {
  const events = timeline(task);
  const [tab, setTab] = useState<Tab>(() =>
    events.length ? "activity" : task.subtasks.length ? "subtasks" : "activity"
  );
  const [adding, setAdding] = useState<"subtask" | "blocker" | "link" | null>(null);
  const done = task.subtasks.filter((s) => s.done).length;
  const openBlock = task.links.some((l) => l.kind === "blocked_by" && !l.done);
  const tabs: { key: Tab; label: string; count: string | null; alert?: boolean }[] = [
    { key: "activity", label: "Activity", count: events.length ? String(events.length) : null },
    { key: "subtasks", label: "Subtasks", count: task.subtasks.length ? `${done}/${task.subtasks.length}` : null },
    { key: "links", label: "Links", count: task.links.length ? String(task.links.length) : null, alert: openBlock },
  ];

  return (
    <div data-lower className="flex flex-col gap-3">
      <LaunchBar task={task} />
      <div className="flex items-center gap-0.5 border-b border-border-subtle">
        {tabs.map((t) => (
          <button
            key={t.key}
            type="button"
            onClick={() => {
              setTab(t.key);
              setAdding(null);
            }}
            className={cn(
              "-mb-px flex items-center gap-1.5 border-b-2 px-2 pb-1.5 pt-0.5 text-[11.5px]",
              tab === t.key
                ? "border-accent text-content"
                : "border-transparent text-content-faint hover:text-content-secondary"
            )}
          >
            {t.label}
            {t.count && (
              <span
                className={cn(
                  "rounded-full px-1.5 text-[10px] tabular-nums",
                  t.alert ? "bg-error/15 text-error" : "bg-content/8 text-content-muted"
                )}
              >
                {t.count}
              </span>
            )}
          </button>
        ))}
      </div>

      {tab === "activity" && (
        <div className="mock-expand flex flex-col gap-3">
          {events.length === 0 ? null : (
            <ul className="flex flex-col gap-3">
              {events.map((e) =>
                e.kind === "run" ? <FeedRun key={e.r.id} r={e.r} /> : <FeedComment key={e.c.id} c={e.c} />
              )}
            </ul>
          )}
          <CompactComposer update={update} placeholder="Comment…  ⌘↩" />
        </div>
      )}

      {tab === "subtasks" && (
        <div className="mock-expand flex flex-col gap-1.5">
          <ul>
            {task.subtasks.map((s, i) => (
              <SubRow key={i} s={s} onToggle={() => toggleSub(update, i)} />
            ))}
          </ul>
          <div className="flex items-center gap-2">
            <Plus size={13} className="shrink-0 text-content-faint" />
            <InlineInputQuiet placeholder="Add subtask" onSubmit={(v) => addSub(update, v)} />
          </div>
        </div>
      )}

      {tab === "links" && (
        <div className="mock-expand flex flex-col gap-1.5">
          {task.links.length > 0 && (
            <ul>
              {task.links.map((l) => (
                <LinkRow key={l.ref} l={l} badge onRemove={() => removeLink(update, l.ref)} />
              ))}
            </ul>
          )}
          <div className="flex gap-1">
            {(["blocker", "link"] as const).map((k) => (
              <button
                key={k}
                type="button"
                onClick={() => setAdding((a) => (a === k ? null : k))}
                className={cn(
                  "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] hover:bg-content/6",
                  adding === k ? "bg-accent/10 text-accent" : "text-content-faint"
                )}
              >
                <Plus size={11} />
                {k === "blocker" ? "Blocked by…" : "Supersedes / relates…"}
              </button>
            ))}
          </div>
          {adding && adding !== "subtask" && (
            <div className="mock-expand">
              <TaskPicker
                onPick={(ref) => {
                  addLink(update, ref, adding === "blocker" ? "blocked_by" : "relates_to");
                  setAdding(null);
                }}
              />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Borderless until focused — an always-open input that doesn't look like a form. */
function InlineInputQuiet({ placeholder, onSubmit }: { placeholder: string; onSubmit: (v: string) => void }) {
  const [v, setV] = useState("");
  return (
    <input
      value={v}
      placeholder={placeholder}
      onChange={(e) => setV(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          onSubmit(v);
          setV("");
        }
      }}
      className="w-full rounded-md border border-transparent bg-transparent px-1 py-0.5 text-[12px] text-content placeholder:text-content-faint hover:border-border-subtle focus:border-border focus:bg-input"
    />
  );
}

/* ── C · One composer ────────────────────────────────────────── */

type ComposeKind = "subtask" | "blocker" | "link";

/**
 * Two regions and one input. What the task is CONNECTED to (subtasks,
 * blockers, continuity) is one typed list that exists only once it has a row.
 * What HAPPENED to it (runs and comments) is one feed, newest at the bottom
 * like any conversation, and the composer under it is the only input in the
 * half: its + adds a subtask, a blocker or a link, and the ⚡ beside it is the
 * launch console — handing the task to an agent is posting to the same feed.
 */
function MockC({ task, update }: { task: MockTask; update: Update }) {
  const [menu, setMenu] = useState(false);
  const [composing, setComposing] = useState<ComposeKind | null>(null);
  const [launch, setLaunch] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  useClickAway(menuRef, () => setMenu(false), menu);
  const events = timeline(task);
  const done = task.subtasks.filter((s) => s.done).length;
  const connected = task.subtasks.length + task.links.length;

  const plus = (
    <div ref={menuRef} className="relative">
      <button
        type="button"
        aria-label="Add a subtask, blocker or link"
        onClick={() => setMenu((v) => !v)}
        className={cn(
          "rounded p-1 text-content-faint hover:bg-content/8 hover:text-content",
          menu && "bg-content/8 text-content"
        )}
      >
        <Plus size={13} />
      </button>
      {menu && (
        <div className="mock-pop absolute bottom-full left-0 z-20 mb-1 w-44 rounded-lg border border-border bg-elevated p-1 shadow-xl">
          {(
            [
              ["subtask", ListChecks, "Subtask"],
              ["blocker", Ban, "Blocked by…"],
              ["link", Link2, "Supersedes / relates…"],
            ] as const
          ).map(([k, Icon, label]) => (
            <button
              key={k}
              type="button"
              onClick={() => {
                setComposing(k);
                setMenu(false);
              }}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px] text-content-secondary hover:bg-content/6 hover:text-content"
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      )}
    </div>
  );

  return (
    <div data-lower className="flex flex-col gap-4">
      {connected > 0 && (
        <section>
          <div className="mb-1 flex items-center gap-2">
            <h3 className={H3}>Connected</h3>
            {task.subtasks.length > 0 && (
              <span className="text-[10.5px] tabular-nums text-content-faint">
                {done}/{task.subtasks.length} subtasks
              </span>
            )}
          </div>
          <ul>
            {task.subtasks.map((s, i) => (
              <SubRow key={i} s={s} onToggle={() => toggleSub(update, i)} />
            ))}
            {task.links.map((l) => (
              <LinkRow key={l.ref} l={l} badge onRemove={() => removeLink(update, l.ref)} />
            ))}
          </ul>
        </section>
      )}

      <section className="flex flex-col gap-3">
        {events.length > 0 && (
          <>
            <h3 className={H3}>Activity</h3>
            <ul className="flex flex-col gap-3">
              {events.map((e) =>
                e.kind === "run" ? <FeedRun key={e.r.id} r={e.r} /> : <FeedComment key={e.c.id} c={e.c} />
              )}
            </ul>
          </>
        )}

        {composing && (
          <div className="mock-expand rounded-lg border border-border-subtle bg-content/[0.03] p-2">
            <div className="mb-1.5 flex items-center">
              <span className="text-[10.5px] font-medium uppercase tracking-wide text-content-faint">
                {composing === "subtask" ? "Add subtask" : composing === "blocker" ? "Blocked by" : "Link a task"}
              </span>
              <button
                type="button"
                onClick={() => setComposing(null)}
                className="ml-auto rounded px-1.5 text-[10.5px] text-accent hover:bg-accent/10"
              >
                Done
              </button>
            </div>
            {composing === "subtask" ? (
              <InlineInput placeholder="Subtask — ↩ adds another" onSubmit={(v) => addSub(update, v)} />
            ) : (
              <TaskPicker
                onPick={(ref) => {
                  addLink(update, ref, composing === "blocker" ? "blocked_by" : "relates_to");
                  setComposing(null);
                }}
              />
            )}
          </div>
        )}

        {launch && (
          <div className="overflow-hidden rounded-lg border border-accent/25 bg-elevated">
            <div className="flex items-center gap-2 border-b border-border-subtle px-2.5 py-1.5 text-[11.5px] text-content-secondary">
              <Zap size={12} className="text-accent" />
              Hand to an agent
              <button type="button" onClick={() => setLaunch(false)} className="ml-auto text-content-faint hover:text-content">
                <X size={12} />
              </button>
            </div>
            <LaunchRail task={task} />
          </div>
        )}

        <div className="flex items-end gap-1.5">
          <div className="min-w-0 flex-1">
            <CompactComposer update={update} placeholder="Comment, or + to add…" leading={plus} />
          </div>
          <button
            type="button"
            onClick={() => setLaunch((v) => !v)}
            title={
              task.repo
                ? `Launch — ${task.repo} · ${task.runner} · ${task.access}`
                : "Hand to an agent — needs a workspace"
            }
            className={cn(
              "relative flex h-[32px] shrink-0 items-center gap-1 rounded-lg border px-2 text-[11.5px]",
              launch
                ? "border-accent bg-accent/12 text-accent"
                : "border-border text-content-secondary hover:border-accent/60 hover:text-accent"
            )}
          >
            <Zap size={13} />
            <span
              className={cn(
                "absolute -right-0.5 -top-0.5 size-2 rounded-full ring-2 ring-surface",
                task.repo ? "bg-success" : "bg-warning"
              )}
            />
          </button>
        </div>
      </section>
    </div>
  );
}

/* ── harness ─────────────────────────────────────────────────── */

/** Height of everything below Notes — the number this mock is about. */
function useLowerHeight<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [h, setH] = useState(0);
  useLayoutEffect(() => {
    const root = ref.current;
    if (!root) return;
    const measure = () => {
      const el = root.querySelector("[data-lower]");
      if (el) setH(Math.round(el.getBoundingClientRect().height));
    };
    const ro = new ResizeObserver(measure);
    const watch = () => {
      const el = root.querySelector("[data-lower]");
      if (el) ro.observe(el);
    };
    const mo = new MutationObserver(() => {
      watch();
      measure();
    });
    mo.observe(root, { childList: true, subtree: true });
    watch();
    measure();
    return () => {
      ro.disconnect();
      mo.disconnect();
    };
  }, []);
  return [ref, h] as const;
}

function Pane({ caption, note, children }: { caption: string; note?: string; children: React.ReactNode }) {
  const [ref, h] = useLowerHeight<HTMLDivElement>();
  return (
    <div className="flex min-w-0 flex-col" style={{ width: "clamp(340px, 30vw, 420px)" }}>
      <div className="mb-2 flex items-baseline gap-2 px-1">
        <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">{caption}</span>
        <span className="h-px flex-1 bg-border-subtle" />
        <span className="font-mono text-[10px] tabular-nums text-accent" title="Height of everything below Notes">
          Below Notes: {h}px
        </span>
      </div>
      <div className="h-[760px] overflow-y-auto rounded-xl bg-surface">
        <div ref={ref} className="flex flex-col gap-5 px-4 pb-8 pt-2">
          {children}
        </div>
      </div>
      {note && <p className="mt-2 px-1 text-[10px] leading-relaxed text-content-faint">{note}</p>}
    </div>
  );
}

type VariantProps = { task: MockTask; update: Update };

const VARIANTS: Record<
  MockVariant,
  {
    name: string;
    blurb: string;
    note: string;
    tagsUp: boolean;
    menu: boolean;
    component: (p: VariantProps) => React.ReactElement;
  }
> = {
  A: {
    name: "Quiet empties",
    blurb: "Same sections, but only the ones with something in them; empties fold into one + line",
    note: "Cheapest to ship — every section keeps its component, it just stops rendering when empty. The empties become one line of adders, the launch card a one-line bar, the composer a line that grows, the footer two icons. A repo-less task gets “Hand to an agent…” instead of a Needs-repo warning.",
    tagsUp: false,
    menu: false,
    component: MockA,
  },
  B: {
    name: "Tabs",
    blurb: "Tags join the chips, Archive/Trash go behind ⋯, the rest is Activity · Subtasks · Links",
    note: "The half is only ever as tall as one tab, and each tab's count (red when a blocker is open) says what's inside without opening it. Runs and comments become one Activity timeline. Cost: a blocker on the Links tab is out of sight — the red count has to carry it, and the property strip's Waiting chip already does.",
    tagsUp: true,
    menu: true,
    component: MockB,
  },
  C: {
    name: "One composer",
    blurb: "Connected list + activity feed; the composer is the only input — + adds, ⚡ launches",
    note: "Two regions that exist only once they hold something, and one input. On a bare task the whole half is a single line. Handing the task to an agent sits beside commenting, because a run's result lands in the same feed. Cost: adding a subtask is two clicks (+ → Subtask) instead of one field.",
    tagsUp: true,
    menu: true,
    component: MockC,
  },
};

const SAMPLE_LABELS: Record<SampleKey, string> = {
  bare: "T211 · bare (your screenshot)",
  busy: "T158 · busy",
  errand: "T214 · no-repo errand",
};

export default function BelowNotesMock() {
  const [variant, setVariant] = useState<MockVariant>("A");
  const [dark, setDark] = useState(() => document.documentElement.getAttribute("data-theme") !== "light");
  const { task, key, setKey, update, reset } = useSamples();

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }, [dark]);

  const V = VARIANTS[variant];
  const Active = V.component;

  return (
    <div className="flex h-full flex-col bg-base text-content">
      <style>{`
        .mock-expand { animation: expandIn .16s ease-out; }
        @keyframes expandIn { from { opacity: 0; transform: translateY(-4px); } }
        .mock-pop { animation: popIn .14s cubic-bezier(.22,.9,.28,1); }
        @keyframes popIn { from { opacity: 0; transform: translateY(-4px) scale(.98); } }
      `}</style>

      <div className="flex flex-wrap items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <div>
          <div className="text-[12px] font-semibold">Task detail — below Notes</div>
          <div className="text-[9px] text-content-faint">
            Seven headed sections, most saying they're empty; the sequel to ?mock=disclosure
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
                variant === item ? "bg-accent text-white" : "bg-surface text-content-secondary hover:bg-elevated"
              )}
            >
              {item} · {VARIANTS[item].name}
            </button>
          ))}
        </div>
        <span className="text-[10px] text-content-muted">{V.blurb}</span>
        <button
          onClick={() => setDark((v) => !v)}
          aria-label="Toggle theme"
          className="ml-auto rounded-md border border-border p-1.5 text-content-muted hover:text-content"
        >
          {dark ? <Sun size={13} /> : <Moon size={13} />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-6">
        <div className="mx-auto flex max-w-[880px] flex-wrap items-center gap-2">
          <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">Sample task</span>
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
        </div>

        <div className="mx-auto mt-6 flex flex-wrap items-start justify-center gap-8">
          <Pane
            caption="Now · shipping"
            note="On a bare task, three of the seven sections exist only to say they're empty, two inputs are always open, and the launch card is the tallest thing in the half."
          >
            <Top task={task} update={update} />
            <ShippedLower task={task} update={update} />
          </Pane>
          <Pane caption={`${variant} · ${V.name}`} note={V.note}>
            <Top task={task} update={update} tagsUp={V.tagsUp} menu={V.menu} />
            <Active key={`${key}-${variant}`} task={task} update={update} />
          </Pane>
        </div>
        <p className="mx-auto mt-6 max-w-[620px] text-center text-[10.5px] leading-relaxed text-content-faint">
          Edits are shared between the two panes — add a subtask or a comment on the right and watch
          what it costs on the left. Switch samples for the bare, busy and no-repo cases.
        </p>
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
