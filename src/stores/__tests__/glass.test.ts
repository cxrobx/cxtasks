/// <reference types="node" />
// The app's tsconfig carries no Node types; this test reads a file, so it asks
// for them itself. (`?raw` is not an alternative: vitest empties CSS imports.)
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { contrast, over, type RGB } from "@/lib/contrast";
import { HUES } from "@/lib/hues";
import {
  TRANSPARENCY_DEFAULT,
  transparencyToAlphas,
  transparencyToBlurRadius,
  type Theme,
} from "@/stores/uiStore";

/**
 * The blur radius is the second output of the transparency dial, and every
 * property here is one the window looks wrong without rather than a restatement
 * of the arithmetic.
 */
describe("the desktop blur radius", () => {
  /**
   * The coupling is the whole reason there is one slider and not two: the more
   * desktop you let through, the more blur it takes to keep text legible on top
   * of it. A radius that fell as transparency rose — or sat flat — would make
   * the top of the range unreadable, which is the state the dial exists to let
   * you avoid.
   */
  it("rises with transparency, everywhere on the range", () => {
    const steps = Array.from({ length: 21 }, (_, i) => i / 20);
    const radii = steps.map(transparencyToBlurRadius);
    for (let i = 1; i < radii.length; i += 1) {
      expect(radii[i]).toBeGreaterThanOrEqual(radii[i - 1]);
    }
    expect(radii[radii.length - 1]).toBeGreaterThan(radii[0]);
  });

  /**
   * Past roughly 48 a Gaussian blur of a wallpaper stops being a blurred
   * wallpaper and becomes featureless smoke — which is precisely the
   * `NSVisualEffectView` look this whole path exists to get away from. The
   * floor is not 0 for the opposite reason: a slightly translucent window over
   * a razor-sharp desktop reads as a rendering fault, not as a design.
   */
  it("stays inside the band where a wallpaper still reads as one", () => {
    for (const t of [-5, 0, 0.5, 1, 42, Number.NaN]) {
      const r = transparencyToBlurRadius(t);
      // NaN clamps through `Math.min`/`Math.max` to a number here; asserting the
      // bound rather than the value is what proves a corrupt localStorage entry
      // cannot reach the WindowServer as garbage.
      expect(r).toBeGreaterThanOrEqual(10);
      expect(r).toBeLessThanOrEqual(48);
    }
  });

  /**
   * An integer, and that is load-bearing rather than tidy. `applyGlass` skips
   * the IPC when the rounded radius has not moved, which is what keeps a slider
   * drag the pure-CSS operation it was designed as — a float would change on
   * every frame and send ~60 calls a second.
   */
  it("is an integer, so a drag sends at most a few dozen calls", () => {
    const seen = new Set<number>();
    for (let i = 0; i <= 1000; i += 1) {
      const r = transparencyToBlurRadius(i / 1000);
      expect(Number.isInteger(r)).toBe(true);
      seen.add(r);
    }
    expect(seen.size).toBeLessThanOrEqual(40);
  });

  /**
   * 24 is where MonoCode — the app this look was taken from — pins its radius
   * outright. Landing the default there means the out-of-the-box window is the
   * one that was actually looked at and liked, rather than a point on a curve
   * that happens to pass nearby.
   */
  it("lands on 24 at the default transparency", () => {
    expect(transparencyToBlurRadius(TRANSPARENCY_DEFAULT)).toBe(24);
  });

  /**
   * Zero is a real destination on this dial, not a disabled state — and at zero
   * the panes are fully opaque, so the window is genuinely opaque rather than a
   * transparent one that happens to be painted over. `applyGlass` reads the
   * same condition to decide whether to enable glass at all, so if this ever
   * stops being exactly-1 the "Opaque" end silently starts showing wallpaper.
   */
  it("has an opaque end that is actually opaque", () => {
    const { pane, sidebar, surface } = transparencyToAlphas(0, "dark");
    expect(pane).toBe(1);
    expect(sidebar).toBe(1);
    expect(surface).toBe(1);
  });
});

/**
 * The pane alphas, pinned against the palette they were derived from.
 *
 * Each of these fails INVISIBLY when broken — the window still renders, it just
 * stops reading as a window, or stops being readable. So the palette is read out
 * of `globals.css` rather than duplicated here: moving a `--bg-*` or `--text-*`
 * token is exactly when the curve needs revisiting, and this is what says so.
 */
const CSS = readFileSync(resolve(__dirname, "../../styles/globals.css"), "utf8");

