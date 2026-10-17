/**
 * Colour arithmetic for the glass: what a translucent layer composites to, and
 * whether text on it can be read.
 *
 * One implementation, shared by the vault palette (`vaultLook.ts`) and the
 * legibility tests, so the maths that picks the colours is the maths that
 * checks them.
 */

export type RGB = readonly [number, number, number];

/** WCAG relative luminance of an sRGB colour. */
export function luminance([r, g, b]: RGB): number {
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** WCAG contrast ratio, 1 → 21, order-independent. */
export function contrast(a: RGB, b: RGB): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** `a` moved toward `b` by `t` (0 = a, 1 = b). */
export function mix(a: RGB, b: RGB, t: number): RGB {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
}

/** A layer of `colour` at `alpha` over an opaque `backdrop`. */
export function over(colour: RGB, alpha: number, backdrop: RGB): RGB {
  return mix(backdrop, colour, alpha);
}

/** `"r g b"`, rounded — the shape every `--bg-*`/`--text-*` token takes. */
export function triplet(colour: RGB): string {
  return colour.map((v) => Math.round(v)).join(" ");
}
