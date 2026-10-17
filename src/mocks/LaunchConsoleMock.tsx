import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronLeft,
  Clipboard,
  Code2,
  Eye,
  FileText,
  FolderGit2,
  GitBranch,
  Moon,
  Search,
  Shield,
  Sparkles,
  Sun,
  Terminal,
  WandSparkles,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — a COLLAPSIBLE launch console whose rail
   nodes ARE the controls, so the four fields below can go away.
   Reached only via `?mock=launch`; all data and actions are static.

   Every variant is shown beside the shipped console at the same pane
   width, with a live height readout — the ask was about space, so the
   comparison has to be measurable rather than asserted.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C";
type Runner = "claude" | "codex";
type Access = "read-only" | "shell" | "write";
/** The four things the rail can edit. `brief` is the fourth node: it is not a
 *  stage of the route, so it sits past the end of the connector line. */
type Setting = "workspace" | "runner" | "capability" | "brief";

const REPOS = [
  "~/Projects/cxventures",
  "~/Projects/cxtasks",
  "~/Projects/artisttracker",
  "~/clients/acme",
];

const ACCESS: Record<
  Access,
  { label: string; short: string; icon: typeof Eye; hint: string }
> = {
  "read-only": {
    label: "Inspect",
    short: "Read only",
    icon: Eye,
    hint: "Read, Glob, Grep. Cannot run commands.",
  },
  shell: {
    label: "Run",
    short: "Commands",
    icon: Terminal,
    hint: "Adds a shell — git, tests, CLIs. Edit and Write stay denied.",
  },
  write: {
    label: "Build",
    short: "Edit files",
    icon: WandSparkles,
    hint: "Adds Edit and Write.",
  },
};

const RUNNERS: Record<
  Runner,
  { label: string; rail: string; short: string; icon: typeof Eye }
> = {
  // `rail` is the 4-up column's version: "Claude Code" truncates to "Claude C…"
  // in ~78px, and the node's icon already says which agent it is.
  claude: { label: "Claude Code", rail: "Claude", short: "Tool grants", icon: Sparkles },
  codex: { label: "Codex", rail: "Codex", short: "OS sandbox", icon: Code2 },
};

const shorten = (path: string) => (path.length > 22 ? `${path.slice(0, 21)}…` : path);
/** The rail gets the LAST segment, not a truncated path. `~/Project…` names no
 *  repo; `cxventures` does, and the full path is one click away in the panel. */
const railPath = (path: string) => path.split("/").filter(Boolean).pop() ?? path;

/* ── shared state ────────────────────────────────────────────── */

function useConsole() {
  const [repo, setRepo] = useState(REPOS[0]);
  const [runner, setRunner] = useState<Runner>("claude");
  const [access, setAccess] = useState<Access>("read-only");
  const [brief, setBrief] = useState("");
  const [collapsed, setCollapsed] = useState(false);
  const [panel, setPanel] = useState<Setting | null>(null);

  return {
    repo,
    setRepo,
    runner,
    setRunner,
    access,
    setAccess,
    brief,
    setBrief,
    collapsed,
    setCollapsed,
    panel,
    /** Clicking the open node closes it — the rail is a toggle, not a tab bar
     *  you can never get out of. */
    toggle: (next: Setting) => setPanel((cur) => (cur === next ? null : next)),
    close: () => setPanel(null),
  };
}

type ConsoleState = ReturnType<typeof useConsole>;

const NODES: { key: Setting; label: string }[] = [
  { key: "workspace", label: "Workspace" },
  { key: "runner", label: "Runner" },
  { key: "capability", label: "Capability" },
  { key: "brief", label: "Brief" },
];

function nodeView(key: Setting, s: ConsoleState) {
  switch (key) {
    case "workspace":
      return {
        icon: FolderGit2,
        value: s.repo ? railPath(s.repo) : "Not set",
        set: Boolean(s.repo),
      };
    case "runner":
      return { icon: RUNNERS[s.runner].icon, value: RUNNERS[s.runner].rail, set: true };
    case "capability":
      return { icon: ACCESS[s.access].icon, value: ACCESS[s.access].label, set: true };
    case "brief":
      return { icon: FileText, value: s.brief.trim() ? "Custom" : "Auto", set: true };
  }
}

