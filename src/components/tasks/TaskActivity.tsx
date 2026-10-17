import { useEffect, useState } from "react";
import { Bot, ClipboardPaste, GitMerge, ImagePlus, Send, Trash2, User, X, Zap } from "lucide-react";
import { api } from "@/lib/tauri";
import { useImagePaste } from "@/hooks/useImagePaste";
import { cn } from "@/lib/utils";
import { Markdown } from "@/components/shared/Markdown";
import { formatAge, parseDbTimestamp, RUN_STATUS_CLASS, RUN_STATUS_LABELS } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";
import { EXECUTOR_LABELS, type Comment, type Task, type TaskRun } from "@/types/task";
import { AttachmentGrid, pickImagePaths } from "@/components/tasks/TaskAttachments";
import { CancelRunButton } from "@/components/tasks/CancelRunButton";
import { RunCheckpoint } from "@/components/tasks/RunCheckpoint";
import { RunResult } from "@/components/tasks/RunResult";
import { RunTimeline } from "@/components/tasks/RunTimeline";

/**
 * The task's history: comments AND runs, on one feed.
 *
 * They were two headed sections — Runs, then Comments — and they are one
 * chronology split in two: `finish_run` writes the full handoff onto the run
 * row and a summary into the comment log, so "what did the run conclude" and
 * "what did Chris say about it" were read in separate lists that each had to
 * be reconciled against the other's timestamps. Interleaved, a review sits
 * right above the work it critiques.
 *
 * Still deliberately NOT the same thing as Notes: Notes is the task's
 * *definition* and is edited in place; this is append-only *history* with an
 * author. Agent-written entries stay visually distinct so "what did the run
 * conclude" never gets confused with "what did Chris decide".
 *
 * **Newest first**, with the composer above the feed so a post lands where you
 * typed it. The latest entry is what you open a task to read, and a log that
 * grows downward buries it further with every entry — the call
 * `project_updates` makes for the same reason. Reversed HERE and never in
 * `db::comments::for_task`, which must stay chronological: `dispatch::
 * history_block` and the MCP's `get_task` both take the newest N and render
 * them oldest-first within that window, so an agent reads a transcript in the
 * order it happened. Flipping the query would silently hand both of them a
 * backwards history.
 */
export function TaskActivity({ task, runs }: { task: Task; runs: TaskRun[] }) {
  const comments = useTaskStore((s) => s.comments);
  const entries = activityEntries(comments, runs);
  const latestWork = runs.find((run) => run.intent === "work" && run.finished_at)?.id;
  return (
    <section className="flex flex-col gap-3">
      <CommentComposer task={task} />
      {entries.length > 0 && (
        <ul className="flex flex-col gap-3">
          {entries.map((e) =>
            e.kind === "run" ? (
              <RunEntry key={`r-${e.run.id}`} run={e.run} task={task} latestWork={latestWork} />
            ) : (
              <CommentEntry key={`c-${e.comment.id}`} comment={e.comment} />
            )
          )}
        </ul>
      )}
    </section>
  );
}

type Entry =
  | { kind: "comment"; comment: Comment; at: number; seq: number }
  | { kind: "run"; run: TaskRun; at: number; seq: number };

/**
 * Newest first, across both sources. Never mutates either array.
 *
 * The two arrive in OPPOSITE orders — comments chronological (see above),
 * runs newest-first (`db::runs::list_for_task`) — so the tiebreak for equal
 * timestamps is each entry's position in its own source, turned to mean
 * "later" the same way for both. Comments carry millisecond timestamps but can
 * still collide (gotcha #12), and a tie must not reorder them between renders.
 */
export function activityEntries(comments: Comment[], runs: TaskRun[]): Entry[] {
  const time = (raw: string) => parseDbTimestamp(raw)?.getTime() ?? 0;
  const entries: Entry[] = [
    ...comments.map((comment, i) => ({ kind: "comment" as const, comment, at: time(comment.created_at), seq: i })),
    ...runs.map((run, i) => ({ kind: "run" as const, run, at: time(run.started_at), seq: runs.length - i })),
  ];
  return entries.sort((a, b) => b.at - a.at || b.seq - a.seq);
}

/** "3h ago", with the exact time one hover away. */
function When({ at }: { at: string }) {
  const age = formatAge(at);
  return (
    <span title={formatCommentTimestamp(at)}>
      {age === null ? at : age === "now" ? "just now" : `${age} ago`}
    </span>
  );
}

/** The left rail of every entry — who, or what, wrote it. */
function Glyph({ agent, children }: { agent: boolean; children: React.ReactNode }) {
  return (
    <span
      className={cn(
        "mt-px flex size-5 shrink-0 items-center justify-center rounded-full",
        agent ? "bg-accent/14 text-accent" : "bg-content/10 text-content-muted"
      )}
    >
      {children}
    </span>
  );
}

