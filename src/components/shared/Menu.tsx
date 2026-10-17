import * as ContextMenu from "@radix-ui/react-context-menu";
import { Check, ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * Styled Radix context-menu primitives, shared by every right-click menu in the
 * app.
 *
 * These replace real `NSMenu`s (see the rewritten gotcha #9). The look is
 * cxmail's `EmailContextMenu` deliberately — the two apps sit beside each other
 * all day and a right-click that renders differently in each reads as two
 * products, so the panel, the item metrics and the separator all match it.
 *
 * `bg-elevated` and not a pane token: menus portal to the document root, so
 * their backdrop is the transparent window itself. A translucent menu at Glass
 * is text floating on raw wallpaper with nothing behind it (gotcha #22).
 */

export const Root = ContextMenu.Root;
export const Portal = ContextMenu.Portal;
export const Sub = ContextMenu.Sub;

export function Trigger({
  children,
  className,
  style,
}: {
  children: React.ReactNode;
  /** `contents` where the row owns its own layout and a wrapper would break it. */
  className?: string;
  style?: React.CSSProperties;
}) {
  return (
    <ContextMenu.Trigger asChild>
      <div className={className ?? "contents"} style={style}>{children}</div>
    </ContextMenu.Trigger>
  );
}

const PANEL =
  "z-50 min-w-[200px] rounded-lg border border-border bg-elevated p-1 shadow-2xl";

export function Content({ children }: { children: React.ReactNode }) {
  return (
    <ContextMenu.Portal>
      <ContextMenu.Content className={PANEL} collisionPadding={8}>
        {children}
      </ContextMenu.Content>
    </ContextMenu.Portal>
  );
}

const ITEM =
  "flex w-full cursor-default select-none items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-[13px] outline-none";
const ENABLED =
  "text-content-secondary data-[highlighted]:bg-accent data-[highlighted]:text-white";
const DISABLED = "text-content-faint opacity-50";

/**
 * `data-[highlighted]`, never `hover:` — Radix drives highlight from keyboard
 * navigation as well as the pointer, and a hover-only rule leaves arrow keys
 * moving an invisible cursor through the menu.
 */
export function Item({
  icon: Icon,
  label,
  onSelect,
  disabled,
  danger,
  checked,
  hint,
}: {
  icon?: React.ComponentType<{ size?: number | string; className?: string }>;
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  /** Destructive. Red only when it can actually fire — a red disabled row reads
   *  as a warning about something that is not going to happen. */
  danger?: boolean;
  /** Renders a trailing tick. `undefined` leaves the slot out entirely. */
  checked?: boolean;
  hint?: string;
}) {
  return (
    <ContextMenu.Item
      disabled={disabled}
      onSelect={onSelect}
      className={cn(
        ITEM,
        disabled ? DISABLED : danger ? "text-error data-[highlighted]:bg-error data-[highlighted]:text-white" : ENABLED
      )}
    >
      {Icon && <Icon size={13} className="shrink-0" />}
      <span className="flex-1 truncate">{label}</span>
      {hint && <span className="shrink-0 font-mono text-[10px] opacity-60">{hint}</span>}
      {checked && <Check size={13} className="shrink-0" />}
    </ContextMenu.Item>
  );
}

export function Label({ children }: { children: React.ReactNode }) {
  return (
    <ContextMenu.Label className="px-2 py-1.5 text-[11px] font-medium text-content-muted">
      {children}
    </ContextMenu.Label>
  );
}

export function Separator() {
  return <ContextMenu.Separator className="my-1 h-px bg-border" />;
}

/** A nested menu — the Radix equivalent of the `Priority` and `Due` submenus
 *  the native version carried. */
export function SubMenu({
  icon: Icon,
  label,
  children,
}: {
  icon?: React.ComponentType<{ size?: number | string; className?: string }>;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <ContextMenu.Sub>
      <ContextMenu.SubTrigger className={cn(ITEM, ENABLED)}>
        {Icon && <Icon size={13} className="shrink-0" />}
        <span className="flex-1 truncate">{label}</span>
        <ChevronRight size={13} className="shrink-0 opacity-60" />
      </ContextMenu.SubTrigger>
      <ContextMenu.Portal>
        <ContextMenu.SubContent className={PANEL} sideOffset={2} collisionPadding={8}>
          {children}
        </ContextMenu.SubContent>
      </ContextMenu.Portal>
    </ContextMenu.Sub>
  );
}

/** One entry in a declarative menu: an item, or a rule between groups. */
export type MenuEntry =
  | { separator: true }
  | {
      separator?: false;
      label: string;
      icon?: React.ComponentType<{ size?: number | string; className?: string }>;
      onSelect: () => void;
      disabled?: boolean;
      danger?: boolean;
    };

/**
 * A whole context menu from a list, for the sidebar's Project and Tag rows.
 *
 * Those two are short and have no submenus, so spelling them out in JSX at the
 * call site would bury a three-item menu inside an already deeply nested list
 * render. The task row keeps its own component — it has submenus and enough
 * conditional labelling that a data description would be the harder thing to
 * read, which is the line between the two shapes.
 */
export function ListMenu({
  items,
  children,
  triggerClassName,
}: {
  items: MenuEntry[];
  children: React.ReactNode;
  triggerClassName?: string;
}) {
  return (
    <Root>
      <Trigger className={triggerClassName}>{children}</Trigger>
      <Content>
        {items.map((entry, i) =>
          entry.separator ? (
            <Separator key={`sep-${i}`} />
          ) : (
            <Item
              key={entry.label}
              icon={entry.icon}
              label={entry.label}
              onSelect={entry.onSelect}
              disabled={entry.disabled}
              danger={entry.danger}
            />
          )
        )}
      </Content>
    </Root>
  );
}
