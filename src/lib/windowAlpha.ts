/**
 * The window-transparency dial as pure arithmetic: one slider value in, the
 * three pane alphas out.
 *
 * Lives outside `uiStore` so the vault palette (`vaultLook.ts`) can reason about
 * the same alphas the window will actually paint without importing the store —
 * the store imports the palette, and a cycle there would evaluate one module
 * half-built. `uiStore` re-exports everything here, so existing imports are
 * unchanged.
 */

/** A theme that can actually be painted. The PREFERENCE (`vault` included) is
 * `ThemePreference` in `uiStore`, and must be resolved before it reaches any of
 * the per-theme tables below. */
export type Theme = "dark" | "light";

/**
 * How much of the desktop shows through the window, 0 → 1.
 *
 * A continuous amount rather than named presets. Presets were the first cut and
 * were wrong: which value is right depends entirely on the wallpaper behind the
 * window, so the useful range is a dial the user turns until it looks right,
 * not three points someone else picked.
 *
 * A separate axis from `Theme` rather than a fourth theme, because it composes
 * with both — "dark and nearly opaque" and "light and nearly glass" are equally
 * valid.
 *
 * 0 is a real destination, not a disabled state: transparency over a busy
 * wallpaper can make the board genuinely hard to read, and the way back has to
 * be a drag away.
 */
export const TRANSPARENCY_DEFAULT = 0.38;

/**
 * The floor each theme's pane alpha is allowed to reach at full transparency.
 *
 * Light needs a much higher floor than dark, and this is not a fudge factor.
 * Light glass over a DARK desktop does not read as airy, it reads as grey: at
 * the old 0.55 the panes let half a near-black wallpaper through and landed on
 * the same value as the muted text, so every secondary label vanished. WKWebView
 * cannot use the brightening blend modes Apple's light materials rely on, so
 * the only lever is how much backdrop gets through. 0.82 is cxmail's value,
 * found the same way on 2026-09-25, and it is what keeps muted text at 3:1 over
 * a black desktop at the top of the slider — pinned by the legibility test in
 * `glass.test.ts`, which is the thing to consult before lowering it.
 */
export const PANE_FLOOR: Record<Theme, number> = { dark: 0.25, light: 0.82 };

/**
 * DARK's sidebar curve: the bar runs FAR more transparent than the content.
 *
 * This is the character of the Codex window and the reason its bar reads as
 * lighter than the work area — not a lighter paint colour, but a much thinner
 * one, letting the blurred desktop lift it. Copying only the hex and keeping
 * the alphas close (the first attempt) produced a bar four values off the
 * content, which is to say no bar at all.
 *
 * `BOOST` multiplies the slider before it reaches the sidebar, so the bar
 * arrives at its glassy end well before the content does; `FLOOR` is how thin
 * it may ever get.
 *
 * Boosting the TRANSPARENCY rather than scaling the resulting alpha is what
 * keeps the opaque end honest: scaling the alpha (`pane * 0.5`) leaves the
 * sidebar half-transparent even with the slider at 0, so "Opaque" wouldn't be.
 * Here t=0 gives 1 for both, and the two curves separate as the slider moves.
 *
 * It holds only because dark's bar is painted 1.75× lighter than its content
 * (42 vs 24). A palette where the two are close — cxmail's light one, or any
 * vault palette — is inverted into a trench by this curve, which is why those
 * take `SIDEBAR_LEAD` instead.
 */
const SIDEBAR_BOOST = 1.9;
const SIDEBAR_FLOOR = 0.1;

/**
 * cxmail's sidebar curve, used by light and by every vault palette: the bar
 * LEADS the content's alpha by a small fraction of it, so it is always a little
 * thinner and never far enough to invert against the content.
 *
 * Light's bar is painted darker than its content (243 vs 248), and in light
 * every point of lead is more dark desktop behind the sidebar's muted text, so
 * its lead is set by legibility: 0.05. Dark's 0.12 is cxmail's bound for a bar
 * only 1.25× lighter than its content — the worst case a vault's dark palette is
 * likely to hand over.
 */
export const SIDEBAR_LEAD: Record<Theme, number> = { dark: 0.12, light: 0.05 };

export type SidebarCurve = "boost" | "lead";

export interface AlphaOptions {
  /** Replaces the theme's `PANE_FLOOR` — a vault palette whose text cannot
   * survive the theme's floor gets a higher one (`deriveVaultTheme`). */
  floor?: number;
  /** Defaults to `boost` for dark and `lead` for light; a vault palette always
   * passes `lead`. */
  curve?: SidebarCurve;
}

/**
 * Turn one slider value into the three pane alphas.
 *
 * Three relationships hold at every position, which is why this is a formula
 * and not three sliders. SIDEBAR always sits below the content — see above.
 * SURFACE always sits above it: it backs the detail pane and the Mission
 * Control cards, which lie *on* the content, so a card has to stay denser than
 * the thing it lies on or the depth inverts. And at t=0 all three are 1, so
 * the opaque end of the slider is genuinely opaque everywhere.
 *
 * A non-finite input is treated as 0 rather than passed through: NaN survives
 * `Math.min`/`Math.max`, and a NaN alpha paints a pane fully TRANSPARENT.
 */
export function transparencyToAlphas(
  transparency: number,
  theme: Theme,
  { floor = PANE_FLOOR[theme], curve = theme === "dark" ? "boost" : "lead" }: AlphaOptions = {}
) {
  const t = Number.isFinite(transparency) ? Math.min(1, Math.max(0, transparency)) : 0;
  const pane = 1 - t * (1 - floor);
  const sidebar =
    curve === "boost"
      ? 1 - Math.min(1, t * SIDEBAR_BOOST) * (1 - SIDEBAR_FLOOR)
      : pane * (1 - t * SIDEBAR_LEAD[theme]);
  return {
    pane,
    sidebar,
    surface: pane + (1 - pane) * 0.5,
  };
}
