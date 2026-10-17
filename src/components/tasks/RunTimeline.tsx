import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Check, ChevronDown, ChevronRight, Circle, X } from "lucide-react";
import { api } from "@/lib/tauri";
import { parseDbTimestamp } from "@/lib/format";
import { cn } from "@/lib/utils";
import { useTaskStore } from "@/stores/taskStore";
import type { RunEvent, TaskRun } from "@/types/task";

type DisplayPhase = RunEvent["phase"] | "interrupted";

/** How close to the end counts as "still watching the tail". A couple of rows
 *  of slack, so a trackpad's inertia overshooting the last row doesn't read as
 *  the user deliberately scrolling back through the history. */
const TAIL_SLACK_PX = 24;

interface DisplayEvent {
  key: string;
  order: number;
  occurred_at: string;
  summary: string;
  phase: DisplayPhase;
  /** How many identical calls this row stands for. 1 renders no badge. */
  count: number;
  /** When the first of them ran. Only differs from `occurred_at` on a run. */
  first_at: string;
}

/** Fold append-only start/result rows into one human-scale tool-call entry. */
function foldEvents(events: RunEvent[], runFinished: boolean): DisplayEvent[] {
  const calls = new Map<string, DisplayEvent>();
  const standalone: DisplayEvent[] = [];

  for (const event of events) {
    if (!event.call_id) {
      standalone.push({
        key: `event-${event.id}`,
        order: event.seq,
        occurred_at: event.occurred_at,
        summary: event.summary,
        phase: event.phase,
        count: 1,
        first_at: event.occurred_at,
      });
      continue;
    }

    const existing = calls.get(event.call_id);
    if (!existing) {
      calls.set(event.call_id, {
        key: event.call_id,
        order: event.seq,
        occurred_at: event.occurred_at,
        summary: event.summary,
        phase: event.phase,
        count: 1,
        first_at: event.occurred_at,
      });
    } else if (event.phase !== "started") {
      existing.phase = event.phase;
      if (!existing.summary) existing.summary = event.summary;
    }
  }

  const ordered = [...calls.values(), ...standalone]
    .map((event) => ({
      ...event,
      phase:
        runFinished && event.phase === "started" ? "interrupted" : event.phase,
    }))
    .sort((a, b) => a.order - b.order);

  return collapseRuns(ordered);
}

/**
 * Merge ADJACENT rows that say the same thing into one, counted.
 *
 * A real run produces ninety-odd tool calls whose labels repeat — six `Bash
 * python3` in a row is six lines that scroll the interesting one off the top,
 * and reading them tells you nothing the first told you. Collapsed, the same
 * run is a dozen rows you can actually scan.
 *
 * **Adjacent only, never global.** Grouping every `Bash cargo test` in the run
 * into one row would reorder the timeline and hide that the agent came back to
 * something — the sequence IS the information here. Two separate bursts stay
 * two rows.
 *
 * A failure never collapses into its neighbours: it is the row you opened this
 * for, and burying it inside "×7" is how you miss it.
 */
function collapseRuns(events: DisplayEvent[]): DisplayEvent[] {
  const out: DisplayEvent[] = [];
  for (const event of events) {
    const last = out[out.length - 1];
    const mergeable =
      last &&
      last.summary === event.summary &&
      last.phase === event.phase &&
      event.phase !== "failed" &&
      event.phase !== "interrupted";
    if (mergeable) {
      last.count += 1;
      // The row carries the SPAN: first_at stays put, occurred_at follows the
      // newest, so a collapsed burst can still say when it started and ended.
      last.occurred_at = event.occurred_at;
      continue;
    }
    out.push({ ...event });
  }
  return out;
}

