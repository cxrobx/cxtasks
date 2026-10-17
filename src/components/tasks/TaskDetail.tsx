import * as Dropdown from "@radix-ui/react-dropdown-menu";
import { Combobox } from "@/components/shared/Combobox";
import { useEffect, useLayoutEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  Archive,
  Check,
  ChevronDown,
  Code2,
  Eye,
  FileText,
  FolderGit2,
  MoreHorizontal,
  Shield,
  Sparkles,
  Terminal,
  Trash2,
  Undo2,
  WandSparkles,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import {
  backgroundTier,
  type BackgroundTier,
  formatShortId,
  lastHandoffRun,
  repoName,
  shortenPath,
  TIER_HINTS,
} from "@/lib/format";
import { ReferenceChip } from "@/components/shared/ReferenceChip";
import { DispatchBar } from "@/components/tasks/DispatchBar";
import { PropertyStrip } from "@/components/tasks/PropertyStrip";
import { DetailTabs } from "@/components/tasks/DetailTabs";
import { NoteAttachments } from "@/components/tasks/TaskAttachments";
import { NotesField } from "@/components/tasks/NotesField";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import {
  BACKGROUND_EXECUTORS,
  RETENTION_DAYS,
  type RepoSuggestion,
  type Task,
  type TaskRun,
} from "@/types/task";

const FIELD =
  "w-full rounded-md border border-border bg-input px-2 py-1 text-content placeholder:text-content-faint";
const LABEL = "text-[11px] font-medium uppercase tracking-wide text-content-faint";

export function TaskDetail() {
  const selectedId = useTaskStore((s) => s.selectedId);
  const tasks = useTaskStore((s) => s.tasks);
  const subtasks = useTaskStore((s) => s.subtasks);
  const runs = useTaskStore((s) => s.runs);

  const task = useMemo(
    () => tasks.find((t) => t.id === selectedId) ?? null,
    [tasks, selectedId]
  );

  if (!task) {
    return (
      <div className="flex h-full items-center justify-center text-content-faint">
        No task selected
      </div>
    );
  }
  return <TaskDetailForm key={task.id} task={task} subtasks={subtasks} runs={runs} />;
}

function TaskDetailForm({
  task,
  subtasks,
  runs,
}: {
  task: Task;
  subtasks: Task[];
  runs: TaskRun[];
}) {
  const patch = useTaskStore((s) => s.patch);
  const restore = useTaskStore((s) => s.restore);
  const unarchive = useTaskStore((s) => s.unarchive);
  const repos = useTaskStore((s) => s.repos);
  const projectRepos = useTaskStore((s) => s.projectRepos);

  // Local draft state for free-text fields so typing isn't an IPC round-trip
  // per keystroke; committed on blur.
  const [title, setTitle] = useState(task.title);
  const [body, setBody] = useState(task.body ?? "");
  const [prompt, setPrompt] = useState(task.prompt ?? "");
  const [repoPath, setRepoPath] = useState(task.repo_path ?? "");
  /**
   * Every task opens with the launch console collapsed.
   *
   * Keyed on `task.id` ALONE, and in its own effect — folding it into the
   * draft reset below would also fire on `updated_at`, which bumps on every
   * patch (including the ones a background run writes), and the console would
   * shut while you were using it.
   *
   * `useLayoutEffect` because this runs before paint: expanding on one task and
   * clicking another would otherwise render the new task's console open for a
   * frame before collapsing it.
   */
  useLayoutEffect(() => {
    useUiStore.setState({ launchCollapsed: true });
  }, [task.id]);

  // A live-refresh from the bridge can replace the task under us.
  useEffect(() => {
    setTitle(task.title);
    setBody(task.body ?? "");
    setPrompt(task.prompt ?? "");
    setRepoPath(task.repo_path ?? "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [task.id, task.updated_at]);

  const commit = (field: string, value: string | null) =>
    void patch(task.id, { [field]: value });

  /**
   * Workspace candidates with this project's OWN repos first.
   *
   * The `~/Projects` scan is a flat alphabetical list of ~25 directories, and
   * the one you want is almost never near the top — nor findable by typing the
   * project's name, since the whole point is that they differ. The registered
   * repos are the answer to "which directory is this task's work in", so they
   * lead; the scan stays underneath for a repo that hasn't been mapped yet.
   */
  const workspaceRepos = useMemo(() => {
    const owned = projectRepos.filter(
      (r) => r.project.toLowerCase() === (task.project ?? "").toLowerCase()
    );
    const seen = new Set(owned.map((r) => r.repo_path.toLowerCase()));
    return [
      ...owned.map((r) => ({ name: r.project, path: r.repo_path })),
      ...repos.filter((r) => !seen.has(r.path.toLowerCase())),
    ];
  }, [repos, projectRepos, task.project]);

  /**
   * The run a review would audit. Derived here, where `runs` lives, and passed
   * down — `DispatchBar` is a leaf and must not reach into the store for the
   * SELECTED task's runs.
   */
  const reviewTarget = useMemo(() => lastHandoffRun(runs), [runs]);

  const reference = formatShortId(task.short_id);

  return (
    // No titlebar padding here: the pane's traffic-light headroom is a real
    // non-scrolling drag strip supplied by `AppLayout`, so that the window stays
    // draggable by this pane's top edge at any scroll position. Padding on the
    // scroller would scroll the drag region away.
    //
    // No `bg-surface` here either, and for the same reason one level along: the
    // detail COLUMN paints it, so that the drag strip above this scroller and
    // the "No task selected" branch are covered by the same layer. See the note
    // in `AppLayout`.
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-y-auto px-4 pb-8">
      {/* Deliberately NOT wrapped in a bordered field: the title input below is
          borderless by design (see its own comment), and giving its label a box
          would make the pane's first two rows disagree with each other. */}
      {/* The reference itself is the copy button — see `ReferenceChip`. The
          glyph stays because this pane has room to say what the click does. */}
      <div className="flex items-center gap-1.5 pt-1">
        {reference && (
          <ReferenceChip
            reference={reference}
            icon
            className="rounded px-0.5 py-0.5 text-[11px] hover:bg-content/8"
          />
        )}
        {!task.deleted_at && !task.archived_at && <TaskMenu task={task} />}
      </div>

      <input
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onBlur={() =>
          title.trim() && title !== task.title && commit("title", title)
        }
        // Every other field in this pane rests inside a hairline border, so the
        // global 2px accent focus ring reads as a refinement on them. This one
        // is borderless and full-width, so the same ring erupts out of nothing
        // and dominates the pane. A hairline that merely *brightens* on focus
        // keeps the indicator (and its keyboard accessibility) while matching
        // the surrounding chrome; the padding stops it hugging the text.
        className="w-full rounded-md border border-transparent bg-transparent px-2 py-1 text-[15px] font-semibold text-content hover:border-border-subtle hover:bg-content/4 focus:bg-input focus-visible:border-accent focus-visible:outline-1 focus-visible:outline-offset-0"
      />

      {/* Only what the task HOLDS, as the row's own chips — every editor is a
          popover, so Notes starts right under the title. See `PropertyStrip`. */}
      <PropertyStrip task={task} />

      {/* A `<section>`, not a `<label>`: a label with no labelable control in
          it forwards a click to nothing, and Notes renders markdown until
          clicked, so the control isn't always there. */}
      <section className="flex flex-col gap-1">
        <span className={LABEL}>Notes</span>
        <NotesField
          value={body}
          stored={task.body ?? ""}
          onPasteImage={(quiet) => useTaskStore.getState().pasteNoteImage(task.id, quiet)}
          onChange={setBody}
          onCommit={(next) => commit("body", next || null)}
        />
        <NoteAttachments task={task} />
      </section>

      <ExecutionConsole
        task={task}
        reviewTarget={reviewTarget}
        repos={workspaceRepos}
        repoPath={repoPath}
        prompt={prompt}
        onRepoPathChange={setRepoPath}
        onPromptChange={setPrompt}
        onRepoCommit={() =>
          repoPath !== (task.repo_path ?? "") &&
          commit("repo_path", repoPath || null)
        }
        onPromptCommit={() =>
          prompt !== (task.prompt ?? "") && commit("prompt", prompt || null)
        }
      />

      <DetailTabs task={task} subtasks={subtasks} runs={runs} />

      {/* A task that's already set aside keeps a visible footer: getting it
          back is the only sensible primary action there, and it must not hide
          behind ⋯. A live task's Archive / Move to Trash moved into that menu
          beside the reference (`TaskMenu`). Permanent deletion is deliberately
          absent here — it lives only in the Trash view's own row action,
          behind a confirm. */}
      {(task.deleted_at || task.archived_at) && (
      <div className="mt-auto flex flex-wrap items-center gap-1">
        {task.deleted_at ? (
          <>
            <button
              type="button"
              onClick={() => void restore(task.id)}
              className="flex items-center gap-1.5 rounded-md px-2 py-1 text-accent hover:bg-accent/10"
            >
              <Undo2 size={13} />
              Put back
            </button>
            <span className="px-1 text-content-faint">
              Deleted {task.deleted_at.slice(0, 10)}
            </span>
          </>
        ) : task.archived_at ? (
          <button
            type="button"
            onClick={() => void unarchive(task.id)}
            className="flex items-center gap-1.5 rounded-md px-2 py-1 text-accent hover:bg-accent/10"
          >
            <Undo2 size={13} />
            Move back to tasks
          </button>
        ) : null}
      </div>
      )}
    </div>
  );
}

/**
 * Archive and Move to Trash, behind ⋯ beside the reference.
 *
 * They were a footer under the comment box, which on a busy task meant
 * scrolling past the whole history to set it aside — and on every task, two
 * more rows of chrome. Both are recoverable (Trash keeps it for
 * `RETENTION_DAYS`, ⌘⌫ still reaches it from the list), which is why a menu
 * is enough; permanent deletion stays out of the pane entirely.
 */
function TaskMenu({ task }: { task: Task }) {
  const archive = useTaskStore((s) => s.archive);
  const trash = useTaskStore((s) => s.trash);
  const item =
    "flex w-full cursor-default select-none items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] text-content-secondary outline-none";
  return (
    <Dropdown.Root>
      <Dropdown.Trigger asChild>
        <button
          type="button"
          aria-label="More actions"
          title="More actions"
          className="ml-auto rounded p-1 text-content-faint outline-none hover:bg-content/8 hover:text-content focus-visible:ring-1 focus-visible:ring-accent data-[state=open]:bg-content/8 data-[state=open]:text-content"
        >
          <MoreHorizontal size={14} />
        </button>
      </Dropdown.Trigger>
      <Dropdown.Portal>
        {/* OPAQUE: menus portal to the window root, where a translucent panel
            is text on raw wallpaper (gotcha #22). */}
        <Dropdown.Content
          align="end"
          sideOffset={4}
          collisionPadding={10}
          className="z-50 w-52 rounded-lg border border-border bg-elevated p-1 shadow-2xl"
        >
          <Dropdown.Item
            title="Keep it, but off the board. Never auto-deleted."
            onSelect={() => void archive(task.id)}
            className={cn(item, "data-[highlighted]:bg-accent data-[highlighted]:text-white")}
          >
            <Archive size={13} />
            Archive
          </Dropdown.Item>
          <Dropdown.Item
            title={`Recoverable from the Trash for ${RETENTION_DAYS} days`}
            onSelect={() => void trash(task.id)}
            className={cn(item, "data-[highlighted]:bg-error data-[highlighted]:text-white")}
          >
            <Trash2 size={13} />
            Move to Trash
            <span className="ml-auto text-[11px] opacity-60">⌘⌫</span>
          </Dropdown.Item>
        </Dropdown.Content>
      </Dropdown.Portal>
    </Dropdown.Root>
  );
}

/** Exported for `TaskContextMenu`, which offers the same three grants from
 *  the right-click menu. One list, or the two surfaces end up describing
 *  different capabilities with the same words. */
export const ACCESS_OPTIONS = [
  {
    value: "read-only",
    label: "Inspect",
    short: "Read only",
    icon: Eye,
  },
  {
    value: "shell",
    label: "Run",
    short: "Shell",
    icon: Terminal,
  },
  {
    value: "write",
    label: "Build",
    short: "Edit files",
    icon: WandSparkles,
  },
] as const;

/**
 * Which rail node's editor is open, or none.
 *
 * `brief` is on the rail but NOT on the route: the accent progress line spans
 * workspace → runner → capability and stops before it, because the seed prompt
 * is a sibling setting rather than a fourth stage of the handoff.
 */
type LaunchSetting = "workspace" | "runner" | "capability" | "brief";

const LAUNCH_NODES: { key: LaunchSetting; label: string; title: string }[] = [
  { key: "workspace", label: "Workspace", title: "Workspace" },
  { key: "runner", label: "Runner", title: "Runner" },
  { key: "capability", label: "Capability", title: "Access envelope" },
  { key: "brief", label: "Brief", title: "Run brief" },
];

/**
 * The unattended-agent handoff as one launch plan rather than a pile of form
 * fields. The production detail pane ranges from 340–420 px. The console owns
 * its inline-size container, so the route and actions respond to the pane
 * itself instead of the much wider app window.
 *
 * **The rail IS the control surface.** Each node is a button that opens its own
 * editor below the rail, one at a time. Before this, the rail stated workspace,
 * runner and capability and then three always-on fieldsets stated the same three
 * things again directly underneath — ~900px of pane for four settings, three of
 * them printed twice. The rail is the summary and the way in; the editor is
 * whichever one you asked for.
 *
 * There is no separate "allow background" switch. Clicking the explicit
 * "Queue for …" action is the opt-in; the backend records that permission in
 * the same write that queues the task. The repo is still a hard prerequisite,
 * and the access envelope still defaults to read-only.
 */
function ExecutionConsole({
  task,
  reviewTarget,
  repos,
  repoPath,
  prompt,
  onRepoPathChange,
  onPromptChange,
  onRepoCommit,
  onPromptCommit,
}: {
  task: Task;
  /** The run a review would audit — see `DispatchBar`, which is where it goes. */
  reviewTarget: TaskRun | null;
  repos: RepoSuggestion[];
  repoPath: string;
  prompt: string;
  onRepoPathChange: (value: string) => void;
  onPromptChange: (value: string) => void;
  onRepoCommit: () => void;
  onPromptCommit: () => void;
}) {
  const collapsed = useUiStore((s) => s.launchCollapsed);
  const toggleCollapsed = useUiStore((s) => s.toggleLaunchConsole);
  const [open, setOpen] = useState<LaunchSetting | null>(null);
  const tier = backgroundTier(task);
  const access = ACCESS_OPTIONS.find((option) => option.value === tier)!;
  const RunnerIcon = task.bg_executor === "claude" ? Sparkles : Code2;
  const hasWorkspace = repoPath.trim().length > 0;
  /**
   * Probed HERE, not inside the runner editor, so the warning survives that
   * editor being closed. "Codex isn't logged in" is only useful while the
   * decision is still free to change; discovered at queue time it is discovered
   * after the fact, which is the failure this check exists to prevent.
   */
  const unavailable = useExecutorAvailability(task);

  /**
   * Leave whichever editor is open, committing its draft on the way out.
   *
   * `repo_path` and `prompt` commit on blur, and blur is NOT guaranteed when a
   * focused input is unmounted — which is exactly what clicking another node or
   * collapsing the console does to it. Miss this and a typed path stays on
   * screen, is never written, and disappears at the next live-refresh: the worst
   * of the three possible outcomes, because it looks saved.
   *
   * Both commits already no-op when the draft matches the stored value, so
   * closing an editor you only looked at writes nothing — which matters, since
   * `patch` bumps `updated_at` and that resets every other draft in the pane.
   */
  const leaveOpenEditor = () => {
    if (open === "workspace") onRepoCommit();
    if (open === "brief") onPromptCommit();
  };

  // The value under each node's label. Deliberately short — a node column is
  // ~78px at the narrow end of the pane, so this is the repo's FOLDER and the
  // agent's short name, with the full path and full label inside the editor.
  const nodeValue: Record<LaunchSetting, string> = {
    workspace: hasWorkspace ? repoName(repoPath) : "Not set",
    runner: task.bg_executor === "claude" ? "Claude" : "Codex",
    capability: access.label,
    brief: prompt.trim() ? "Custom" : "Auto",
  };
  const nodeIcon: Record<LaunchSetting, typeof Eye> = {
    workspace: FolderGit2,
    runner: RunnerIcon,
    capability: access.icon,
    brief: FileText,
  };
  const openNode = open ? LAUNCH_NODES.find((n) => n.key === open)! : null;

  return (
    <section
      data-testid="execution-console"
      className={cn(
        "execution-console shrink-0 overflow-hidden",
        // A task with no workspace — most errands — gets a single faint line
        // while collapsed, not a warning-tinted card: for them "needs repo" is
        // the normal state, not a problem to flag on every open. The SECTION
        // stays full width even then: it is an inline-size container (for the
        // rail's breakpoints), and a container can't size to its content — so
        // `self-start` here collapses it to 0px and the line vanishes. Only the
        // button below shrinks.
        !(!hasWorkspace && collapsed) && "rounded-lg border border-accent/25 bg-elevated"
      )}
    >
      {/* One line, and the header is the collapse control. Collapsed, the line
          IS the plan (repo · runner · access) — a section that hides itself has
          to keep saying what it is set to, or it is just missing. Expanded, the
          rail below states it in more detail, so the line names the console
          instead of repeating it. It was a 36px tile, a title, a subtitle, a
          pill and a chevron: the tallest thing below Notes on a task that
          would never be launched. */}
      <button
        type="button"
        aria-expanded={!collapsed}
        onClick={() => {
          leaveOpenEditor();
          toggleCollapsed();
        }}
        title={collapsed ? "Show the launch console" : "Collapse the launch console"}
        className={cn(
          "flex items-center gap-2 text-left",
          !hasWorkspace && collapsed
            ? "w-fit rounded-md px-1.5 py-1 text-[11.5px] text-content-faint hover:bg-content/6 hover:text-content"
            : "w-full px-2.5 py-1.5 hover:bg-content/4",
          !collapsed && "border-b border-border-subtle"
        )}
      >
        <Zap size={13} className={cn("shrink-0", (hasWorkspace || !collapsed) && "text-accent")} />
        {!hasWorkspace && collapsed ? (
          <span>Hand to an agent…</span>
        ) : (
          <>
            <span className="min-w-0 flex-1 truncate text-[11.5px] text-content-secondary">
              {collapsed
                ? [repoName(repoPath), nodeValue.runner, access.label].join(" · ")
                : "Launch console"}
            </span>
            <span
              className={cn(
                "flex shrink-0 items-center gap-1 rounded-full px-1.5 py-0.5 text-[10px] font-medium",
                hasWorkspace ? "bg-success/10 text-success" : "bg-warning/10 text-warning"
              )}
            >
              <span
                className={cn("size-1.5 rounded-full", hasWorkspace ? "bg-success" : "bg-warning")}
              />
              {hasWorkspace ? "Ready" : "Needs repo"}
            </span>
            <ChevronDown
              size={13}
              className={cn(
                "shrink-0 text-content-faint transition-transform duration-200",
                collapsed && "-rotate-90"
              )}
            />
          </>
        )}
      </button>

      {!collapsed && (
        <>
          <div className="space-y-3 p-4 pb-3">
            <div className="relative px-1">
              {/* The line spans the three ROUTE nodes only — 12.5% is the centre
                  of the first of four columns, 62.5% the centre of the third. */}
              <div className="execution-console__route-line absolute left-[12.5%] right-[37.5%] top-[21px] h-px bg-border" />
              <div
                className={cn(
                  "execution-console__route-line absolute left-[12.5%] top-[21px] h-px w-[50%] origin-left bg-accent shadow-[0_0_8px_rgb(var(--accent)/0.55)] transition-transform duration-300",
                  hasWorkspace ? "scale-x-100" : "scale-x-0"
                )}
              />
              <div className="execution-console__rail relative grid grid-cols-4 gap-1">
                {LAUNCH_NODES.map((node) => (
                  <LaunchRailNode
                    key={node.key}
                    icon={nodeIcon[node.key]}
                    label={node.label}
                    value={nodeValue[node.key]}
                    // Only the workspace can be genuinely unset; the other three
                    // always have a real value, so they are never "missing".
                    unset={node.key === "workspace" && !hasWorkspace}
                    open={open === node.key}
                    title={
                      node.key === "workspace" && hasWorkspace
                        ? shortenPath(repoPath)
                        : `Edit ${node.title.toLowerCase()}`
                    }
                    onClick={() => {
                      leaveOpenEditor();
                      setOpen((current) => (current === node.key ? null : node.key));
                    }}
                  />
                ))}
              </div>
            </div>

            {openNode && (
              <div className="execution-console__editor rounded-lg border border-accent/25 bg-content/4 p-3">
                <div className="mb-2 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wider text-accent">
                  {openNode.key === "capability" ? (
                    <Shield size={11} />
                  ) : (
                    (() => {
                      const Icon = nodeIcon[openNode.key];
                      return <Icon size={11} />;
                    })()
                  )}
                  {openNode.title}
                  <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">
                    {EDITOR_NOTES[openNode.key]}
                  </span>
                </div>

                {open === "workspace" && (
                  <>
                    <Combobox
                      autoFocus
                      value={repoPath}
                      placeholder="Choose a repository…"
                      onValueChange={onRepoPathChange}
                      options={repos.map((repo) => ({
                        value: repo.path,
                        label: shortenPath(repo.path),
                      }))}
                      onBlur={onRepoCommit}
                      className={cn(FIELD, "py-2.5 font-mono text-[12px]")}
                      panelClassName="font-mono text-[12px]"
                    />
                  </>
                )}

                {open === "runner" && <BackgroundAgentPicker task={task} embedded />}

                {open === "brief" && (
                  <div className="rounded-lg border border-border bg-input p-3 focus-within:border-accent">
                    <textarea
                      autoFocus
                      value={prompt}
                      rows={3}
                      placeholder="What should the agent do first?"
                      onChange={(event) => onPromptChange(event.target.value)}
                      onBlur={onPromptCommit}
                      className="w-full resize-y bg-transparent text-[12px] leading-relaxed text-content placeholder:text-content-faint focus:outline-none"
                    />
                    <div className="mt-2 border-t border-border-subtle pt-2 text-[10px] text-content-faint">
                      {prompt.trim() ? "Custom seed prompt" : "Using task context"}
                    </div>
                  </div>
                )}

                {open === "capability" && <AccessEnvelope task={task} tier={tier} />}
              </div>
            )}
          </div>

          {/* Outside the editor block on purpose — see `useExecutorAvailability`
              above. A closed runner editor must not hide the reason the run
              would fail. */}
          {unavailable && (
            <p className="flex items-start gap-1.5 px-4 pb-3 text-[10.5px] leading-relaxed text-warning">
              <AlertTriangle size={11} className="mt-[2px] shrink-0" />
              <span>{unavailable}</span>
            </p>
          )}
        </>
      )}

      {!collapsed && <DispatchBar task={task} reviewTarget={reviewTarget} />}
    </section>
  );
}

/** The right-hand note in each editor's header — the thing the old always-on
 *  legends carried ("required", "start narrow"). Kept because each one answers
 *  a question the field itself doesn't. */
const EDITOR_NOTES: Record<LaunchSetting, string> = {
  workspace: "required",
  runner: "background agent",
  capability: "start narrow",
  brief: "blank uses title + notes",
};

/**
 * A rail node: the current value, and the way to change it.
 *
 * `unset` is only ever true for the workspace, which is the one setting with no
 * usable default — a warning ring on a node reads as "this blocks the launch",
 * and that is a claim only the missing repo can make.
 */
function LaunchRailNode({
  icon: Icon,
  label,
  value,
  unset,
  open,
  title,
  onClick,
}: {
  icon: typeof Eye;
  label: string;
  value: string;
  unset: boolean;
  open: boolean;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-expanded={open}
      title={title}
      onClick={onClick}
      className="group flex min-w-0 cursor-default flex-col items-center rounded-lg px-0.5 pb-1 pt-0.5 text-center hover:bg-content/4"
    >
      <span
        className={cn(
          "z-10 flex size-[42px] items-center justify-center rounded-xl border bg-elevated shadow-macos transition-colors",
          open
            ? "border-accent bg-accent/12 text-accent ring-2 ring-accent/25"
            : unset
              ? "border-warning/50 text-warning"
              : "border-accent/40 text-accent group-hover:border-accent/70"
        )}
      >
        <Icon size={16} />
      </span>
      {/* 8px / 0.05em, not the 9px / 0.12em this rail used at three-up: a fourth
          column leaves ~78px, where "CAPABILITY" truncated to "CAPABILI…". A
          label you have to decode is worse than a small one. */}
      <span className="mt-1.5 w-full truncate text-[8px] font-semibold uppercase tracking-[0.05em] text-content-faint">
        {label}
      </span>
      <span
        className={cn(
          "mt-0.5 w-full truncate text-[11px] font-medium",
          open ? "text-accent" : "text-content-secondary"
        )}
      >
        {value}
      </span>
    </button>
  );
}

/** The three tool tiers. Split out of `ExecutionConsole` only because it is now
 *  one branch of the editor rather than an always-on fieldset. */
function AccessEnvelope({ task, tier }: { task: Task; tier: BackgroundTier }) {
  const patch = useTaskStore((s) => s.patch);
  return (
    <fieldset>
      <div className="execution-console__access-grid grid grid-cols-3 gap-2">
        {ACCESS_OPTIONS.map((option) => {
          const Icon = option.icon;
          const selected = tier === option.value;
          return (
            <label
              key={option.value}
              className={cn(
                "execution-console__access-option relative min-w-0 cursor-default rounded-lg border px-2.5 py-2.5 transition-colors",
                selected
                  ? "border-accent/40 bg-accent/10"
                  : "border-border-subtle bg-content/3 hover:border-border hover:bg-content/6"
              )}
            >
              <input
                className="sr-only"
                type="radio"
                name={`bg-tier-${task.id}`}
                checked={selected}
                onChange={() =>
                  void patch(task.id, {
                    bg_writes_allowed: option.value === "write",
                    bg_shell_allowed: option.value === "shell",
                  })
                }
              />
              <Icon size={14} className={selected ? "text-accent" : "text-content-faint"} />
              <span className="mt-2 block text-[11px] font-medium">{option.label}</span>
              <span className="mt-0.5 block text-[9.5px] leading-snug text-content-faint">
                {option.short}
              </span>
              {selected && (
                <Check size={11} className="absolute right-2 top-2 text-accent" />
              )}
            </label>
          );
        })}
      </div>
      {/* No `min-h` reservation any more: the hint only renders while this
          editor is open, so it can't shift anything by appearing. */}
      <p className="mt-2 text-[10.5px] leading-relaxed text-content-faint">
        {TIER_HINTS[task.bg_executor][tier]}
      </p>

      <RemoteEnvelope task={task} tier={tier} />
    </fieldset>
  );
}

/**
 * May this task leave the machine that holds its repo?
 *
 * Sits under the tool grant rather than beside it, because it is a strictly
 * narrower question: a task that may not run unattended at all cannot run
 * unattended elsewhere. Off by default, and the copy says what turning it on
 * means rather than what the flag is called — "this Mac only" is a fact about
 * where work happens, `bg_remote_allowed` is a column name.
 *
 * Disabled above read-only, and that is not a UI opinion: the only remote host
 * declares `read-only` and cannot checkpoint, so `routing::satisfies` would
 * refuse a Shell or Write task anyway. Showing an enabled switch that produces
 * a refusal at queue time is worse than showing why it is off.
 */
function RemoteEnvelope({ task, tier }: { task: Task; tier: BackgroundTier }) {
  const patch = useTaskStore((s) => s.patch);
  const mutating = tier !== "read-only";
  return (
    <div className="mt-3 border-t border-border-subtle pt-2.5">
      <label className="flex cursor-default items-start gap-2">
        <input
          type="checkbox"
          className="mt-[2px]"
          checked={task.bg_remote_allowed}
          disabled={mutating}
          onChange={(event) =>
            void patch(task.id, { bg_remote_allowed: event.target.checked })
          }
        />
        <span className="min-w-0">
          <span className="block text-[11px] font-medium">May run on another host</span>
          <span className="mt-0.5 block text-[10.5px] leading-relaxed text-content-faint">
            {mutating
              ? "Only read-only work can leave this Mac — a remote host works from a mirror, so there is no tree to write back to."
              : task.bg_remote_allowed
                ? "This can run on a mirror of the repo while the lid is shut. It still runs here when this Mac is awake — the live tree wins."
                : "This Mac only. Turn on to let an idle host pick it up while the lid is shut."}
          </span>
        </span>
      </label>
    </div>
  );
}

/**
 * Is the task's background agent actually usable right now?
 *
 * The check runs when Codex is picked, and the reason is timing: "Codex isn't
 * logged in" discovered at queue time is discovered after you've already
 * decided. Surfaced early it can still change the decision. It is deliberately
 * a warning and not a disabled radio — a task can legitimately be configured
 * for Codex on a machine where it isn't set up yet, and the run refuses later
 * rather than the configuration being impossible now.
 *
 * A hook rather than local state inside the picker, because the picker now lives
 * behind a closed editor and the WARNING must not. `ExecutionConsole` renders it
 * at rail level; the picker renders it only when it is standing on its own.
 */
function useExecutorAvailability(task: Task): string | null {
  const [unavailable, setUnavailable] = useState<string | null>(null);

  useEffect(() => {
    // Claude is the incumbent and is checked by the runner anyway; probing it on
    // every render would put a subprocess behind opening a task.
    if (task.bg_executor === "claude") {
      setUnavailable(null);
      return;
    }
    let live = true;
    api.dispatch
      .executorAvailable(task.bg_executor)
      // Never block the pane on a probe: a failed check is not the same claim
      // as "the agent is missing", and rendering it as one would be a lie.
      .then((reason) => live && setUnavailable(reason))
      .catch(() => live && setUnavailable(null));
    return () => {
      live = false;
    };
  }, [task.bg_executor, task.id]);

  return unavailable;
}

/**
 * Which agent runs this task in the background.
 *
 * Only ever the *background* agent — foreground "Open in Claude" is unchanged
 * and unaffected, which is why this control lives inside the launch console.
 *
 * `embedded` is set when the launch console's rail has opened this as an editor:
 * the panel already carries the title and the availability warning, so
 * repeating either here would print one fact twice. Left off, the component is
 * self-contained.
 */
export function BackgroundAgentPicker({
  task,
  embedded = false,
}: {
  task: Task;
  embedded?: boolean;
}) {
  const patch = useTaskStore((s) => s.patch);
  const unavailable = useExecutorAvailability(task);

  return (
    <fieldset>
      {!embedded && (
        <legend className="mb-1.5 flex w-full items-center gap-1.5 text-[10.5px] font-semibold uppercase tracking-wider text-content-muted">
          <Sparkles size={12} /> Runner
          <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">
            background agent
          </span>
        </legend>
      )}
      <div className="execution-console__runner-grid grid grid-cols-2 gap-2">
        {BACKGROUND_EXECUTORS.map((e) => (
          <label
            key={e.value}
            className={cn(
              "relative flex min-w-0 cursor-default items-center gap-2.5 rounded-lg border px-2.5 py-2.5 transition-colors",
              task.bg_executor === e.value
                ? "border-accent/40 bg-accent/10"
                : "border-border-subtle bg-content/3 hover:border-border hover:bg-content/6"
            )}
          >
            <input
              className="sr-only"
              type="radio"
              name={`bg-executor-${task.id}`}
              checked={task.bg_executor === e.value}
              onChange={() => {
                // Only send the switch. The backend moves the enabled flag to
                // the new executor's column and clears an incompatible model
                // in the same write — doing either of those from here would be
                // two round-trips with a visibly wrong state in between.
                if (task.bg_executor !== e.value) {
                  void patch(task.id, { bg_executor: e.value });
                }
              }}
            />
            <span
              className={cn(
                "flex size-7 shrink-0 items-center justify-center rounded-lg",
                task.bg_executor === e.value
                  ? "bg-accent text-white"
                  : "bg-content/6 text-content-faint"
              )}
            >
              {e.value === "claude" ? <Sparkles size={13} /> : <Code2 size={13} />}
            </span>
            <span className="min-w-0">
              <span className="block truncate text-[11.5px] font-medium">{e.label}</span>
              <span className="mt-0.5 block truncate text-[9.5px] text-content-faint">
                {e.value === "claude" ? "Tool grants" : "OS sandbox"}
              </span>
            </span>
            {task.bg_executor === e.value && (
              <Check size={11} className="ml-auto shrink-0 text-accent" />
            )}
          </label>
        ))}
      </div>
      {!embedded && unavailable && (
        <p className="flex items-start gap-1.5 pt-2 text-[10.5px] leading-relaxed text-warning">
          <AlertTriangle size={11} className="mt-[2px] shrink-0" />
          <span>{unavailable}</span>
        </p>
      )}
    </fieldset>
  );
}
