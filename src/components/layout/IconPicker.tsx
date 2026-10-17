import { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { open } from "@tauri-apps/plugin-dialog";
import { ImagePlus, Loader2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { api, type IconCandidate } from "@/lib/tauri";
import { HUES, hueStyle } from "@/lib/hues";
import { BUILTIN_ICONS, isImageIcon, storedHue } from "@/lib/nodeIcons";
import { useTaskStore } from "@/stores/taskStore";

export interface IconTarget {
  kind: "project" | "tag";
  name: string;
  icon: string | null;
  color: string | null;
}

/**
 * Icon & Colour for one sidebar row.
 *
 * Every click SAVES — there is no OK button. The row it edits is on screen
 * beside the dialog (the overlay does not dim, like Settings), so the preview
 * IS the sidebar, and a pick you do not like is undone by picking again or by
 * "Reset to default".
 *
 * Three sources, in the order they are usually wanted: the app icons already
 * in this project's repos (`commands::icons` finds and trims them; a project
 * only — tags have no repos), any image file, and the built-in glyphs. Colour
 * applies to glyphs only; an app icon keeps its own colours.
 */
export function IconPicker({ target, onClose }: { target: IconTarget | null; onClose: () => void }) {
  const setAppearance = useTaskStore((s) => s.setAppearance);
  const [icon, setIcon] = useState<string | null>(null);
  const [color, setColor] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<IconCandidate[]>([]);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset per row, and look in the repos once per open.
  useEffect(() => {
    setIcon(target?.icon ?? null);
    setColor(target?.color ?? null);
    setCandidates([]);
    setError(null);
    if (target?.kind !== "project") return;
    let cancelled = false;
    setSearching(true);
    api.icons
      .repoCandidates(target.name)
      .then((found) => !cancelled && setCandidates(found))
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setSearching(false));
    return () => {
      cancelled = true;
    };
  }, [target]);

  const save = async (nextIcon: string | null, nextColor: string | null) => {
    if (!target) return;
    const prev = { icon, color };
    setIcon(nextIcon);
    setColor(nextColor);
    const err = await setAppearance(target.kind, target.name, nextColor, nextIcon);
    if (err) {
      setIcon(prev.icon);
      setColor(prev.color);
      setError(err);
    } else {
      setError(null);
    }
  };

  const chooseFile = async () => {
    const path = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "ico"] }],
    });
    if (typeof path !== "string") return;
    try {
      const made = await api.icons.fromFile(path);
      setCandidates((c) => [...made, ...c.filter((x) => !made.some((m) => m.dataUrl === x.dataUrl))]);
      // One variant: nothing to choose between, so take it.
      if (made.length === 1) void save(made[0].dataUrl, color);
    } catch (e) {
      setError(String(e));
    }
  };

  const hue = storedHue(color);
  const imageChosen = isImageIcon(icon);
  const glyphStyle = hue ? hueStyle(hue) : undefined;

  return (
    <Dialog.Root open={target !== null} onOpenChange={(o) => !o && onClose()}>
      <Dialog.Portal>
        {/* Near-invisible, like Settings: the row being edited has to stay in
            view, because it is the preview. */}
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/5" />
        <Dialog.Content
          aria-describedby={undefined}
          className={cn(
            "fixed left-[260px] top-[12vh] z-50 w-[316px]",
            "max-h-[80vh] overflow-y-auto rounded-xl border border-border bg-elevated p-3 shadow-2xl",
            "focus:outline-none"
          )}
        >
          <div className="mb-1 flex items-center justify-between">
            <Dialog.Title className="text-[13px] font-semibold text-content">
              Icon &amp; Colour <span className="ml-1 font-normal text-content-muted">{target?.name}</span>
            </Dialog.Title>
            <Dialog.Close
              aria-label="Close"
              className="rounded-md p-1 text-content-faint transition-colors duration-75 hover:bg-content/10 hover:text-content"
            >
              <X size={14} />
            </Dialog.Close>
          </div>

          {target?.kind === "project" && (
            <>
              <Heading>From your repos</Heading>
              {searching ? (
                <p className="flex items-center gap-1.5 py-2 text-[11px] text-content-faint">
                  <Loader2 size={12} className="animate-spin" /> Looking through the linked repos…
                </p>
              ) : candidates.length === 0 ? (
                <p className="py-1 text-[11px] leading-snug text-content-faint">
                  No app icons in this project’s repos. Link one under Repositories…, or choose an
                  image below.
                </p>
              ) : null}
            </>
          )}
          {candidates.length > 0 && (
            <div className="grid grid-cols-5 gap-1.5">
              {candidates.map((c) => (
                <button
                  key={c.dataUrl}
                  type="button"
                  title={c.label}
                  aria-label={c.label}
                  aria-pressed={icon === c.dataUrl}
                  onClick={() => void save(c.dataUrl, color)}
                  className={cn(
                    "flex aspect-square items-center justify-center rounded-lg border p-2 transition-colors duration-75",
                    // A cut-out sits on a checkerboard-free neutral so its
                    // transparency is visible as "no square behind it".
                    c.cutout ? "bg-content/5" : "bg-transparent",
                    icon === c.dataUrl
                      ? "border-accent ring-1 ring-accent"
                      : "border-border-subtle hover:border-border"
                  )}
                >
                  <img src={c.dataUrl} alt="" draggable={false} className="h-7 w-7 rounded-md object-contain" />
                </button>
              ))}
            </div>
          )}
          <button
            type="button"
            onClick={() => void chooseFile()}
            className={cn(
              "mt-1.5 flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-border py-1.5",
              "text-[12px] text-content-secondary transition-colors duration-75 hover:bg-content/5 hover:text-content"
            )}
          >
            <ImagePlus size={13} /> Choose image…
          </button>

          <Heading>Built-in</Heading>
          <div className="grid grid-cols-8 gap-0.5">
            {Object.entries(BUILTIN_ICONS).map(([name, Glyph]) => {
              const value = `lucide:${name}`;
              const on = icon === value;
              return (
                <button
                  key={name}
                  type="button"
                  title={name}
                  aria-label={name}
                  aria-pressed={on}
                  onClick={() => void save(value, color)}
                  className={cn(
                    "flex h-8 items-center justify-center rounded-md transition-colors duration-75",
                    on ? "bg-accent/15" : "text-content-secondary hover:bg-content/8"
                  )}
                >
                  <Glyph size={15} style={on || hue ? glyphStyle : undefined} className={on && !hue ? "text-accent" : undefined} />
                </button>
              );
            })}
          </div>

          <Heading>
            Colour{" "}
            <span className="font-normal normal-case tracking-normal text-content-faint">
              {imageChosen ? "(app icons keep their own)" : "(built-in icons)"}
            </span>
          </Heading>
          <div className="flex flex-wrap items-center gap-1.5">
            <button
              type="button"
              title="Automatic"
              aria-label="Automatic colour"
              aria-pressed={hue === null}
              onClick={() => void save(icon, null)}
              className={cn(
                "flex h-5 w-5 items-center justify-center rounded-full border border-border text-[9px] text-content-muted",
                hue === null && "ring-2 ring-accent ring-offset-2 ring-offset-elevated"
              )}
            >
              A
            </button>
            {HUES.map((h) => (
              <button
                key={h}
                type="button"
                title={h}
                aria-label={h}
                aria-pressed={hue === h}
                onClick={() => void save(icon, h)}
                className={cn(
                  "h-5 w-5 rounded-full",
                  hue === h && "ring-2 ring-accent ring-offset-2 ring-offset-elevated"
                )}
                style={{ backgroundColor: `rgb(var(--hue-${h}))` }}
              />
            ))}
          </div>

          {error && (
            <p role="alert" className="mt-2 text-[11px] leading-snug text-error">
              {error}
            </p>
          )}

          <div className="mt-3 flex justify-end border-t border-border-subtle pt-2">
            <button
              type="button"
              onClick={() => void save(null, null)}
              className="rounded-md px-2 py-1 text-[12px] text-content-muted transition-colors duration-75 hover:bg-content/8 hover:text-content"
            >
              Reset to default
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function Heading({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="mb-1.5 mt-3 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
      {children}
    </h3>
  );
}
