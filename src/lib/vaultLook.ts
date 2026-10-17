/**
 * CXTasks in the Obsidian vault's colours — ported from cxmail's
 * `vaultLook.ts`, which is the reference; keep the two in step.
 *
 * The palette comes from Onyx (`fetch_vault_look` on the Rust side, which
 * validates every value into `[u8; 3]`); this module turns it into CXTasks' own
 * tokens. Onyx derived its palette for an OPAQUE window, and CXTasks' is glass,
 * so two things are re-derived here rather than taken as given:
 *
 * 1. **Text shades, in light mode.** Light glass over a dark desktop composites
 *    grey, and Onyx's muted shade holds 3.2:1 against the vault's
 *    ground — which is 3.2:1 only while nothing shows through. So secondary,
 *    muted and faint are re-mixed from the vault's ink toward its ground until
 *    they hold the same floors the built-in palette is pinned to, measured
 *    against the darkest ground the glass can produce: a black desktop at the
 *    top of the slider (`GLASS_TEXT_FLOORS`).
 * 2. **The glass floor.** If even the vault's ink cannot hold muted's 3:1 there,
 *    the palette gets a higher `paneFloor` — the window goes more opaque for
 *    that palette instead of the text going unreadable.
 *
 * Dark mode takes Onyx's shades as they are. Its failure is the mirror image —
 * light text on a pane washed out by a WHITE wallpaper — and the built-in dark
 * palette has the same open question; answering it for one and not the other
 * would make the vault theme the odd one out.
 */

import { contrast, mix, over, triplet, type RGB } from "@/lib/contrast";
import { PANE_FLOOR, transparencyToAlphas, TRANSPARENCY_DEFAULT, type Theme } from "@/lib/windowAlpha";

/** What `fetch_vault_look` returns — Onyx's tokens, validated. */
export interface VaultPalette {
  mode: Theme;
  /** Onyx's hash of the palette; "" when Onyx sent none. */
  revision: string;
  bgPrimary: RGB;
  bgSidebar: RGB;
  bgSurface: RGB;
  bgElevated: RGB;
  bgInput: RGB;
  ink: RGB;
  secondary: RGB;
  muted: RGB;
  faint: RGB;
  accent: RGB;
  accentHover: RGB;
}

const COLOUR_KEYS = [
  "bgPrimary",
  "bgSidebar",
  "bgSurface",
  "bgElevated",
  "bgInput",
  "ink",
  "secondary",
  "muted",
  "faint",
  "accent",
  "accentHover",
] as const;

/**
 * A palette from anywhere untrusted — the IPC result, or the copy persisted in
 * localStorage, which is hand-editable — or `null`.
 *
 * Rust already validated the IPC copy; this is the same check again for the
 * persisted one, because every value here ends up in a CSS custom property.
 */
export function parseVaultPalette(value: unknown): VaultPalette | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (v.mode !== "light" && v.mode !== "dark") return null;
  const out: Record<string, unknown> = {
    mode: v.mode,
    revision: typeof v.revision === "string" && /^[0-9a-f]{0,64}$/i.test(v.revision) ? v.revision : "",
  };
  for (const key of COLOUR_KEYS) {
    const c = v[key];
    if (
      !Array.isArray(c) ||
      c.length !== 3 ||
      !c.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)
    ) {
      return null;
    }
    out[key] = [c[0], c[1], c[2]] as const;
  }
  return out as unknown as VaultPalette;
}

/** Same palette, colour for colour — the poll's "nothing changed" test. */
export function samePalette(a: VaultPalette, b: VaultPalette): boolean {
  return (
    a.mode === b.mode &&
    a.revision === b.revision &&
    COLOUR_KEYS.every((k) => a[k].every((c, i) => c === b[k][i]))
  );
}

/**
 * Every CSS variable the vault look may set. Clearing walks this list, so a
 * variable set by one palette can never outlive it into the next — or into the
 * built-in theme after "Match vault" is turned off.
 */
export const VAULT_VARS = [
  "--bg-primary",
  "--bg-sidebar",
  "--bg-surface",
  "--bg-elevated",
  "--bg-input",
  "--border-default",
  "--border-subtle",
  "--text-primary",
  "--text-secondary",
  "--text-muted",
  "--text-faint",
  "--accent",
  "--accent-hover",
  "--selected",
] as const;

export type VaultVar = (typeof VAULT_VARS)[number];

/**
 * The contrast each light text shade must hold, as `[slider position, ratio]`
 * pairs, against the vault's sidebar and pane composited over a BLACK desktop.
 *
 * The same floors `glass.test.ts` pins for the built-in palette,
 * so a vault theme is held to exactly what the default is. Position 0 carries
 * Onyx's own opaque targets (`vault_look.SHADES`) so no shade comes out lighter
 * than Onyx itself would have drawn it. Over black the ground only darkens as
 * the slider rises, so the highest position listed covers every one below it.
 */
export const GLASS_TEXT_FLOORS: Record<"secondary" | "muted" | "faint", [number, number][]> = {
  secondary: [
    [0, 6.0],
    [1, 4.5],
  ],
  muted: [
    [0, 3.2],
    [1, 3.0],
  ],
  faint: [
    [0, 2.1],
    [TRANSPARENCY_DEFAULT, 2.5],
    [1, 1.75],
  ],
};