/** The one-line plan, for the collapsed header. Says the same three things the
 *  rail does, in the space of a subtitle. */
const summarize = (s: ConsoleState) =>
  [
    s.repo ? railPath(s.repo) : "No workspace",
    RUNNERS[s.runner].rail,
    ACCESS[s.access].label,
  ].join(" · ");

/* ── shared chrome ───────────────────────────────────────────── */

function ConsoleHeader({
  s,
  collapsible = true,
}: {
  s: ConsoleState;
  collapsible?: boolean;
}) {
  const ready = Boolean(s.repo);
  const body = (
    <>
      <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-accent/14 text-accent shadow-sm">
        <Zap size={16} />
      </span>
      <span className="min-w-0 flex-1 text-left">
        <span className="block text-[14px] font-semibold tracking-[-0.01em]">
          Launch console
        </span>
        {/* Collapsed HAS to state the plan or this is just a hidden section.
            Expanded, the rail below states it in more detail — so the static
            "Workspace → runner → capability" tagline is dropped rather than
            restated, which also stops it truncating against the new chevron. */}
        {(!collapsible || s.collapsed) && (
          <span className="mt-0.5 block truncate text-[11px] text-content-muted">
            {collapsible ? summarize(s) : "Workspace → runner → capability"}
          </span>
        )}
      </span>
      <span
        className={cn(
          "flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-1.5 text-[10.5px] font-medium",
          ready ? "bg-success/10 text-success" : "bg-warning/10 text-warning"
        )}
      >
        <span
          className={cn(
            "size-2 rounded-full shadow-[0_0_10px_currentColor]",
            ready ? "bg-success" : "bg-warning"
          )}
        />
        {ready ? "Ready" : "Needs repo"}
      </span>
      {collapsible && (
        <ChevronDown
          size={15}
          className={cn(
            "shrink-0 text-content-faint transition-transform duration-200",
            s.collapsed && "-rotate-90"
          )}
        />
      )}
    </>
  );

  if (!collapsible) {
    return (
      <div className="flex items-center gap-3 border-b border-border-subtle px-4 py-3.5">
        {body}
      </div>
    );
  }
  return (
    <button
      type="button"
      aria-expanded={!s.collapsed}
      onClick={() => s.setCollapsed(!s.collapsed)}
      className={cn(
        "flex w-full items-center gap-3 px-4 py-3.5 text-left hover:bg-content/4",
        !s.collapsed && "border-b border-border-subtle"
      )}
    >
      {body}
    </button>
  );
}

/** A rail node. `as` is a button in every variant — that is the whole point of
 *  the exercise — except in the shipped reference, where it is inert. */
function RailNode({
  icon: Icon,
  label,
  value,
  set,
  open,
  onClick,
}: {
  icon: typeof Eye;
  label: string;
  value: string;
  set: boolean;
  open?: boolean;
  onClick?: () => void;
}) {
  const Tag = onClick ? "button" : "div";
  return (
    <Tag
      {...(onClick ? { type: "button" as const, onClick, "aria-expanded": open } : {})}
      className={cn(
        "group flex min-w-0 flex-col items-center rounded-lg px-0.5 pb-1 pt-0.5 text-center",
        onClick && "cursor-default hover:bg-content/4"
      )}
    >
      <span
        className={cn(
          "z-10 flex size-[42px] items-center justify-center rounded-xl border bg-elevated shadow-macos transition-colors",
          open
            ? "border-accent bg-accent/12 text-accent ring-2 ring-accent/25"
            : set
              ? "border-accent/40 text-accent"
              : "border-warning/50 text-warning",
          onClick && !open && "group-hover:border-accent/70"
        )}
      >
        <Icon size={16} />
      </span>
      {/* 8px / 0.05em, not the 9px / 0.12em of the 3-up rail. A fourth column
          leaves ~78px per node, and "CAPABILITY" at the old size truncated to
          "CAPABILI…" — a label that has to be decoded is worse than a small
          one. */}
      <span className="mt-1.5 w-full truncate text-[8px] font-semibold uppercase tracking-[0.05em] text-content-faint">
        {label}
      </span>
      <span
        className={cn(
          "mt-0.5 w-full truncate text-[11px] font-medium",
          open ? "text-accent" : "text-content-secondary"
        )}
      >
        {value}
      </span>
    </Tag>
  );
}

