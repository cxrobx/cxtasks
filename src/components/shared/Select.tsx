import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronsUpDown } from "lucide-react";
import { cn } from "@/lib/utils";

interface SelectProps<T extends string | number> {
  value: T;
  onValueChange: (value: string) => void;
  options: { value: T; label: string }[];
  className?: string;
  disabled?: boolean;
  "aria-label"?: string;
}

/**
 * macOS-style pop-up button + menu.
 *
 * **Why not a plain `<select>`:** WKWebView draws the popup with its own
 * chrome, and that chrome is light grey regardless of the page theme — a
 * bright menu erupting out of a dark app. `option` elements can't be styled on
 * macOS either, so there is no CSS fix; the menu has to be ours. Radix gives
 * us the parts a native menu has for free (keyboard nav, type-ahead, focus
 * management, portal + collision handling) so the only thing we own is the
 * look.
 *
 * Styled to match a real macOS dark menu: translucent dark panel, hairline
 * border, rounded corners, checkmark on the LEFT for the selected row, and an
 * accent-filled highlight that follows the pointer and the keyboard.
 */
export function Select<T extends string | number>({
  value,
  onValueChange,
  options,
  className,
  disabled,
  ...rest
}: SelectProps<T>) {
  return (
    <SelectPrimitive.Root
      value={String(value)}
      onValueChange={onValueChange}
      disabled={disabled}
    >
      <SelectPrimitive.Trigger
        {...rest}
        className={cn(
          "flex w-full items-center justify-between gap-2 rounded-md border border-border",
          "bg-input py-1 pl-2 pr-1.5 text-left text-content outline-none",
          "hover:border-content-muted/50 data-[state=open]:border-accent",
          "disabled:opacity-50",
          className
        )}
      >
        <SelectPrimitive.Value />
        <SelectPrimitive.Icon>
          <ChevronsUpDown size={12} className="text-content-faint" />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>

      <SelectPrimitive.Portal>
        <SelectPrimitive.Content
          position="popper"
          sideOffset={4}
          className={cn(
            "z-50 min-w-[var(--radix-select-trigger-width)] overflow-hidden",
            // The macOS menu look: translucent panel over a blur, hairline
            // border that reads as a highlight, and a deep shadow. The border
            // is a theme token, not `white/10` — on a light panel a white
            // hairline is invisible and the menu loses its edge entirely.
            "rounded-lg border border-border bg-elevated/95 p-1 shadow-2xl backdrop-blur-xl"
          )}
        >
          <SelectPrimitive.Viewport>
            {options.map((o) => (
              <SelectPrimitive.Item
                key={o.value}
                value={String(o.value)}
                className={cn(
                  "relative flex cursor-default select-none items-center rounded-[5px]",
                  "py-[3px] pl-6 pr-3 text-content outline-none",
                  "data-[highlighted]:bg-accent data-[highlighted]:text-white",
                  "data-[disabled]:opacity-40"
                )}
              >
                <SelectPrimitive.ItemIndicator className="absolute left-1.5 flex items-center">
                  <Check size={11} strokeWidth={3} />
                </SelectPrimitive.ItemIndicator>
                <SelectPrimitive.ItemText>{o.label}</SelectPrimitive.ItemText>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}
