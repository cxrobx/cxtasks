export interface RunQuestion {
  text: string;
  options: { label: string; description?: string }[];
}

export interface RunCompletion {
  status: "ready_for_review" | "needs_action" | "needs_verification";
  verified: string[];
  remaining: string[];
  /** A decision only the person can make (the runner's CONTRACT). Null when
   *  absent or malformed: a bad question never costs the report. */
  question: RunQuestion | null;
}

/** The runner's own first paragraph (`runner::finish_run`), which restates
 *  `remaining` as one run-on line. Kept in storage for the task log and the
 *  Waiting reason; dropped on screen when the report parsed, because "Still
 *  needed" shows the same items as a list. */
const RUNNER_PREFIX = /^(Needs action|Needs verification): [^\n]*(\n\n|$)/;

function readQuestion(value: unknown): RunQuestion | null {
  if (!value || typeof value !== "object") return null;
  const q = value as { text?: unknown; options?: unknown };
  if (typeof q.text !== "string" || !q.text.trim() || q.text.length > 300) return null;
  if (!Array.isArray(q.options) || q.options.length < 2 || q.options.length > 4) return null;
  const options = q.options.map((o) => {
    const opt = (o ?? {}) as { label?: unknown; description?: unknown };
    return typeof opt.label === "string" && opt.label.trim() && opt.label.length <= 60
      ? { label: opt.label.trim(), description: typeof opt.description === "string" ? opt.description : undefined }
      : null;
  });
  return options.every(Boolean) ? { text: q.text.trim(), options: options as RunQuestion["options"] } : null;
}

/** The reply above a `## Details` heading, and the record below it — the two
 *  parts the runner asks for (`handoff_tail`). No heading: all reply. */
export function splitDetails(text: string): { reply: string; details: string | null } {
  const m = /^#{1,6}[ \t]*Details[ \t]*$/im.exec(text);
  if (!m) return { reply: text, details: null };
  const details = text.slice(m.index + m[0].length).trim();
  return { reply: text.slice(0, m.index).trimEnd(), details: details || null };
}

/** Keep the machine footer in stored handoffs, and show its contents as normal UI. */
export function splitRunCompletion(result: string): { text: string; completion: RunCompletion | null } {
  const lines = result.trimEnd().split("\n");
  const last = lines[lines.length - 1].trim();
  if (!last.startsWith("CXTASKS_RESULT:")) return { text: result, completion: null };
  const text = lines.slice(0, -1).join("\n").trimEnd();
  try {
    const report = JSON.parse(last.slice("CXTASKS_RESULT:".length));
    const strings = (value: unknown): value is string[] => Array.isArray(value) &&
      value.length <= 20 && value.every((item) => typeof item === "string" && item.trim().length > 0 && [...item].length <= 1000);
    if (!report || !["ready_for_review", "needs_action", "needs_verification"].includes(report.status) ||
      !strings(report.verified) || !strings(report.remaining)) return { text, completion: null };
    return {
      text: text.replace(RUNNER_PREFIX, "").trimStart() || text,
      completion: { ...report, question: readQuestion(report.question) },
    };
  } catch {
    return { text, completion: null };
  }
}
