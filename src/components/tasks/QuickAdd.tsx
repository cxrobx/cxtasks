import { forwardRef, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check, Folder, Plus, Sparkles } from "lucide-react";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";

// One budget for the entire assessment, including any provider fallback.
const CAPTURE_ENRICHMENT_BUDGET_MS = 1000;

/**
 * The capture bar. Inline syntax is deliberately tiny:
 *   `#project`   `!0`–`!3` priority   `@today` `@tomorrow` `@fri` due date
 * Anything richer belongs in the detail pane — full natural-language date
 * parsing was explicitly cut from v1.
 */
export const QuickAdd = forwardRef<HTMLTextAreaElement>(function QuickAdd(_props, ref) {
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const submitting = useRef(false);
  const [confirmation, setConfirmation] = useState<{
    taskId: string;
    message: string;
  } | null>(null);
  const quickAdd = useTaskStore((s) => s.quickAdd);
  const activeProject = useTaskStore((s) => s.activeProject);
  const projects = useTaskStore((s) => s.projects);
  const allTags = useTaskStore((s) => s.allTags);
  const showDetail = useUiStore((s) => s.showDetail);

  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    const resize = () => {
      input.style.height = "auto";
      input.style.height = `${input.scrollHeight}px`;
    };
    resize();
    // Only width changes need reflow. Updating height inside the observer
    // changes the form being observed and causes a ResizeObserver loop in
    // WKWebView, so apply the new height on the next animation frame.
    let width: number | undefined;
    let frame = 0;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry || entry.contentRect.width === width) return;
      width = entry.contentRect.width;
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(resize);
    });
    if (input.parentElement) observer.observe(input.parentElement);
    return () => {
      observer.disconnect();
      window.cancelAnimationFrame(frame);
    };
  }, [value, activeProject]);

  useEffect(() => {
    if (!confirmation) return;
    const timer = window.setTimeout(() => setConfirmation(null), 3000);
    return () => window.clearTimeout(timer);
  }, [confirmation]);

  const submit = async () => {
    if (!value.trim() || submitting.current) return;
    submitting.current = true;
    setBusy(true);
    const input = value;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      let suggestion = null;
      try {
        suggestion = await Promise.race([
          api.jev.assessCapture({
            text: input,
            projects: projects.map((p) => p.project),
            tags: allTags.map((t) => t.name),
          }),
          new Promise<null>((resolve) => {
            timer = setTimeout(() => resolve(null), CAPTURE_ENRICHMENT_BUDGET_MS);
          }),
        ]);
      } catch {
        // Capture is durable; enrichment is optional. Race also handles late
        // rejection, and late suggestions never write back to the task.
      } finally {
        clearTimeout(timer);
      }
      const created = await quickAdd(input, suggestion);
      if (!created) return;

      setValue("");
      showDetail();
      setConfirmation({
        taskId: created.id,
        message: created.project
          ? `Created in ${created.project} — details opened`
          : "Created in Inbox — details opened",
      });
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };

  return (
    <div className="border-b border-border-subtle px-3 py-2">
      <form
        aria-label="Create a task"
        className="flex items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {busy ? (
          <Sparkles size={14} className="shrink-0 animate-pulse text-accent" />
        ) : (
          <Plus size={14} className="shrink-0 text-accent" />
        )}
        <textarea
          ref={(element) => {
            inputRef.current = element;
            if (typeof ref === "function") return ref(element);
            if (ref) ref.current = element;
          }}
          rows={1}
          // Read-only, not disabled: disabling a focused field drops focus,
          // and type-Enter-type-Enter is this bar's main gesture.
          readOnly={busy}
          aria-busy={busy}
          value={value}
          aria-label="Task title"
          placeholder={activeProject ? `New task in ${activeProject}` : "New task title"}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submit();
            }
            if (e.key === "Escape" && !submitting.current) {
              setValue("");
              e.currentTarget.blur();
            }
          }}
          className="min-w-0 flex-1 resize-none overflow-hidden bg-transparent [overflow-wrap:anywhere] placeholder:text-content-faint focus:outline-none"
        />
        {activeProject && (
          <span
            className="flex max-w-40 shrink-0 items-center gap-1 truncate rounded bg-content/6 px-1.5 py-0.5 text-[11px] text-content-faint"
            title={`This task will be added to ${activeProject}`}
          >
            <Folder size={10} />
            <span className="truncate">{activeProject}</span>
          </span>
        )}
        <button
          type="submit"
          disabled={!value.trim() || busy}
          className="shrink-0 rounded-md bg-accent px-2.5 py-1 text-[12px] font-medium text-white transition-colors hover:bg-accent-hover disabled:cursor-default disabled:opacity-40"
        >
          {busy ? "Creating…" : "Create task"}
        </button>
      </form>
      <div aria-live="polite" className="min-h-0">
        {confirmation && (
          <p className="mt-1.5 flex items-center gap-1 pl-[22px] text-[11px] text-success">
            <Check size={11} />
            {confirmation.message}
          </p>
        )}
      </div>
    </div>
  );
});
