import { describe, it, expect, afterEach, vi } from "vitest";
import { contrast, type RGB } from "@/lib/contrast";
import {
  deriveVaultTheme,
  glassGrounds,
  GLASS_TEXT_FLOORS,
  parseVaultPalette,
  samePalette,
  VAULT_VARS,
  type VaultPalette,
} from "@/lib/vaultLook";
import { PANE_FLOOR, TRANSPARENCY_DEFAULT } from "@/lib/windowAlpha";
import { applyTheme, resolveTheme, useUiStore } from "@/stores/uiStore";

/**
 * The vault palette on CXTasks' glass — cxmail's `vaultLook.test.ts`, ported.
 *
 * Onyx mixes its text shades for an opaque window, and CXTasks' is not, so the
 * light shades are re-mixed here to hold the floors the built-in palette is
 * pinned to (`glass.test.ts`). The fixture is what Onyx actually served on
 * 2026-09-25 (and still serves, 2026-09-27) for the AnuPpuccin vault in light
 * mode.
 */

const ANUPPUCCIN_LIGHT: VaultPalette = {
  mode: "light",
  revision: "469ad111e7f682141d2c",
  bgPrimary: [253, 246, 227],
  bgSidebar: [253, 246, 227],
  bgSurface: [241, 234, 210],
  bgElevated: [253, 246, 227],
  bgInput: [244, 237, 214],
  ink: [0, 43, 54],
  secondary: [68, 98, 101],
  muted: [121, 140, 137],
  faint: [164, 175, 166],
  accent: [203, 75, 22],
  accentHover: [152, 67, 30],
};

/** Ink that passes Onyx's own 4.5:1 on an opaque ground but cannot hold muted's
 * 3:1 once the glass lets a black desktop through at the usual floor. */
const WEAK_INK: VaultPalette = { ...ANUPPUCCIN_LIGHT, bgPrimary: [250, 250, 250], bgSidebar: [250, 250, 250], ink: [110, 110, 110] };

const DARK: VaultPalette = {
  ...ANUPPUCCIN_LIGHT,
  mode: "dark",
  bgPrimary: [30, 30, 46],
  bgSidebar: [24, 24, 37],
  bgSurface: [49, 50, 68],
  bgElevated: [36, 36, 54],
  bgInput: [36, 36, 54],
  ink: [205, 214, 244],
  secondary: [186, 194, 222],
  muted: [147, 153, 178],
  faint: [108, 112, 134],
  accent: [137, 180, 250],
  accentHover: [116, 199, 236],
};

const POSITIONS = [0, 0.1, 0.25, TRANSPARENCY_DEFAULT, 0.5, 0.75, 0.9, 1];
const rgb = (v: string | undefined): RGB => v!.split(" ").map(Number) as unknown as RGB;

