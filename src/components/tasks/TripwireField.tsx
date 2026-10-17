import { Combobox } from "@/components/shared/Combobox";
import { useEffect, useState } from "react";
import { RefreshCw, Zap } from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import {
  formatAge,
  formatCadence,
  isDeferred,
  isMonitor,
  isTripwireWatching,
  tripwireOutcomeLabel,
  parseAgentTripwire,
  parseJevTripwire,
  parseTripwireSpec,
  tripwireUnchecked,
} from "@/lib/format";
import { Select } from "@/components/shared/Select";
import { useTaskStore } from "@/stores/taskStore";
import {
  TRIPWIRE_CADENCES,
  type Task,
  type TripwireRuntime,
} from "@/types/task";

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";
const LABEL = "text-[11px] font-medium uppercase tracking-wide text-content-faint";

type Kind = "none" | "script" | "jev" | "agent";

/**
 * The tripwire editor — a CONDITION the task waits for, beside the date it
 * waits for.
 *
 * Five columns on the task resolve together server-side (the status +
 * blocked_reason shape), so every control here sends exactly the field it
 * owns and lets `db::tasks::update` do the joining: a new spec re-arms, a
 * cleared spec takes the cadence and policy with it, a re-arm is the one
 * state write a client may make. Nothing here evaluates anything — "Check
 * now" stamps a request the runner honours on its next tick, and the outcome
 * arrives back through the live bridge like any other change.
 *
 * The script name is a free-text `Combobox` rather than a picker: the
 * list is what `tripwire_scripts` found on THIS Mac, and a name that isn't in
 * it is still a legal thing to type (a script you are about to write, or one
 * that lives on the machine whose runner will evaluate it). Drafts commit on
 * blur, and the whole field is always mounted (gotcha #24).
 */
