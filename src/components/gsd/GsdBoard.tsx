import { Fragment, useState } from "react";
import { CalendarClock, ChevronDown, ChevronRight, Flag } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  formatAge,
  formatDue,
  formatShortId,
  healthColor,
} from "@/lib/format";
import { Select } from "@/components/shared/Select";
import { useTaskStore } from "@/stores/taskStore";
import {
  HEALTH_OPTIONS,
  type ActivityState,
  type BoardRow,
  type ProjectHealth,
} from "@/types/task";

/**
 * The state chip's words and colour. The chip reports what the CLOCK says, so
 * every label here is a fact rather than a claim — see `ActivityState`.
 *
 * `empty` is deliberately not `dormant`: "you have left this project" and "this
 * project has never begun" are different sentences, and rendering a brand-new
 * project as abandoned is the accusation-by-default the health dot used to make.
 */
const ACTIVITY_STYLE: Record<ActivityState, string> = {
  active: "text-success bg-success/13",
  idle: "text-warning bg-warning/13",
  dormant: "text-content-faint bg-content/6",
  empty: "text-content-faint bg-content/6",
};

/**
 * **An area never wears warning colour for being quiet.**
 *
 * The state comes from the clock and is the same either way, but what it MEANS
 * is not: a finite project untouched for a week is drifting, an area untouched
 * for a week is a week you spent elsewhere. Colouring both amber is how a board
 * ends up permanently yellow and stops being read at all — the precise failure
 * the health dot already had. So an area's quiet is neutral, and only a project
 * gets the warning.
 */
function activityStyle(row: BoardRow): string {
  if (row.kind === "area" && row.activity !== "active") {
    return "text-content-faint bg-content/6";
  }
  return ACTIVITY_STYLE[row.activity];
}

function activityLabel(row: BoardRow): string {
  if (row.activity === "active") return "Active";
  if (row.activity === "empty") return "Empty";
  // The day count IS the message — a bare word reads as a judgement, a word
  // plus a number reads as a measurement you can weigh yourself.
  const days = (word: string) =>
    row.idle_days === null ? word : `${word} ${row.idle_days}d`;
  if (row.kind === "area") return days("Quiet");
  return row.activity === "idle" ? days("Idle") : days("Stalled");
}

/**
 * Fourteen bars, one per local day, oldest first.
 *
 * Heights are scaled against the row's own busiest day, not a global maximum:
 * the question each strip answers is "what is this project's rhythm", and one
 * 40-event day in another project would otherwise flatten every bar here to
 * nothing. A day with events is never shorter than 3px, so "a little happened"
 * and "nothing happened" can never render identically.
 */
function ActivityStrip({ strip }: { strip: number[] }) {
  const peak = Math.max(...strip, 1);
  const total = strip.reduce((a, b) => a + b, 0);
  return (
    <span
      className="flex h-[14px] shrink-0 items-end gap-[2px]"
      aria-label={`${total} events in the last ${strip.length} days`}
      title={`${total} events in the last ${strip.length} days`}
    >
      {strip.map((n, i) => (
        <span
          key={i}
          className={cn(
            "w-[3px] rounded-t-[1px]",
            n > 0 ? "bg-accent/70" : "bg-content/10"
          )}
          style={{ height: n > 0 ? `${Math.max(3, (n / peak) * 14)}px` : "2px" }}
        />
      ))}
    </span>
  );
}

/**
 * The GSD board: one row per project answering "what's on track and what's
 * rotting", and — expanded — the written log behind each answer.
 *
 * The design decision worth keeping: **staleness is rendered as loudly as bad
 * health.** A project sitting at `on_track` whose last update is six weeks old
 * is not on track, it's unobserved, and a board that renders those two states
 * identically stops being an accountability mechanism within a month. So an
 * update older than a week — or the absence of any update at all — is called out
 * in warning colour next to the dot that's claiming everything is fine.
 */
export function GsdBoard() {
  const board = useTaskStore((s) => s.board);

  if (board.length === 0) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-center text-content-faint">
        <p>
          Nothing here yet. Create an area in the sidebar — an ongoing
          responsibility — or file a task with a project from a Claude session.
        </p>
      </div>
    );
  }

  return (
    // `min-h-0` on the column and the scroller both — without it the list
    // overflows the grid row and never becomes scrollable (gotcha #10).
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto">
        {/* Areas carry their projects; a project belonging to no area follows
            under its own heading. Same shape as the sidebar, and for the same
            reason — an area's numbers already INCLUDE its projects, so the
            indented rows are what makes the top line legible instead of a
            total you have to take on faith. */}
        {/* Areas nest to any depth, so this is a tree walk from the top-level
            areas; each node renders under its parent, once. */}
        {board
          .filter((r) => r.kind === "area" && !hasParentIn(board, r))
          .map((area) => renderNode(board, area, 0))}
        {board.some((r) => r.kind === "project" && r.parent === null) && (
          <>
            <h3 className="px-3 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Projects with no area
            </h3>
            {board
              .filter((r) => r.kind === "project" && r.parent === null)
              .map((row) => (
                <BoardRowView key={row.project} row={row} />
              ))}
          </>
        )}
      </div>
    </div>
  );
}

