import { useEffect, type RefObject } from "react";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import { useImagePaste } from "@/hooks/useImagePaste";

/** True when focus is in a field, so a bare `j` types a `j` instead of moving. */
function isTyping(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  const tag = el.tagName;
  return (
    tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable
  );
}

export function useKeyboardShortcuts(
  quickAddRef: RefObject<HTMLInputElement | HTMLTextAreaElement | null>,
  searchRef: RefObject<HTMLInputElement | null>
) {
  const imagePaste = useImagePaste((quiet) => {
    const store = useTaskStore.getState();
    const task = store.tasks.find((task) => task.id === store.selectedId);
    if (task && !task.deleted_at && !task.archived_at) return store.pasteNoteImage(task.id, quiet);
  });

  useEffect(() => {
    const eligible = (target: EventTarget | null) => !isTyping(target) &&
      !(target as HTMLElement | null)?.closest?.("[data-image-paste], [role='dialog'], [role='alertdialog']") &&
      !useUiStore.getState().settingsOpen;
    const onKeyDown = (event: KeyboardEvent) => {
      if (eligible(event.target)) imagePaste.onKeyDown(event);
    };
    const onPaste = (event: ClipboardEvent) => {
      if (eligible(event.target)) imagePaste.onPaste(event);
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("paste", onPaste);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("paste", onPaste);
    };
  }, [imagePaste.onKeyDown, imagePaste.onPaste]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const store = useTaskStore.getState();
      const typing = isTyping(e.target);

      // ⌘N / ⌘F work from anywhere, including inside a field.
      if (e.metaKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        quickAddRef.current?.focus();
        return;
      }
      if (e.metaKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }
      // ⌘\ hides/shows the detail pane.
      if (e.metaKey && e.key === "\\") {
        e.preventDefault();
        useUiStore.getState().toggleDetail();
        return;
      }
      if (typing) return;

      // ⌘⌫ = Move to Trash, matching Finder.
      //
      // This sits BELOW the `typing` guard on purpose, and moving it back above
      // re-introduces a genuinely destructive bug: in any macOS text field ⌘⌫
      // means "delete to the start of the line", so editing a title, a note or
      // a comment and clearing a line would destroy the task instead. The
      // previous version ran before this guard and advertised the fact
      // ("deletes the selected task even while typing elsewhere") — it cost a
      // real task before it was caught.
      //
      // Recoverable now in two ways regardless: ⌘Z, and the Trash for 30 days.
      if (e.metaKey && e.key === "Backspace") {
        // The checked set wins over the focused row, exactly as it does for
        // Enter below — once six rows are ticked, "delete" can only mean those
        // six, and quietly trashing the one that happens to hold focus would be
        // both surprising and hard to notice.
        if (store.checkedIds.length > 0) {
          e.preventDefault();
          void store.bulkTrash();
          return;
        }
        if (store.selectedId) {
          e.preventDefault();
          void store.trash(store.selectedId);
          return;
        }
      }

      // ⌘Z walks back through recent actions — completing a task, a priority
      // change, a trash, a drag onto another project. NOT limited to the toast:
      // the toast is how you catch a mis-click immediately, this is how you fix
      // one you noticed a minute later.
      //
      // Still below the `typing` guard, so ⌘Z inside a text field stays the
      // field's own undo. Taking that over would mean a mistyped character
      // reverting a whole task instead of a keystroke.
      if (e.metaKey && e.key.toLowerCase() === "z") {
        if (store.undoStack.length === 0) return;
        e.preventDefault();
        void store.undoLast();
        return;
      }

      const { tasks, selectedId } = store;
      const index = tasks.findIndex((t) => t.id === selectedId);

      switch (e.key) {
        case "j":
        case "ArrowDown": {
          e.preventDefault();
          const next = tasks[Math.min(index + 1, tasks.length - 1)];
          if (next) void store.select(next.id);
          break;
        }
        case "k":
        case "ArrowUp": {
          e.preventDefault();
          const prev = tasks[Math.max(index - 1, 0)];
          if (prev) void store.select(prev.id);
          break;
        }
        // `x` ticks the selected row's checkbox — the Gmail/Superhuman verb for
        // exactly this, and the only way to build a selection without a mouse.
        case "x": {
          if (!selectedId) break;
          e.preventDefault();
          store.toggleChecked(selectedId);
          break;
        }
        // Enter only — Space used to complete too, and closed tasks by accident:
        // it is the key a stray press is most likely to be, and completing is
        // the one verb here that makes a row leave the list.
        case "Enter": {
          // A checked set wins over the focused row. Once you've ticked six
          // things, "done" can only sensibly mean those six — completing the
          // one row that happens to hold focus instead would be both surprising
          // and silent.
          if (store.checkedIds.length > 0) {
            e.preventDefault();
            void store.bulkComplete();
            break;
          }
          if (!selectedId) break;
          e.preventDefault();
          void store.complete(selectedId);
          break;
        }
        case "0":
        case "1":
        case "2":
        case "3": {
          if (!selectedId) break;
          e.preventDefault();
          void store.patch(selectedId, { priority: Number(e.key) });
          break;
        }
        case "Escape": {
          // Selection first, search second. Escape means "back out of the
          // thing I'm in the middle of", and a live selection is more recent
          // and more consequential than a filter — clearing the query would
          // also reshuffle the list under a set you were still building.
          if (store.checkedIds.length > 0) {
            store.clearChecked();
            break;
          }
          // Text and chips go together: they are one filter state, and
          // clearing only the text would leave the board narrowed by chips
          // whose bar the user just backed out of.
          if (store.query || store.filters.length > 0) void store.clearSearch();
          break;
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [quickAddRef, searchRef]);
}
