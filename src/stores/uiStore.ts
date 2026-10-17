import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { DEFAULT_SORT, SORT_OPTIONS } from "@/types/task";
import type { ActiveFilter, Sort } from "@/types/task";
import { api } from "@/lib/tauri";
import type { ViewName } from "@/types/task";
import {
  PANE_FLOOR,
  TRANSPARENCY_DEFAULT,
  transparencyToAlphas,
  type Theme,
} from "@/lib/windowAlpha";
import {
  deriveVaultTheme,
  parseVaultPalette,
  samePalette,
  VAULT_VARS,
  type VaultPalette,
  type VaultTheme,
} from "@/lib/vaultLook";

export { PANE_FLOOR, TRANSPARENCY_DEFAULT, transparencyToAlphas, type Theme };

/**
 * What the user CHOSE. `vault` is a deferral, not a third palette: it defers to
 * the Obsidian vault's palette as Onyx measures it (`vaultLook.ts`), and
 * resolves to that palette's own mode — so it can never paint something that is
 * neither light nor dark. Everything keyed per theme (the floors, `data-theme`,
 * the native pin) takes the RESOLVED `Theme`, never this.
 */
export type ThemePreference = Theme | "vault";

/** Where the vault palette in use came from: Onyx just now, the copy kept from
 * last time (Onyx not answering), or nowhere. */
export type VaultLookSource = "onyx" | "cache" | "none";

/** Onyx's server, on its default port. Loopback only — the Rust side refuses
 * anything else. */
export const ONYX_URL = "http://127.0.0.1:8899";

/**
 * How the task list is carved up.
 *
 * `section` is the default and means exactly what the app did before this
 * existed: render the sectioned board when the current view or project has
 * sections, and a flat list when it doesn't. `project` groups by
 * `tasks.project` instead, which is the only grouping that means anything in
 * Inbox/Today/Waiting/Open — those views cut across every project, and until now
 * the only way to see "what's due today, by project" was to visit each project
 * in turn. `none` flattens a sectioned board without deleting its sections.
 *
 * The two grouping modes are alternatives rather than a nesting: sections
 * inside projects is a two-level board, and a two-level board needs its own
 * drag rules, its own collapse semantics and its own empty states. One axis at
 * a time — the same reasoning that keeps `tasks.status` on one axis.
 */
export type GroupBy = "none" | "section" | "project";

/**
 * How much room a task row gets.
 *
 * Same vocabulary as cxmail's, deliberately — the two apps sit beside each
 * other and a control that says "compact" in one and something else in the
 * other is two things to learn for one idea.
 *
 * What each mode DROPS is the design, not the padding:
 * - `comfortable` — today's row. Two lines: title, then tags and evidence.
 * - `compact` — one line. Tags go, because they are the part you scan for
 *   only when you are looking for a tag; the badges that say a row cannot be
 *   worked (blocked, deferred, tripwire) stay, since hiding those would make
 *   the list lie about what is actionable.
 * - `ultra` — one tight line, and the reference chip goes too.
 */
export type Density = "comfortable" | "compact" | "ultra";

export const DENSITY_OPTIONS = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
  { value: "ultra", label: "Ultra" },
] as const satisfies readonly { value: Density; label: string }[];

/**
 * The virtualizer's row-height estimate per mode.
 *
 * Not cosmetic and not optional: the scrollbar is sized from the estimate
 * until each row is measured, so a stale one makes the thumb jump on the first
 * scroll (gotcha #33). It has to move when the padding does.
 */
export const DENSITY_ROW_ESTIMATE: Record<Density, number> = {
  comfortable: 62,
  compact: 40,
  ultra: 30,
};

export const GROUP_BY_OPTIONS = [
  { value: "none", label: "None" },
  { value: "section", label: "Sections" },
  { value: "project", label: "Project" },
] as const satisfies readonly { value: GroupBy; label: string }[];

/**
 * The blur radius the desktop gets, as a function of the same slider.
 *
 * One dial, not two, and the coupling is not arbitrary: the more of the desktop
 * you let through, the more blur it takes to keep text on top of it readable.
 * Two independent controls would let you pick the one combination that is
 * always wrong — wide open and perfectly sharp — and would make the useful
 * setting a two-dimensional hunt instead of a drag.
 *
 * The range is deliberately narrower than the WindowServer's. Past roughly 48
 * the wallpaper stops being a wallpaper and becomes the same featureless smoke
 * that `NSVisualEffectView` produced, which is the exact look this whole change
 * exists to get away from. The floor is 10 rather than 0 because a *slightly*
 * transparent window over a sharp desktop reads as a rendering bug rather than
 * a design.
 *
 * At the default transparency this lands on 24, which is where MonoCode — the
 * app this look was taken from — pins it outright.
 */
const BLUR_MIN = 10;
const BLUR_MAX = 48;