/** Is `row`'s parent on the board? A parent that isn't (archived) must not
 * take the row off the board with it — it is drawn at the top instead. */
function hasParentIn(board: BoardRow[], row: BoardRow) {
  return row.parent !== null && board.some((r) => r.project.toLowerCase() === row.parent!.toLowerCase());
}

function renderNode(board: BoardRow[], node: BoardRow, depth: number): React.ReactNode {
  if (depth > 32) return null; // a hand-edited parent cycle must not recurse forever
  const children = board.filter(
    (c) => c.parent !== null && c.parent.toLowerCase() === node.project.toLowerCase()
  );
  return (
    <Fragment key={node.project}>
      <BoardRowView row={node} depth={depth} />
      {children.map((child) => renderNode(board, child, depth + 1))}
    </Fragment>
  );
}

function BoardRowView({ row, depth = 0 }: { row: BoardRow; depth?: number }) {
  const nested = depth > 0;
  const [expanded, setExpanded] = useState(false);
  const setProject = useTaskStore((s) => s.setProject);
  const loadProjectUpdates = useTaskStore((s) => s.loadProjectUpdates);

  const age = formatAge(row.last_update?.created_at ?? null);

  const toggle = () => {
    const next = !expanded;
    setExpanded(next);
    // Lazy: the log is only fetched for a row someone actually opens.
    if (next) void loadProjectUpdates(row.project);
  };

  return (
    <div
      className={cn(
        "border-b border-border-subtle",
        // A project nobody has begun recedes rather than shouting. It is on the
        // board because it exists, not because it needs something.
        row.activity === "empty" && "opacity-55"
      )}
    >
      <div
        className={cn(
          "flex min-h-0 items-start gap-2.5 py-2.5 pr-3 hover:bg-content/4",
          nested ? "border-l-2 border-border-subtle" : "pl-3"
        )}
        // 12px per level on top of the base inset: one level reads as the old
        // `pl-6`, and each deeper level steps in again.
        style={nested ? { paddingLeft: 12 + depth * 12 } : undefined}
      >
        <button
          type="button"
          aria-label={expanded ? "Collapse" : "Expand"}
          aria-expanded={expanded}
          onClick={toggle}
          className="mt-0.5 shrink-0 text-content-faint hover:text-content"
        >
          {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        </button>

        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => void setProject(row.project)}
              title={`Open ${row.project}`}
              className="truncate text-content hover:underline"
            >
              {row.project}
            </button>

            {/* The state chip replaces the health dot as the row's marker. It
                cannot go stale, because nothing has to maintain it. */}
            <span
              className={cn(
                "shrink-0 rounded-full px-1.5 py-px text-[10px] font-semibold",
                activityStyle(row)
              )}
              title={
                row.last_activity
                  ? `last activity ${formatAge(row.last_activity)} ago`
                  : "nothing has happened in this project yet"
              }
            >
              {activityLabel(row)}
            </span>

            {/* A project's finish line, beside its name — the one fact an area
                can never have, and the reason a project is a project. */}
            {row.target_date && (
              <span
                className="flex shrink-0 items-center gap-1 text-[11px] text-content-muted"
                title="Target date for the whole project"
              >
                <CalendarClock size={9} />
                {formatDue(row.target_date)}
              </span>
            )}
            {row.closed_at && (
              <span className="shrink-0 rounded-full bg-content/6 px-1.5 py-px text-[10px] font-semibold text-content-faint">
                Closed
              </span>
            )}

            {/* A health that WAS deliberately reported still shows — a written
                "at risk" outranks "something moved yesterday". It is just no
                longer invented for projects nobody reports on. */}
            {row.health && (
              <span
                className={cn("shrink-0 text-[10px] font-medium", healthColor(row.health))}
                title={`reported ${age ? `${age} ago` : ""}`.trim()}
              >
                {row.health.replace("_", " ")}
              </span>
            )}

            {row.next_milestone && (
              <span className="flex min-w-0 items-center gap-1 text-[11px] text-content-muted">
                <Flag size={9} className="shrink-0" />
                <span className="truncate">{row.next_milestone.name}</span>
                {row.next_milestone.target_date && (
                  <span className="shrink-0 text-content-faint">
                    {formatDue(row.next_milestone.target_date)}
                  </span>
                )}
                <span className="shrink-0 tabular-nums text-content-faint">
                  {row.next_milestone.total_count - row.next_milestone.open_count}/
                  {row.next_milestone.total_count}
                </span>
              </span>
            )}

            {/* The strip anchors right, so every row's 14 days line up as one
                column down the board — the comparison is the whole point. */}
            <span className="ml-auto shrink-0 pl-2">
              <ActivityStrip strip={row.activity_strip} />
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-2 text-[11px] text-content-faint">
            {/* FIRST, and the only count with a filled chip: a finished run
                nobody has looked at is the one thing on this board the reader
                does not already know. Everything else is a number; this is a
                thing to do. */}
            {row.in_review_count > 0 && (
              <span className="rounded bg-success/13 px-1.5 py-px font-medium tabular-nums text-success">
                {row.in_review_count} waiting on you
              </span>
            )}
            <span className="tabular-nums text-content-muted">
              {row.actionable_count} actionable
            </span>
            {row.planned_count > 0 && (
              <span className="tabular-nums text-warning/90">
                {row.planned_count} planned
              </span>
            )}
            {row.blocked_count > 0 && (
              <span className="tabular-nums text-error">
                {row.blocked_count} blocked
              </span>
            )}
            <span className="tabular-nums">{row.done_this_week} done this wk</span>
            {row.created_this_week > 0 && (
              <span className="tabular-nums">{row.created_this_week} filed</span>
            )}

            {/* Garnish, not headline: the written update is reported when it
                exists and its absence says nothing. The old line printed
                "never reported on" in warning colour, which described a
                reporting habit rather than the project. */}
            {row.last_update && (
              <span className="min-w-0 truncate text-content-muted">
                <span className="text-content-faint">{age} ago: </span>
                {row.last_update.body}
              </span>
            )}
          </div>

          {row.next_task_title ? (
            <p className="truncate text-[11px] text-content-muted">
              <span className="text-content-faint">do next: </span>
              {row.next_task_short_id !== null && (
                <span className="font-mono tabular-nums text-content-faint">
                  {formatShortId(row.next_task_short_id)}{" "}
                </span>
              )}
              {row.next_task_title}
            </p>
          ) : (
            // A project that is MOVING with nothing filed is a real state the
            // old board could not express — it read as an empty project. The
            // next step exists, it just isn't written down anywhere the board
            // can see, and saying so is more useful than saying nothing.
            row.activity === "active" &&
            row.open_count === 0 && (
              <p className="truncate text-[11px] italic text-content-faint">
                moving, but nothing is filed
              </p>
            )
          )}
        </div>
      </div>

      {expanded && <UpdateLog row={row} />}
    </div>
  );
}

