import { useLayoutEffect, useRef, useState } from "react";
import { Markdown } from "@/components/shared/Markdown";
import { cn } from "@/lib/utils";
import { useImagePaste } from "@/hooks/useImagePaste";

/**
 * The FLOOR a long note opens at, in px. Notes arrive from `file_task` and
 * `/cxtasks:breakdown` as whole briefs — the one in the screenshot that
 * prompted this is a page of prose and a GFM table — so an uncapped box turns
 * the detail pane into one long scroll with Subtasks and the comment log pushed
 * off the end. A long note opens at `openingHeight` — down to the bottom of the
 * visible pane — and this is only what it never goes below.
 */
const DEFAULT_HEIGHT = 260;

/** What is left visible under an opened note: the "Add images" line, so the
 *  pane still shows there is more below the box. */
const ROOM_BELOW = 48;

/**
 * The height a long note opens at: down to the bottom of the visible pane.
 *
 * Since the property strip replaced the field grid, Notes starts right under
 * the title, and a fixed 260px left most of a laptop window empty beneath a
 * note that scrolled inside its box. Filling the space that is actually there
 * — measured, not a bigger constant — suits every window height: a larger
 * constant overshoots a short window and still undershoots a tall one.
 * Measured against the nearest scrolling ancestor, so it is the PANE's bottom
 * edge, not the window's. With no scroller above it there is no pane to fill,
 * and it opens at the floor.
 */
function openingHeight(el: HTMLElement): number {
  let scroller: HTMLElement | null = el.parentElement;
  while (scroller && !/(auto|scroll)/.test(getComputedStyle(scroller).overflowY)) {
    scroller = scroller.parentElement;
  }
  if (!scroller) return DEFAULT_HEIGHT;
  const room = Math.floor(
    scroller.getBoundingClientRect().bottom - el.getBoundingClientRect().top - ROOM_BELOW
  );
  return Math.max(DEFAULT_HEIGHT, room);
}

/** Roughly the five rows the plain textarea used to open at. */
const MIN_HEIGHT = 96;

/**
 * Notes, rendered as markdown until you click into it.
 *
 * Notes is the task's *definition*, and most of them are written by an agent in
 * markdown, so the field spent its life printing `##` and pipe-tables as
 * literal syntax. Rendering goes through the app's single `Markdown` surface
 * (no `rehype-raw`, links hand off to the OS) — a second renderer here is how
 * the same string starts looking different in two panes.
 *
 * **The WRAPPER owns the height, not the textarea.** Both modes render inside
 * one element that stays mounted across the swap, so the grip you drag in
 * preview is the same grip, at the same height, once you are typing. Giving
 * each mode its own resizable box instead means every click into the note
 * resets a height you deliberately set. The height is *pinned* rather than
 * capped for the reason spelled out in `RunResult`: the UA resizer works by
 * writing an inline `height`, which a `max-height` then clamps, so a capped box
 * grows by nothing however far you drag and the grip reads as broken.
 *
 * **The draft is committed by `leave()`, not by the textarea's `onBlur`**
 * (gotcha #24). Blur happens to fire on the ordinary click-away, but the
 * textarea also unmounts when Escape is pressed or the pane switches tasks, and
 * a focused element that is removed from the DOM does not reliably fire blur.
 * The commit would be skipped, `updated_at` would bump on some later write, and
 * the draft-reset effect in `TaskDetailForm` would erase what you typed —
 * minutes after the fact, looking unrelated. Every path out of edit mode goes
 * through `leave()`.
 */