/**
 * The rail. Four nodes; the accent progress line spans only the three ROUTE
 * nodes (workspace → runner → capability) and stops before Brief, which is a
 * sibling setting rather than a fourth stage.
 */
function Rail({ s, onPick }: { s: ConsoleState; onPick?: (k: Setting) => void }) {
  return (
    <div className="relative px-1">
      <div className="absolute left-[12.5%] right-[37.5%] top-[21px] h-px bg-border" />
      <div
        className={cn(
          "absolute left-[12.5%] top-[21px] h-px w-[50%] origin-left bg-accent shadow-[0_0_8px_rgb(var(--accent)/0.55)] transition-transform duration-300",
          s.repo ? "scale-x-100" : "scale-x-0"
        )}
      />
      <div className="mock-rail relative grid grid-cols-4 gap-1">
        {NODES.map((n) => {
          const v = nodeView(n.key, s);
          return (
            <RailNode
              key={n.key}
              icon={v.icon}
              label={n.label}
              value={v.value}
              set={v.set}
              open={s.panel === n.key}
              onClick={onPick ? () => onPick(n.key) : undefined}
            />
          );
        })}
      </div>
    </div>
  );
}

/* ── the editors, shared by every variant ────────────────────── */

const LEGEND =
  "mb-1.5 flex w-full items-center gap-1.5 text-[10.5px] font-semibold uppercase tracking-wider text-content-muted";
const HINT = "ml-auto font-normal normal-case tracking-normal text-content-faint";
const CARD =
  "relative flex min-w-0 cursor-default items-center gap-2.5 rounded-lg border px-2.5 py-2.5 text-left transition-colors";

function WorkspaceEditor({ s, legend = true }: { s: ConsoleState; legend?: boolean }) {
  return (
    <div>
      {legend && (
        <div className={LEGEND}>
          <FolderGit2 size={12} /> Workspace <span className={HINT}>required</span>
        </div>
      )}
      <input
        value={s.repo}
        placeholder="Choose a repository…"
        onChange={(e) => s.setRepo(e.target.value)}
        className="w-full rounded-lg border border-border bg-input px-2.5 py-2.5 font-mono text-[12px] text-content placeholder:text-content-faint focus:border-accent focus:outline-none"
      />
      <div className="mt-1.5 flex flex-wrap gap-1">
        {REPOS.map((r) => (
          <button
            key={r}
            type="button"
            onClick={() => s.setRepo(r)}
            className={cn(
              "rounded-md border px-1.5 py-1 font-mono text-[10px]",
              s.repo === r
                ? "border-accent/40 bg-accent/10 text-accent"
                : "border-border-subtle text-content-faint hover:border-border hover:text-content-secondary"
            )}
          >
            {shorten(r)}
          </button>
        ))}
      </div>
    </div>
  );
}