function CommentEntry({ comment: c }: { comment: Comment }) {
  const attachments = useTaskStore((s) => s.attachments);
  const removeComment = useTaskStore((s) => s.removeComment);
  const removeAttachment = useTaskStore((s) => s.removeAttachment);
  const isAgent = c.author !== "me";
  return (
    <li className="group flex gap-2">
      <Glyph agent={isAgent}>{isAgent ? <Bot size={11} /> : <User size={11} />}</Glyph>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 text-[10.5px] text-content-faint">
          <span className={cn("font-medium", isAgent ? "text-accent" : "text-content-secondary")}>
            {c.author}
          </span>
          <When at={c.created_at} />
          <button
            type="button"
            aria-label="Delete comment"
            onClick={() => void removeComment(c.id)}
            className="ml-auto self-center opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100"
          >
            <Trash2 size={10} className="hover:text-error" />
          </button>
        </div>
        {/* 12px, a step under the pane's body text: in one feed with run
            results (11px, `RunResult`) the comment shouldn't shout over them. */}
        {c.body && <Markdown className="text-[12px]">{c.body}</Markdown>}
        <AttachmentGrid
          attachments={attachments.filter((a) => a.comment_id === c.id)}
          onRemove={(id) => void removeAttachment(id)}
        />
      </div>
    </li>
  );
}

/**
 * A run, as an entry in the same feed. Everything the old Runs card carried
 * stays — the timeline, the handoff, the checkpoint, Stop — only the frame
 * changed.
 *
 * A critique must not read identically to the work it critiques. `mode`
 * stays — mode (fg/bg) and intent (work/review) are orthogonal, and the
 * timeline still gates on `mode === "background"`. The reviewing AGENT is
 * named only for reviews, because "which agent said this" is load-bearing
 * precisely there; this matches the comment header Rust writes ("Background
 * review · Codex"), so the two surfaces agree.
 */
function RunEntry({ run: r, task, latestWork }: { run: TaskRun; task: Task; latestWork?: string }) {
  const review = r.intent === "review";
  const [continuing, setContinuing] = useState(false);
  const canContinue = latestWork === r.id && (r.status === "needs_action" || r.status === "unverified") &&
    !task.deleted_at && !task.archived_at && task.status !== "done" && task.status !== "in_progress" && task.status !== "queued";
  return (
    <li className="flex gap-2">
      <Glyph agent>{review ? <GitMerge size={10} /> : <Zap size={10} />}</Glyph>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 text-[10.5px] text-content-faint">
          <span className="font-medium text-content-secondary">{review ? "background review" : r.mode}</span>
          {review && <span>{EXECUTOR_LABELS[r.executor]}</span>}
          {/* Both maps are shared with Mission Control (`RUN_STATUS_LABELS`),
              so a new status lands on both surfaces or on neither. */}
          <span className={RUN_STATUS_CLASS[r.status]}>{RUN_STATUS_LABELS[r.status] ?? r.status}</span>
          <When at={r.started_at} />
          {/* Only while it can do something. A Stop that is present but inert
              on a finished run is worse than no Stop at all — it is the
              control you reach for in a hurry. */}
          {r.status === "running" && (
            <span className="ml-auto self-center">
              <CancelRunButton runId={r.id} />
            </span>
          )}
        </div>
        {r.mode === "background" && <RunTimeline run={r} />}
        {r.result_text && <RunResult>{r.result_text}</RunResult>}
        {canContinue && (
          <button type="button" disabled={continuing}
            className="mt-2 rounded border border-border-subtle px-2 py-1 text-[11px] text-content-secondary hover:bg-content/6 disabled:opacity-50"
            onClick={async () => {
              setContinuing(true);
              try {
                await api.dispatch.foreground(r.task_id);
                await useTaskStore.getState().refresh();
                if (useTaskStore.getState().selectedId === r.task_id) await useTaskStore.getState().select(r.task_id);
              } catch (error) {
                useTaskStore.setState({ error: String(error) });
              } finally { setContinuing(false); }
            }}>
            {continuing ? "Opening…" : "Continue in Claude"}
          </button>
        )}
        <RunCheckpoint run={r} />
      </div>
    </li>
  );
}

/**
 * One line at rest; it grows while it holds text, and Post appears only then —
 * a disabled Post button is a control that says nothing until you have typed.
 */
