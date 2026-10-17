import { Undo2, X } from "lucide-react";
import { useTaskStore } from "@/stores/taskStore";

/**
 * The undo affordance.
 *
 * Two things share this slot, and the order between them matters. The **offer**
 * comes up for eight seconds after any reversible action and names what
 * happened; the **confirmation** replaces it for a moment after a ⌘Z, because
 * an undo fired from the keyboard long after the offer expired would otherwise
 * produce a silent change to a list you may not even be looking at.
 *
 * The offer expiring does NOT expire the undo — the stack behind it keeps every
 * recent action, and ⌘Z still reaches them. That split is the point: the toast
 * is how you notice a mis-click in the two seconds after it, and the stack is
 * what saves you when you notice a minute later. Tying the two together, which
 * is what this used to do, means the only recovery past eight seconds was
 * finding the row in the Trash — and only a delete was recoverable at all.
 *
 * Deliberately not a confirm dialog: interrupting every action to ask "are you
 * sure?" trains you to dismiss the question, and stops being a safety mechanism
 * the moment it becomes reflex. Acting immediately and offering a cheap
 * reversal is the trade that holds up.
 */
export function UndoToast() {
  const entry = useTaskStore((s) =>
    s.undoToastKey === null
      ? null
      : (s.undoStack.find((e) => e.key === s.undoToastKey) ?? null)
  );
  const notice = useTaskStore((s) => s.undoNotice);
  const undoLast = useTaskStore((s) => s.undoLast);
  const dismissUndo = useTaskStore((s) => s.dismissUndo);

  // The confirmation wins while it's up: it reports the more recent event, and
  // the action it would otherwise be offering has just been consumed.
  if (!entry && !notice) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-none fixed inset-x-0 bottom-5 z-50 flex justify-center px-4"
    >
      <div className="pointer-events-auto flex max-w-md items-center gap-3 rounded-lg border border-border bg-surface/95 py-2 pl-3.5 pr-2 shadow-lg backdrop-blur-md">
        {notice ? (
          <span className="min-w-0 truncate px-1 text-content-secondary">
            {notice}
          </span>
        ) : (
          <>
            <span className="min-w-0 truncate text-content-secondary">
              <span className="font-medium text-content">{entry!.label}</span>{" "}
              {entry!.verb}
            </span>
            <button
              type="button"
              onClick={() => void undoLast()}
              className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1 font-medium text-accent transition-colors duration-75 hover:bg-accent/10"
            >
              <Undo2 size={12} />
              Undo
              <span className="text-content-faint">⌘Z</span>
            </button>
            <button
              type="button"
              aria-label="Dismiss"
              onClick={dismissUndo}
              className="shrink-0 rounded-md p-1 text-content-faint transition-colors duration-75 hover:bg-content/10 hover:text-content"
            >
              <X size={12} />
            </button>
          </>
        )}
      </div>
    </div>
  );
}