describe("deriveVaultTheme", () => {
  it("holds every light text floor at every slider position, over a black desktop", () => {
    for (const palette of [ANUPPUCCIN_LIGHT, WEAK_INK]) {
      const v = deriveVaultTheme(palette);
      const shades = {
        secondary: rgb(v.vars["--text-secondary"]),
        muted: rgb(v.vars["--text-muted"]),
        faint: rgb(v.vars["--text-faint"]),
      };
      for (const [name, floors] of Object.entries(GLASS_TEXT_FLOORS)) {
        for (const [upTo, ratio] of floors) {
          for (const t of POSITIONS.filter((p) => p <= upTo)) {
            for (const ground of glassGrounds(palette, t, v.paneFloor)) {
              // Secondary's 6:1 is Onyx's opaque target and the vault's ink may
              // not reach it — then the shade IS the ink, which is the best
              // that palette has. Everything else must hold outright.
              const shade = shades[name as keyof typeof shades];
              if (name === "secondary" && shade.join() === palette.ink.join()) continue;
              expect(
                contrast(shade, ground),
                `${palette === WEAK_INK ? "weak ink" : "anuppuccin"} ${name} at t=${t}`,
              ).toBeGreaterThanOrEqual(ratio);
            }
          }
        }
      }
    }
  });

  it("keeps muted text at 3:1 even for ink that cannot, by raising that palette's floor", () => {
    const usual = deriveVaultTheme(ANUPPUCCIN_LIGHT);
    expect(usual.paneFloor).toBe(PANE_FLOOR.light);

    const weak = deriveVaultTheme(WEAK_INK);
    expect(weak.paneFloor).toBeGreaterThan(PANE_FLOOR.light);
    const muted = rgb(weak.vars["--text-muted"]);
    for (const ground of glassGrounds(WEAK_INK, 1, weak.paneFloor)) {
      expect(contrast(muted, ground)).toBeGreaterThanOrEqual(3);
    }
    // …and no higher than it needs: one step less would fail.
    const below = glassGrounds(WEAK_INK, 1, weak.paneFloor - 0.01);
    expect(below.some((g) => contrast(WEAK_INK.ink, g) < 3)).toBe(true);
  });

  it("darkens light shades beyond Onyx's opaque ones, never lightens them", () => {
    const v = deriveVaultTheme(ANUPPUCCIN_LIGHT);
    for (const [token, onyx] of [
      ["--text-muted", ANUPPUCCIN_LIGHT.muted],
      ["--text-faint", ANUPPUCCIN_LIGHT.faint],
    ] as const) {
      expect(contrast(rgb(v.vars[token]), ANUPPUCCIN_LIGHT.bgPrimary)).toBeGreaterThanOrEqual(
        contrast(onyx, ANUPPUCCIN_LIGHT.bgPrimary) - 0.05,
      );
    }
  });

  it("takes Onyx's shades and the dark floor as they are in dark mode", () => {
    const v = deriveVaultTheme(DARK);
    expect(v.paneFloor).toBe(PANE_FLOOR.dark);
    expect(v.vars["--text-muted"]).toBe("147 153 178");
    expect(v.vars["--text-faint"]).toBe("108 112 134");
  });

  it("uses the vault's accent only when it can carry white text", () => {
    const orange = deriveVaultTheme(ANUPPUCCIN_LIGHT);
    expect(orange.accentFromVault).toBe(true);
    expect(orange.vars["--accent"]).toBe("203 75 22");

    // Catppuccin's pastel blue reads fine as text on its dark ground, but white
    // on it is ~2:1 — a primary button nobody could read.
    const pastel = deriveVaultTheme(DARK);
    expect(pastel.accentFromVault).toBe(false);
    expect(pastel.vars["--accent"]).toBeUndefined();
    expect(pastel.vars["--accent-hover"]).toBeUndefined();
  });

  it("only ever sets variables it knows how to clear", () => {
    for (const p of [ANUPPUCCIN_LIGHT, WEAK_INK, DARK]) {
      for (const name of Object.keys(deriveVaultTheme(p).vars)) {
        expect(VAULT_VARS as readonly string[]).toContain(name);
      }
    }
  });

  it("paints the window the vault's ground when glass is off", () => {
    expect(deriveVaultTheme(ANUPPUCCIN_LIGHT).base).toEqual([253, 246, 227]);
  });
});

describe("parseVaultPalette", () => {
  it("accepts what the Rust command returns", () => {
    expect(parseVaultPalette(JSON.parse(JSON.stringify(ANUPPUCCIN_LIGHT)))).toEqual(ANUPPUCCIN_LIGHT);
  });

  it("refuses anything that is not three integers 0-255 per colour", () => {
    for (const bad of [
      { ...ANUPPUCCIN_LIGHT, ink: [0, 43, 256] },
      { ...ANUPPUCCIN_LIGHT, ink: [0, 43] },
      { ...ANUPPUCCIN_LIGHT, ink: [0, 43, 54.5] },
      { ...ANUPPUCCIN_LIGHT, ink: ["0", 43, 54] },
      { ...ANUPPUCCIN_LIGHT, ink: "0 43 54" },
      { ...ANUPPUCCIN_LIGHT, mode: "sepia" },
      { ...ANUPPUCCIN_LIGHT, accent: undefined },
      null,
      "palette",
    ]) {
      expect(parseVaultPalette(bad)).toBeNull();
    }
  });

  it("drops a revision that is not a hash rather than refusing the palette", () => {
    expect(parseVaultPalette({ ...ANUPPUCCIN_LIGHT, revision: "</style>" })?.revision).toBe("");
  });

  it("tells a changed palette from an unchanged one", () => {
    expect(samePalette(ANUPPUCCIN_LIGHT, { ...ANUPPUCCIN_LIGHT })).toBe(true);
    expect(samePalette(ANUPPUCCIN_LIGHT, { ...ANUPPUCCIN_LIGHT, faint: [164, 175, 167] })).toBe(false);
  });
});