export function transparencyToBlurRadius(transparency: number): number {
  // `Number.isFinite` before the clamp, not a bare `Math.min`/`Math.max` pair:
  // those pass NaN straight through, and a NaN here reaches `invoke` as JSON
  // `null`, which serde refuses — so a corrupt `localStorage` entry would take
  // the blur out entirely rather than falling back. Same trap `readTransparency`
  // documents one screen down, arriving through a different door.
  const t = Number.isFinite(transparency)
    ? Math.min(1, Math.max(0, transparency))
    : 0;
  return Math.round(BLUR_MIN + t * (BLUR_MAX - BLUR_MIN));
}

/**
 * Each theme's opaque base colour, as the RGB triple Rust needs.
 *
 * Must track `--bg-primary` in `globals.css`. It is duplicated rather than read
 * back out of the computed style because this is also the LAUNCH colour, and at
 * launch there is no element mounted to compute a style from. The Rust side
 * carries the dark value a third time, in `lib.rs`, for the same reason — that
 * one paints the window before any JavaScript has run at all.
 */
const THEME_BASE_RGB: Record<Theme, readonly [number, number, number]> = {
  dark: [24, 24, 24],
  light: [248, 247, 245],
};

/**
 * The vault palette being worn right now, derived — or `null` for the built-in
 * theme. Module state for the same reason `reduceTransparency` is:
 * `applyWindow` and `applyGlass` run on every frame of a slider drag and need
 * the floor and the window colour without re-deriving the palette each time.
 * Only `applyTheme` writes it.
 */
let activeVault: VaultTheme | null = null;

/** The theme actually painted. Written only by `applyTheme`; read by the
 * callers that repaint the window without changing the theme. */
let paintedTheme: Theme = "dark";

/**
 * Put the vault's tokens on `<html>` as inline custom properties, or take every
 * one of them off.
 *
 * Inline on the root beats the `[data-theme]` blocks in `globals.css` (which
 * match that same element), and every rule in the app reads those variables,
 * so this repaints the whole UI with no other change. The tokens the vault does
 * not set — status colours, the card, shadows — fall through to the block for
 * the palette's mode, which is why `data-theme` still carries the resolved
 * theme. Always clears first: a palette whose accent is rejected must not
 * inherit the previous palette's.
 */
function paintVaultVars(vault: VaultTheme | null): void {
  const root = document.documentElement;
  for (const name of VAULT_VARS) root.style.removeProperty(name);
  if (!vault) return;
  for (const [name, value] of Object.entries(vault.vars)) root.style.setProperty(name, value);
}

/**
 * Glass is off until the first paint, then tracks the slider.
 *
 * `glassReady` exists because the native call has to come AFTER React has
 * painted something. The window is `transparent: true`, so a clear window with
 * an empty webview in it is bare wallpaper and three floating traffic lights.
 * `lib.rs` boots it opaque; `startGlass` below flips it once there is a board
 * to look at. Every other caller no-ops until then rather than racing it.
 *
 * `lastSent` is the IPC guard. `applyWindow` runs on every frame of a slider
 * drag, but the radius is an integer over a 38-wide range, so it genuinely
 * changes at most 38 times across a full sweep. Comparing before sending is
 * what keeps the drag the pure-CSS operation it was designed to be.
 */
let glassReady = false;
let lastSent: { enabled: boolean; radius: number; theme: Theme; base: string } | null = null;

/**
 * macOS's **Accessibility → Display → Reduce transparency**, mirrored here.
 *
 * It pins the EFFECTIVE transparency to 0 — not merely the window's blur. The
 * native side goes opaque on its own (`glass_macos::set_state` refuses glass
 * while the setting is on), but the panes carry their own alpha, so leaving the
 * CSS alone would composite a translucent sidebar over an opaque window and land
 * it a shade darker than its own token. Treating the setting as "the slider is
 * at 0 right now" makes one rule cover both halves, and it is also just true:
 * 0 is the opaque end of this dial, which is exactly what the setting asks for.
 *
 * The slider itself is NOT moved. `transparency` stays where the user put it,
 * so turning the setting back off restores their window rather than stranding
 * them at opaque with no memory of what they had.
 */
let reduceTransparency = false;

/** What the window should actually render at, once accessibility has its say. */
function effectiveTransparency(transparency: number): number {
  return reduceTransparency ? 0 : transparency;
}

/**
 * Push the window's glass state to AppKit.
 *
 * Off at transparency 0 — and that is a real off, not a radius of zero. At 0
 * the panes are fully opaque, so the blur would be invisible anyway; making the
 * NSWindow genuinely opaque there means "Opaque" gives you an ordinary window
 * with an ordinary shadow, rather than a transparent one that happens to be
 * fully painted over.
 *
 * Unlike `vibrancy`, this is NOT installed once and left alone: the radius is
 * part of the dial and the base colour is part of the theme, so both edges have
 * to be re-sent. Failures are swallowed for the reason `applyTheme` documents —
 * a missing Tauri bridge (a plain `vite dev` tab) throws synchronously, and a
 * theme that renders without its blur beats a white screen.
 */
