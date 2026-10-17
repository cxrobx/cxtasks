import { useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { Check, Copy } from "lucide-react";
import { cn } from "@/lib/utils";

/** How long the chip reports the copy before going quiet again. */
const COPIED_MS = 1200;

/**
 * A task's speakable reference — and the button that copies it.
 *
 * The T-number's whole value is that it travels: into a commit message, a
 * comment, a message to someone else, the next session's prompt. Printing it
 * as inert text makes the user retype an address the app is already holding,
 * which is the one thing a permanent identifier should never cost. So the
 * chip IS the copy control, everywhere it appears.
 *
 * Feedback is a colour flash rather than a text swap on purpose: "T86" and
 * "Copied" are different widths, and in a list row that shift shoves the title
 * sideways every time you click one. The label stays put; the chip goes green.
 *
 * The click never reaches whatever the chip is sitting on. In the list the row
 * underneath is the selection control, and copying an address is not a click
 * on the task — without this, copying a T-number out of a six-row selection
 * would silently collapse it to one.
 *
 * Callers own the look (`className`) and get `data-copied` to key off, since
 * the same chip is a bordered token in the list and bare text in the detail
 * pane. It renders a real `<button>`, so it cannot be nested inside a card
 * that is itself a button — see `MissionControl`'s Stop control for the
 * sibling-wrapper shape that problem needs.
 */
export function ReferenceChip({
  reference,
  className,
  icon = false,
}: {
  reference: string;
  className?: string;
  /** Show a copy/check glyph beside the reference. */
  icon?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      data-copied={copied ? "" : undefined}
      title={`Copy ${reference}`}
      aria-label={copied ? `Copied ${reference}` : `Copy ${reference}`}
      onClick={(e) => {
        e.stopPropagation();
        void writeText(reference).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), COPIED_MS);
        });
      }}
      className={cn(
        "inline-flex shrink-0 items-center gap-1 font-mono tabular-nums transition-colors duration-75",
        copied ? "text-success" : "text-content-faint hover:text-content",
        className
      )}
    >
      {reference}
      {icon && (copied ? <Check size={11} /> : <Copy size={11} />)}
    </button>
  );
}