export function RunTimeline({ run }: { run: TaskRun }) {
  const [open, setOpen] = useState(run.status === "running");
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const lastSeq = useRef(0);
  const scroller = useRef<HTMLDivElement>(null);
  const pinnedToTail = useRef(true);
  const runActivity = useTaskStore((state) => state.runActivity);
  const live = run.status === "running";

  const loadAfter = useCallback(async () => {
    setLoading(true);
    try {
      const rows = await api.runs.events(run.id, lastSeq.current);
      if (rows.length === 0) return;
      lastSeq.current = Math.max(lastSeq.current, ...rows.map((row) => row.seq));
      setEvents((current) => {
        const known = new Set(current.map((row) => row.id));
        return [...current, ...rows.filter((row) => !known.has(row.id))].sort(
          (a, b) => a.seq - b.seq
        );
      });
    } catch {
      // The bridge is best-effort and the next hint/open retries. A transient
      // read failure must not turn the task detail pane into an error surface.
    } finally {
      setLoading(false);
    }
  }, [run.id]);

  useEffect(() => {
    if (open) void loadAfter();
  }, [loadAfter, open]);

  useEffect(() => {
    if (open && runActivity?.runId === run.id) void loadAfter();
  }, [loadAfter, open, run.id, runActivity?.key, runActivity?.runId]);

  const display = useMemo(() => foldEvents(events, !live), [events, live]);

  const trackTail = useCallback(() => {
    const box = scroller.current;
    if (!box) return;
    pinnedToTail.current =
      box.scrollHeight - box.scrollTop - box.clientHeight <= TAIL_SLACK_PX;
  }, []);

  // A capped list would otherwise show the START of a live run and hide every
  // new call below the fold — the opposite of what you open it for. Follow the
  // tail, but only while the reader hasn't scrolled back to read something:
  // yanking them to the bottom mid-read is worse than the growth this replaces.
  // Layout effect, not passive: a paint at the old offset is a visible jump.
  useLayoutEffect(() => {
    const box = scroller.current;
    if (!box || !live || !pinnedToTail.current) return;
    box.scrollTop = box.scrollHeight;
  }, [display, live, open]);

  const toggle = useCallback(() => {
    // Collapsing unmounts the scroller, so re-opening starts at the top of a
    // fresh node. Carrying a stale "scrolled away" flag across that would
    // strand the reopened list at the first row while the run kept reporting.
    pinnedToTail.current = true;
    setOpen((value) => !value);
  }, []);

  return (
    <div className="mt-1.5 border-t border-border-subtle pt-1.5">
      <button
        type="button"
        onClick={toggle}
        className="flex w-full items-center gap-1 text-left text-[10.5px] text-content-muted hover:text-content"
      >
        {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        Activity
        {display.length > 0 && (
          <span className="ml-1 font-mono text-[9px] text-content-faint">
            {display.length}
          </span>
        )}
        {loading && <span className="ml-auto text-content-faint">updating…</span>}
      </button>

      {open && (
        // The scroller wraps the rail rather than BEING it: `overflow-y-auto`
        // computes `overflow-x` from visible to auto, which would clip the
        // phase dots — they hang outside the padding box at -17px to sit on
        // the rail. `overscroll-contain` stops hitting the end of a long
        // timeline from scrolling the whole detail pane instead.
        <div
          ref={scroller}
          onScroll={trackTail}
          className="mt-1.5 max-h-60 overflow-y-auto overscroll-contain"
        >
          <div className="ml-1.5 border-l border-border-subtle pl-3">
            {display.length === 0 && !loading ? (
              <p className="py-1 text-[10px] text-content-faint">
                No recorded tool activity.
              </p>
            ) : (
              <ol className="flex flex-col gap-2 py-1">
                {display.map((event) => (
                  <li key={event.key} className="relative flex min-w-0 gap-2">
                    <span
                      className="absolute -left-[17px] top-0.5 flex size-2.5 items-center justify-center rounded-full bg-elevated"
                      title={phaseLabel(event.phase)}
                      aria-label={phaseLabel(event.phase)}
                    >
                      <PhaseIcon phase={event.phase} />
                    </span>
                    <span className="min-w-0 flex-1 truncate text-[10.5px] text-content-secondary">
                      {event.summary}
                    </span>
                    {event.count > 1 && (
                      <span
                        className="shrink-0 rounded bg-content/8 px-1 font-mono text-[9px] tabular-nums text-content-muted"
                        title={`${event.count} calls, ${formatEventTime(event.first_at)} – ${formatEventTime(event.occurred_at)}`}
                      >
                        ×{event.count}
                      </span>
                    )}
                    {/* The LAST of a collapsed burst, not the first: the useful
                        question about a repeating call is when it stopped, and
                        the full span is on the badge's title. */}
                    <time className="shrink-0 font-mono text-[9px] tabular-nums text-content-faint">
                      {formatEventTime(event.occurred_at)}
                    </time>
                  </li>
                ))}
              </ol>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function PhaseIcon({ phase }: { phase: DisplayPhase }) {
  if (phase === "succeeded") return <Check size={9} className="text-success" />;
  if (phase === "failed" || phase === "interrupted") {
    return <X size={9} className="text-error" />;
  }
  return <Circle size={7} className={cn("animate-pulse fill-accent text-accent")} />;
}

function phaseLabel(phase: DisplayPhase): string {
  if (phase === "started") return "In progress";
  if (phase === "succeeded") return "Succeeded";
  if (phase === "failed") return "Failed";
  return "Interrupted";
}

function formatEventTime(value: string): string {
  const date = parseDbTimestamp(value);
  if (!date) return "";
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });
}