export function applyGlass(transparency: number, theme: Theme): void {
  if (!glassReady) return;
  const t = Math.min(1, Math.max(0, effectiveTransparency(transparency)));
  const enabled = t > 0;
  const radius = transparencyToBlurRadius(t);
  // The opaque window colour is the vault's ground while one is worn — the
  // built-in base would paint the window a different colour from every pane
  // the moment glass turns off. Part of the guard, so a palette change with the
  // theme unchanged still reaches AppKit.
  const rgb = activeVault?.base ?? THEME_BASE_RGB[theme];
  const base = rgb.join(" ");
  if (
    lastSent != null &&
    lastSent.enabled === enabled &&
    lastSent.radius === radius &&
    lastSent.theme === theme &&
    lastSent.base === base
  ) {
    return;
  }
  // Radius-only when nothing else moved: the common case during a drag, and it
  // skips re-running the window setup (opacity, background, shadow) 38 times.
  const radiusOnly =
    lastSent != null &&
    lastSent.enabled === enabled &&
    enabled &&
    lastSent.theme === theme &&
    lastSent.base === base;
  lastSent = { enabled, radius, theme, base };
  try {
    const call = radiusOnly
      ? api.glass.setRadius(radius)
      : api.glass.set(enabled, radius, [rgb[0], rgb[1], rgb[2]]);
    void call.catch(() => {
      // Not macOS, or the window is gone. The CSS half still applied.
    });
  } catch {
    // No Tauri runtime at all. CSS-only transparency is the correct
    // degradation — the panes just sit on an unblurred desktop.
  }
}

/**
 * Arm the glass, once, after the first paint.
 *
 * Called from `main.tsx` behind a double `requestAnimationFrame`: one frame to
 * get the render scheduled, the second to land after it has actually been
 * composited. A single frame fires too early often enough to see the flash it
 * exists to prevent.
 */
export function startGlass(): void {
  const paint = () => applyWindow(readTransparency(), paintedTheme);

  // Read the accessibility setting BEFORE arming, so a user who has it on never
  // sees a frame of glass. If the call fails we are not on macOS (or there is no
  // Tauri bridge at all), and `false` is the right answer in both cases.
  try {
    void api.glass
      .reduceTransparency()
      .catch(() => false)
      .then((on) => {
        reduceTransparency = on;
        glassReady = true;
        paint();
      });
  } catch {
    glassReady = true;
    paint();
  }

  // Someone turns this on because they are struggling *now*, so it has to apply
  // without a relaunch. The native half is already handled by the observer in
  // `glass_macos`; this is the CSS half.
  void listen<boolean>("reduce-transparency-changed", (event) => {
    reduceTransparency = event.payload;
    // The guard in `applyGlass` compares against the last values SENT, and the
    // native side has already moved underneath it — so clear it or a flip back
    // to glass is skipped as a no-op and the window stays opaque.
    lastSent = null;
    paint();
  }).catch(() => {
    // No Tauri event bus. The hydrate read above still applied.
  });
}

/** Chrome state that isn't task data, kept out of `taskStore` so a refresh of
 * the board never touches it. Its own store rather than component state so
 * `useKeyboardShortcuts` can reach it through `getState()` like the rest. */