function RunnerEditor({ s, legend = true }: { s: ConsoleState; legend?: boolean }) {
  return (
    <div>
      {legend && (
        <div className={LEGEND}>
          <Sparkles size={12} /> Runner <span className={HINT}>background agent</span>
        </div>
      )}
      <div className="mock-runner-grid grid grid-cols-2 gap-2">
        {(Object.keys(RUNNERS) as Runner[]).map((key) => {
          const r = RUNNERS[key];
          const on = s.runner === key;
          return (
            <button
              key={key}
              type="button"
              onClick={() => s.setRunner(key)}
              className={cn(
                CARD,
                on
                  ? "border-accent/40 bg-accent/10"
                  : "border-border-subtle bg-content/3 hover:border-border hover:bg-content/6"
              )}
            >
              <span
                className={cn(
                  "flex size-7 shrink-0 items-center justify-center rounded-lg",
                  on ? "bg-accent text-white" : "bg-content/6 text-content-faint"
                )}
              >
                <r.icon size={13} />
              </span>
              <span className="min-w-0">
                <span className="block truncate text-[11.5px] font-medium">{r.label}</span>
                <span className="mt-0.5 block truncate text-[9.5px] text-content-faint">
                  {r.short}
                </span>
              </span>
              {on && <Check size={11} className="ml-auto shrink-0 text-accent" />}
            </button>
          );
        })}
      </div>
    </div>
  );
}

function CapabilityEditor({ s, legend = true }: { s: ConsoleState; legend?: boolean }) {
  return (
    <div>
      {legend && (
        <div className={LEGEND}>
          <Shield size={12} /> Access envelope <span className={HINT}>start narrow</span>
        </div>
      )}
      <div className="mock-access-grid grid grid-cols-3 gap-2">
        {(Object.keys(ACCESS) as Access[]).map((key) => {
          const a = ACCESS[key];
          const on = s.access === key;
          return (
            <button
              key={key}
              type="button"
              onClick={() => s.setAccess(key)}
              className={cn(
                "mock-access-option relative min-w-0 cursor-default rounded-lg border px-2.5 py-2.5 text-left transition-colors",
                on
                  ? "border-accent/40 bg-accent/10"
                  : "border-border-subtle bg-content/3 hover:border-border hover:bg-content/6"
              )}
            >
              <a.icon size={14} className={on ? "text-accent" : "text-content-faint"} />
              <span className="mt-2 block text-[11px] font-medium">{a.label}</span>
              <span className="mt-0.5 block text-[9.5px] leading-snug text-content-faint">
                {a.short}
              </span>
              {on && <Check size={11} className="absolute right-2 top-2 text-accent" />}
            </button>
          );
        })}
      </div>
      <p className="mt-2 text-[10.5px] leading-relaxed text-content-faint">
        {ACCESS[s.access].hint}
      </p>
    </div>
  );
}

function BriefEditor({ s, legend = true }: { s: ConsoleState; legend?: boolean }) {
  return (
    <div>
      {legend && (
        <div className={LEGEND}>
          <FileText size={12} /> Run brief <span className={HINT}>blank uses title + notes</span>
        </div>
      )}
      <div className="rounded-lg border border-border bg-input p-3 focus-within:border-accent">
        <textarea
          value={s.brief}
          rows={3}
          placeholder="What should the agent do first?"
          onChange={(e) => s.setBrief(e.target.value)}
          className="w-full resize-y bg-transparent text-[12px] leading-relaxed text-content placeholder:text-content-faint focus:outline-none"
        />
        <div className="mt-2 border-t border-border-subtle pt-2 text-[10px] text-content-faint">
          {s.brief.trim() ? "Custom seed prompt" : "Using task context"}
        </div>
      </div>
    </div>
  );
}

const EDITORS: Record<
  Setting,
  { title: string; icon: typeof Eye; render: (s: ConsoleState) => React.ReactElement }
> = {
  workspace: {
    title: "Workspace",
    icon: FolderGit2,
    render: (s) => <WorkspaceEditor s={s} legend={false} />,
  },
  runner: {
    title: "Runner",
    icon: Sparkles,
    render: (s) => <RunnerEditor s={s} legend={false} />,
  },
  capability: {
    title: "Access envelope",
    icon: Shield,
    render: (s) => <CapabilityEditor s={s} legend={false} />,
  },
  brief: { title: "Run brief", icon: FileText, render: (s) => <BriefEditor s={s} legend={false} /> },
};

/** The warning that must outlive a closed panel: "Codex isn't logged in"
 *  discovered at queue time is discovered after you've already decided. */
