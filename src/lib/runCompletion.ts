export interface RunCompletion {
  status: "ready_for_review" | "needs_action" | "needs_verification";
  verified: string[];
  remaining: string[];
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
    return { text, completion: report };
  } catch {
    return { text, completion: null };
  }
}