export function TripwireField({
  task,
  runtime,
}: {
  task: Task;
  runtime?: TripwireRuntime;
}) {
  const patch = useTaskStore((s) => s.patch);
  const parsed = parseTripwireSpec(task.tripwire);
  const agentCondition = parseAgentTripwire(task.tripwire);
  const jevCondition = parseJevTripwire(task.tripwire);
  const [scripts, setScripts] = useState<string[]>([]);
  const [kind, setKind] = useState<Kind>(
    parsed ? "script" : jevCondition ? "jev" : agentCondition ? "agent" : "none",
  );
  const [name, setName] = useState(parsed?.name ?? "");
  const [arg, setArg] = useState(parsed?.arg ?? "");
  const [condition, setCondition] = useState(jevCondition ?? agentCondition ?? "");

  useEffect(() => {
    let live = true;
    api.tripwires
      .scripts()
      .then((s) => {
        if (live) setScripts(s);
      })
      .catch(() => {
        // No directory yet is the common case; the input still takes a name.
      });
    return () => {
      live = false;
    };
  }, []);

  // A live-refresh from the bridge can replace the task under us — the same
  // reset every other draft in the pane does.
  useEffect(() => {
    const p = parseTripwireSpec(task.tripwire);
    const c = parseAgentTripwire(task.tripwire);
    const j = parseJevTripwire(task.tripwire);
    setKind(p ? "script" : j ? "jev" : c ? "agent" : "none");
    setName(p?.name ?? "");
    setArg(p?.arg ?? "");
    setCondition(j ?? c ?? "");
  }, [task.id, task.updated_at, task.tripwire]);

  const commitSpec = (nextName: string, nextArg: string) => {
    const n = nextName.trim();
    const a = nextArg.trim();
    if (!n) return; // picked "Script" but not named it yet — nothing to write
    const spec = a ? `script:${n} ${a}` : `script:${n}`;
    if (spec !== task.tripwire) void patch(task.id, { tripwire: spec });
  };

  const commitCondition = (next: string) => {
    const c = next.trim();
    if (!c) return; // picked "Agent" but not written the condition yet
    const spec = `${kind === "jev" ? "jev" : "agent"}:${c}`;
    if (spec !== task.tripwire) void patch(task.id, { tripwire: spec });
  };

  const changeKind = (next: Kind) => {
    setKind(next);
    if (next === "none" && task.tripwire !== null) {
      void patch(task.id, { tripwire: null });
    }
  };

  // Watching, not merely armed: a DONE monitor is still checked, so "Check
  // now" belongs on it too.
  const armed = isTripwireWatching(task);
  const monitor = isMonitor(task);
  const deferred = isDeferred(task);
  const unchecked = tripwireUnchecked(task, runtime);
  const state = task.tripwire ? task.tripwire_state : null;

  return (
    <div className="flex flex-col gap-2 rounded-md border border-border-subtle bg-content/3 p-2">
      <div className="grid grid-cols-2 gap-3">
        <label className="flex flex-col gap-1">
          <span className={LABEL}>Tripwire</span>
          <Select
            value={kind}
            onValueChange={(v) => changeKind(v as Kind)}
            options={[
              { value: "none", label: "None" },
              { value: "script", label: "Script" },
              { value: "jev", label: "Jev" },
              { value: "agent", label: "Agent" },
            ]}
            aria-label="Tripwire kind"
          />
          <p className="text-[11px] leading-snug text-content-faint">
            {kind === "none"
              ? "Wait for a condition instead of a date."
              : kind === "agent"
                ? "A bounded read-only Claude run answers the question in the task's repo. Costs a run each time — prefer a script when a command can answer."
                : kind === "jev"
                  ? "Jev judges the condition from task text and recent comments. It cannot fetch new evidence."
                : "Hidden like a deferred task until the check comes back true."}
          </p>
        </label>

        {(kind === "agent" || kind === "jev") && (
          <label className="col-span-2 flex flex-col gap-1">
            <span className={LABEL}>Condition</span>
            <textarea
              value={condition}
              placeholder="the CHANGELOG mentions a 2.0 release"
              aria-label="Tripwire condition"
              rows={2}
              onChange={(e) => setCondition(e.target.value)}
              onBlur={() => commitCondition(condition)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  e.currentTarget.blur();
                }
              }}
              className={cn(FIELD, "resize-none text-[12px]")}
            />
          </label>
        )}

        {(kind === "script" || kind === "agent" || kind === "jev") && (
          <>
            {kind === "script" && (
            <label className="flex flex-col gap-1">
              <span className={LABEL}>Script</span>
              <Combobox
                value={name}
                placeholder="name in tripwires/"
                aria-label="Tripwire script"
                onValueChange={setName}
                options={scripts.map((s) => ({ value: s }))}
                panelClassName="font-mono text-[12px]"
                onBlur={() => commitSpec(name, arg)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
                className={cn(FIELD, "font-mono text-[12px]")}
              />
            </label>
            )}

            {kind === "script" && (
            <label className="flex flex-col gap-1">
              <span className={LABEL}>Argument ($1)</span>
              <input
                value={arg}
                placeholder="optional"
                aria-label="Tripwire argument"
                onChange={(e) => setArg(e.target.value)}
                onBlur={() => commitSpec(name, arg)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
                className={cn(FIELD, "font-mono text-[12px]")}
              />
            </label>
            )}

            {task.tripwire !== null && (
              <>
                <label className="flex flex-col gap-1">
                  <span className={LABEL}>Check</span>
                  <Select
                    value={task.tripwire_every_minutes ?? "manual"}
                    onValueChange={(v) =>
                      void patch(task.id, {
                        tripwire_every_minutes: v === "manual" ? null : Number(v),
                      })
                    }
                    // An agent check floors at hourly (each one is a
                    // background run); offering 5m here would be a choice the
                    // server refuses with nothing on screen to say so.
                    options={TRIPWIRE_CADENCES.filter(
                      (c) =>
                        kind === "agent"
                          ? c.value === null || c.value >= 60
                          : kind === "jev"
                            ? c.value === null || c.value >= 5
                            : true,
                    ).map((c) => ({
                      value: c.value ?? "manual",
                      label: c.label,
                    }))}
                    aria-label="Tripwire cadence"
                  />
                </label>

                <label className="flex flex-col gap-1">
                  <span className={LABEL}>On trip</span>
                  <Select
                    value={task.tripwire_on_trip ?? "surface"}
                    onValueChange={(v) =>
                      void patch(task.id, {
                        tripwire_on_trip:
                          v === "complete" || v === "monitor" ? v : null,
                      })
                    }
                    options={[
                      { value: "surface", label: "Surface it" },
                      { value: "complete", label: "Complete it" },
                      { value: "monitor", label: "Monitor (repeat)" },
                    ]}
                    aria-label="Tripwire on trip"
                  />
                  <p className="text-[11px] leading-snug text-content-faint">
                    {task.tripwire_on_trip === "complete"
                      ? "Marks it done at once and releases anything blocked by it — nobody reads the evidence first."
                      : monitor
                        ? "A standing check. It comes back every time the check fails; completing it means handled and re-arms it. Archive it to stop."
                        : "Back on the board with the evidence, for you to read."}
                  </p>
                </label>
              </>
            )}
          </>
        )}
      </div>

      {task.tripwire !== null && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] leading-snug">
          <TripwireStatus task={task} runtime={runtime} state={state} unchecked={unchecked} />
          <span className="ml-auto flex items-center gap-1">
            {armed && (
              <button
                type="button"
                onClick={() => void patch(task.id, { tripwire_check_requested: true })}
                disabled={deferred || Boolean(runtime?.requested_at)}
                title={
                  deferred
                    ? "Deferred — a tripwire isn't evaluated until its date passes"
                    : runtime?.requested_at
                      ? "Already requested; the runner picks it up on its next tick"
                      : "Ask the runner to evaluate it on its next tick"
                }
                className="flex items-center gap-1 rounded-md border border-border px-2 py-0.5 text-content hover:border-content-muted/50 disabled:opacity-50"
              >
                <Zap size={11} />
                Check now
              </button>
            )}
            {/* A monitor has no Re-arm: completing it re-arms it, and two
                ways to do one thing is how T107's confusion started. */}
            {(state === "tripped" || state === "error") && !monitor && (
              <button
                type="button"
                onClick={() => void patch(task.id, { tripwire_state: "armed" })}
                title="Hide the task again until the next trip"
                className="flex items-center gap-1 rounded-md border border-border px-2 py-0.5 text-content hover:border-content-muted/50"
              >
                <RefreshCw size={11} />
                Re-arm
              </button>
            )}
          </span>
        </div>
      )}
    </div>
  );
}

