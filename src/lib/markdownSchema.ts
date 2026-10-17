import { defaultSchema, type Options as SanitizeSchema } from "rehype-sanitize";
import allowlist from "../../crates/cxtasks-core/src/markdown_allowlist.json";

/**
 * What embedded HTML in a task's prose may mount.
 *
 * Notes, comments and run results are written by agents — often unattended —
 * so raw HTML used to be dropped outright. That kept `<script>` and
 * `<img onerror>` out of the DOM but also threw away every inline diagram: the
 * Onyx artifacts Chris reads are built from inline SVG and a handful of styled
 * `<div>`s, and a note pasted from one rendered as escaped angle brackets.
 *
 * This schema is the middle: GitHub's allowlist (`defaultSchema`) widened to
 * inline SVG and `style` attributes, with the things that execute or escape
 * the box still stripped. Concretely, what stays OUT:
 *
 * - `script`, `iframe`, `object`, `embed`, `form`, `<style>` blocks and every
 *   `on*` handler — none are in the allowlist, and sanitize drops what it does
 *   not list. `foreignObject` is out for the same reason: it re-enters HTML
 *   from inside SVG, and `<use href>` is out because it can reach an external
 *   document.
 * - `javascript:` and `data:` URLs on `href` / `src` / `xlink:href` — the
 *   protocol lists below are the whole set that survives.
 *
 * `style` ATTRIBUTES are allowed on purpose. A diagram is nothing without
 * `fill`, `stroke` and a little inline layout, and a style attribute cannot run
 * code; the worst it can do is paint over the pane, which a resize or a click
 * into the editor undoes. `<style>` ELEMENTS stay out because a selector can
 * reach outside the note.
 *
 * `class` is allowed too, so the artifact kit's markup keeps its hooks, and the
 * kit's colour tokens are mapped onto the app theme in `Markdown.tsx` — an SVG
 * that paints with `var(--clay)` themes itself here without knowing it moved.
 *
 * The tag AND attribute lists are shared with the MCP's render check, which
 * warns an agent at write time about anything this schema will drop. One list,
 * so the warning and the pane cannot disagree — see the JSON's own comment.
 */
type Entry = NonNullable<NonNullable<SanitizeSchema["attributes"]>[string]>[number];
const nameOf = (e: Entry): string => (typeof e === "string" ? e : String(e[0]));

const SVG_TAGS = new Set(allowlist.svg);
const shared = allowlist.attributes as Record<string, string[]>;
const defaults = defaultSchema.attributes ?? {};

/**
 * Attributes per tag: rehype-sanitize's own entries FIRST, because some carry
 * value constraints the JSON cannot express (`input` may only be a checkbox,
 * `code`'s class only `language-*`), then every name in the shared list that
 * the defaults don't already cover. The JSON is a superset of the defaults'
 * names (pinned by the test), so the names this allows are exactly the JSON's.
 */
function attributesFor(tag: string): Entry[] {
  const own = defaults[tag] ?? [];
  const have = new Set(own.map(nameOf));
  const wanted = [...(shared[tag] ?? []), ...(SVG_TAGS.has(tag) ? allowlist.svgAttributes : [])];
  return [...own, ...wanted.filter((n) => !have.has(n))];
}

const ATTRIBUTE_TAGS = new Set([
  ...Object.keys(defaults),
  ...Object.keys(shared),
  ...allowlist.svg,
]);

/** Everything sanitize lets through — the schema is data, so it is exported for the test. */
export const markdownSchema: SanitizeSchema = {
  ...defaultSchema,
  tagNames: [...allowlist.html, ...allowlist.svg],
  // `href` on SVG is only `textPath`'s, and the protocol table below still
  // refuses anything but http(s)/mailto, so `<use href="evil.svg#x">`-style
  // reaches are moot: `use` isn't a tag here at all.
  attributes: Object.fromEntries([...ATTRIBUTE_TAGS].map((t) => [t, attributesFor(t)])),
  protocols: {
    ...defaultSchema.protocols,
    href: ["http", "https", "mailto"],
    src: ["http", "https"],
  },
  strip: ["script", "style"],
  // `defaultSchema` prefixes ids with `user-content-`, which breaks
  // `marker-end="url(#a)"` inside an SVG: the marker's id moves, the reference
  // doesn't. Diagrams are the reason HTML is mounted at all, so clobbering is
  // off. What the prefix guarded against — a note's `id` shadowing one of the
  // app's own — is moot here: the app addresses nothing by id.
  clobber: [],
  clobberPrefix: "",
};