export function NotesField({
  value,
  stored,
  onChange,
  onCommit,
  onPasteImage,
}: {
  /** The live draft — owned by `TaskDetailForm` alongside every other field. */
  value: string;
  /** What is actually on the task, so `leave()` can no-op when nothing changed. */
  stored: string;
  onChange: (next: string) => void;
  onCommit: (next: string) => void;
  onPasteImage?: (quiet?: boolean) => void | Promise<unknown>;
}) {
  const imagePaste = useImagePaste(onPasteImage);
  const [editing, setEditing] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  /**
   * Where the pointer went down, so a resize DRAG that happens to end inside
   * the preview isn't mistaken for a click into the editor. Letting go of the
   * grip fires a click on whatever is under the cursor, and the grip sits in
   * the box's own bottom-right corner — so without this, every resize opens the
   * editor and throws away the render you were resizing to read.
   */
  const pressedAt = useRef<{ x: number; y: number } | null>(null);

  useLayoutEffect(() => {
    const el = boxRef.current;
    if (!el || el.style.height) return;
    const opening = openingHeight(el);
    if (el.scrollHeight > opening) el.style.height = `${opening}px`;
    // Only ever pins a box the user has not sized themselves — an inline height
    // is the resizer's own mark, and re-running would snap a dragged box back
    // on the next live refresh.
  }, [value, editing]);

  const enter = () => {
    const el = boxRef.current;
    // Pin the height the preview is currently at before the textarea replaces
    // it. `h-full` needs a definite parent height to resolve against, and
    // measuring after the swap would mean measuring the collapsed box.
    // `offsetHeight` is 0 for a box that isn't laid out yet; pinning that
    // would collapse the field to nothing the moment it is clicked.
    if (el && !el.style.height && el.offsetHeight > 0) {
      el.style.height = `${el.offsetHeight}px`;
    }
    setEditing(true);
  };

  const leave = () => {
    setEditing(false);
    if (value !== stored) onCommit(value);
  };

  return (
    <div
      ref={boxRef}
      onPaste={imagePaste.onPaste}
      onKeyDown={imagePaste.onKeyDown}
      data-image-paste
      tabIndex={editing ? undefined : 0}
      // Opts out of the app's custom scrollbars, which would swap the macOS
      // grip for WebKit's boxed one — see the scrollbar block in globals.css.
      // The rule there covers `textarea` by name; this is a div.
      data-resizable
      style={{ minHeight: MIN_HEIGHT }}
      onPointerDown={(e) => (pressedAt.current = { x: e.clientX, y: e.clientY })}
      onClick={(e) => {
        if (editing) return;
        const from = pressedAt.current;
        pressedAt.current = null;
        // A drag, not a click. 4px is generous enough to survive the hand
        // wobble in a deliberate tap and far below any real resize.
        if (from && Math.hypot(e.clientX - from.x, e.clientY - from.y) > 4) return;
        // Links inside the render open in the OS browser; clicking one must not
        // also drop the render for a textarea.
        if ((e.target as HTMLElement).closest("a")) return;
        enter();
      }}
      className={cn(
        // `cx-notes` moves the focus ring to this box and takes it off the
        // textarea — see the rule in globals.css, and note that doing it with
        // a utility class instead does not work.
        "cx-notes w-full resize-y overflow-auto rounded-md border border-border bg-input px-2 py-1",
        !editing && "cursor-text"
      )}
    >
      {editing ? (
        <textarea
          ref={textareaRef}
          autoFocus
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onBlur={leave}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              // **Escape COMMITS, it does not discard** — deliberately unlike
              // the blocked-reason field, where Escape abandons a staged
              // status. Notes is a page of prose, not a one-line staging
              // decision, and there is no discard on the mouse path to be
              // consistent with: clicking away saves. A keystroke that threw
              // away a note because it usually means "cancel" is gotcha #16's
              // lesson in a new field.
              //
              // Blurring rather than calling `leave()` directly keeps one exit
              // path: the blur handler runs it, and focus lands somewhere sane
              // instead of being orphaned on an unmounting element.
              textareaRef.current?.blur();
            }
          }}
          // The wrapper is the resizable one — a second grip inside it reads as
          // a different control and drags a box that isn't the visible one.
          className="h-full w-full resize-none bg-transparent leading-relaxed text-content placeholder:text-content-faint"
          placeholder="Notes — markdown renders when you click away"
        />
      ) : value.trim() ? (
        // `reflow` + `breaks` are Notes-only: a person types here as often as
        // an agent does, and a long unbroken paragraph or a note with hand-
        // pressed Returns should both read as the author meant, not as one
        // block. See the props on `Markdown`.
        <Markdown className="px-1 py-1 text-content" reflow breaks>
          {value}
        </Markdown>
      ) : (
        <span className="text-content-faint">Add notes…</span>
      )}
    </div>
  );
}