function RunnerWarning({ s }: { s: ConsoleState }) {
  if (s.runner !== "codex") return null;
  return (
    <p className="flex items-start gap-1.5 px-4 pb-1 text-[10.5px] leading-relaxed text-warning">
      <AlertTriangle size={11} className="mt-[2px] shrink-0" />
      <span>Codex is not logged in. Run `codex login`.</span>
    </p>
  );
}

function DispatchBar({ s }: { s: ConsoleState }) {
  const mutating = s.access !== "read-only";
  return (
    <div className="flex flex-col gap-2.5 rounded-b-xl border-t border-accent/15 bg-accent/7 px-4 py-3.5">
      <div className="flex items-center gap-1.5 text-[10.5px] leading-snug text-content-faint">
        {mutating ? (
          <GitBranch size={10} className="shrink-0 text-accent" />
        ) : (
          <Search size={10} className="shrink-0 text-accent" />
        )}
        {mutating
          ? "Snapshot on launch · changes can be rolled back"
          : "Read-only run · workspace changes are denied"}
      </div>
      <div className="mock-actions grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-2">
        <button
          type="button"
          disabled={!s.repo}
          className="mock-queue flex min-w-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-lg bg-accent px-2.5 py-2.5 text-[11.5px] font-semibold text-white hover:bg-accent-hover disabled:opacity-40"
        >
          <Moon size={13} />
          Queue for {s.runner === "claude" ? "Claude" : "Codex"}
        </button>
        <button
          type="button"
          className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-2.5 text-[11px] font-medium text-content-secondary hover:bg-content/8"
        >
          <Terminal size={12} />
          Open in Claude
        </button>
        <button
          type="button"
          className="shrink-0 rounded-lg border border-border bg-surface p-2.5 text-content-muted hover:bg-content/8"
        >
          <Clipboard size={12} />
        </button>
      </div>
    </div>
  );
}

/** `mock-console` carries the container query, matching production where
 *  `.execution-console` is itself the container. One level up (the pane wrapper)
 *  is 24px wider, which is enough for the 320px breakpoint to never fire. */
const SHELL_BASE = "mock-console rounded-xl border border-accent/25 bg-elevated shadow-macos";
const SHELL = `${SHELL_BASE} overflow-hidden`;
/**
 * B only. `overflow-hidden` on the shell CLIPS an absolutely-positioned popover
 * — the Codex option was simply cut off mid-panel. In production this is the
 * argument for a portalled Radix popover, which lands the menu on the document
 * root and straight into gotcha #22 (a translucent window behind it, so the
 * panel must be painted opaque).
 */
const SHELL_OVERFLOWING = `${SHELL_BASE} overflow-visible`;

/* ── Reference — what ships today ────────────────────────────── */

/** Not collapsible, rail inert, all four fields always on. The thing the ask is
 *  about: the rail states workspace/runner/capability and then three of the
 *  four fields below state them again. */
function ShippedConsole({ s }: { s: ConsoleState }) {
  return (
    <section className={SHELL}>
      <ConsoleHeader s={s} collapsible={false} />
      <div className="space-y-4 p-4">
        <Rail s={s} />
        <WorkspaceEditor s={s} />
        <RunnerEditor s={s} />
        <RunnerWarning s={s} />
        <BriefEditor s={s} />
        <CapabilityEditor s={s} />
      </div>
      <DispatchBar s={s} />
    </section>
  );
}

/* ── A — panel below the rail ────────────────────────────────── */

function MockA({ s }: { s: ConsoleState }) {
  const open = s.panel ? EDITORS[s.panel] : null;
  return (
    <section className={SHELL}>
      <ConsoleHeader s={s} />
      {!s.collapsed && (
        <>
          <div className="space-y-3 p-4 pb-3">
            <Rail s={s} onPick={s.toggle} />
            {open && (
              <div className="mock-panel rounded-lg border border-accent/25 bg-content/4 p-3">
                <div className="mb-2 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wider text-accent">
                  <open.icon size={11} />
                  {open.title}
                </div>
                {open.render(s)}
              </div>
            )}
          </div>
          <RunnerWarning s={s} />
          <DispatchBar s={s} />
        </>
      )}
    </section>
  );
}

