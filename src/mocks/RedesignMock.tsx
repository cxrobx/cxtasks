import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Bot,
  Calendar,
  Check,
  CheckCircle2,
  ChevronRight,
  Circle,
  CircleDot,
  Flag,
  FolderGit2,
  GitBranch,
  Inbox,
  Moon,
  Sparkles,
  Sun,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
// Aliased: this file already has its own looser `PRIORITY_EDGE` for variants
// B and C. Variant D reads the SHIPPED scales instead — including the P3 rule
// (no bar at all), which is the case the glass treatment has to survive.
import {
  PRIORITY_COLORS,
  PRIORITY_EDGE as REAL_PRIORITY_EDGE,
  STATUS_LABELS,
  statusClass,
} from "@/lib/format";
import type { TaskStatus } from "@/types/task";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — three visual directions for CXTasks.
   Reached only via `?mock` (see main.tsx); data is static.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C" | "D";

type MockTask = {
  t: number;
  title: string;
  project: string;
  priority: 0 | 1 | 2 | 3;
  status: string;
  due?: string;
  /** Real ISO instant for the shipped row, which formats its own due label. */
  dueAt?: string;
  overdue?: boolean;
  tags?: string[];
  repo?: string;
  agent?: "running" | "queued" | "review";
  tokens?: number;
  model?: string;
  /** In the multi-select set — the row's own highlight, with no tick box left
   * to show it. Worth seeing next to an unselected row, since the two now
   * differ by nothing but a background. */
  picked?: boolean;
};

const inHours = (h: number) => new Date(Date.now() + h * 3_600_000).toISOString();

const TASKS: MockTask[] = [
  { t: 41, title: "Wire intake webhook retry queue", project: "acme", priority: 0, status: "in_progress", repo: "~/clients/acme", agent: "running", tokens: 182_400, model: "sonnet-5", tags: ["keystone"] },
  { t: 31, title: "Invoice July retainer", project: "globex", priority: 0, status: "todo", due: "5:00 PM", dueAt: inHours(-3), overdue: true, tags: ["billable"] },
  { t: 38, title: "Proposal e-sign audit trail", project: "cxventures", priority: 1, status: "in_review", repo: "~/Projects/cxventures", agent: "review", tokens: 96_200, model: "sonnet-5" },
  { t: 29, title: "Renew Developer ID certificate", project: "ops", priority: 1, status: "todo", due: "Tomorrow", dueAt: inHours(26), tags: ["quick-win"], picked: true },
  { t: 44, title: "Backfill Spotify follower snapshots", project: "artist-advisory", priority: 2, status: "queued", repo: "~/Projects/artisttracker", agent: "queued", picked: true },
  { t: 35, title: "Draft Learning Hub kickoff email", project: "globex", priority: 2, status: "done", due: "Fri" },
  { t: 47, title: "Fix light-theme select popup contrast", project: "cxtasks", priority: 3, status: "inbox", repo: "~/Projects/cxtasks" },
];

const PROJECTS = [
  { name: "acme", health: "on_track", count: 6, updated: "2h" },
  { name: "globex", health: "on_track", count: 4, updated: "1d" },
  { name: "artist-advisory", health: "at_risk", count: 9, updated: "12d", stale: true },
  { name: "cxtasks", health: "on_track", count: 3, updated: "3h" },
];

const PRIORITY_EDGE = ["bg-error", "bg-warning", "bg-content-muted/60", "bg-content-faint/40"];
const PRIORITY_TEXT = ["text-error", "text-warning", "text-content-muted", "text-content-faint"];
const HEALTH_DOT: Record<string, string> = {
  on_track: "bg-success",
  at_risk: "bg-warning",
  off_track: "bg-error",
};

function TChip({ t, accent = false }: { t: number; accent?: boolean }) {
  return (
    <span
      className={cn(
        "rounded border px-1 font-mono text-[10px] tabular-nums",
        accent
          ? "border-accent/30 bg-accent/10 text-accent"
          : "border-border-subtle bg-content/5 text-content-faint"
      )}
    >
      T{t}
    </span>
  );
}

/* ── Variant A — "Refined rows" ─────────────────────────────────
   The full window on glass: sidebar, header, sectioned list. This
   used to render the real shipped `TaskRow` so the switcher read as
   before/after — that comparison has been made, so A now shows the
   LAYOUT under the page's material like every other variant.
   (Consequence worth knowing: the shipped row is no longer reachable
   from this page. It was the only place outside the Tauri app where
   its four priority states could be seen side by side.) */