export function CommentComposer({ task }: { task: Task }) {
  const addComment = useTaskStore((s) => s.addComment);
  const [draft, setDraft] = useState("");
  const [clipboardImages, setClipboardImages] = useState<number[][]>([]);
  const [imagePaths, setImagePaths] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const ready = (draft.trim().length > 0 || imagePaths.length > 0 || clipboardImages.length > 0) && !busy;

  const submit = async () => {
    if (!ready) return;
    setBusy(true);
    const created = await addComment(task.id, draft, imagePaths, clipboardImages);
    setBusy(false);
    if (created) {
      setDraft("");
      setImagePaths([]);
      setClipboardImages([]);
    }
  };

  const pasteImage = async (quiet = false) => {
    if (busy) return;
    setBusy(true);
    try {
      const bytes = quiet ? await api.attachments.tryReadClipboard() : await api.attachments.readClipboard();
      if (!bytes) return;
      setClipboardImages((current) => [...current, bytes]);
    } catch (error) {
      useTaskStore.setState({ error: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const imagePaste = useImagePaste(pasteImage);

  const chooseImages = async () => {
    const picked = await pickImagePaths();
    if (picked.length > 0) setImagePaths((current) => [...new Set([...current, ...picked])]);
  };

  return (
    // `cx-notes` moves the focus ring onto this box: the generic textarea
    // ring is unlayered and would otherwise draw a second box inside it.
    <div data-image-paste className="cx-notes rounded-lg border border-border bg-input">
      <div className="flex items-end gap-1 px-1.5 py-1">
        <textarea
          value={draft}
          disabled={busy}
          onPaste={imagePaste.onPaste}
          rows={draft.includes("\n") || draft.length > 48 ? 3 : 1}
          aria-label="Add a comment"
          placeholder="Comment…  ⌘↩ posts"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            imagePaste.onKeyDown(e);
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void submit();
            }
          }}
          // `resize-none`: the row count follows the text, so a grip would
          // fight it.
          className="min-w-0 flex-1 resize-none bg-transparent px-1 py-0.5 leading-relaxed text-content placeholder:text-content-faint"
        />
        <button
          type="button"
          aria-label="Add images"
          title="Add images"
          disabled={busy}
          onClick={() => void chooseImages()}
          className="rounded p-1 text-content-faint hover:bg-content/8 hover:text-content disabled:opacity-40"
        >
          <ImagePlus size={13} />
        </button>
        <button
          type="button"
          aria-label="Paste image"
          title="Paste an image from the clipboard"
          disabled={busy}
          onClick={() => void pasteImage()}
          className="rounded p-1 text-content-faint hover:bg-content/8 hover:text-content disabled:opacity-40"
        >
          <ClipboardPaste size={13} />
        </button>
        {ready && (
          <button
            type="button"
            aria-label="Post"
            title="Post (⌘↩)"
            onClick={() => void submit()}
            className="rounded bg-accent p-1 text-white hover:bg-accent-hover"
          >
            <Send size={12} />
          </button>
        )}
      </div>
      {clipboardImages.map((bytes, index) => (
        <ClipboardImagePreview key={index} bytes={bytes} disabled={busy}
          onRemove={() => setClipboardImages((current) => current.filter((_, i) => i !== index))} />
      ))}
      {imagePaths.length > 0 && (
        <ul className="flex flex-wrap gap-1.5 px-2 pb-1.5">
          {imagePaths.map((path) => {
            const name = path.split(/[\\/]/).pop() ?? "image";
            return (
              <li
                key={path}
                className="flex max-w-full items-center gap-1 rounded bg-content/6 px-1.5 py-1 text-[10px] text-content-secondary"
              >
                <span className="max-w-52 truncate">{name}</span>
                <button
                  type="button"
                  disabled={busy}
                  aria-label={`Remove ${name}`}
                  onClick={() => setImagePaths((current) => current.filter((item) => item !== path))}
                  className="text-content-faint hover:text-content"
                >
                  <X size={10} />
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

export function formatCommentTimestamp(value: string): string {
  const match = /^(\d{4}-\d{2}-\d{2}) (\d{2}):(\d{2}):(\d{2})(?:\.\d+)?$/.exec(value);
  if (!match) return value;

  const [, date, rawHour, minute, second] = match;
  const hour = Number(rawHour);
  if (hour > 23) return value;
  return `${date} ${hour % 12 || 12}:${minute}:${second} ${hour < 12 ? "AM" : "PM"}`;
}

function ClipboardImagePreview({ bytes, disabled, onRemove }: {
  bytes: number[]; disabled: boolean; onRemove: () => void;
}) {
  const [url, setUrl] = useState<string>();
  useEffect(() => {
    const preview = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
    setUrl(preview);
    return () => URL.revokeObjectURL(preview);
  }, [bytes]);
  return (
    <div className="flex items-center gap-2 px-2 pb-2">
      <img src={url} alt="Clipboard image" className="h-20 max-w-40 rounded object-contain" />
      <button type="button" aria-label="Remove clipboard image" disabled={disabled} onClick={onRemove}
        className="text-content-faint hover:text-content disabled:opacity-40"><X size={13} /></button>
    </div>
  );
}
