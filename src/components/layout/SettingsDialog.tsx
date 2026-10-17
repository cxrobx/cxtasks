import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useRef, useState } from "react";
import { Gem, Monitor, Moon, Sun, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { api, type DockMenuSettings } from "@/lib/tauri";
import { Select } from "@/components/shared/Select";
import { deriveVaultTheme } from "@/lib/vaultLook";
import {
  DENSITY_OPTIONS,
  ONYX_URL,
  PANE_FLOOR,
  useUiStore,
  type ThemePreference,
} from "@/stores/uiStore";
import { HostSettings } from "./HostSettings";
import { JevSettingsPanel } from "./JevSettings";
import { SyncSettings } from "./SyncSettings";

/**
 * App settings.
 *
 * Behind a dialog rather than pinned in the sidebar, which is where the
 * transparency control started. A control you set once and then live with does
 * not deserve permanent real estate in the one pane that is always on screen —
 * it pushed the budget panel down and read as a debug affordance.
 *
 * The dialog deliberately does NOT dim the app behind it. Every control in here
 * changes how the window itself looks, so the thing being adjusted has to stay
 * visible while adjusting it; a standard scrim would black out the exact
 * surface the slider is moving. That is also why the panel is opaque
 * (`bg-elevated`) — at high transparency an translucent settings panel over a
 * translucent window is two sheets of glass and no readable text.
 */
const THEMES = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
] as const satisfies readonly { value: ThemePreference; label: string; icon: typeof Sun }[];
/** Offered only once Onyx has served a palette (or it is already chosen): an
 * option that can only fall back to dark is a control that does nothing. */
const VAULT_THEME = { value: "vault", label: "Vault", icon: Gem } as const;