interface UiState {
  /**
   * The order of the sidebar's view rows, as `ViewName`s.
   *
   * Here rather than in the DB, unlike project order — a view is not a row.
   * There is no `views` table to hang a `sort_order` off, the set is a compile-
   * time constant, and this is a preference about chrome in the same class as
   * `groupBy` and `tagsCollapsed`. The DB is shared by three binaries and only
   * one of them draws a sidebar.
   *
   * **A sort key over `SIDEBAR_VIEWS`, never a replacement for it** — see
   * `orderViews`. Storage decides sequence and nothing else; which views exist
   * is always the constant's answer.
   */
  viewOrder: ViewName[];
  /** The right-hand task detail pane. Collapsed = the board gets its width. */
  detailCollapsed: boolean;
  /**
   * Mission Control replaces the main pane with the agent dashboard.
   *
   * Chrome state, not a `ViewName`: it's a different *lens* on whatever the
   * sidebar has selected, not another place tasks can live. Making it a view
   * would put it in the sidebar's mutually-exclusive list and force it to answer
   * questions it has no answer for — what a section means in it, what quick-add
   * would file into it.
   */
  missionControl: boolean;
  /**
   * The launch console's body, collapsed to its header.
   *
   * **Per-task, and deliberately NOT remembered.** Opening any task collapses
   * it again (`TaskDetail` resets this on `task.id`); expanding is a decision
   * about the task in front of you, not a setting. Most tasks carry no
   * execution context at all, so a console that stayed open followed you across
   * the whole board after one use.
   *
   * It lives in the store rather than in component state because the header,
   * the body and `DispatchBar` all read it, and because the reset must key on
   * `task.id` ALONE — `TaskDetailForm`'s draft-reset effect also fires on
   * `updated_at`, so sharing it would slam the console shut on every patch,
   * including the ones the runner writes while you are looking at it.
   *
   * Not persisted: a value that every task open overwrites is one nobody reads.
   */
  launchCollapsed: boolean;
  /**
   * The sidebar's Tags section, collapsed to its header.
   *
   * **Persisted**, unlike `launchCollapsed` — the opposite call, for the
   * opposite reason. That one is scoped to the task in front of you and every
   * task open overwrites it. This is a standing decision about how much of the
   * sidebar you want tags to occupy, and tags churn: they arrive from agents
   * and at filing time and are never curated, so how long the list runs is not
   * something you re-decide each launch.
   */
  tagsCollapsed: boolean;
  /** The whole Areas section, independent of each area's nested-project toggle. */
  areasCollapsed: boolean;
  /**
   * Draw the areas and projects with nothing open in them. **Persisted**: it
   * is a standing choice about what the sidebar is for, and re-making it each
   * launch would make "hidden" mean "hidden until you restart".
   */
  showHidden: boolean;
  /**
   * Areas whose nested projects are hidden, by project **id**.
   *
   * **Persisted**, for `tagsCollapsed`'s reason: how much of the sidebar an
   * area is allowed to occupy is a standing decision, not one you re-make each
   * launch. An area is a permanent bucket, so the projects inside it accumulate
   * exactly the way tags do — and an area holding six projects you are not
   * currently working in costs six rows on every screen.
   *
   * **Keyed by id, not by name** — the deliberate exception here, and the same
   * one `projects.parent_id` makes. Every text join has to move in the same
   * breath as a rename (invariant 17); this is not a join, it is a note *about*
   * a row, and a renamed area springing open is a preference quietly forgetting
   * itself. The cost is a dead id left behind by a deleted area, which never
   * matches anything again.
   *
   * A set semantically, an array because that is what `JSON.stringify` stores
   * and what a Zustand selector can compare.
   */
  collapsedAreas: string[];
  /**
   * How the task list groups its rows.
   *
   * One preference for the whole app rather than one per view. Per-view state
   * would be more precise and is the wrong trade: the mode is a way of *reading*
   * the board that you settle into, not a property of Today, and a control that
   * silently means something different in each view is a control you have to
   * re-check every time you move.
   */
  groupBy: GroupBy;
  /** How much room a task row gets — see `Density`. */
  density: Density;
  /**
   * How the task list is ordered.
   *
   * ONE preference for the whole app, exactly like `groupBy` and for its
   * reason: a sort is a way of *reading* the board you settle into, not a
   * property of Today, and a control that silently means something different
   * in each view is one you have to re-check every time you move.
   */
  sort: Sort;
  /**
   * The filters a view or node opens with, keyed by view name or project id.
   *
   * **Here rather than in the DB**, following `viewOrder`'s argument: this is
   * chrome, the database is shared by three binaries and only one of them
   * draws a filter strip, and `projects` is swept on `created_at` — a column
   * written after the row is created would oscillate forever (gotcha #36).
   *
   * **Keyed by project ID, not name**, the same exception `collapsedAreas`
   * makes: a note *about* a row, not one of the text joins a rename has to
   * carry (invariant 17). A stale id from a deleted project simply never
   * matches again.
   */
  viewFilters: Record<string, ActiveFilter[]>;
  /** The CHOICE — `vault` included. What is painted is `resolveTheme(theme)`. */
  theme: ThemePreference;
  /**
   * The last vault palette Onyx served, kept across launches: the first frame of
   * a `vault` launch is painted from it before Onyx has been asked, and it keeps
   * the vault's colours while Onyx is not running. Re-validated on read, since
   * localStorage is hand-editable and every value ends up in a CSS property.
   */
  vaultLook: VaultPalette | null;
  /** Where `vaultLook` came from on the latest check. Not persisted. */
  vaultLookSource: VaultLookSource;
  /** 0 = fully opaque, 1 = as translucent as the current theme allows. */
  transparency: number;
  /** The settings sheet. Chrome state like the rest of this store. */
  settingsOpen: boolean;
  toggleDetail: () => void;
  /** Reveal the task inspector without accidentally closing it when it is
   * already visible. Used after capture so the new task has an obvious home. */
  showDetail: () => void;
  toggleMissionControl: () => void;
  toggleLaunchConsole: () => void;
  setGroupBy: (groupBy: GroupBy) => void;
  setDensity: (density: Density) => void;
  setSort: (sort: Sort) => void;
  /** Remember these filters for this view or node. */
  saveViewFilters: (key: string, filters: ActiveFilter[]) => void;
  /** Forget them — the scope goes back to its baseline. */
  forgetViewFilters: (key: string) => void;
  setTheme: (theme: ThemePreference) => void;
  /** Record a vault-look check: a palette from Onyx, or `null` for "Onyx did
   * not answer", which keeps the last good palette. */
  setVaultLook: (palette: VaultPalette | null) => void;
  toggleTags: () => void;
  toggleAreas: () => void;
  toggleShowHidden: () => void;
  expandAreas: () => void;
  toggleArea: (id: string) => void;
  /** Open an area without the risk of shutting it — the counterpart to
   * `showDetail`. Used where something is about to be REVEALED inside an area
   * (the inline "new project" field), which a blind toggle would hide instead. */
  expandArea: (id: string) => void;
  toggleTheme: () => void;
  setTransparency: (value: number) => void;
  setSettingsOpen: (open: boolean) => void;
  setViewOrder: (order: ViewName[]) => void;
}