function UpdateLog({ row }: { row: BoardRow }) {
  const updates = useTaskStore((s) => s.boardUpdates[row.project]);
  const postProjectUpdate = useTaskStore((s) => s.postProjectUpdate);

  const [body, setBody] = useState("");
  // Pre-seeded to the CURRENT health, so posting a note doesn't silently reset
  // a status to whatever happened to be first in the list.
  const [health, setHealth] = useState<ProjectHealth | "unchanged">(
    row.health ?? "unchanged"
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!body.trim() || busy) return;
    setBusy(true);
    const err = await postProjectUpdate(
      row.project,
      body,
      health === "unchanged" ? null : health
    );
    setBusy(false);
    if (err) {
      setError(err);
      return;
    }
    setError(null);
    setBody("");
  };

  return (
    <div className="flex min-h-0 flex-col gap-3 border-t border-border-subtle bg-content/3 px-3 py-3 pl-8">
      {updates === undefined ? (
        <p className="text-[11px] text-content-faint">Loading…</p>
      ) : updates.length === 0 ? (
        <p className="text-[11px] text-content-faint">
          Nothing written down yet. The board reads this project from its
          activity either way — notes are for the things activity can’t show:
          why it stalled, what you’re waiting on, what you promised someone.
        </p>
      ) : (
        <ul className="flex min-h-0 flex-col gap-2">
          {updates.map((u) => (
            <li key={u.id} className="flex flex-col gap-0.5">
              <div className="flex items-baseline gap-2 text-[10px] text-content-faint">
                <span>{u.author}</span>
                <span>{formatAge(u.created_at)} ago</span>
                {u.health && (
                  <span className={healthColor(u.health)}>→ {u.health}</span>
                )}
              </div>
              <p className="whitespace-pre-wrap text-content-secondary">{u.body}</p>
            </li>
          ))}
        </ul>
      )}

      <div className="flex min-h-0 flex-col gap-2">
        <textarea
          value={body}
          rows={2}
          placeholder="What actually happened? Why it stalled, what you're waiting on, what you promised."
          onChange={(e) => setBody(e.target.value)}
          className="w-full resize-y rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint"
        />
        <div className="flex items-center gap-2">
          {/* Shared Radix Select, never a native `<select>` — WKWebView draws
              those popups light grey regardless of theme (gotcha #6). */}
          <Select
            aria-label="Health"
            value={health}
            onValueChange={(v) => setHealth(v as ProjectHealth | "unchanged")}
            options={[
              { value: "unchanged" as const, label: "Leave health unchanged" },
              ...HEALTH_OPTIONS.map((h) => ({ value: h.value, label: h.label })),
            ]}
            className="w-56"
          />
          <button
            type="button"
            disabled={!body.trim() || busy}
            onClick={() => void submit()}
            className={cn(
              "rounded-md border border-border px-2 py-1 text-content",
              "hover:border-accent hover:text-accent disabled:opacity-40"
            )}
          >
            Post
          </button>
          {error && <span className="text-error">{error}</span>}
        </div>
      </div>
    </div>
  );
}