/* ── B — popover anchored to the node ────────────────────────── */

function MockB({ s }: { s: ConsoleState }) {
  const open = s.panel ? EDITORS[s.panel] : null;
  const index = s.panel ? NODES.findIndex((n) => n.key === s.panel) : 0;
  return (
    <section className={cn(SHELL_OVERFLOWING, "relative")}>
      <ConsoleHeader s={s} />
      {!s.collapsed && (
        <>
          <div className="relative p-4 pb-3">
            {/* The rail sits ABOVE the click-away layer. Underneath it, clicking
                a second node only dismisses the popover — so moving from
                Workspace to Runner costs two clicks where A and C cost one. */}
            <div className="relative z-30">
              <Rail s={s} onPick={s.toggle} />
            </div>
            {open && (
              <>
                {/* Click-away, inside the console so the page behind stays live. */}
                <button
                  type="button"
                  aria-label="Close"
                  onClick={s.close}
                  className="fixed inset-0 z-20 cursor-default"
                />
                <div
                  className="mock-pop absolute left-3 right-3 top-[104px] z-40 rounded-lg border border-accent/40 bg-elevated p-3 shadow-macos-lg"
                  style={{ boxShadow: "0 18px 44px rgb(0 0 0 / 0.45)" }}
                >
                  <span
                    className="absolute -top-[5px] size-[9px] rotate-45 border-l border-t border-accent/40 bg-elevated"
                    style={{ left: `calc(${index * 25 + 12.5}% - 5px)` }}
                  />
                  <div className="mb-2 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wider text-accent">
                    <open.icon size={11} />
                    {open.title}
                  </div>
                  {open.render(s)}
                </div>
              </>
            )}
          </div>
          <RunnerWarning s={s} />
          <DispatchBar s={s} />
        </>
      )}
    </section>
  );
}

/* ── C — drill-down: the rail swaps for the editor ───────────── */

function MockC({ s }: { s: ConsoleState }) {
  const open = s.panel ? EDITORS[s.panel] : null;
  return (
    <section className={SHELL}>
      <ConsoleHeader s={s} />
      {!s.collapsed && (
        <>
          <div className="p-4 pb-3">
            {open ? (
              <div className="mock-drill">
                <button
                  type="button"
                  onClick={s.close}
                  className="-ml-1 mb-2.5 flex items-center gap-1 rounded-md px-1 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-content-muted hover:bg-content/6 hover:text-content"
                >
                  <ChevronLeft size={12} />
                  {open.title}
                </button>
                {open.render(s)}
              </div>
            ) : (
              <Rail s={s} onPick={s.toggle} />
            )}
          </div>
          <RunnerWarning s={s} />
          <DispatchBar s={s} />
        </>
      )}
    </section>
  );
}

/* ── harness ─────────────────────────────────────────────────── */

function useHeight<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [h, setH] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() =>
      setH(Math.round(el.getBoundingClientRect().height))
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, h] as const;
}

function Pane({
  caption,
  note,
  children,
}: {
  caption: string;
  note?: string;
  children: React.ReactNode;
}) {
  const [ref, height] = useHeight<HTMLDivElement>();
  return (
    // `container-type` has to sit on the element that CARRIES the width. Put it
    // on a width-less flex wrapper instead and `contain: inline-size` stops the
    // contents contributing any width at all — the pane collapses to zero and
    // the two previews render on top of each other.
    <div
      className="mock-pane flex min-w-0 flex-col"
      // 29vw lands the console around 350px at a 1280 window — the middle of
      // production's 340–420px range. Narrow the window to push it under the
      // 320px query and watch the grids stack.
      style={{ width: "clamp(340px, 29vw, 400px)" }}
    >
      <div className="mb-2 flex items-baseline gap-2 px-1">
        <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">
          {caption}
        </span>
        <span className="h-px flex-1 bg-border-subtle" />
        <span className="font-mono text-[10px] tabular-nums text-accent">{height}px</span>
      </div>
      <div ref={ref} className="rounded-xl bg-surface p-3">
        {children}
      </div>
      {note && <p className="mt-2 px-1 text-[10px] leading-relaxed text-content-faint">{note}</p>}
    </div>
  );
}