export function SettingsDialog() {
  const open = useUiStore((s) => s.settingsOpen);
  const setOpen = useUiStore((s) => s.setSettingsOpen);
  const transparency = useUiStore((s) => s.transparency);
  const setTransparency = useUiStore((s) => s.setTransparency);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);
  const vaultLook = useUiStore((s) => s.vaultLook);
  const vaultLookSource = useUiStore((s) => s.vaultLookSource);
  const density = useUiStore((s) => s.density);
  const setDensity = useUiStore((s) => s.setDensity);
  const [dock, setDock] = useState<DockMenuSettings | null>(null);
  const [dockError, setDockError] = useState("");
  const dockSaveQueue = useRef<Promise<unknown>>(Promise.resolve());

  useEffect(() => {
    if (!open) return;
    void dockSaveQueue.current
      .then(() => api.dock.settings())
      .then(setDock)
      .catch(() => setDockError("Dock settings are unavailable."));
  }, [open]);

  const updateDock = (patch: Partial<DockMenuSettings>) => {
    if (!dock) return;
    const next = { ...dock, ...patch };
    setDock(next);
    setDockError("");
    dockSaveQueue.current = dockSaveQueue.current
      .catch(() => undefined)
      .then(() => api.dock.saveSettings(next))
      .then(() => setDockError(""))
      .catch(() => setDockError("Could not save Dock settings."));
  };

  const pct = Math.round(transparency * 100);

  const themes =
    vaultLook || theme === "vault" ? [...THEMES, VAULT_THEME] : THEMES;
  const vault = vaultLook ? deriveVaultTheme(vaultLook) : null;
  // Said out loud rather than done silently: a palette whose text cannot
  // survive the theme's usual glass gets a more opaque window.
  const vaultCapped = vault !== null && vault.paneFloor > PANE_FLOOR[vault.mode];
  const vaultStatus =
    vaultLookSource === "onyx" && vaultLook
      ? `Vault uses your Obsidian vault's ${vaultLook.mode} palette, from Onyx.`
      : vaultLookSource === "cache" && vaultLook
        ? `Onyx isn't answering at ${ONYX_URL}, so Vault uses the palette from last time.`
        : `No vault palette yet. Onyx shares one at ${ONYX_URL} while it is running with "Match vault appearance" on.`;

  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Portal>
        {/* Near-invisible overlay: it exists only to catch the click-outside,
            not to dim. See the component note above. */}
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/10" />
        <Dialog.Content
          aria-describedby={undefined}
          className={cn(
            "fixed left-1/2 top-1/2 z-50 w-[420px] -translate-x-1/2 -translate-y-1/2",
            "max-h-[86vh] overflow-y-auto rounded-xl border border-border bg-elevated p-4 shadow-2xl",
            "focus:outline-none"
          )}
        >
          <div className="mb-4 flex items-center justify-between">
            <Dialog.Title className="text-[13px] font-semibold text-content">
              Settings
            </Dialog.Title>
            <Dialog.Close
              aria-label="Close settings"
              className="rounded-md p-1 text-content-faint transition-colors duration-75 hover:bg-content/10 hover:text-content"
            >
              <X size={14} />
            </Dialog.Close>
          </div>

          {/* Density first: it changes how much of the board you can see at
              once, which is the setting people come in here for. */}
          <section className="mb-4 border-b border-border pb-3.5">
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Row density
            </h3>
            <div
              role="radiogroup"
              aria-label="Row density"
              className="flex items-center gap-px rounded-md border border-border-subtle p-px text-[11px]"
            >
              {DENSITY_OPTIONS.map((o) => (
                <button
                  key={o.value}
                  type="button"
                  role="radio"
                  aria-checked={density === o.value}
                  onClick={() => setDensity(o.value)}
                  className={cn(
                    "flex-1 rounded-[5px] px-2 py-1 transition-colors duration-75",
                    // `accent-hover` rather than `accent`, the same reason the
                    // group-by segments use it: at 11px the 3.64:1 pairing
                    // turns to mud.
                    density === o.value
                      ? "bg-accent-hover font-medium text-white"
                      : "text-content-secondary hover:bg-content/8 hover:text-content"
                  )}
                >
                  {o.label}
                </button>
              ))}
            </div>
            <p className="mt-2 text-[11px] leading-snug text-content-faint">
              Compact puts each task on one line and drops its tags. Badges that
              say a task can’t be worked — blocked, deferred, tripwire — stay at
              every density.
            </p>
          </section>

          <section className="mb-4 border-b border-border pb-3.5">
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Theme
            </h3>
            <div
              role="radiogroup"
              aria-label="Theme"
              className="flex items-center gap-px rounded-md border border-border-subtle p-px text-[11px]"
            >
              {themes.map(({ value, label, icon: Icon }) => (
                <button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={theme === value}
                  onClick={() => setTheme(value)}
                  className={cn(
                    "flex flex-1 items-center justify-center gap-1.5 rounded-[5px] px-2 py-1 transition-colors duration-75",
                    theme === value
                      ? "bg-accent-hover font-medium text-white"
                      : "text-content-secondary hover:bg-content/8 hover:text-content"
                  )}
                >
                  <Icon size={12} />
                  {label}
                </button>
              ))}
            </div>
            <p className="mt-2 text-[11px] leading-snug text-content-faint">{vaultStatus}</p>
            {theme === "vault" && vault && !vault.accentFromVault && (
              <p className="mt-1 text-[11px] leading-snug text-content-faint">
                The vault’s accent can’t carry white text, so buttons keep the blue.
              </p>
            )}
            {theme === "vault" && vaultCapped && (
              <p className="mt-1 text-[11px] leading-snug text-content-faint">
                This palette’s text needs a more opaque window, so transparency is capped lower
                than usual to keep it readable.
              </p>
            )}
          </section>

          <section>
            <div className="mb-1.5 flex items-baseline justify-between">
              <h3 className="text-[11px] font-semibold uppercase tracking-wider text-content-faint">
                Window transparency
              </h3>
              <span className="tabular-nums text-[11px] text-content-muted">{pct}%</span>
            </div>
            <input
              type="range"
              className="cx-slider"
              min={0}
              max={100}
              step={1}
              value={pct}
              aria-label="Window transparency"
              // `--fill` drives the track's filled portion; see globals.css.
              style={{ ["--fill" as string]: `${pct}%` }}
              onChange={(e) => setTransparency(Number(e.target.value) / 100)}
            />
            <div className="mt-1 flex justify-between text-[11px] text-content-faint">
              <span>Opaque</span>
              <span>Glass</span>
            </div>
            <p className="mt-2.5 flex items-start gap-1.5 text-[11px] leading-snug text-content-faint">
              <Monitor size={12} className="mt-px shrink-0" />
              {/* Worth saying out loud: the same percentage is not the same
                  result in both themes, and without this the light window looks
                  like the slider is broken at the top of its range. */}
              <span>
                Light mode holds more opacity at the same setting — dark text loses to a bright
                wallpaper much faster than light text does.
              </span>
            </p>
          </section>

          {(dock || dockError) && (
            <section className="mt-4 border-t border-border pt-3.5">
              <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
                Dock menu
              </h3>
              <p className="mb-2 text-[11px] text-content-faint">
                Choose what appears when you right-click CXTasks in the Dock.
              </p>
              {dock && ([
                ["showToday", "Today tasks"],
                ["showInbox", "Inbox tasks"],
                ["showQuickAdd", "New Task shortcut"],
              ] as const).map(([key, label]) => (
                <label key={key} className="flex items-center gap-2 py-1 text-[12px] text-content-secondary">
                  <input type="checkbox" checked={dock[key]} onChange={(e) => updateDock({ [key]: e.target.checked })} />
                  {label}
                </label>
              ))}
              {dock && <label className="mt-2 flex items-center justify-between text-[12px] text-content-secondary">
                Tasks per section
                <Select
                  aria-label="Tasks per Dock menu section"
                  value={dock.maxTasks}
                  onValueChange={(v) => updateDock({ maxTasks: Number(v) })}
                  options={Array.from({ length: 10 }, (_, i) => ({ value: i + 1, label: String(i + 1) }))}
                  className="w-auto"
                />
              </label>}
              {dockError && <p role="alert" className="mt-1 text-[11px] text-error">{dockError}</p>}
            </section>
          )}

          {/* Sync sits BELOW appearance rather than in its own sheet. It is
              read far more often than it is changed — "is it talking to the
              node, and is anything stuck" — and the answer belongs somewhere
              you already know how to reach, especially the Pause and Unpair
              controls, which exist for moments when something feels wrong. */}
          <section className="mt-4 border-t border-border pt-3.5">
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Sync
            </h3>
            <SyncSettings />
          </section>

          {/* Below sync, because sync is what makes a second host possible and
              reading top to bottom is then the order the thing was built in.
              Read far more often than changed: the question is almost always
              "why has nothing run", not "let me reconfigure the NAS". */}
          <section className="mt-4 border-t border-border pt-3.5">
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Jev
            </h3>
            <JevSettingsPanel />
          </section>

          <section className="mt-4 border-t border-border pt-3.5">
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Hosts
            </h3>
            <HostSettings />
          </section>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
