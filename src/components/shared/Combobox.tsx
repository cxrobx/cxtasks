import * as Popover from "@radix-ui/react-popover";
import { Check } from "lucide-react";
import { forwardRef, useEffect, useId, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";

export interface ComboboxOption {
  value: string;
  /** Shown instead of `value` when set — the suggestion list, never the field. */
  label?: string;
}

type InputProps = Omit<React.InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "list">;

interface ComboboxProps extends InputProps {
  value: string;
  onValueChange: (value: string) => void;
  options: ComboboxOption[];
  /** Optional immediate commit when a suggestion is picked. */
  onSelectOption?: (value: string) => void;
  /** Extra classes for the suggestion panel. */
  panelClassName?: string;
}

/**
 * A free-text field with suggestions — the replacement for `<input list>`.
 *
 * **Why not a `<datalist>`:** WKWebView draws its popup natively, and in our
 * transparent window it lands at the far LEFT edge of the screen, outside the
 * app entirely, in system blue. Like `<select>` (see `Select.tsx`) there is no
 * CSS for it, so the list has to be ours. It borrows `Select`'s panel, row
 * metrics, tick and highlight so every popup in the app reads as one thing.
 *
 * Behaves like the datalist it replaces, and nothing more: picking a
 * suggestion SETS the value and leaves focus in the field, so each caller's
 * existing commit (blur, Enter, comma) saves, unless the caller opts into
 * immediate suggestion commits with `onSelectOption`. Rows are
 * picked on `mousedown` with the default prevented — a `click` would blur the
 * input first and commit whatever half-word was typed.
 */
export const Combobox = forwardRef<HTMLInputElement, ComboboxProps>(function Combobox(
  { value, onValueChange, options, onSelectOption, panelClassName, onKeyDown, onFocus, onBlur, onMouseDown, className, ...rest },
  forwardedRef
) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const listId = useId();

  // Filter on what's typed — unless it already IS a suggestion, when the list
  // shows everything: reopening a filled field is asking for the alternatives.
  const shown = useMemo(() => {
    const q = value.trim().toLowerCase();
    if (!q || options.some((o) => o.value.toLowerCase() === q)) return options;
    return options.filter(
      (o) => o.value.toLowerCase().includes(q) || o.label?.toLowerCase().includes(q)
    );
  }, [value, options]);

  const visible = open && shown.length > 0;

  useEffect(() => {
    setActive(-1);
  }, [value, open]);

  useEffect(() => {
    if (active < 0) return;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-index="${active}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  const pick = (o: ComboboxOption) => {
    onValueChange(o.value);
    setOpen(false);
    onSelectOption?.(o.value);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!visible) {
        setOpen(true);
        return;
      }
      const step = e.key === "ArrowDown" ? 1 : -1;
      setActive((i) => (i + step + shown.length) % shown.length);
      return;
    }
    // Enter and Escape belong to the LIST while a row is highlighted / it is
    // open; otherwise they fall through to the caller's own handler.
    if (e.key === "Enter" && visible && active >= 0) {
      e.preventDefault();
      pick(shown[active]);
      return;
    }
    if (e.key === "Escape" && visible) {
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      return;
    }
    onKeyDown?.(e);
  };

  return (
    <Popover.Root open={visible} onOpenChange={setOpen}>
      <Popover.Anchor asChild>
        <input
          {...rest}
          ref={(el) => {
            inputRef.current = el;
            if (typeof forwardedRef === "function") forwardedRef(el);
            else if (forwardedRef) forwardedRef.current = el;
          }}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={visible}
          aria-controls={visible ? listId : undefined}
          aria-activedescendant={visible && active >= 0 ? `${listId}-${active}` : undefined}
          autoComplete="off"
          value={value}
          onChange={(e) => {
            onValueChange(e.target.value);
            setOpen(true);
          }}
          onFocus={(e) => {
            setOpen(true);
            onFocus?.(e);
          }}
          onMouseDown={(e) => {
            setOpen(true);
            onMouseDown?.(e);
          }}
          onBlur={(e) => {
            setOpen(false);
            onBlur?.(e);
          }}
          onKeyDown={handleKeyDown}
          className={className}
        />
      </Popover.Anchor>

      <Popover.Portal>
        <Popover.Content
          side="bottom"
          align="start"
          sideOffset={4}
          collisionPadding={8}
          // Focus never leaves the field — the list is driven from its keys.
          onOpenAutoFocus={(e) => e.preventDefault()}
          onCloseAutoFocus={(e) => e.preventDefault()}
          onInteractOutside={(e) => {
            if (inputRef.current?.contains(e.target as Node)) e.preventDefault();
          }}
          className={cn(
            "z-50 min-w-[max(var(--radix-popover-trigger-width),10rem)] overflow-hidden",
            "rounded-lg border border-border bg-elevated/95 p-1 shadow-2xl backdrop-blur-xl",
            panelClassName
          )}
        >
          <div
            ref={listRef}
            id={listId}
            role="listbox"
            className="max-h-[min(16rem,var(--radix-popover-content-available-height))] overflow-y-auto"
          >
            {shown.map((o, i) => {
              const selected = o.value === value;
              return (
                <div
                  key={o.value}
                  id={`${listId}-${i}`}
                  role="option"
                  aria-selected={selected}
                  data-index={i}
                  data-highlighted={i === active ? "" : undefined}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    pick(o);
                  }}
                  onMouseMove={() => i !== active && setActive(i)}
                  className={cn(
                    "relative flex cursor-default select-none items-center rounded-[5px]",
                    "py-[3px] pl-6 pr-3 text-content outline-none",
                    "data-[highlighted]:bg-accent data-[highlighted]:text-white"
                  )}
                >
                  {selected && (
                    <span className="absolute left-1.5 flex items-center">
                      <Check size={11} strokeWidth={3} />
                    </span>
                  )}
                  <span className="truncate">{o.label ?? o.value}</span>
                </div>
              );
            })}
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
});