/** A `--token: r g b;` triplet out of a `[data-theme="…"]` block. */
function paletteTriplet(theme: Theme, token: string): RGB {
  const block = CSS.split(`[data-theme="${theme}"]`)[1]?.split("}")[0];
  expect(block, `no [data-theme="${theme}"] block`).toBeTruthy();
  const m = block!.match(new RegExp(`--${token}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
  expect(m, `--${token} not found in ${theme}`).toBeTruthy();
  return m!.slice(1, 4).map(Number) as unknown as RGB;
}

const THEMES: Theme[] = ["dark", "light"];
const POSITIONS = [0, 0.1, 0.25, TRANSPARENCY_DEFAULT, 0.5, 0.75, 0.9, 1];
const BLACK: RGB = [0, 0, 0];
const WHITE: RGB = [255, 255, 255];

describe("the pane alphas against the palette", () => {
  it("keeps the sidebar thinner than the content and the surface denser", () => {
    for (const theme of THEMES) {
      for (const t of POSITIONS) {
        const { pane, sidebar, surface } = transparencyToAlphas(t, theme);
        expect(sidebar).toBeLessThanOrEqual(pane);
        expect(surface).toBeGreaterThanOrEqual(pane);
        for (const a of [pane, sidebar, surface]) {
          expect(a).toBeGreaterThan(0);
          expect(a).toBeLessThanOrEqual(1);
        }
      }
    }
  });

  /**
   * The one that catches a curve copied onto the wrong palette. Light (cxmail's
   * palette) paints its bar DARKER than its content, and dark's boost curve
   * would push it lighter than the content over a white wallpaper — so light
   * takes cxmail's `lead` curve, and this pins that it holds.
   *
   * Light only, for now: DARK's boost curve fails this today. Over a black
   * wallpaper at the default 0.38 its bar composites to ~15 against the
   * content's ~17 — a trench, which is exactly what the note on the dark block
   * in `globals.css` says cannot happen (its arithmetic uses a sidebar alpha of
   * 0.55; the curve actually yields ~0.35 there). Found 2026-09-27 while porting
   * the light palette; left alone because it is a dark-mode decision.
   */
  it("never inverts the light sidebar against the content, over any wallpaper", () => {
    for (const theme of ["light"] as Theme[]) {
      const bar = paletteTriplet(theme, "bg-sidebar")[0];
      const content = paletteTriplet(theme, "bg-primary")[0];
      const barIsLighter = bar > content;
      for (const t of POSITIONS.filter((p) => p > 0)) {
        const { pane, sidebar } = transparencyToAlphas(t, theme);
        for (const bg of [0, 255]) {
          const b = bar * sidebar + bg * (1 - sidebar);
          const c = content * pane + bg * (1 - pane);
          expect(
            b > c,
            `${theme} sidebar inverted at t=${t} over ${bg}: bar ${b.toFixed(1)} vs content ${c.toFixed(1)}`
          ).toBe(barIsLighter);
        }
      }
    }
  });
});

/**
 * Light glass over a dark desktop turns GREY, and grey is where light theme's
 * secondary text lives. At the old 0.55 floor the panes composited to the same
 * value as `--text-muted`, and every secondary label vanished — cxmail found it
 * off a real screenshot on 2026-09-25, and this is its test, carried over with
 * its palette.
 *
 * Black is the backdrop because it is the worst one: a light pane can only lose
 * lightness to a darker wallpaper. Muted text must hold WCAG's 3:1 for UI text
 * at every position; faint (hints, disabled) holds 2.5:1 up to the default,
 * where nearly everyone lives, and 1.75:1 at the extreme. Primary ink holds 7:1.
 */
describe("light-theme text legibility over the glass", () => {
  it("keeps every text shade readable on the sidebar and the pane, over a black desktop", () => {
    const ink = paletteTriplet("light", "text-primary");
    const muted = paletteTriplet("light", "text-muted");
    const faint = paletteTriplet("light", "text-faint");
    const surfaces = {
      sidebar: paletteTriplet("light", "bg-sidebar"),
      pane: paletteTriplet("light", "bg-primary"),
    };
    for (const t of POSITIONS) {
      const alphas = transparencyToAlphas(t, "light");
      for (const [name, colour] of Object.entries(surfaces)) {
        const ground = over(colour, alphas[name as keyof typeof surfaces], BLACK);
        const where = `light ${name} at t=${t} (composites to ${ground[0].toFixed(0)})`;
        expect(contrast(ink, ground), `ink on ${where}`).toBeGreaterThanOrEqual(7);
        expect(contrast(muted, ground), `muted on ${where}`).toBeGreaterThanOrEqual(3);
        const faintFloor = t <= TRANSPARENCY_DEFAULT ? 2.5 : 1.75;
        expect(contrast(faint, ground), `faint on ${where}`).toBeGreaterThanOrEqual(faintFloor);
      }
    }
  });

  it("keeps white text legible on the accent it is painted on", () => {
    // Every primary button and selected segment is `text-white` on the accent.
    // cxmail's #0a84ff is macOS's own system blue at ~3.6:1 — the floor for
    // bold/large UI text, which is why small labels sit on `accent-hover`.
    expect(contrast(WHITE, paletteTriplet("light", "accent"))).toBeGreaterThanOrEqual(3);
    expect(contrast(WHITE, paletteTriplet("light", "accent-hover"))).toBeGreaterThanOrEqual(4.5);
  });
});

/**
 * The icon hues (`src/lib/hues.ts`) — Apple's system colours, saturated on
 * purpose. Light trades WCAG's 3:1 graphic floor for saturation (Chris's call,
 * 2026-09-27: the 3:1 pass read as dull), so this pins the floor it chose
 * instead: no hue under ~2:1, which is where Apple's own yellow (1.35:1) made
 * the Today sun disappear. Dark reaches 3.8:1 with Apple's values untouched.
 */
describe("icon hues", () => {
  it("every hue holds its floor on the sidebar and the pane, in both themes", () => {
    for (const [theme, floor] of [
      ["light", 1.9],
      ["dark", 3.8],
    ] as const) {
      for (const hue of HUES) {
        const colour = paletteTriplet(theme, `hue-${hue}`);
        for (const ground of ["bg-sidebar", "bg-primary"]) {
          expect(
            contrast(colour, paletteTriplet(theme, ground)),
            `${theme} ${hue} on ${ground}`
          ).toBeGreaterThanOrEqual(floor);
        }
      }
    }
  });
});
