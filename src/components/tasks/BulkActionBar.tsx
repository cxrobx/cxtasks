import { confirm } from "@tauri-apps/plugin-dialog";
import { Check, Trash2, Undo2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { PRIORITY_COLORS } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import { PRIORITY_LOWEST } from "@/types/task";

/**
 * The bulk bar for the multi-select set.
 *
 * Renders nothing until something is ticked, so the list is unchanged for the
 * (overwhelmingly common) single-task case. It sits at the BOTTOM of the list
 * column as a `shrink-0` sibling of the scroller rather than floating over it:
 * a bar that covers rows hides exactly the thing you're deciding about, and one
 * that scrolls is gone the moment you scroll up to tick a seventh row.
 *
 * Every action here goes through the store's `bulk*` methods, which loop the
 * same per-task commands the single-task paths use — see `runOnMany`.
 */
export function BulkActionBar() {
  const checkedIds = useTaskStore((s) => s.checkedIds);
  const view = useTaskStore((s) => s.view);
  const clearChecked = useTaskStore((s) => s.clearChecked);
  const bulkComplete = useTaskStore((s) => s.bulkComplete);
  const bulkPriority = useTaskStore((s) => s.bulkPriority);
  const bulkTrash = useTaskStore((s) => s.bulkTrash);
  const bulkRestore = useTaskStore((s) => s.bulkRestore);
  const bulkDestroy = useTaskStore((s) => s.bulkDestroy);

  const n = checkedIds.length;
  if (n === 0) return null;

  const hidden = view === "trash" || view === "archive";
  const count = `${n} selected`;

  return (
    <div
      role="toolbar"
      aria-label="Bulk actions"
      className="flex shrink-0 items-center gap-1 border-t border-border-subtle bg-surface px-3 py-1.5"
    >
      <span className="shrink-0 tabular-nums font-medium text-content">
        {count}
      </span>

      <div className="ml-2 flex items-center gap-1">
        {hidden ? (
          <>
            <BulkButton
              icon={<Undo2 size={12} />}
              label="Put back"
              onClick={() => void bulkRestore()}
            />
            {view === "trash" && (
              <BulkButton
                icon={<Trash2 size={12} />}
                label="Delete permanently"
                danger
                onClick={async () => {
                  // Same bargain as the single-task destroy: this is the one
                  // action with no way back, so it is the one that earns an
                  // interruption. The count is in the prompt because "delete
                  // the selected tasks" is precisely the sentence that hides a
                  // miscount.
                  const ok = await confirm(
                    `Permanently delete ${n} task${n === 1 ? "" : "s"}? This cannot be undone.`,
                    { title: "Delete Tasks", kind: "warning", okLabel: "Delete" }
                  );
                  if (ok) await bulkDestroy();
                }}
              />
            )}
          </>
        ) : (
          <>
            <BulkButton
              icon={<Check size={12} strokeWidth={3} />}
              label="Done"
              onClick={() => void bulkComplete()}
            />

            {/* Four flat buttons rather than a dropdown: the whole scale fits
                in the space a menu trigger would take, and showing it inline
                keeps the colours — which ARE the scale — visible instead of
                one click away. P0 is highest (invariant 9). */}
            <span className="ml-1 mr-0.5 text-content-faint">Priority</span>
            {Array.from({ length: PRIORITY_LOWEST + 1 }, (_, p) => (
              <button
                key={p}
                type="button"
                aria-label={`Set priority P${p}`}
                onClick={() => void bulkPriority(p)}
                className={cn(
                  "rounded px-1.5 py-0.5 font-semibold tabular-nums transition-colors duration-75 hover:bg-content/10",
                  PRIORITY_COLORS[p]
                )}
              >
                P{p}
              </button>
            ))}

            <BulkButton
              icon={<Trash2 size={12} />}
              label="Move to Trash"
              className="ml-1"
              onClick={() => void bulkTrash()}
            />
          </>
        )}
      </div>

      <button
        type="button"
        onClick={clearChecked}
        title="Clear selection (Esc)"
        className="ml-auto flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-content-faint transition-colors duration-75 hover:bg-content/10 hover:text-content"
      >
        <X size={12} />
        Clear
      </button>
    </div>
  );
}

function BulkButton({
  icon,
  label,
  onClick,
  danger,
  className,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  danger?: boolean;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex items-center gap-1 rounded px-1.5 py-0.5 transition-colors duration-75",
        danger
          ? "text-content-secondary hover:bg-error/15 hover:text-error"
          : "text-content-secondary hover:bg-content/10 hover:text-content",
        className
      )}
    >
      {icon}
      {label}
    </button>
  );
}