function MockA({ kit, tilt }: GlassProps) {
  const [focused, setFocused] = useState(31);
  return (
    <div
      className="mx-auto flex h-full max-w-5xl gap-0 overflow-hidden rounded-xl"
      style={kit.frame}
    >
      {/* Sidebar — part of the same sheet, separated by a hairline rather than
          its own fill, or it reads as a second pane stacked on the first. */}
      <aside className="flex w-52 shrink-0 flex-col border-r border-border-subtle p-3">
        <div className="mb-4 flex items-center gap-2 px-1">
          <div className="flex size-6 items-center justify-center rounded-md bg-accent/15">
            <Zap size={13} className="text-accent" />
          </div>
          <span className="text-[13px] font-semibold">CXTasks</span>
        </div>
        {[
          { icon: Calendar, label: "Today", count: 5, active: true },
          { icon: Inbox, label: "Inbox", count: 2 },
          { icon: Flag, label: "Upcoming", count: 8 },
          { icon: Bot, label: "Agent runs", count: 3 },
        ].map((v) => (
          <button
            key={v.label}
            className={cn(
              "flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px]",
              v.active ? "font-medium text-accent" : "text-content-secondary hover:bg-content/5"
            )}
            style={v.active ? kit.picked : undefined}
          >
            <v.icon size={13} />
            <span className="flex-1">{v.label}</span>
            <span className="tabular-nums text-[10px] text-content-faint">{v.count}</span>
          </button>
        ))}
        <div className="mt-5 mb-1 px-2 text-[10px] font-semibold uppercase tracking-wider text-content-faint">
          Projects
        </div>
        {PROJECTS.map((p) => (
          <button
            key={p.name}
            className="flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px] text-content-secondary hover:bg-content/5"
          >
            <span className={cn("size-1.5 rounded-full", HEALTH_DOT[p.health], p.stale && "opacity-40")} />
            <span className="flex-1 truncate">{p.name}</span>
            {p.stale && <span className="text-[9px] text-warning">stale</span>}
            <span className="tabular-nums text-[10px] text-content-faint">{p.count}</span>
          </button>
        ))}
        <div className="mt-auto rounded-lg p-2.5" style={kit.row}>
          <div className="mb-1.5 flex items-center justify-between text-[10px] text-content-muted">
            <span>Daily tokens</span>
            <span className="tabular-nums">1.24M / 2M</span>
          </div>
          <div className="h-1 overflow-hidden rounded-full bg-content/10">
            <div className="h-full w-[62%] rounded-full bg-accent" />
          </div>
        </div>
      </aside>

      {/* List */}
      <main className="flex min-w-0 flex-1 flex-col">
        <header className="flex items-center gap-2 border-b border-border-subtle px-5 py-3">
          <h1 className="text-[15px] font-semibold">Today</h1>
          <span className="text-[11px] text-content-faint">Wed, Jul 30 · 5 tasks</span>
          <button className="ml-auto flex items-center gap-1.5 rounded-md bg-accent px-2.5 py-1 text-[11px] font-medium text-white hover:bg-accent-hover">
            <Sparkles size={11} /> New task
          </button>
        </header>
        <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto px-3 py-2">
          <div className="px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-content-muted">
            Morning
          </div>
          {TASKS.slice(0, 4).map((task) => (
            <GlassRow
              key={task.t}
              task={task}
              kit={kit}
              mode="every"
              tiltable={tilt}
              focused={focused === task.t}
              onFocus={() => setFocused(task.t)}
            />
          ))}
          <div className="mt-3 px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-content-muted">
            After standup
          </div>
          {TASKS.slice(4).map((task) => (
            <GlassRow
              key={task.t}
              task={task}
              kit={kit}
              mode="every"
              tiltable={tilt}
              focused={focused === task.t}
              onFocus={() => setFocused(task.t)}
            />
          ))}
        </div>
      </main>
    </div>
  );
}

/* ── Variant B — "Mission control" ──────────────────────────────
   Leads with the agent story: stat tiles, live run cards with
   token tickers, budget meter — the board reads like an ops room. */
