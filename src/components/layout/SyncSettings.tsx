import { useCallback, useEffect, useState } from "react";
import { Check, Link2, Loader2, PauseCircle, PlayCircle, Unlink } from "lucide-react";
import { api, type ReconcileReport, type SyncStatus } from "@/lib/tauri";
import { cn } from "@/lib/utils";

/**
 * How often the panel re-reads its own status while it is open.
 *
 * The app reconciles on a timer now (`sync::auto`), so this pane is the one
 * place the result of a pass nobody triggered can be seen — and a pane that
 * only refreshed on mount would show a five-minute-old answer for as long as
 * you sat looking at it, which is the staleness this whole feature exists to
 * remove, one layer up. Matches `BudgetPanel`'s cadence.
 *
 * It re-reads `sync_status` only. It never reconciles: a panel that synced by
 * being looked at would make "open Settings" a hidden side effect.
 */
const STATUS_POLL_MS = 15_000;

/** "just now" · "4 minutes ago" · "3 days ago", or null for an unusable stamp. */
function ago(iso: string | null): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  const seconds = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (seconds < 60) return "just now";
  const units: [number, string][] = [
    [60, "minute"],
    [60, "hour"],
    [24, "day"],
  ];
  let value = seconds;
  let label = "second";
  for (const [step, name] of units) {
    if (value < step) break;
    value = Math.floor(value / step);
    label = name;
  }
  return `${value} ${label}${value === 1 ? "" : "s"} ago`;
}

/**
 * Sync, from the Mac's side.
 *
 * The interesting control here is the one that turns it OFF. A rollback that
 * needs a terminal is not a rollback you own, and the whole safety story for
 * pairing is that coming back is one toggle — so the toggle has to be somewhere
 * you can find it while the thing you are worried about is happening.
 *
 * Three states, and they need different words:
 *   never paired  a setup prompt, not a failure
 *   paused        a decision you made; the peer, the outbox and the lease
 *                 are all still here and resuming reconciles normally
 *   on            actually talking to a node
 */
