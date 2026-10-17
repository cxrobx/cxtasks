/**
 * Which machines may run tasks, and what each of them is allowed to do.
 *
 * # What this pane is for
 *
 * Reading, mostly. The question it answers ninety times out of a hundred is
 * "why has nothing run" — and the three answers are switched off, asleep, and
 * capped below what the task asks for. So each row says which of those it is,
 * in those words, before offering anything to change.
 *
 * # What it deliberately does not offer
 *
 * There is no control for `executors_ready`, `last_seen_at` or `gate`. Those
 * are OBSERVED — what a host reported after looking at itself — and a settable
 * "Claude is installed here" checkbox would be a lie waiting to be believed by
 * the router, which would then send work to a machine that cannot run it.
 *
 * There is also no "add a host" button. A host row means "a machine that runs
 * tasks", and it appears by that machine's runner starting up. Anything a
 * person could type here would be a machine that might not exist.
 */
import { useCallback, useEffect, useState } from "react";

import { api, type HostEntry, type ToolTier } from "@/lib/tauri";

const TIERS: ToolTier[] = ["read-only", "shell", "write"];

/** Why this host is not going to run anything, in the order the user should
 *  check them — configuration before liveness, because a disabled host that is
 *  also asleep is a configuration problem. */
function idleReason(h: HostEntry): string | null {
  if (!h.capability.enabled) return "switched off";
  if (h.capability.paused) return "paused";
  if (!h.alive) return "not reporting in";
  if (h.capability.executors_ready.length === 0) {
    return h.capability.last_seen_at ? "no agent installed" : "has not checked in yet";
  }
  return null;
}

export function HostSettings() {
  const [hosts, setHosts] = useState<HostEntry[] | null>(null);
  const [note, setNote] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setHosts(await api.hosts.list());
    } catch (e) {
      setNote({ kind: "err", text: String(e) });
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** Every action re-reads the truth afterwards rather than trusting its own
   *  optimistic version of it — the same rule the sync panel follows, and for
   *  the same reason: what is on screen here is used to decide whether
   *  something is broken. */
  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(label);
    setNote(null);
    try {
      await fn();
    } catch (e) {
      setNote({ kind: "err", text: String(e) });
    } finally {
      setBusy(null);
      await refresh();
    }
  };

  if (!hosts) return <p className="text-[11px] text-content-faint">Checking…</p>;

  if (hosts.length === 0) {
    return (
      <p className="text-[11px] text-content-faint">
        No hosts have registered. A machine appears here when its background runner starts —
        there is nothing to add by hand.
      </p>
    );
  }

  return (
    <div className="space-y-2.5">
      {hosts.map((h) => {
        const c = h.capability;
        const idle = idleReason(h);
        return (
          <div key={c.id} className="rounded border border-border px-2.5 py-2">
            <div className="flex items-baseline gap-2">
              <span className="text-[12px] font-medium text-content">{c.label}</span>
              {h.is_this_host && (
                <span className="text-[10px] text-content-faint">this Mac</span>
              )}
              <span className="ml-auto text-[10px] text-content-faint">
                {idle ? idle : "ready"}
              </span>
            </div>

            <p className="mt-0.5 text-[11px] text-content-muted">
              {c.max_tier} · {c.workspaces.join(" + ")}
              {c.max_concurrent > 0 && ` · ${c.max_concurrent} at a time`}
              {c.headroom_pct > 0 && ` · leaves ${c.headroom_pct}% of the budget`}
            </p>

            {/* The mirror set, because "no mirror of this repo yet" is the most
                common reason a remote host is refused, and it is invisible
                otherwise. */}
            {!c.workspaces.includes("live") && (
              <p className="mt-0.5 text-[11px] text-content-faint">
                {h.mirrors.length === 0
                  ? "no repos mirrored here yet"
                  : `${h.mirrors.length} repo${h.mirrors.length === 1 ? "" : "s"} mirrored`}
                {h.mirror_remote ? "" : " · no mirror destination set, so none will be"}
              </p>
            )}

            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              <label className="text-[11px] text-content-muted">
                Ceiling{" "}
                <select
                  className="rounded border border-border bg-surface px-1 py-0.5 text-[11px]"
                  value={c.max_tier}
                  disabled={busy !== null}
                  onChange={(e) =>
                    void run("tier", () =>
                      api.hosts.setPolicy(c.id, { max_tier: e.target.value as ToolTier })
                    )
                  }
                >
                  {TIERS.map((t) => (
                    <option key={t} value={t}>
                      {t}
                    </option>
                  ))}
                </select>
              </label>

              <button
                type="button"
                className="rounded border border-border px-1.5 py-0.5 text-[11px]"
                disabled={busy !== null}
                onClick={() =>
                  void run("enabled", () =>
                    api.hosts.setPolicy(c.id, { enabled: !c.enabled })
                  )
                }
              >
                {c.enabled ? "Switch off" : "Switch on"}
              </button>

              {/* Removing the machine you are sitting at would take the local
                  runner out of its own registry, and it would put itself back
                  on the next tick — a button that appears to do something and
                  undoes itself is worse than one that is not there. */}
              {!h.is_this_host && (
                <button
                  type="button"
                  className="rounded border border-border px-1.5 py-0.5 text-[11px] text-content-faint"
                  disabled={busy !== null}
                  onClick={() => void run("remove", () => api.hosts.remove(c.id).then(() => {}))}
                >
                  Forget
                </button>
              )}
            </div>
          </div>
        );
      })}

      {note && (
        <p
          className={
            note.kind === "err" ? "text-[11px] text-danger" : "text-[11px] text-content-muted"
          }
        >
          {note.text}
        </p>
      )}
    </div>
  );
}