const VARIANTS: Record<
  MockVariant,
  { name: string; blurb: string; note: string; component: (p: { s: ConsoleState }) => React.ReactElement }
> = {
  A: {
    name: "Panel below rail",
    blurb: "The rail never moves; the editor opens underneath it",
    note: "Rail stays a stable summary you can click straight across. Costs the panel's height while open — the console still grows, just by one editor instead of four.",
    component: MockA,
  },
  B: {
    name: "Popover on node",
    blurb: "The editor floats over the console, anchored to the node",
    note: "Zero layout shift, and the dispatch bar never moves under the cursor. But it floats over the window at Glass transparency, so it must be painted opaque (gotcha #22), and click-away is a thing you have to get right.",
    component: MockB,
  },
  C: {
    name: "Drill-down",
    blurb: "The rail swaps for a full-width editor with a back chevron",
    note: "Smallest while editing — the editor REPLACES the rail instead of stacking under it, and gets the full pane width. Cost: while editing one setting you cannot see the other three, and leaving takes a deliberate click.",
    component: MockC,
  },
};

export default function LaunchConsoleMock() {
  const [variant, setVariant] = useState<MockVariant>("A");
  const [dark, setDark] = useState(
    () => document.documentElement.getAttribute("data-theme") !== "light"
  );
  const shipped = useConsole();
  const proposed = useConsole();

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }, [dark]);

  const Active = VARIANTS[variant].component;

  return (
    <div className="flex h-full flex-col bg-base text-content">
      <style>{`
        .mock-panel { animation: panelIn .16s ease-out; }
        @keyframes panelIn { from { opacity: 0; transform: translateY(-4px); } }
        .mock-pop { animation: popIn .14s cubic-bezier(.22,.9,.28,1); }
        @keyframes popIn { from { opacity: 0; transform: translateY(-6px) scale(.98); } }
        .mock-drill { animation: drillIn .18s ease-out; }
        @keyframes drillIn { from { opacity: 0; transform: translateX(8px); } }

        /* Four columns in a 308px pane is the new pressure point: the shipped
           container queries assume 3-up and 2-up grids. Below ~300px the rail
           goes 2x2 rather than shrinking every label into an ellipsis. */
        .mock-console { container-type: inline-size; }
        @container (max-width: 300px) {
          .mock-rail { grid-template-columns: repeat(2, minmax(0, 1fr)); row-gap: 10px; }
        }
        @container (max-width: 360px) {
          .mock-runner-grid { grid-template-columns: 1fr; }
        }
        @container (max-width: 320px) {
          .mock-access-grid { grid-template-columns: 1fr; }
          .mock-actions { grid-template-columns: minmax(0, 1fr) auto; }
          .mock-queue { grid-column: 1 / -1; }
        }
      `}</style>

      <div className="flex flex-wrap items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <div>
          <div className="text-[12px] font-semibold">Launch console — collapsible rail</div>
          <div className="text-[9px] text-content-faint">
            The rail nodes become the controls, so the fields below go away
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
        <div className="mx-auto flex flex-wrap items-start justify-center gap-8">
          <Pane
            caption="Now · shipping"
            note="Three of the four fields restate the rail directly above them. Nothing here collapses."
          >
            <ShippedConsole s={shipped} />
          </Pane>
          <Pane caption={`${variant} · ${VARIANTS[variant].name}`} note={VARIANTS[variant].note}>
            <Active s={proposed} />
          </Pane>
        </div>
        <p className="mx-auto mt-6 max-w-[560px] text-center text-[10.5px] leading-relaxed text-content-faint">
          Click a rail node to edit that setting · click the header to collapse the whole
          console · pick Codex to see the warning that has to stay visible whatever panel is
          open.
        </p>
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