const COLLAPSED_KEY = "cxtasks.detailCollapsed";
const TAGS_KEY = "cxtasks.tagsCollapsed";
const AREAS_SECTION_KEY = "cxtasks.areasCollapsed";
const SHOW_HIDDEN_KEY = "cxtasks.showHidden";
const MISSION_KEY = "cxtasks.missionControl";
const GROUP_KEY = "cxtasks.groupBy";
const THEME_KEY = "cxtasks.theme";
const VAULT_LOOK_KEY = "cxtasks.vaultLook";
const WINDOW_KEY = "cxtasks.transparency";
const VIEW_ORDER_KEY = "cxtasks.viewOrder";
const AREAS_KEY = "cxtasks.collapsedAreas";
const SORT_KEY = "cxtasks.sortBy";
const DENSITY_KEY = "cxtasks.density";
const VIEW_FILTERS_KEY = "cxtasks.viewFilters";

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeFlag(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // A write failure only costs the preference across restarts.
  }
}

/**
 * Persist a new collapsed-areas set and apply it.
 *
 * Both actions route through here so the stored value and the rendered one
 * cannot disagree — the failure mode of a second write path is a sidebar that
 * looks right until the next launch.
 */
function writeAreas(
  next: string[],
  set: (partial: { collapsedAreas: string[] }) => void
): void {
  try {
    localStorage.setItem(AREAS_KEY, JSON.stringify(next));
  } catch {
    // A write failure only costs the preference across restarts.
  }
  set({ collapsedAreas: next });
}

/**
 * Read the saved grouping mode.
 *
 * Anything unrecognised falls back to `section`, which is the mode that
 * reproduces the pre-grouping behaviour exactly — so a corrupt or
 * older/newer-schema entry degrades to "the app as it was" rather than to a
 * flat list that looks like every section was deleted.
 */
/**
 * Persist a new saved-filters map and apply it.
 *
 * One write path, `writeAreas`' reason: the failure mode of a second is a
 * strip that looks right until the next launch.
 */
function writeViewFilters(
  next: Record<string, ActiveFilter[]>,
  set: (partial: { viewFilters: Record<string, ActiveFilter[]> }) => void
): void {
  try {
    localStorage.setItem(VIEW_FILTERS_KEY, JSON.stringify(next));
  } catch {
    // A write failure only costs the preference across restarts.
  }
  set({ viewFilters: next });
}

function readGroupBy(): GroupBy {
  try {
    const saved = localStorage.getItem(GROUP_KEY);
    return GROUP_BY_OPTIONS.some((o) => o.value === saved)
      ? (saved as GroupBy)
      : "section";
  } catch {
    return "section";
  }
}

/**
 * Read the saved density.
 *
 * Exported like `readTheme`, because `main.tsx` applies it BEFORE React mounts:
 * the attribute drives row padding, so setting it after the first paint makes
 * the list visibly re-flow on every launch.
 */
export function readDensity(): Density {
  try {
    const saved = localStorage.getItem(DENSITY_KEY);
    return DENSITY_OPTIONS.some((o) => o.value === saved)
      ? (saved as Density)
      : "comfortable";
  } catch {
    return "comfortable";
  }
}

/** Push the density to the DOM. One attribute; `globals.css` does the rest. */
export function applyDensity(density: Density): void {
  document.documentElement.setAttribute("data-density", density);
}

/**
 * Read the saved sort.
 *
 * Same degradation as `readGroupBy`: anything unrecognised — a corrupt entry,
 * a key from a newer build, no storage at all — means "no opinion", and the
 * no-opinion sort is `default`, which is every view's own order and so is
 * byte-identical to the app before this existed.
 */
function readSort(): Sort {
  try {
    const raw = localStorage.getItem(SORT_KEY);
    if (!raw) return DEFAULT_SORT;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return DEFAULT_SORT;
    const { key, dir } = parsed as { key?: unknown; dir?: unknown };
    const okKey = SORT_OPTIONS.some((o) => o.value === key);
    const okDir = dir === "asc" || dir === "desc";
    return okKey && okDir ? { key: key as Sort["key"], dir } : DEFAULT_SORT;
  } catch {
    return DEFAULT_SORT;
  }
}

/**
 * Read the saved per-view filters.
 *
 * Each entry is validated field by field rather than trusted: these are
 * replayed into a query, so a half-written chip would reach the backend as a
 * facet with an undefined value and come back as a validation error the user
 * cannot connect to anything they did. A bad entry is dropped, which lands the
 * scope on its baseline — the state it had before anything was saved.
 */
