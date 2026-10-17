import { CalendarPlus, X } from "lucide-react";
import { cn } from "@/lib/utils";

/** `2026-08-01T17:00:00.000Z` ⇄ the value a `datetime-local` input wants. */
export function isoToLocalInput(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(
    d.getHours()
  )}:${pad(d.getMinutes())}`;
}

export function localInputToIso(value: string): string | null {
  if (!value) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

/** Today at 5pm local — the default when a date is first added. */
export function defaultDate(now = new Date()): string {
  const d = new Date(now);
  d.setHours(17, 0, 0, 0);
  if (d.getTime() <= now.getTime()) d.setDate(d.getDate() + 1);
  return d.toISOString();
}

interface DateFieldProps {
  value: string | null;
  onChange: (iso: string | null) => void;
  addLabel?: string;
}

/**
 * Optional date/time control.
 *
 * **Why this isn't just a bare `<input type="datetime-local">`:** WKWebView
 * renders an *empty* datetime-local as a greyed sample date ("07/29/2026,
 * 12:30 PM") rather than the `mm/dd/yyyy, --:-- --` placeholder every other
 * engine shows. Verified against the live app with a debug render: React was
 * passing `value=""` and the DB column was NULL, yet a plausible-looking date
 * was painted in the field — so every task read as though it had a due and a
 * defer date. It's only a placeholder (dimmer than a real value), which is
 * exactly what makes it dangerous: it looks like data.
 *
 * So: when there's no date, render a button instead of an input. The input
 * only exists once there is a real value to show, and it gets a clear button.
 */
export function DateField({ value, onChange, addLabel = "Add date" }: DateFieldProps) {
  if (!value) {
    return (
      <button
        type="button"
        onClick={() => onChange(defaultDate())}
        className={cn(
          "flex w-full items-center gap-1.5 rounded-md border border-dashed border-border",
          "px-2 py-1 text-left text-content-faint hover:border-accent hover:text-accent"
        )}
      >
        <CalendarPlus size={13} />
        {addLabel}
      </button>
    );
  }

  return (
    <div className="flex items-center gap-1">
      <input
        type="datetime-local"
        value={isoToLocalInput(value)}
        onChange={(e) => onChange(localInputToIso(e.target.value))}
        className="w-full rounded-md border border-border bg-input px-2 py-1 text-content"
      />
      <button
        type="button"
        aria-label="Clear date"
        onClick={() => onChange(null)}
        className="shrink-0 rounded p-1 text-content-faint hover:bg-content/8 hover:text-content"
      >
        <X size={12} />
      </button>
    </div>
  );
}