function TripwireStatus({
  task,
  runtime,
  state,
  unchecked,
}: {
  task: Task;
  runtime?: TripwireRuntime;
  state: Task["tripwire_state"];
  unchecked: boolean;
}) {
  const stateLabel =
    state === "tripped" ? (
      <span className="font-medium text-success">
        Tripped{task.tripwire_tripped_at ? ` ${formatAge(task.tripwire_tripped_at) ?? ""} ago` : ""}
      </span>
    ) : state === "error" ? (
      <span className="font-medium text-error">
        {isMonitor(task)
          ? "Check failed — fix the script, then complete it to re-arm"
          : "Check failed — fix the script and re-arm"}
      </span>
    ) : isMonitor(task) ? (
      <span className="flex items-center gap-1 font-medium text-accent" data-testid="monitor-state">
        <RefreshCw size={10} />
        Monitor · {formatCadence(task.tripwire_every_minutes)}
      </span>
    ) : (
      <span className="font-medium text-warning">
        Armed · {formatCadence(task.tripwire_every_minutes)}
      </span>
    );

  let last: string;
  if (runtime?.checked_at) {
    const outcome = tripwireOutcomeLabel(task, runtime.last_outcome);
    const note = runtime.last_note?.trim() ? ` — ${runtime.last_note.trim()}` : "";
    last = `Last checked ${formatAge(runtime.checked_at) ?? "?"} ago: ${outcome}${note}`;
  } else {
    last = "Never checked";
  }

  return (
    <>
      {stateLabel}
      <span className="text-content-faint" title={runtime?.last_note ?? undefined}>
        {last}
      </span>
      {runtime?.requested_at && (
        <span className="text-content-muted">Check requested — waiting for the runner</span>
      )}
      {unchecked && (
        <span className="font-medium text-error" data-testid="tripwire-unchecked">
          Unchecked for over three intervals — is the runner running?
        </span>
      )}
    </>
  );
}
