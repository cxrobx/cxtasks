/**
 * The header's text search.
 *
 * Just the field now — the chips moved to their own strip. Typing REFINES the
 * current view rather than escaping it, which is the other half of invariant
 * 21's change: text is one more filter on the list you are looking at. To
 * search everything, go to Open and take the "not Done" chip off.
 */
import { Search, X } from "lucide-react";
import type { KeyboardEvent, RefObject } from "react";
import { cn } from "@/lib/utils";
import { useTaskStore } from "@/stores/taskStore";

interface SearchBoxProps {
  /** Owned by `AppLayout` so ⌘F can focus it. */
  searchRef: RefObject<HTMLInputElement | null>;
}

export function SearchBox({ searchRef }: SearchBoxProps) {
  const query = useTaskStore((s) => s.query);
  const setQuery = useTaskStore((s) => s.setQuery);

  function onKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape" && query) {
      // Clear the text but stay in the view — the global handler's Escape
      // resets the chips, and doing both on one keystroke would throw away a
      // filter set the user built while merely meaning "stop searching".
      e.stopPropagation();
      void setQuery("");
      searchRef.current?.blur();
    }
  }

  return (
    <div
      className={cn(
        "cx-filterbar relative ml-auto flex min-h-[26px] w-full max-w-[16rem] items-center gap-1.5",
        "rounded-md border border-border bg-input py-0.5 pl-2 pr-6"
      )}
    >
      <Search size={13} className="pointer-events-none shrink-0 text-content-faint" />
      <input
        ref={searchRef}
        value={query}
        placeholder="Search"
        onChange={(e) => void setQuery(e.target.value)}
        onKeyDown={onKeyDown}
        className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-content-faint"
      />
      {query && (
        <button
          type="button"
          aria-label="Clear search"
          onClick={() => void setQuery("")}
          className="absolute right-1.5 top-1/2 -translate-y-1/2 text-content-faint hover:text-content"
        >
          <X size={12} />
        </button>
      )}
    </div>
  );
}