describe("wearing the vault palette", () => {
  const root = document.documentElement;
  const inline = (name: string) => root.style.getPropertyValue(name);
  afterEach(() => {
    applyTheme("dark", null);
    useUiStore.setState({ theme: "dark", vaultLook: null, vaultLookSource: "none" });
    localStorage.clear();
    vi.unstubAllGlobals();
  });

  it("puts the tokens on <html> under `vault`, and takes every one off again", () => {
    useUiStore.setState({ theme: "vault", vaultLook: ANUPPUCCIN_LIGHT });
    applyTheme("vault", ANUPPUCCIN_LIGHT);
    expect(root.getAttribute("data-theme")).toBe("light");
    expect(inline("--bg-primary")).toBe("253 246 227");
    expect(inline("--accent")).toBe("203 75 22");

    applyTheme("light");
    for (const name of VAULT_VARS) expect(inline(name), name).toBe("");
  });

  it("does not let one palette's accent outlive it into the next", () => {
    applyTheme("vault", ANUPPUCCIN_LIGHT);
    expect(inline("--accent")).toBe("203 75 22");
    applyTheme("vault", DARK);
    expect(inline("--accent")).toBe("");
    expect(root.getAttribute("data-theme")).toBe("dark");
  });

  it("wears nothing under an explicit theme even when a palette is held", () => {
    applyTheme("dark", ANUPPUCCIN_LIGHT);
    expect(inline("--bg-primary")).toBe("");
    expect(root.getAttribute("data-theme")).toBe("dark");
  });

  it("paints the app's default (dark) while `vault` has no palette yet", () => {
    useUiStore.setState({ vaultLook: null });
    expect(resolveTheme("vault")).toBe("dark");
    useUiStore.setState({ vaultLook: DARK });
    expect(resolveTheme("vault")).toBe("dark");
    useUiStore.setState({ vaultLook: ANUPPUCCIN_LIGHT });
    expect(resolveTheme("vault")).toBe("light");
  });

  it("paints the raised floor for a palette that needs one", () => {
    localStorage.setItem("cxtasks.transparency", "1");
    applyTheme("vault", WEAK_INK);
    const floor = deriveVaultTheme(WEAK_INK).paneFloor;
    expect(floor).toBeGreaterThan(PANE_FLOOR.light);
    expect(Number(inline("--alpha-pane"))).toBeCloseTo(floor, 5);
  });

  it("keeps the last palette when Onyx stops answering, and repaints only on a change", () => {
    useUiStore.setState({ theme: "vault", vaultLook: null });
    const { setVaultLook } = useUiStore.getState();
    setVaultLook(ANUPPUCCIN_LIGHT);
    expect(useUiStore.getState().vaultLookSource).toBe("onyx");
    expect(inline("--bg-primary")).toBe("253 246 227");

    setVaultLook(null);
    expect(useUiStore.getState().vaultLook).toEqual(ANUPPUCCIN_LIGHT);
    expect(useUiStore.getState().vaultLookSource).toBe("cache");
    expect(inline("--bg-primary")).toBe("253 246 227");

    // Same palette again: the stored object is not replaced, so nothing
    // subscribed to it re-renders on the minute poll.
    const held = useUiStore.getState().vaultLook;
    setVaultLook({ ...ANUPPUCCIN_LIGHT });
    expect(useUiStore.getState().vaultLook).toBe(held);
  });
});

describe("the persisted copy", () => {
  it("is re-validated on launch — a tampered palette reads as none", async () => {
    localStorage.setItem("cxtasks.vaultLook", JSON.stringify(ANUPPUCCIN_LIGHT));
    vi.resetModules();
    let store = (await import("@/stores/uiStore")).useUiStore.getState();
    expect(store.vaultLook).toEqual(ANUPPUCCIN_LIGHT);
    expect(store.vaultLookSource).toBe("cache");

    localStorage.setItem(
      "cxtasks.vaultLook",
      JSON.stringify({ ...ANUPPUCCIN_LIGHT, ink: "0 0 0;} body{display:none" })
    );
    vi.resetModules();
    store = (await import("@/stores/uiStore")).useUiStore.getState();
    expect(store.vaultLook).toBeNull();
    expect(store.vaultLookSource).toBe("none");
    localStorage.clear();
  });

  it("is written when Onyx serves a new palette", () => {
    useUiStore.getState().setVaultLook(ANUPPUCCIN_LIGHT);
    expect(JSON.parse(localStorage.getItem("cxtasks.vaultLook")!)).toEqual(ANUPPUCCIN_LIGHT);
    useUiStore.setState({ vaultLook: null, vaultLookSource: "none" });
    localStorage.clear();
  });

  it("restores `vault` as the theme preference", async () => {
    localStorage.setItem("cxtasks.theme", "vault");
    vi.resetModules();
    expect((await import("@/stores/uiStore")).readTheme()).toBe("vault");
    localStorage.setItem("cxtasks.theme", "sepia");
    expect((await import("@/stores/uiStore")).readTheme()).toBe("dark");
    localStorage.clear();
  });
});

describe("the header toggle", () => {
  afterEach(() => {
    applyTheme("dark", null);
    useUiStore.setState({ theme: "dark", vaultLook: null, vaultLookSource: "none" });
    localStorage.clear();
  });

  it("flips what is PAINTED, leaving the vault for the built-in opposite", () => {
    useUiStore.setState({ theme: "vault", vaultLook: ANUPPUCCIN_LIGHT });
    useUiStore.getState().toggleTheme();
    expect(useUiStore.getState().theme).toBe("dark");
    useUiStore.getState().toggleTheme();
    expect(useUiStore.getState().theme).toBe("light");
  });
});
