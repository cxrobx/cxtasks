/**
 * Reflow for prose that arrived as one block.
 *
 * Most notes are written by an agent through `file_task`, and the common shape
 * is a single paragraph of six or eight sentences: correct, dense, and a wall
 * on screen (the 2026-08-22 expense-ledger note that prompted this ran to 900
 * characters with no line break). Markdown cannot help with that — there is no
 * structure to render — so this splits such a paragraph at sentence boundaries
 * and regroups the sentences into paragraphs of a readable length.
 *
 * It is a RENDER-TIME transform. The stored body is never rewritten: click
 * into the note and you get the text exactly as it was saved. That is also
 * why it is conservative — a wrong split shows for one glance and costs
 * nothing, but the transform must never touch a block the author structured.
 * So it only reflows a paragraph that is one physical line (an author who
 * pressed Return already chose their breaks), has no markdown structure, and
 * is longer than a paragraph anyone would want to read whole.
 */

/** A paragraph shorter than this is left exactly as written. */
export const LONG_PARAGRAPH = 320;

/** Sentences are grouped until the group would pass this. */
export const TARGET_PARAGRAPH = 220;

/**
 * Tokens a period does NOT end a sentence after. Matched against the word
 * before the period, case-insensitively. Single capital initials (`J. Smith`)
 * are handled separately.
 */
const ABBREVIATIONS = new Set([
  "e.g",
  "i.e",
  "vs",
  "etc",
  "cf",
  "approx",
  "dr",
  "mr",
  "mrs",
  "ms",
  "st",
  "no",
  "inc",
  "ltd",
  "co",
  "jr",
  "sr",
  "u.s",
  "fig",
  "ch",
  "p",
  "pp",
  "ca",
  "min",
  "max",
  "est",
]);

/** Lines that open a markdown block other than a plain paragraph. */
const STRUCTURED_LINE =
  /^(\s{4,}|\t|#{1,6}\s|[-*+]\s|\d+[.)]\s|>|\||<|```|~~~|[-*_]{3,}\s*$|\[[^\]]+\]:\s)/;

/** A fence opener or closer — everything between two of them is left alone. */
const FENCE = /^\s*(```|~~~)/;

/**
 * Split one line of prose into sentences. A boundary is terminal punctuation,
 * optionally followed by a closing quote or bracket, then whitespace, then a
 * capital letter, digit, or opening quote/bracket. Lower-case after a period
 * is not a boundary (`e.g. the` / `sync_expenses. it`), which is what keeps
 * most abbreviations and code identifiers whole without a list at all.
 */
export function splitSentences(line: string): string[] {
  const out: string[] = [];
  const re = /[.!?]+["')\]]?\s+(?=["'(\[]?[A-Z0-9])/g;
  let start = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line)) !== null) {
    const end = m.index + m[0].length;
    const head = line.slice(start, m.index);
    // The word before the punctuation, minus any trailing period so `e.g.`
    // and `e.g` compare the same.
    const word = (head.match(/(\S+)$/)?.[1] ?? "").replace(/\.$/, "").toLowerCase();
    const isInitial = /^[A-Z]$/.test(head.match(/(\S+)$/)?.[1] ?? "");
    if (ABBREVIATIONS.has(word) || isInitial) continue;
    out.push(line.slice(start, end).trimEnd());
    start = end;
  }
  const tail = line.slice(start).trim();
  if (tail) out.push(tail);
  return out;
}

/** Regroup sentences into paragraphs that stop around `TARGET_PARAGRAPH`. */
function regroup(sentences: string[]): string[] {
  const paras: string[] = [];
  let current = "";
  for (const s of sentences) {
    if (current && current.length + 1 + s.length > TARGET_PARAGRAPH) {
      paras.push(current);
      current = s;
    } else {
      current = current ? `${current} ${s}` : s;
    }
  }
  if (current) paras.push(current);
  return paras;
}

/**
 * Reflow every long, single-line, unstructured paragraph in a markdown
 * source. Everything else — fences, lists, tables, headings, HTML, and any
 * paragraph the author broke into lines themselves — passes through
 * byte-for-byte.
 */
export function shapeProse(md: string): string {
  const lines = md.split("\n");
  const out: string[] = [];
  let inFence = false;
  let para: string[] = [];

  const flush = () => {
    if (para.length === 0) return;
    if (
      para.length === 1 &&
      para[0].length > LONG_PARAGRAPH &&
      !STRUCTURED_LINE.test(para[0])
    ) {
      const sentences = splitSentences(para[0]);
      if (sentences.length > 1) {
        out.push(regroup(sentences).join("\n\n"));
        para = [];
        return;
      }
    }
    out.push(...para);
    para = [];
  };

  for (const line of lines) {
    if (FENCE.test(line)) {
      flush();
      inFence = !inFence;
      out.push(line);
      continue;
    }
    if (inFence) {
      out.push(line);
      continue;
    }
    if (line.trim() === "") {
      flush();
      out.push(line);
      continue;
    }
    para.push(line);
  }
  flush();
  return out.join("\n");
}