function readViewFilters(): Record<string, ActiveFilter[]> {
  try {
    const raw = localStorage.getItem(VIEW_FILTERS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
    const out: Record<string, ActiveFilter[]> = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (!Array.isArray(value)) continue;
      const chips = value.filter(
        (f): f is ActiveFilter =>
          typeof f === "object" &&
          f !== null &&
          typeof (f as ActiveFilter).facet === "string" &&
          typeof (f as ActiveFilter).op === "string" &&
          typeof (f as ActiveFilter).value === "string"
      );
      out[key] = chips.map((f) => ({ facet: f.facet, op: f.op, value: f.value }));
    }
    return out;
  } catch {
    return {};
  }
}

/**
 * Read the saved sidebar view order.
 *
 * Returns whatever was stored, filtered to strings and nothing more — it is
 * deliberately NOT reconciled against the canonical list here. That is
 * `orderViews`' job, at render, because the canonical list lives with the rows
 * it describes (icons, labels, counts) and a store has no business importing
 * those.
 *
 * An empty array is the correct answer for "never reordered", a corrupt entry,
 * and a storage-less environment alike: all three mean "no opinion", which
 * `orderViews` turns into the canonical order.
 */
function readViewOrder(): ViewName[] {
  try {
    const raw = localStorage.getItem(VIEW_ORDER_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed
          .filter((v): v is ViewName => typeof v === "string")
          // The "all" sidebar row became "open" when the actionable split
          // landed. Mapping the saved key keeps the row where the user had
          // dragged it; without this, `orderViews` drops the stale key and
          // the renamed row silently sinks to the bottom of the order.
          .map((v) => (v === ("all" as ViewName) ? ("open" as ViewName) : v))
      : [];
  } catch {
    // Unparseable, or no localStorage. Either way: no opinion.
    return [];
  }
}

/**
 * Read the saved set of collapsed areas.
 *
 * Same degradation as `readViewOrder`, and for the same reason: an empty array
 * is the right answer for "never collapsed anything", a corrupt entry, and a
 * storage-less environment alike. All three mean "no opinion", and the sidebar's
 * no-opinion state is every area open — the shape it had before this existed.
 *
 * Nothing is reconciled against the real projects here; a stale id simply never
 * matches a row again.
 */
function readCollapsedAreas(): string[] {
  try {
    const raw = localStorage.getItem(AREAS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((v): v is string => typeof v === "string")
      : [];
  } catch {
    return [];
  }
}

/**
 * Apply a saved order to the canonical view list.
 *
 * **The saved order is a sort key, never the list itself**, which is the whole
 * robustness property here. Storage answers only "in what sequence"; the
 * constant always answers "which views exist". So:
 *
 * - a key storage names that no longer exists is **dropped** — a renamed or
 *   retired view can't leave a dead row behind, or worse, a row that renders as
 *   `undefined`;
 * - a canonical key storage doesn't mention is **appended** — add a view in a
 *   later build and every user who has ever dragged a row still sees it, at the
 *   bottom where it is visible and movable. Storing the list itself instead
 *   would make a new view invisible to exactly the users who use this feature;
 * - a duplicate is collapsed, because a hand-edited or half-written entry would
 *   otherwise render one row twice under one React key.
 *
 * Same degradation discipline as `readGroupBy`: anything unrecognised falls
 * back to the app as it ships rather than to a broken sidebar.
 */
export function orderViews<T extends ViewName>(
  saved: readonly ViewName[],
  canonical: readonly T[]
): T[] {
  const known = new Map<ViewName, T>(canonical.map((k) => [k, k]));
  const placed = new Set<ViewName>();
  const out: T[] = [];
  for (const key of saved) {
    const match = known.get(key);
    if (match !== undefined && !placed.has(key)) {
      placed.add(key);
      out.push(match);
    }
  }
  for (const key of canonical) if (!placed.has(key)) out.push(key);
  return out;
}

/** Dark stays the default for a fresh install, matching `index.html`'s
 * `data-theme="dark"` so the first paint needs no JS. Exported because
 * `main.tsx` reads it *before* React mounts — see `applyTheme`. */
export function readTheme(): ThemePreference {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    return saved === "light" || saved === "vault" ? saved : "dark";
  } catch {
    return "dark";
  }
}

/** The persisted vault palette, validated, or `null`. */
export function readVaultLook(): VaultPalette | null {
  try {
    return parseVaultPalette(JSON.parse(localStorage.getItem(VAULT_LOOK_KEY) ?? "null"));
  } catch {
    return null;
  }
}

/**
 * Collapse a preference to the theme that actually gets painted.
 *
 * `vault` with no palette yet (Onyx has never answered) paints dark, the app's
 * default, rather than guessing — Settings only offers Vault once a palette has
 * arrived, so this is the hand-edited-storage case.
 */
export function resolveTheme(
  preference: ThemePreference,
  vaultLook: VaultPalette | null = useUiStore.getState().vaultLook
): Theme {
  if (preference === "vault") return vaultLook?.mode ?? "dark";
  return preference;
}

/**
 * Read the saved transparency.
 *
 * Exported for the same reason as `readTheme`: `main.tsx` needs it *before*
 * React mounts. `Number.isFinite` rather than a bare `parseFloat` guard — a
 * corrupt entry parses to `NaN`, which would sail through a `!= null` check and
 * make every alpha `NaN`, painting the window fully transparent.
 */
export function readTransparency(): number {
  try {
    const saved = Number.parseFloat(localStorage.getItem(WINDOW_KEY) ?? "");
    return Number.isFinite(saved) ? Math.min(1, Math.max(0, saved)) : TRANSPARENCY_DEFAULT;
  } catch {
    return TRANSPARENCY_DEFAULT;
  }
}

/**
 * Push the transparency to the DOM as the three pane alphas.
 *
 * Takes the THEME as well, and must be re-run on every theme change — the
 * mapping is theme-dependent (`PANE_FLOOR`), so leaving it alone when the theme
 * flips silently applies dark's floor to a light window, which is the
 * unreadable case this exists to prevent. `applyTheme` calls it for that
 * reason; there is no path that sets one without the other.
 *
 * The CSS write comes FIRST and is never gated on the native call. That
 * ordering is what keeps a drag smooth: the panes repaint every frame from
 * these three variables, and `applyGlass` below only reaches AppKit on the ~38
 * frames where the rounded blur radius actually changes. A failed IPC call can
 * therefore leave the blur stale, but never strand the window half-transparent.
 *
 * This used to be a pure CSS change, back when the blur was an
 * `NSVisualEffectView` installed once at startup and left there. It isn't any
 * more: `glass_macos` takes a radius, and the radius is part of the same dial.
 */
export function applyWindow(transparency: number, theme: Theme): void {
  // A vault palette may need a higher floor than the theme's to keep its text
  // legible on the glass (`deriveVaultTheme`), and always takes cxmail's
  // sidebar curve — dark's boost assumes a bar far lighter than the content,
  // which a vault palette does not promise.
  const { pane, sidebar, surface } = transparencyToAlphas(
    effectiveTransparency(transparency),
    theme,
    activeVault ? { floor: activeVault.paneFloor, curve: "lead" } : {}
  );
  const root = document.documentElement;
  root.style.setProperty("--alpha-pane", String(pane));
  root.style.setProperty("--alpha-sidebar", String(sidebar));
  root.style.setProperty("--alpha-surface", String(surface));
  applyGlass(transparency, theme);
}

/** Push the theme to both layers that have to agree.
 *
 * `data-theme` drives the CSS variables in `globals.css`. The native call drives
 * AppKit's `NSApp.appearance`, which CSS cannot reach — skip it and the
 * right-click NSMenu, sheets and scrollbars stay on the old theme, which is the
 * same bug the appearance pinning was introduced to fix, just inverted. Both,
 * always. */
export function applyTheme(
  preference: ThemePreference,
  vaultLook: VaultPalette | null = readVaultLook()
): void {
  const palette = preference === "vault" ? vaultLook : null;
  const theme = resolveTheme(preference, vaultLook);
  // Palette, `data-theme` and alphas together, before anything paints: the
  // floor and the window colour below both read `activeVault`.
  activeVault = palette ? deriveVaultTheme(palette) : null;
  paintedTheme = theme;
  paintVaultVars(activeVault);
  document.documentElement.setAttribute("data-theme", theme);
  // The pane alphas are theme-dependent (`PANE_FLOOR`), so the theme cannot
  // move without them. Re-derived here rather than left to the caller so there
  // is no path that changes one and forgets the other — that path applies
  // dark's floor to a light window, which is the unreadable case.
  applyWindow(readTransparency(), theme);
  // Fire-and-forget, and defended twice on purpose. This runs from `main.tsx`
  // BEFORE `createRoot`, so anything that escapes here is a white screen rather
  // than a missed theme. `invoke` throws SYNCHRONOUSLY when the Tauri IPC bridge
  // isn't on `window` (a plain `vite dev` browser tab), which a bare `.catch()`
  // would sail straight past — hence the try as well.
  try {
    void api.appearance.setNative(theme === "dark").catch(() => {
      // Not macOS, or no window yet. The CSS half still applied.
    });
  } catch {
    // No Tauri runtime at all. CSS-only theming is the correct degradation.
  }
}

export const useUiStore = create<UiState>((set, get) => ({
  detailCollapsed: readFlag(COLLAPSED_KEY),
  missionControl: readFlag(MISSION_KEY),
  // Always collapsed to begin with; `TaskDetail` re-collapses on every task.
  launchCollapsed: true,
  // `readFlag` is false for an unset key, so the section starts expanded.
  tagsCollapsed: readFlag(TAGS_KEY),
  areasCollapsed: readFlag(AREAS_SECTION_KEY),
  showHidden: readFlag(SHOW_HIDDEN_KEY),
  collapsedAreas: readCollapsedAreas(),
  groupBy: readGroupBy(),
  density: readDensity(),
  sort: readSort(),
  viewFilters: readViewFilters(),
  viewOrder: readViewOrder(),
  theme: readTheme(),
  vaultLook: readVaultLook(),
  vaultLookSource: readVaultLook() ? "cache" : "none",
  transparency: readTransparency(),
  settingsOpen: false,

  toggleDetail() {
    const next = !get().detailCollapsed;
    writeFlag(COLLAPSED_KEY, next);
    set({ detailCollapsed: next });
  },

  showDetail() {
    if (!get().detailCollapsed) return;
    writeFlag(COLLAPSED_KEY, false);
    set({ detailCollapsed: false });
  },

  toggleMissionControl() {
    const next = !get().missionControl;
    writeFlag(MISSION_KEY, next);
    set({ missionControl: next });
  },

  toggleTags() {
    const next = !get().tagsCollapsed;
    writeFlag(TAGS_KEY, next);
    set({ tagsCollapsed: next });
  },

  toggleAreas() {
    const next = !get().areasCollapsed;
    writeFlag(AREAS_SECTION_KEY, next);
    set({ areasCollapsed: next });
  },

  toggleShowHidden() {
    const next = !get().showHidden;
    writeFlag(SHOW_HIDDEN_KEY, next);
    set({ showHidden: next });
  },

  expandAreas() {
    if (!get().areasCollapsed) return;
    writeFlag(AREAS_SECTION_KEY, false);
    set({ areasCollapsed: false });
  },

  toggleArea(id) {
    const open = get().collapsedAreas;
    writeAreas(
      open.includes(id) ? open.filter((a) => a !== id) : [...open, id],
      set
    );
  },

  expandArea(id) {
    const open = get().collapsedAreas;
    if (!open.includes(id)) return;
    writeAreas(open.filter((a) => a !== id), set);
  },

  toggleLaunchConsole() {
    // Not written to storage — see `launchCollapsed`. It is scoped to the task
    // you are looking at, and every task open resets it.
    set({ launchCollapsed: !get().launchCollapsed });
  },

  setDensity(density) {
    try {
      localStorage.setItem(DENSITY_KEY, density);
    } catch {
      // A write failure only costs the preference across restarts.
    }
    applyDensity(density);
    set({ density });
  },

  setSort(sort) {
    try {
      localStorage.setItem(SORT_KEY, JSON.stringify(sort));
    } catch {
      // A write failure only costs the preference across restarts.
    }
    set({ sort });
  },

  saveViewFilters(key, filters) {
    writeViewFilters({ ...get().viewFilters, [key]: filters.map((f) => ({ ...f })) }, set);
  },

  forgetViewFilters(key) {
    const next = { ...get().viewFilters };
    delete next[key];
    writeViewFilters(next, set);
  },

  setGroupBy(groupBy) {
    try {
      localStorage.setItem(GROUP_KEY, groupBy);
    } catch {
      // A write failure only costs the preference across restarts.
    }
    set({ groupBy });
  },

  setTheme(theme) {
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      // A write failure only costs the preference across restarts.
    }
    applyTheme(theme, get().vaultLook);
    set({ theme });
  },

  setVaultLook(palette) {
    const { theme, vaultLook } = get();
    if (!palette) {
      // Onyx is down or has no palette. Keep wearing the last good one —
      // restarting Onyx or quitting Obsidian must not repaint the window.
      set({ vaultLookSource: vaultLook ? "cache" : "none" });
      return;
    }
    // Polled every minute: an unchanged palette must not repaint anything.
    if (vaultLook !== null && samePalette(vaultLook, palette)) {
      set({ vaultLookSource: "onyx" });
      return;
    }
    try {
      localStorage.setItem(VAULT_LOOK_KEY, JSON.stringify(palette));
    } catch {
      // A write failure only costs the palette across restarts.
    }
    if (theme === "vault") applyTheme("vault", palette);
    set({ vaultLook: palette, vaultLookSource: "onyx" });
  },

  toggleTheme() {
    // Flips what is PAINTED: from the vault's light palette the header's moon
    // means "dark", so the toggle leaves the vault for the built-in opposite.
    get().setTheme(resolveTheme(get().theme) === "dark" ? "light" : "dark");
  },

  setTransparency(value) {
    const next = Math.min(1, Math.max(0, value));
    try {
      localStorage.setItem(WINDOW_KEY, String(next));
    } catch {
      // A write failure only costs the preference across restarts.
    }
    // Paint first, store second. This runs on every frame of a slider drag, and
    // the DOM write is what the user is actually watching; putting the Zustand
    // set first would re-render the dialog before the window updates.
    applyWindow(next, paintedTheme);
    set({ transparency: next });
  },

  setSettingsOpen(open) {
    set({ settingsOpen: open });
  },

  setViewOrder(order) {
    try {
      localStorage.setItem(VIEW_ORDER_KEY, JSON.stringify(order));
    } catch {
      // A write failure only costs the preference across restarts.
    }
    set({ viewOrder: order });
  },
}));
