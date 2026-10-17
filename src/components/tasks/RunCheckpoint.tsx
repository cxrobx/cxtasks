import { useState } from "react";
import { FileDiff, Loader2, Undo2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import type { TaskRun } from "@/types/task";

/**
 * Review / undo controls for a write-enabled background run.
 *
 * Only shown when the run captured a checkpoint (`scratch_dir` holds the ref).
 * The diff is against the snapshot taken immediately before the agent started,
 * so it is exactly what the agent did — even if the repo already had
 * uncommitted work when the run began.
 */
export function RunCheckpoint({ run }: { run: TaskRun }) {
  const refresh = useTaskStore((s) => s.refresh);
  const [diff, setDiff] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | "diff" | "restore">(null);
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const hasCheckpoint = run.scratch_dir?.startsWith("refs/cxtasks/");
  if (!hasCheckpoint) return null;

  const showDiff = async () => {
    setBusy("diff");
    try {
      const d = await api.checkpoint.diff(run.id);
      setDiff(d.trim() || "No changes since the checkpoint.");
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(null);
    }
  };

  const restore = async () => {
    setBusy("restore");
    try {
      setNote(await api.checkpoint.restore(run.id));
      setDiff(null);
      setConfirming(false);
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(null);
      void refresh();
    }
  };

  return (
    <div className="mt-1.5 flex flex-col gap-1.5 border-t border-border-subtle pt-1.5">
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={busy !== null}
          onClick={() => void showDiff()}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-content-muted hover:bg-content/8 hover:text-content"
        >
          {busy === "diff" ? (
            <Loader2 size={11} className="animate-spin" />
          ) : (
            <FileDiff size={11} />
          )}
          What changed
        </button>

        {confirming ? (
          <span className="flex items-center gap-1.5 text-[11px]">
            <span className="text-warning">Discard everything since the run?</span>
            <button
              type="button"
              disabled={busy !== null}
              onClick={() => void restore()}
              className="rounded bg-error/20 px-1.5 py-0.5 font-medium text-error hover:bg-error/30"
            >
              {busy === "restore" ? "Restoring…" : "Yes, undo"}
            </button>
            <button
              type="button"
              onClick={() => setConfirming(false)}
              className="rounded px-1.5 py-0.5 text-content-faint hover:text-content"
            >
              Cancel
            </button>
          </span>
        ) : (
          <button
            type="button"
            disabled={busy !== null}
            onClick={() => setConfirming(true)}
            title="Roll the repo back to the snapshot taken before this run"
            className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-content-muted hover:bg-error/10 hover:text-error"
          >
            <Undo2 size={11} />
            Undo this run
          </button>
        )}
      </div>

      {note && <p className="text-[11px] text-content-muted">{note}</p>}
      {diff && (
        <pre className={cn(
          "max-h-48 overflow-auto whitespace-pre-wrap rounded bg-base/60 p-1.5",
          "font-mono text-[10px] leading-relaxed text-content-secondary"
        )}>
          {diff}
        </pre>
      )}
    </div>
  );
}
