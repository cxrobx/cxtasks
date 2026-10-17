/**
 * Icon colour — cxmail's coloured group icons: Apple's system colours.
 *
 * A NAMED hue rather than a hex, so the value means something in both themes:
 * each name is a `--hue-*` triplet defined per `[data-theme]` in `globals.css`,
 * Apple's dark-appearance value on dark and its light one on light, so one
 * stored choice renders right in either. A hex would be one colour forced onto
 * two grounds.
 *
 * Only ever an ICON's colour. Labels stay ink: the hues are saturated system
 * colours at ~2:1 on light, which an icon's shape carries and a label never
 * could.
 */

export const HUES = [
  "blue",
  "red",
  "green",
  "orange",
  "amber",
  "purple",
  "teal",
  "pink",
  "sand",
  "grey",
] as const;

export type Hue = (typeof HUES)[number];

/** The palette a name is hashed into. `grey` is left out: it is the colour of
 * "put away" (Archive), and a live project drawn in it reads as dormant. */
const AUTO: readonly Hue[] = HUES.filter((h) => h !== "grey");

function isHue(value: string): value is Hue {
  return (HUES as readonly string[]).includes(value);
}

/**
 * The hue for a project, area or tag.
 *
 * `stored` is the row's `color` column, honoured when it names a hue — nothing
 * writes it yet, so in practice every row is hashed. The hash is of the NAME,
 * lower-cased (names are unique case-insensitively), so a row keeps its colour
 * across launches and machines with nothing stored. A rename recolours it,
 * which is the price of storing nothing.
 *
 * FNV-1a rather than a sum of char codes: a sum gives anagrams — and names that
 * differ only in letter order, like `ops`/`sop` — the same colour.
 */
export function hueFor(name: string, stored?: string | null): Hue {
  if (stored && isHue(stored)) return stored;
  let h = 0x811c9dc5;
  for (const ch of name.toLowerCase()) {
    h ^= ch.codePointAt(0)!;
    h = Math.imul(h, 0x01000193);
  }
  return AUTO[(h >>> 0) % AUTO.length];
}

/** An inline style that paints an icon in `hue`. Inline rather than a
 * `text-hue-*` class built from the name: Tailwind only emits classes it can
 * find spelled out in the source, and a template string is not spelled out. */
export function hueStyle(hue: Hue): { color: string } {
  return { color: `rgb(var(--hue-${hue}))` };
}