export function SyncSettings() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.sync.status());
    } catch (e) {
      setNote({ kind: "err", text: String(e) });
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), STATUS_POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  /** Every action reports what happened and then re-reads the truth, rather
   *  than assuming its own optimistic version of it. */
  const run = async (label: string, fn: () => Promise<string | null>) => {
    setBusy(label);
    setNote(null);
    try {
      const message = await fn();
      if (message) setNote({ kind: "ok", text: message });
    } catch (e) {
      setNote({ kind: "err", text: String(e) });
    } finally {
      setBusy(null);
      await refresh();
    }
  };

  if (!status) {
    return <p className="text-[11px] text-content-faint">Checking…</p>;
  }

  const describe = (r: ReconcileReport): string => {
    if (r.held_back) {
      return `${r.failed.length} change(s) were refused, so nothing was pulled — staying behind is recoverable, being rolled back is not.`;
    }
    const moved = r.pulled.inserted_or_updated + r.pulled.deleted;
    // Images are counted separately from rows because they are a separate
    // channel with its own failure mode — one unreachable file is a missing
    // thumbnail, not a failed sync, and folding it into "received 12" would
    // make that indistinguishable from everything having arrived.
    const images = (r.blobs?.uploaded ?? 0) + (r.blobs?.downloaded ?? 0);
    const stranded = r.blobs?.failed.length ?? 0;
    if (r.pushed === 0 && moved === 0 && images === 0 && stranded === 0) {
      return "Already in step — nothing to send or fetch.";
    }
    const parts: string[] = [];
    if (r.pushed) parts.push(`sent ${r.pushed}`);
    if (moved) parts.push(`received ${moved}`);
    if (images) parts.push(`${images} image${images === 1 ? "" : "s"}`);
    let text = parts.length ? `${parts.join(", ")}.` : "";
    if (stranded) {
      text += `${text ? " " : ""}${stranded} image${stranded === 1 ? "" : "s"} could not be fetched — it will retry.`;
    }
    return text;
  };

  return (
    <div className="space-y-2.5">
      {/* ── the state, in a sentence ────────────────────────────────────── */}
      <div className="flex items-baseline justify-between">
        <span className="text-[12px] text-content">
          {status.enabled
            ? `Syncing with ${status.peer_id ?? "a node"}`
            : status.paired
              ? "Paused"
              : "Not paired"}
        </span>
        {status.pending > 0 && (
          <span className="text-[11px] text-warning">
            {status.pending} waiting to send
          </span>
        )}
      </div>

      {status.paired ? (
        <>
          <p className="text-[11px] leading-snug text-content-faint">
            {status.peer_url}
            {status.lease_remaining !== null && (
              <> · {status.lease_remaining} T-numbers reserved</>
            )}
          </p>

          {/*
            The automatic pass, said out loud.

            This line is the feature. Sync ran manual-only for months and the
            Mac fell four days behind with nothing erroring anywhere, because
            "up to date" and "nobody has pressed the button since Tuesday" look
            identical from outside. So the panel reports the attempt, not just
            the pull: an error line here means it is trying and failing, and a
            "never" means the loop is not running at all.
          */}
          <AutoLine status={status} />

          <div className="flex flex-wrap gap-1.5">
            <Action
              busy={busy === "sync"}
              disabled={!status.enabled}
              icon={<Check size={12} />}
              label="Sync now"
              onClick={() =>
                run("sync", async () => describe(await api.sync.now()))
              }
            />
            {status.enabled ? (
              <Action
                busy={busy === "toggle"}
                icon={<PauseCircle size={12} />}
                label="Pause"
                onClick={() =>
                  run("toggle", async () => {
                    await api.sync.setEnabled(false);
                    return "Paused. The node, the queue and the reserved T-numbers are all still here.";
                  })
                }
              />
            ) : (
              <Action
                busy={busy === "toggle"}
                icon={<PlayCircle size={12} />}
                label="Resume"
                onClick={() =>
                  run("toggle", async () => {
                    await api.sync.setEnabled(true);
                    return "Resumed.";
                  })
                }
              />
            )}
            <Action
              busy={busy === "unpair"}
              destructive
              icon={<Unlink size={12} />}
              label="Unpair"
              onClick={() =>
                run("unpair", async () => {
                  await api.sync.unpair(false);
                  return "Unpaired. This database is standalone again — nothing was removed from it.";
                })
              }
            />
          </div>

          <p className="text-[11px] leading-snug text-content-faint">
            Pausing keeps everything and can be undone at any time. Unpairing forgets the node;
            the tasks stay exactly where they are, because this Mac holds the real database
            rather than a copy of one.
          </p>
        </>
      ) : (
        <>
          <input
            className="w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content placeholder:text-content-faint"
            placeholder="cx.tailbff47a.ts.net:18870"
            spellCheck={false}
            autoCapitalize="off"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
          />
          <input
            className="w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content placeholder:text-content-faint"
            type="password"
            placeholder="Full token"
            spellCheck={false}
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
          <Action
            busy={busy === "pair"}
            disabled={!url.trim() || !token.trim()}
            icon={<Link2 size={12} />}
            label="Pair"
            onClick={() => run("pair", () => api.sync.pair(url, token))}
          />
          <p className="text-[11px] leading-snug text-content-faint">
            The <strong>full</strong> token here, not the board one — this end seeds the node and
            reserves T-number ranges, which a phone deliberately cannot do.
          </p>
        </>
      )}

      {note && (
        <p
          className={cn(
            "text-[11px] leading-snug",
            note.kind === "ok" ? "text-success" : "text-error"
          )}
        >
          {note.text}
        </p>
      )}
    </div>
  );
}

/** What the timer has been doing, in one line. */
function AutoLine({ status }: { status: SyncStatus }) {
  if (!status.enabled) {
    return (
      <p className="text-[11px] leading-snug text-content-faint">
        Paused, so nothing reconciles on its own. Resume to sync every{" "}
        {Math.round(status.auto_interval_seconds / 60)} minutes.
      </p>
    );
  }
  const attempted = ago(status.last_attempt_at);
  return (
    <p
      className={cn(
        "text-[11px] leading-snug",
        status.last_error ? "text-error" : "text-content-faint"
      )}
    >
      {attempted
        ? `Checked ${attempted}, then every ${Math.round(status.auto_interval_seconds / 60)} minutes.`
        : `Nothing has reconciled yet — the first automatic pass runs shortly, then every ${Math.round(status.auto_interval_seconds / 60)} minutes.`}
      {status.last_error && <> Last attempt failed: {status.last_error}</>}
    </p>
  );
}

function Action({
  busy,
  disabled,
  destructive,
  icon,
  label,
  onClick,
}: {
  busy: boolean;
  disabled?: boolean;
  destructive?: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={busy || disabled}
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md border px-2 py-1 text-[11px]",
        "transition-colors duration-75 disabled:opacity-40",
        destructive
          ? "border-error/40 text-error hover:bg-error/10"
          : "border-border text-content hover:bg-content/10"
      )}
    >
      {busy ? <Loader2 size={12} className="animate-spin" /> : icon}
      {label}
    </button>
  );
}