const BLACK: RGB = [0, 0, 0];
const WHITE: RGB = [255, 255, 255];

/** The sidebar and pane as they composite over black at slider `t`. */
export function glassGrounds(p: VaultPalette, t: number, floor: number): RGB[] {
  const a = transparencyToAlphas(t, "light", { floor, curve: "lead" });
  return [over(p.bgSidebar, a.sidebar, BLACK), over(p.bgPrimary, a.pane, BLACK)];
}

function holds(colour: RGB, floors: [number, number][], p: VaultPalette, floor: number): boolean {
  return floors.every(([t, ratio]) =>
    glassGrounds(p, t, floor).every((ground) => contrast(colour, ground) >= ratio),
  );
}

/**
 * The lightest mix of ink toward ground that still holds every floor — the
 * vault's own ink when nothing lighter does.
 *
 * Checked on the ROUNDED colour, since that is what reaches CSS; checking the
 * float could pass at 3.001 and ship 2.998.
 */
function shade(p: VaultPalette, floors: [number, number][], floor: number): RGB {
  const round = (c: RGB): RGB => [Math.round(c[0]), Math.round(c[1]), Math.round(c[2])];
  let best: RGB = p.ink;
  for (let step = 1; step <= 200; step += 1) {
    const candidate = round(mix(p.ink, p.bgPrimary, step / 200));
    if (!holds(candidate, floors, p, floor)) break;
    best = candidate;
  }
  return best;
}

/**
 * The glass floor this palette needs: the theme's own, raised only as far as
 * it takes for the vault's INK to hold muted's floors. (If ink holds them, some
 * shade between ink and ground does too; if ink cannot, no shade can.)
 */
function lightFloor(p: VaultPalette): number {
  let floor = PANE_FLOOR.light;
  while (floor < 1 && !holds(p.ink, GLASS_TEXT_FLOORS.muted, p, floor)) {
    floor = Math.min(1, Math.round((floor + 0.01) * 100) / 100);
  }
  return floor;
}

export interface VaultTheme {
  mode: Theme;
  /** CSS variable → `"r g b"`. Only ever keys from `VAULT_VARS`. */
  vars: Partial<Record<VaultVar, string>>;
  /** The glass floor for this palette (≥ the theme's own). */
  paneFloor: number;
  /** The opaque window colour when glass is off — the vault's ground. */
  base: readonly [number, number, number];
  /** The vault's accent was used (false: it could not carry white text). */
  accentFromVault: boolean;
}

/** The vault palette as CXTasks' tokens, floor and window colour. */
export function deriveVaultTheme(p: VaultPalette): VaultTheme {
  const light = p.mode === "light";
  const paneFloor = light ? lightFloor(p) : PANE_FLOOR.dark;
  const [secondary, muted, faint] = light
    ? [
        shade(p, GLASS_TEXT_FLOORS.secondary, paneFloor),
        shade(p, GLASS_TEXT_FLOORS.muted, paneFloor),
        shade(p, GLASS_TEXT_FLOORS.faint, paneFloor),
      ]
    : [p.secondary, p.muted, p.faint];

  const vars: Partial<Record<VaultVar, string>> = {
    "--bg-primary": triplet(p.bgPrimary),
    "--bg-sidebar": triplet(p.bgSidebar),
    "--bg-surface": triplet(p.bgSurface),
    "--bg-elevated": triplet(p.bgElevated),
    "--bg-input": triplet(p.bgInput),
    // Onyx draws its lines as ink at 14% and 7%. Composited onto the ground
    // here, and written as full colours: CXTasks' border tokens are colours
    // rather than triplets (`--color-border: var(--border-default)`).
    "--border-default": `rgb(${triplet(mix(p.bgPrimary, p.ink, 0.14))})`,
    "--border-subtle": `rgb(${triplet(mix(p.bgPrimary, p.ink, 0.07))})`,
    "--text-primary": triplet(p.ink),
    "--text-secondary": triplet(secondary),
    "--text-muted": triplet(muted),
    "--text-faint": triplet(faint),
    // A selected sidebar row is the vault's ink lifted off the bar, at the
    // same strengths the built-in palettes use (light 0.07, dark 0.10).
    "--selected": `rgb(${triplet(p.ink)} / ${light ? 0.07 : 0.1})`,
  };

  // CXTasks FILLS with its accent (primary buttons, the selected segment of a
  // switch, the queue) and puts white text on it; Onyx only ever uses it as
  // text. So the vault's accent is taken only when it can carry white text AND
  // read on the ground — otherwise the built-in blue stays, which is always
  // legible.
  const accentFromVault =
    contrast(WHITE, p.accent) >= 3 && contrast(p.accent, p.bgPrimary) >= 3;
  if (accentFromVault) {
    vars["--accent"] = triplet(p.accent);
    vars["--accent-hover"] = triplet(p.accentHover);
  }

  return { mode: p.mode, vars, paneFloor, base: p.bgPrimary, accentFromVault };
}
