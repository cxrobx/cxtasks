import { useLayoutEffect, useRef } from "react";
import { Markdown } from "@/components/shared/Markdown";
import { splitRunCompletion } from "@/lib/runCompletion";

/**
 * The tall-by-default cap this box opens at, in px — what `max-h-56` was.
 * A handoff is several thousand characters, so an uncapped run turns the
 * detail pane into one long scroll with the comment log pushed off the end.
 */
const DEFAULT_HEIGHT = 224;

/** Below this a dragged-shut box has no grip left to drag back open. */
const MIN_HEIGHT = 40;

/**
 * A run's handoff, in a box with the same drag-to-resize grip the Notes field
 * has. Handoffs vary from two lines to a full implementation brief, so one
 * fixed cap is always wrong for somebody.
 *
 * **The height is PINNED on mount rather than capped with `max-height`**, and
 * the two cannot coexist: the UA resizer works by writing an inline `height`,
 * which `max-height` then clamps — so a capped box grows by nothing however
 * far you drag it, and the grip reads as broken rather than as constrained.
 * Pinning applies the cap only to a result that would actually overrun it; a
 * two-line handoff still renders as two lines, exactly as `max-h-56` did.
 *
 * The pin is skipped once the element carries an inline height, because that
 * is the resizer's own mark — re-running it would snap a box the user dragged
 * taller back to the default on the next re-render, and the runs list
 * re-renders on every live refresh while a background run is streaming.
 */
export function RunResult({ children }: { children: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const { text, completion } = splitRunCompletion(children);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || el.style.height) return;
    if (el.scrollHeight > DEFAULT_HEIGHT) el.style.height = `${DEFAULT_HEIGHT}px`;
  }, [children]);

  return (
    <div
      ref={ref}
      // Opts out of the app's custom scrollbars, which would otherwise swap
      // the macOS grip for WebKit's own boxed one — see the scrollbar block in
      // globals.css. Every other resizable surface is a textarea and is
      // covered there by element name; this is the one div.
      data-resizable
      style={{ minHeight: MIN_HEIGHT }}
      className="mt-1.5 resize-y overflow-y-auto"
    >
      {completion && completion.remaining.length > 0 && (
        <div className="mb-2 rounded border border-warning/20 bg-warning/5 px-2 py-1.5 text-[11px]">
          <p className="font-medium text-warning">Still needed</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-4 text-content-secondary">
            {completion.remaining.map((item, index) => <li key={index}>{item}</li>)}
          </ul>
        </div>
      )}
      <Markdown className="text-[11px]">{text}</Markdown>
      {completion && completion.verified.length > 0 && (
        <details className="mt-2 text-[11px] text-content-secondary">
          <summary className="cursor-pointer text-content-muted">Evidence reported</summary>
          <ul className="mt-1 list-disc space-y-0.5 pl-4">
            {completion.verified.map((item, index) => <li key={index}>{item}</li>)}
          </ul>
        </details>
      )}
    </div>
  );
}