function MockB({ kit }: GlassProps) {
  const [tokens, setTokens] = useState(182_400);
  useEffect(() => {
    const id = setInterval(() => setTokens((n) => n + Math.floor(Math.random() * 900) + 200), 800);
    return () => clearInterval(id);
  }, []);

  return (
    <div
      className="mx-auto flex h-full max-w-5xl flex-col gap-4 overflow-y-auto rounded-xl p-5"
      style={kit.frame}
    >
      <header className="flex items-center gap-3">
        <div>
          <h1 className="text-[17px] font-semibold tracking-tight">Mission Control</h1>
          <p className="text-[11px] text-content-muted">Wednesday, July 30 · 3 agents on the board</p>
        </div>
        <div className="ml-auto flex items-center gap-2 rounded-lg px-3 py-2" style={kit.row}>
          <span className="text-[10px] uppercase tracking-wider text-content-faint">Daily budget</span>
          <div className="h-1.5 w-28 overflow-hidden rounded-full bg-content/10">
            <div className="mock-meter h-full rounded-full bg-gradient-to-r from-accent to-success" />
          </div>
          <span className="font-mono text-[11px] tabular-nums text-content-secondary">1.24M<span className="text-content-faint">/2M</span></span>
        </div>
      </header>

      {/* Stat tiles */}
      <div className="grid grid-cols-4 gap-3">
        {[
          { label: "Running", value: "1", icon: CircleDot, tone: "text-accent" },
          { label: "Queued", value: "2", icon: Circle, tone: "text-warning" },
          { label: "In review", value: "1", icon: GitBranch, tone: "text-success" },
          { label: "Done today", value: "7", icon: CheckCircle2, tone: "text-content-muted" },
        ].map((s) => (
          <div key={s.label} className="rounded-xl p-3.5" style={kit.row}>
            <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wider text-content-faint">
              <s.icon size={11} className={s.tone} /> {s.label}
            </div>
            <div className="mt-1 text-[22px] font-semibold tabular-nums tracking-tight">{s.value}</div>
          </div>
        ))}
      </div>

      {/* Live agent runs */}
      <div>
        <div className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-content-muted">
          Agent activity
        </div>
        <div className="grid grid-cols-3 gap-3">
          {/* Running */}
          <div className="mock-shimmer relative overflow-hidden rounded-xl p-3.5" style={kit.live}>
            <div className="flex items-center gap-2">
              <span className="relative flex size-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-accent opacity-60" />
                <span className="relative inline-flex size-2 rounded-full bg-accent" />
              </span>
              <span className="text-[11px] font-medium text-accent">Running · 4m 12s</span>
              <span className="ml-auto rounded bg-content/6 px-1.5 py-0.5 font-mono text-[9px] text-content-muted">sonnet-5</span>
            </div>
            <div className="mt-2 flex items-center gap-1.5">
              <TChip t={41} accent />
              <span className="truncate text-[12px] font-medium">Wire intake webhook retry queue</span>
            </div>
            <div className="mt-0.5 text-[10px] text-content-faint">acme · ~/clients/acme</div>
            <div className="mt-2.5 font-mono text-[11px] tabular-nums text-content-secondary">
              {tokens.toLocaleString()} <span className="text-content-faint">tokens</span>
            </div>
          </div>
          {/* In review */}
          <div
            className="rounded-xl p-3.5"
            style={{ ...kit.row, boxShadow: "inset 0 0 0 1px rgb(var(--success) / 0.4)" }}
          >
            <div className="flex items-center gap-2">
              <GitBranch size={11} className="text-success" />
              <span className="text-[11px] font-medium text-success">Finished · needs review</span>
            </div>
            <div className="mt-2 flex items-center gap-1.5">
              <TChip t={38} />
              <span className="truncate text-[12px] font-medium">Proposal e-sign audit trail</span>
            </div>
            <div className="mt-0.5 text-[10px] text-content-faint">cxventures · 96.2k tokens</div>
            <div className="mt-2.5 flex items-center gap-2">
              <span className="font-mono text-[10px]"><span className="text-success">+142</span> <span className="text-error">−38</span></span>
              <button className="ml-auto rounded-md bg-success/15 px-2 py-1 text-[10px] font-medium text-success hover:bg-success/25">
                Review diff
              </button>
            </div>
          </div>
          {/* Queued */}
          <div className="rounded-xl p-3.5 opacity-80" style={kit.row}>
            <div className="flex items-center gap-2">
              <Circle size={11} className="text-warning" />
              <span className="text-[11px] font-medium text-warning">Queued · overnight batch</span>
            </div>
            <div className="mt-2 flex items-center gap-1.5">
              <TChip t={44} />
              <span className="truncate text-[12px] font-medium">Backfill Spotify snapshots</span>
            </div>
            <div className="mt-0.5 text-[10px] text-content-faint">artist-advisory · read-only</div>
            <div className="mt-2.5 text-[10px] text-content-faint">Runs when you step away</div>
          </div>
        </div>
      </div>

      {/* Compact human queue */}
      <div>
        <div className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-content-muted">
          Your queue
        </div>
        <div className="overflow-hidden rounded-xl" style={kit.row}>
          {TASKS.filter((t) => !t.agent).map((task, i) => (
            <div
              key={task.t}
              className={cn(
                "flex items-center gap-2.5 px-3.5 py-2.5 hover:bg-content/6",
                i > 0 && "border-t border-border-subtle"
              )}
            >
              <span className={cn("font-mono text-[10px] font-semibold", PRIORITY_TEXT[task.priority])}>
                P{task.priority}
              </span>
              <TChip t={task.t} />
              <span className="min-w-0 flex-1 truncate text-[12.5px]">{task.title}</span>
              <span className="text-[10px] text-content-faint">{task.project}</span>
              {task.due && <span className="w-16 text-right text-[11px] tabular-nums text-content-muted">{task.due}</span>}
              <ChevronRight size={12} className="text-content-faint" />
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/* ── Variant C — "Flow" ─────────────────────────────────────────
   Experimental: a single focused timeline for the day, milestone
   progress ring, and expand-in-place cards (click a card). */
function MockC({ kit }: GlassProps) {
  const [open, setOpen] = useState<number | null>(41);
  return (
    <div
      className="mx-auto h-full max-w-2xl overflow-y-auto rounded-xl px-8 py-6"
      style={kit.frame}
    >
      <header className="mb-6">
        <h1 className="text-[19px] font-semibold tracking-tight">Wednesday flow</h1>
        <p className="mt-0.5 text-[11.5px] text-content-muted">
          5 tasks · 1 agent working alongside you · next milestone in 9 days
        </p>
      </header>

      {/* Milestone ring */}
      <div className="mb-6 flex items-center gap-4 rounded-xl p-4" style={kit.row}>
        <svg width="44" height="44" viewBox="0 0 44 44" className="shrink-0 -rotate-90">
          <circle cx="22" cy="22" r="18" fill="none" strokeWidth="4" className="stroke-content/10" />
          <circle
            cx="22" cy="22" r="18" fill="none" strokeWidth="4" strokeLinecap="round"
            className="mock-ring stroke-accent"
            strokeDasharray={2 * Math.PI * 18}
            strokeDashoffset={2 * Math.PI * 18 * (1 - 6 / 9)}
          />
        </svg>
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <Flag size={12} className="text-accent" />
            <span className="text-[13px] font-semibold">Acme demo ready</span>
            <span className="rounded bg-content/6 px-1.5 py-0.5 text-[10px] text-content-muted">Aug 8</span>
          </div>
          <p className="mt-0.5 text-[11px] text-content-muted">6 of 9 tasks done · on pace</p>
        </div>
        <span className="ml-auto text-[20px] font-semibold tabular-nums text-content-secondary">67%</span>
      </div>

      {/* Timeline */}
      <div className="relative ml-2 border-l border-border pl-6">
        {TASKS.slice(0, 5).map((task) => {
          const isOpen = open === task.t;
          return (
            <div key={task.t} className="relative mb-3">
              <span
                className={cn(
                  "absolute -left-[30.5px] top-3.5 size-2 rounded-full border-2 border-base",
                  task.agent === "running" ? "animate-pulse bg-accent" : PRIORITY_EDGE[task.priority]
                )}
              />
              <div
                role="button"
                tabIndex={0}
                onClick={() => setOpen(isOpen ? null : task.t)}
                className="w-full cursor-default rounded-xl p-3.5 text-left transition-all"
                style={isOpen ? kit.selected : kit.row}
              >
                <div className="flex items-center gap-2">
                  <TChip t={task.t} accent={isOpen} />
                  <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{task.title}</span>
                  {task.agent === "running" && (
                    <span className="flex items-center gap-1 text-[10px] text-accent">
                      <Bot size={11} /> working
                    </span>
                  )}
                  <span className="text-[10px] text-content-faint">{task.project}</span>
                </div>
                {isOpen && (
                  <div className="mock-expand mt-3 space-y-2.5 border-t border-border-subtle pt-3">
                    {task.repo && (
                      <div className="flex items-center gap-1.5 text-[11px] text-content-muted">
                        <FolderGit2 size={11} /> {task.repo}
                        <span className="ml-auto rounded bg-content/6 px-1.5 py-0.5 font-mono text-[9px]">{task.model ?? "sonnet-5"}</span>
                      </div>
                    )}
                    <div className="rounded-lg bg-content/8 p-2.5 font-mono text-[10.5px] leading-relaxed text-content-secondary">
                      Add exponential backoff to the intake webhook consumer. Failed deliveries
                      land in `webhook_retries`; drain with jitter, cap at 6 attempts…
                    </div>
                    {task.tokens && (
                      <div className="flex items-center gap-3 text-[10.5px] text-content-muted">
                        <span className="flex items-center gap-1"><Zap size={10} className="text-accent" /> {(task.tokens / 1000).toFixed(1)}k tokens</span>
                        <span>run 2 of today</span>
                        <div className="ml-auto flex h-3 items-end gap-[3px]">
                          {[3, 7, 4, 10, 6, 12].map((h, i) => (
                            <span key={i} className="w-[4px] rounded-sm bg-accent/50" style={{ height: h }} />
                          ))}
                        </div>
                      </div>
                    )}
                    <div className="flex gap-2 pt-0.5">
                      <button className="rounded-md bg-accent px-2.5 py-1 text-[10.5px] font-medium text-white">Open in Claude</button>
                      <button className="rounded-md border border-border px-2.5 py-1 text-[10.5px] text-content-secondary">Copy seed prompt</button>
                    </div>
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/* ── Variant D — "Glass" ────────────────────────────────────────
   The question: should a task row be a raised glass card, or should
   depth be reserved for the rows that earned it? Both modes render
   the SAME rows out of the SAME material; the only variable is how
   many get it, which is the whole argument and not one prose wins.

   The material is FROSTED — a white scrim in light, a black scrim
   in dark — and it took two wrong turns to get there, both worth
   keeping written down. First it was cxmail's kit B ("Specular
   depth · no blur"), which that kit's own verdict already warns
   about: "rows read as raised keys rather than glass." Opaque,
   embossed, the one element in the window not participating in the
   light. Then it went translucent but was built from the palette
   tokens, which are near-neutral and very dark — at the alpha
   translucency needs they stop being a colour and become a veil,
   so the wallpaper's hue walked through and both themes came out
   BROWN. That is TINTED glass: it takes its colour from behind it.
   Frosted takes its colour from the scrim and only its LIGHT from
   behind. Details in `glassKit`.

   Two things from those passes were worth keeping:

   1. EVERY COLOUR IS A PALETTE TOKEN, never a hand-mixed white
      overlay. Tokens re-resolve per theme on their own; the
      hand-mixed first pass needed a `[data-theme="light"]` twin per
      state, and one of them lost to the base rule's specificity —
      light theme painted the FOCUSED row weaker than the
      multi-selected rows beside it. Tokens plus inline styles
      remove both failure modes: nothing left to cascade.
   2. LIGHT AND DARK ARE NOT ONE MATERIAL AT TWO VALUES. They are a
      white sheet and a black sheet over different backdrops, so
      they carry their own translucency pair — a black scrim at
      0.58 is a comfortable dark frost, a white one at 0.58 is a
      washed-out sheet. See `GLASS_DEFAULTS`. */

type GlassMode = "every" | "lifted";

/**
 * FROSTED GLASS: a white scrim in light, a black scrim in dark.
 *
 * The previous pass built the material out of the palette tokens
 * (`--bg-surface`, `--bg-primary`) at low alpha, which sounds right and is the
 * reason both themes came out BROWN. Those tokens are near-neutral and very
 * dark, so at the alpha translucency needs they stop being a colour and start
 * being a veil — and whatever hue the wallpaper carries walks straight through
 * and becomes the app's hue. That is tinted glass, not frosted: it takes its
 * colour from what is behind it.
 *
 * Frosted takes its colour from the SCRIM and only its light from behind.
 * So the fill is explicit white or explicit black, keyed to the theme, and the
 * backdrop's saturation is pulled DOWN rather than boosted — the desktop should
 * come through as blurred luminance, not as a colour cast.
 *
 * Still no `backdrop-filter` on a row (gotcha #22: a row's backdrop is the
 * pane's own tint, so it would blur a solid colour). It does not need one —
 * alpha composites all the way down, so a row rides on the pane's blur, which
 * in the shipped window is the NSVisualEffectView.
 */
type GlassKit = ReturnType<typeof glassKit>;

/** Every variant renders ON the glass now rather than beside it — the switcher
 *  answers "which layout", and the material is the page's, held by the shell.
 *  Showing one variant in the shipped flat style made the switcher a
 *  before/after, which is a comparison that had already been made. */
interface GlassProps {
  kit: GlassKit;
  theme: "dark" | "light";
  tilt: boolean;
}

function glassKit(theme: "dark" | "light", paneA: number, cardA: number) {
  const d = theme === "dark";
  // The scrim. Everything else here is a fill fraction of one of these two.
  const scrim = d ? "0 0 0" : "255 255 255";
  // A card sits ABOVE the pane, so in dark it is white-on-black rather than
  // more black — more black would read as a hole, not a panel.
  const card = `rgb(255 255 255 / ${cardA})`;
  // cxventures' `.glass-card` values (app/globals.css). The rim is 0.06 white
  // in dark and 0.1 black in light, and the restraint is the whole point: at
  // 0.12 the rim stops being an edge and becomes a drawn outline, which is what
  // made the earlier pass read as boxes rather than glass.
  const edge = d ? "rgb(255 255 255 / 0.06)" : "rgb(0 0 0 / 0.1)";
  const edgeHover = d ? "rgb(255 255 255 / 0.12)" : "rgb(0 0 0 / 0.14)";
  const rim = (c: string) => `inset 0 0 0 1px ${c}`;
  // A soft OUTER glow, not a drop shadow — it lights the card's surroundings
  // instead of casting it, which is the difference between "hovered" and
  // "raised". `0 0 30px` straight from the same file.
  const glow = `0 0 30px rgb(${d ? "255 255 255" : "0 0 0"} / ${d ? 0.03 : 0.04})`;

  return {
    row: { background: card, boxShadow: rim(edge) } as React.CSSProperties,
    /** Hover: fill up one step, rim up one step, plus the glow. cxventures goes
     *  0.03 → 0.06 on the fill, so this doubles whatever the dial is set to. */
    rowHover: {
      background: `rgb(255 255 255 / ${cardA * 2})`,
      boxShadow: `${rim(edgeHover)}, ${glow}`,
    } as React.CSSProperties,
    glow,
    edgeHover: rim(edgeHover),
    /** The row you are on: accent rim, fill one step denser. Depth is not the
     *  signal — the rim is. */
    selected: {
      background: `rgb(255 255 255 / ${cardA + (d ? 0.04 : 0.16)})`,
      boxShadow: rim("rgb(var(--accent) / 0.55)"),
    } as React.CSSProperties,
    /** A live run: accent-RIMMED, never accent-filled, so it cannot be read as
     *  the focused row. */
    live: {
      background: card,
      boxShadow: rim("rgb(var(--accent) / 0.28)"),
    } as React.CSSProperties,
    /** Multi-selected: fill, no accent rim. Two channels, still — and pulled
     *  well down, or the one saturated element in a 3%-white list is the thing
     *  the eye lands on first. */
    picked: {
      background: `rgb(var(--accent) / ${d ? 0.14 : 0.18})`,
      boxShadow: rim(edge),
    } as React.CSSProperties,
    /** Flat rows keep the shipped tint exactly. */
    pickedFlat: { background: "rgb(var(--accent) / 0.22)" } as React.CSSProperties,
    /** The pane — the frosted sheet itself. `saturate` sits BELOW 100% on
     *  purpose: above it the wallpaper's hue survives the scrim and the whole
     *  window takes on the desktop's colour. */
    frame: {
      background: `rgb(${scrim} / ${paneA})`,
      backdropFilter: "blur(34px) saturate(85%)",
      WebkitBackdropFilter: "blur(34px) saturate(85%)",
      boxShadow: `${rim(d ? "rgb(255 255 255 / 0.08)" : "rgb(255 255 255 / 0.7)")}, 0 30px 70px -20px rgb(0 0 0 / ${d ? 0.8 : 0.25})`,
    } as React.CSSProperties,
  };
}

/** Per-theme translucency. Light and dark are different materials over
 *  different backdrops, so one shared pair of numbers cannot serve both — a
 *  black scrim at 0.55 is a comfortable dark frost, a white one at 0.55 is a
 *  washed-out sheet. Each theme keeps its own, and switching restores them. */
const GLASS_DEFAULTS: Record<"dark" | "light", { pane: number; card: number }> = {
  dark: { pane: 0.58, card: 0.02 },
  light: { pane: 0.62, card: 0.02 },
};

/** One translucency dial. Two of them ride the caption line, so the label,
 *  the readout and the fill maths live in one place rather than three. */
function GlassSlider({
  label,
  title,
  value,
  onChange,
}: {
  label: string;
  title: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <span className="flex shrink-0 items-center gap-2" title={title}>
      <span className="shrink-0">{label}</span>
      <input
        type="range"
        min={0.02}
        max={1}
        step={0.01}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="cx-slider max-w-[130px]"
        style={{ ["--fill" as string]: `${((value - 0.02) / 0.98) * 100}%` }}
        aria-label={`${title} opacity`}
      />
      <span className="shrink-0 font-mono tabular-nums">{value.toFixed(2)}</span>
    </span>
  );
}

/** The desktop the window sits on. Present so the pane translucency has
 *  something real to pick up — without it the mock reports the material
 *  against a flat colour and flatters it. cxmail's values, with the hotspots
 *  moved inward and widened: theirs are placed for a full-bleed window, and
 *  this frame is centred at ~60% width, so the originals landed in the margins
 *  and decayed before they reached the list — the window read neutral and the
 *  translucency looked like it was doing nothing. This is one particular
 *  desktop, and how warm the app looks is a property of HIS wallpaper, not of
 *  the design; it is here so the material is judged under some light rather
 *  than none. Busy enough to be an honest test rather than a flattering one. */
function MockWallpaper({ theme }: { theme: "dark" | "light" }) {
  const dark =
    "radial-gradient(1500px 1150px at 22% 2%, rgb(140 100 58 / .7), transparent 68%)," +
    "radial-gradient(1000px 820px at 86% 22%, rgb(48 74 112 / .5), transparent 64%)," +
    "radial-gradient(1300px 1050px at 50% 102%, rgb(150 104 62 / .55), transparent 70%)," +
    "linear-gradient(158deg, #1d1913, #0c0a08 55%, #15110c)";
  const light =
    "radial-gradient(1500px 1150px at 22% 2%, rgb(255 214 160 / .9), transparent 68%)," +
    "radial-gradient(1000px 820px at 86% 22%, rgb(178 206 240 / .8), transparent 64%)," +
    "radial-gradient(1300px 1050px at 50% 102%, rgb(255 196 148 / .75), transparent 70%)," +
    "linear-gradient(158deg, #f6e9d8, #e6dccd 55%, #f2e6d6)";
  return (
    <div
      aria-hidden
      className="pointer-events-none absolute inset-0"
      style={{ background: theme === "dark" ? dark : light }}
    />
  );
}

/** Rows chosen to put the material against every state the row background
 *  already carries: a live run, the focused row, two multi-picked rows, an
 *  overdue P0, and an untriaged P3 that paints no edge bar at all. */
const GLASS_ROWS: MockTask[] = [
  TASKS[0], // T41 · running agent
  TASKS[1], // T31 · P0, overdue — focused by default
  TASKS[3], // T29 · P1, multi-picked
  TASKS[2], // T38 · in review
  TASKS[4], // T44 · P2, multi-picked
  TASKS[6], // T47 · P3 — no edge bar
];

function MockD({ kit, tilt }: GlassProps) {
  const [mode, setMode] = useState<GlassMode>("every");
  const [focused, setFocused] = useState(31);
  const listRef = useRef<HTMLDivElement>(null);
  const [pitch, setPitch] = useState<number | null>(null);

  // The density cost, MEASURED rather than asserted — the modes differ by
  // padding, gap and shadow spread, and quoting a number nobody computed is how
  // a design argument stops being checkable. Pitch is top-to-top, so it counts
  // the gap the cards need and the flat rows do not.
  useLayoutEffect(() => {
    const rows = listRef.current?.querySelectorAll<HTMLElement>("[data-mock-row]");
    if (!rows || rows.length < 2) return;
    setPitch(
      Math.round(rows[1].getBoundingClientRect().top - rows[0].getBoundingClientRect().top)
    );
  }, [mode]);

  // A 720pt list pane is a normal CXTasks window less header and quick-add.
  const perScreen = pitch ? Math.floor(720 / pitch) : null;

  return (
    <div
      className="mx-auto flex h-full max-w-3xl flex-col overflow-hidden rounded-xl"
      style={kit.frame}
    >
        <header className="flex shrink-0 items-center gap-3 border-b border-border-subtle px-5 py-3">
          <div className="min-w-0">
            <h1 className="text-[15px] font-semibold">Today</h1>
            <p className="text-[11px] text-content-faint">
              Same rows, same material — only the count changes
            </p>
          </div>
          <div className="ml-auto flex shrink-0 overflow-hidden rounded-lg border border-border">
            {(
              [
                ["every", "Every row"],
                ["lifted", "Only what's lifted"],
              ] as [GlassMode, string][]
            ).map(([m, label]) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className={cn(
                  "px-2.5 py-1 text-[11px] font-medium",
                  mode === m
                    ? "bg-accent text-white"
                    : "bg-surface text-content-secondary hover:bg-elevated"
                )}
              >
                {label}
              </button>
            ))}
          </div>
        </header>

        <div className="flex shrink-0 flex-col border-b border-border-subtle px-5 py-2 text-[11px] leading-relaxed text-content-muted">
          <span>
          {mode === "every" ? (
            <>
              Every row is a card, and every card is translucent — so the pane&rsquo;s
              light carries through all of them rather than being covered by
              them. That answers the objection the opaque version earned: cards
              tint the glass now instead of tiling over it. Focus is the accent
              rim (T31); the selection set is fill (T29, T44) — still two
              channels, so both still read. What it costs is pitch.
            </>
          ) : (
            <>
              Flat by default; only the focused row and a live agent run take
              the material. Cheaper per row and the rail stays quiet — but with
              a translucent card the case for this is now density alone, not
              legibility.
            </>
          )}
          {pitch !== null && (
            <span className="ml-1.5 whitespace-nowrap font-mono text-[10.5px] tabular-nums text-content-faint">
              · pitch {pitch}px · ~{perScreen} rows per 720px pane
            </span>
          )}
          </span>
        </div>

        <div
          ref={listRef}
          className={cn(
            "min-h-0 flex-1 overflow-y-auto px-3 py-2.5",
            mode === "every" && "space-y-1.5"
          )}
        >
          {/* The sticky header is the one place in this app where a blur is
              REAL — it has rows scrolling under it, so it has something to
              blur. Left as the shipped treatment: it is the control. */}
          <div className="sticky top-0 z-10 -mx-3 mb-1.5 bg-base/95 px-5 py-1 text-[10px] font-semibold uppercase tracking-wider text-content-muted backdrop-blur">
            Morning
          </div>
          {GLASS_ROWS.map((task) => (
            <GlassRow
              key={task.t}
              task={task}
              kit={kit}
              mode={mode}
              tiltable={tilt}
              focused={focused === task.t}
              onFocus={() => setFocused(task.t)}
            />
          ))}
        </div>

        <div className="shrink-0 border-t border-border-subtle px-5 py-2 text-[10.5px] leading-relaxed text-content-faint">
          Rows are translucent, not blurred — and they do not need to be. A row
          cannot add its own blur (its backdrop is the pane&rsquo;s flat tint, so
          backdrop-filter there blurs nothing — gotcha #22), but alpha
          composites all the way down, so it inherits the real desktop blur from
          the NSVisualEffectView underneath. The pane blur here stands in for
          that layer; the wallpaper is what it would be picking up.
      </div>
    </div>
  );
}

function GlassRow({
  task,
  kit,
  mode,
  tiltable,
  focused,
  onFocus,
}: {
  task: MockTask;
  kit: GlassKit;
  mode: GlassMode;
  tiltable: boolean;
  focused: boolean;
  onFocus: () => void;
}) {
  const live = task.agent === "running";
  const picked = Boolean(task.picked);
  const closed = task.status === "done";
  // "Lifted" is the recommendation in one expression: the row you are on, and
  // the row something is happening to. Everything else stays in the plane.
  const glass = mode === "every" || focused || live;
  const [hover, setHover] = useState({ on: false, rx: 0, ry: 0, mx: 50, my: 50 });
  const [hovered, setHovered] = useState(false);

  // Cursor-tracked tilt + sheen — the literal "3D" of the original ask, ported
  // from cxmail's kit C. Deliberately behind a toggle and kept under 1.5deg: a
  // task list is scanned down a column, and a row that moves as the pointer
  // crosses it fights the scan it is meant to serve.
  const onMove = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!tiltable || !glass) return;
    const r = e.currentTarget.getBoundingClientRect();
    const mx = ((e.clientX - r.left) / r.width) * 100;
    const my = ((e.clientY - r.top) / r.height) * 100;
    // /46 caps the tilt near 1 degree. A task list is scanned DOWN a column, so
    // a row that visibly swings as the pointer crosses it fights the scan it is
    // meant to serve — the effect has to be felt more than seen.
    setHover({ on: true, rx: (50 - my) / 46, ry: (mx - 50) / 46, mx, my });
  };

  // Hover composes ONTO the state rather than replacing it: a focused or
  // multi-selected row keeps its own rim and only gains the glow, or hovering
  // would silently strip the thing telling you which row you are on.
  const base = focused
    ? kit.selected
    : live
      ? kit.live
      : picked
        ? glass
          ? kit.picked
          : kit.pickedFlat
        : glass
          ? kit.row
          : undefined;

  return (
    <div
      data-mock-row
      role="option"
      aria-selected={focused || picked}
      onClick={onFocus}
      onMouseMove={onMove}
      onMouseLeave={() => {
        setHovered(false);
        setHover((p) => ({ ...p, on: false, rx: 0, ry: 0 }));
      }}
      className={cn(
        "group relative flex cursor-default select-none items-start gap-2.5 py-2.5 pl-4 pr-3",
        glass ? "overflow-hidden rounded-[10px]" : "border-b border-border-subtle",
        !glass && !picked && "hover:bg-content/4",
        live && !focused && "mock-shimmer"
      )}
      onMouseEnter={() => setHovered(true)}
      style={{
        ...base,
        ...(glass && hovered
          ? focused || live || picked
            ? { boxShadow: `${base?.boxShadow}, ${kit.glow}` }
            : kit.rowHover
          : null),
        transform:
          tiltable && glass && hover.on
            ? `perspective(900px) rotateX(${hover.rx}deg) rotateY(${hover.ry}deg) translateZ(4px)`
            : undefined,
        // Transform stays fast so the tilt tracks the pointer; fill and glow ride
        // cxventures' 0.3s, which is what makes the hover read as a light coming
        // up rather than a state flipping.
        transition:
          "transform 140ms ease-out, background 300ms ease, box-shadow 300ms ease",
      }}
    >
      {/* A pointer-following specular sweep. Only meaningful while the surface
          is lit, so it rides with the tilt rather than being its own control. */}
      {tiltable && glass && hover.on && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0"
          style={{
            background: `radial-gradient(220px circle at ${hover.mx}% ${hover.my}%, rgb(255 255 255 / 0.055), transparent 65%)`,
          }}
        />
      )}

      {/* The bar stays at `left-0` in BOTH materials, and that is a correction
          rather than a preference: insetting it to clear the card's rim (the
          obvious move — a 3px bar on a rounded corner sounds like a chipped
          edge) put the lifted rows' bars 8px right of the flat ones, and the
          priority rail visibly zigzagged down the list. A rail that only reads
          as one scale when every row shares a material is not a trade-off, it
          is a defect. `overflow-hidden` lets the 10px radius clip the bar
          instead; at `inset-y-2` the corner curve has already run out, so it
          takes off ~0.2px and nothing is visible. P3 paints nothing either way
          (the companion rule to invariant 9, in `PRIORITY_EDGE`). */}
      <span
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-y-2 left-0 w-[3px] rounded-full",
          REAL_PRIORITY_EDGE[task.priority]
        )}
      />

      <button
        type="button"
        role="checkbox"
        aria-checked={closed}
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "mt-px flex size-[15px] shrink-0 items-center justify-center rounded-[4px] border transition-colors duration-75",
          closed
            ? "border-success bg-success text-white"
            : "border-content-muted/60 hover:border-success hover:bg-success/10"
        )}
      >
        {closed && <Check size={10} strokeWidth={3} />}
      </button>

      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-baseline gap-2">
          <TChip t={task.t} accent={focused} />
          {task.priority < 3 && (
            <span
              className={cn(
                "shrink-0 text-[11px] font-semibold tabular-nums",
                PRIORITY_COLORS[task.priority]
              )}
            >
              P{task.priority}
            </span>
          )}
          <span className={cn("truncate", closed ? "text-content-faint line-through" : "text-content")}>
            {task.title}
          </span>
          {live && (
            <span className="ml-auto flex shrink-0 items-center gap-1 text-[10px] text-accent">
              <Bot size={11} /> working
            </span>
          )}
        </div>

        <div className="flex items-center gap-2 text-[11px] text-content-faint">
          <span className={cn("rounded px-1.5 py-px font-medium", statusClass(task.status as TaskStatus))}>
            {STATUS_LABELS[task.status as TaskStatus]}
          </span>
          <span className="truncate">#{task.project}</span>
          {task.tags?.map((tag) => (
            <span key={tag} className="shrink-0 text-content-faint/70">
              #{tag}
            </span>
          ))}
          {task.overdue && task.due && (
            <span className="shrink-0 font-medium text-error">{task.due}</span>
          )}
          {task.repo && <GitBranch size={11} className="ml-auto shrink-0 text-accent" />}
        </div>
      </div>
    </div>
  );
}

/* ── Shell ─────────────────────────────────────────────────────── */

const VARIANTS: Record<
  MockVariant,
  { name: string; blurb: string; el: React.ComponentType<GlassProps> }
> = {
  A: { name: "Refined rows", blurb: "The list layout, on glass — edge bars, chips, calmer hierarchy", el: MockA },
  B: { name: "Mission control", blurb: "Leads with the agent story — stat tiles, live runs, budget meter", el: MockB },
  C: { name: "Flow", blurb: "One focused timeline — milestone ring, expand-in-place cards", el: MockC },
  D: { name: "Glass", blurb: "The material study — every row a card, or only the ones that earned it", el: MockD },
};

export default function RedesignMock() {
  const [variant, setVariant] = useState<MockVariant>("A");
  const [dark, setDark] = useState(
    () => document.documentElement.getAttribute("data-theme") !== "light"
  );
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }, [dark]);
  const theme: "dark" | "light" = dark ? "dark" : "light";

  // The material is the PAGE's, not one variant's. Two dials, kept per theme:
  // the sheet one mirrors `uiStore::applyWindow` (the shipped transparency
  // setting), the card one is new, because with a frosted material the sheet
  // and the panels on it are separately legible and one number cannot serve
  // both. Per theme rather than shared — a black scrim and a white one need
  // different values to look like the same idea, so one pair would make every
  // theme switch a re-tune.
  const [alphas, setAlphas] = useState(GLASS_DEFAULTS);
  const { pane, card } = alphas[theme];
  const setAlpha = (key: "pane" | "card", v: number) =>
    setAlphas((prev) => ({ ...prev, [theme]: { ...prev[theme], [key]: v } }));
  const kit = glassKit(theme, pane, card);
  // On by default now — it is part of the material rather than a thing to
  // opt into, and it is subtle enough that hiding it behind a checkbox meant
  // it was mostly never seen.
  const [tilt, setTilt] = useState(true);

  const Active = VARIANTS[variant].el;

  return (
    <div className="flex h-full flex-col bg-base text-content">
      <style>{`
        .mock-meter { width: 62%; animation: mockMeter 1.2s cubic-bezier(.2,.8,.2,1); }
        @keyframes mockMeter { from { width: 0 } }
        .mock-ring { transition: stroke-dashoffset 1s cubic-bezier(.2,.8,.2,1); }
        .mock-expand { animation: mockExpand .18s ease-out; }
        @keyframes mockExpand { from { opacity: 0; transform: translateY(-3px) } }
        .mock-shimmer::after {
          content: ""; position: absolute; inset: 0; pointer-events: none;
          background: linear-gradient(105deg, transparent 40%, rgb(var(--accent) / 0.07) 50%, transparent 60%);
          background-size: 250% 100%;
          animation: mockShimmer 2.6s linear infinite;
        }
        @keyframes mockShimmer { from { background-position: 200% 0 } to { background-position: -50% 0 } }
      `}</style>

      {/* Switcher */}
      <div className="flex items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <span className="text-[12px] font-semibold">CXTasks — design exploration</span>
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
              {v} · {VARIANTS[v].name}
            </button>
          ))}
        </div>
        <span className="min-w-0 truncate text-[11px] text-content-muted">
          {VARIANTS[variant].blurb}
        </span>
        <button
          onClick={() => setDark((d) => !d)}
          className="ml-auto shrink-0 rounded-md border border-border p-1.5 text-content-muted hover:text-content"
          title="Toggle theme"
        >
          {dark ? <Sun size={13} /> : <Moon size={13} />}
        </button>
      </div>

      {/* The material's controls, alongside the layout switcher — they apply to
          whichever variant is showing, which is the point of hoisting them. */}
      <div className="flex flex-wrap items-center gap-x-5 gap-y-1 border-b border-border-subtle bg-sidebar px-5 py-1.5 text-[10.5px] text-content-faint">
        <GlassSlider
          label="Sheet"
          title={dark ? "Black scrim" : "White scrim"}
          value={pane}
          onChange={(v) => setAlpha("pane", v)}
        />
        <GlassSlider label="Card" title="Card fill" value={card} onChange={(v) => setAlpha("card", v)} />
        <label className="flex shrink-0 cursor-pointer items-center gap-1.5">
          <input
            type="checkbox"
            checked={tilt}
            onChange={(e) => setTilt(e.target.checked)}
            className="accent-[rgb(var(--accent))]"
          />
          3D tilt
        </label>
        <span className="shrink-0">
          {dark ? "Black" : "White"} frosted, kept per theme — the sheet dial is
          the shipped transparency setting
        </span>
      </div>

      {/* One desktop behind every variant. Without it the translucency has
          nothing to pick up and the material reports against a flat colour. */}
      <div className="relative min-h-0 flex-1 overflow-hidden">
        <MockWallpaper theme={theme} />
        <div className="relative h-full overflow-y-auto p-6">
          <Active kit={kit} theme={theme} tilt={tilt} />
        </div>
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
