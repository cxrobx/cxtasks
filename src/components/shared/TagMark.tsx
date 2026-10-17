import { Bot, Scale } from "lucide-react";
import { AGENT_FILED_TAG, DECISION_TAG, tagLabel } from "@/lib/format";
import { cn } from "@/lib/utils";

/**
 * One tag as it reads inline — on a row and in the detail pane's chips.
 *
 * A label is `#billable` and a person is `@sam`, as before. The two SYSTEM
 * tags render as marks instead (`SYSTEM_TAGS` in `lib/format.ts`): `decision`
 * is a warning-tinted "Decision", because it changes who may act on the task
 * (no agent will take it); `agent-filed` is a bot glyph with the meaning in its
 * title, because it is provenance — worth a glance, not a word.
 */
export function TagMark({ name, className }: { name: string; className?: string }) {
  const lower = name.toLowerCase();
  if (lower === DECISION_TAG) {
    return (
      <span
        data-tag-kind="system"
        title="Decision — only Chris can answer this; agents won't pick it up"
        // The caller's className comes FIRST: rows pass their faint text colour,
        // and tailwind-merge keeps the last colour, so the other order painted
        // the mark grey and it read as one more label.
        className={cn(className, "inline-flex items-center gap-0.5 text-warning")}
      >
        <Scale size={10} aria-hidden />
        Decision
      </span>
    );
  }
  if (lower === AGENT_FILED_TAG) {
    return (
      <span
        data-tag-kind="system"
        title="Filed by an agent on its own (agent-filed)"
        aria-label="agent-filed"
        className={cn("inline-flex items-center", className)}
      >
        <Bot size={11} aria-hidden />
      </span>
    );
  }
  return <span className={className}>{tagLabel(name)}</span>;
}
