import { useState } from "react";
import { Loader2, Square } from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

/**
 * Stop an in-flight background run.
 *
 * **"Stopping…" is not a spinner waiting on a promise — the promise resolves
 * immediately.** Cancelling writes a flag the runner reads at its next 250ms
 * poll (see `paths::cancel_flag_path`), so the only thing the call confirms is
 * that the request was *accepted*, never that the process is dead. The label
 * therefore stays until the run's status actually changes and this unmounts,
 * which is the only honest signal that it worked.
 *
 * No confirm. The gesture is already deliberate, the run keeps whatever text it
 * had produced, and a write-enabled run has a checkpoint — so "Undo this run"
 * next door is the real undo, and a dialog here would only add friction to the
 * one control you reach for when something is going wrong.
 */
export function CancelRunButton({
  runId,
  className,
}: {
  runId: string;
  className?: string;
}) {
  const [state, setState] = useState<"idle" | "asked" | "gone">("idle");

  if (state === "gone") {
    // The runner said this run was already terminal. Saying so beats a button
    // that silently does nothing — the run finished between paint and click,
    // which is common on a short run and is not an error.
    return <span className="text-[10px] text-content-faint">already finished</span>;
  }

  return (
    <button
      type="button"
      disabled={state === "asked"}
      title="Stop this run. It keeps whatever it has already produced."
      onClick={(e) => {
        // The run row sits inside clickable cards on both surfaces; without
        // this, stopping a run also selects it.
        e.stopPropagation();
        setState("asked");
        void api.runs
          .cancel(runId)
          .then((accepted) => setState(accepted ? "asked" : "gone"))
          .catch(() => setState("idle"));
      }}
      className={cn(
        "flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium",
        "text-error hover:bg-error/15 disabled:opacity-60",
        className
      )}
    >
      {state === "asked" ? (
        <>
          <Loader2 size={9} className="animate-spin" />
          Stopping…
        </>
      ) : (
        <>
          <Square size={9} className="fill-current" />
          Stop
        </>
      )}
    </button>
  );
}
