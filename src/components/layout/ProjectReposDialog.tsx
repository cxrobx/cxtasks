import { Combobox } from "@/components/shared/Combobox";
import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { FolderGit2, Star, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { shortenPath } from "@/lib/format";
import { useTaskStore } from "@/stores/taskStore";

/**
 * The project ↔ repository mapping, for one project.
 *
 * This exists because a project's NAME and its directory are different things
 * and neither can be derived from the other: `pocketbuddy` lives at
 * `~/Projects/finance-app`. Before this, three places papered over that by
 * treating the folder name AS the project name, which manufactured a phantom
 * project for every repo whose directory didn't match its product — permanently
 * split from the real one holding the work.
 *
 * Many repos per project, because half the portfolio is a web+iOS pair. At most
 * one project per repo, because that is what makes "which project owns this
 * directory" a question with one answer — which is what a filed task resolves
 * against instead of guessing from the basename.
 *
 * Opaque panel and a near-invisible overlay, matching `SettingsDialog`: popovers
 * portal to the document root, where the backdrop is the transparent window
 * itself (gotcha #22).
 */
export function ProjectReposDialog({
  project,
  onClose,
}: {
  project: string | null;
  onClose: () => void;
}) {
  const projectRepos = useTaskStore((s) => s.projectRepos);
  const repos = useTaskStore((s) => s.repos);
  const addProjectRepo = useTaskStore((s) => s.addProjectRepo);
  const removeProjectRepo = useTaskStore((s) => s.removeProjectRepo);
  const setPrimaryProjectRepo = useTaskStore((s) => s.setPrimaryProjectRepo);

  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const mine = useMemo(
    () =>
      projectRepos.filter(
        (r) => r.project.toLowerCase() === (project ?? "").toLowerCase()
      ),
    [projectRepos, project]
  );

  // Directories already spoken for — by this project or any other. Offering
  // them would just walk the user into the "already belongs to X" error.
  const claimed = useMemo(
    () => new Set(projectRepos.map((r) => r.repo_path.toLowerCase())),
    [projectRepos]
  );

  const submit = async () => {
    const path = draft.trim();
    if (!path || !project) return;
    const err = await addProjectRepo(project, path, mine.length === 0);
    if (err) {
      setError(err);
      return;
    }
    setDraft("");
    setError(null);
  };

  return (
    <Dialog.Root open={project !== null} onOpenChange={(o) => !o && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/10" />
        <Dialog.Content
          aria-describedby={undefined}
          className={cn(
            "fixed left-1/2 top-1/2 z-50 w-[440px] -translate-x-1/2 -translate-y-1/2",
            "rounded-xl border border-border bg-elevated p-4 shadow-2xl",
            "focus:outline-none"
          )}
        >
          <div className="mb-1 flex items-center justify-between">
            <Dialog.Title className="text-[13px] font-semibold text-content">
              Repositories — {project}
            </Dialog.Title>
            <Dialog.Close
              aria-label="Close"
              className="rounded-md p-1 text-content-faint transition-colors duration-75 hover:bg-content/10 hover:text-content"
            >
              <X size={14} />
            </Dialog.Close>
          </div>
          <p className="mb-3 text-[11px] leading-snug text-content-muted">
            Directories that belong to this project. A task filed from any of
            them lands here — the folder name doesn't have to match the project
            name. The starred one prefills a task's workspace.
          </p>

          <ul className="mb-3 flex flex-col gap-1">
            {mine.length === 0 && (
              <li className="rounded-md border border-dashed border-border-subtle px-2.5 py-3 text-center text-[11px] text-content-faint">
                No repositories mapped yet.
              </li>
            )}
            {mine.map((repo) => (
              <li
                key={repo.id}
                className="flex items-center gap-2 rounded-md border border-border-subtle bg-input px-2.5 py-1.5"
              >
                <FolderGit2 size={12} className="shrink-0 text-content-faint" />
                <span
                  title={repo.repo_path}
                  className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-content"
                >
                  {shortenPath(repo.repo_path)}
                </span>
                <button
                  type="button"
                  aria-label={
                    repo.is_primary ? "Default repository" : "Make default"
                  }
                  title={
                    repo.is_primary
                      ? "Default — prefills a task's workspace"
                      : "Make default"
                  }
                  disabled={repo.is_primary}
                  onClick={() => void setPrimaryProjectRepo(repo.id)}
                  className={cn(
                    "rounded p-1 transition-colors duration-75",
                    repo.is_primary
                      ? "text-accent"
                      : "text-content-faint hover:bg-content/10 hover:text-content"
                  )}
                >
                  <Star
                    size={12}
                    fill={repo.is_primary ? "currentColor" : "none"}
                  />
                </button>
                <button
                  type="button"
                  aria-label="Remove repository"
                  title="Remove"
                  onClick={() => void removeProjectRepo(repo.id)}
                  className="rounded p-1 text-content-faint transition-colors duration-75 hover:bg-error/10 hover:text-error"
                >
                  <Trash2 size={12} />
                </button>
              </li>
            ))}
          </ul>

          <label className="block">
            <span className="mb-1.5 block text-[11px] font-semibold uppercase tracking-wider text-content-faint">
              Add a repository
            </span>
            <div className="flex gap-1.5">
              <Combobox
                value={draft}
                placeholder="/Users/…/Projects/finance-app"
                onValueChange={(v) => {
                  setDraft(v);
                  setError(null);
                }}
                options={repos
                  .filter((r) => !claimed.has(r.path.toLowerCase()))
                  .map((r) => ({ value: r.path, label: shortenPath(r.path) }))}
                panelClassName="font-mono text-[11px]"
                onKeyDown={(e) => {
                  if (e.key === "Enter") void submit();
                }}
                className="min-w-0 flex-1 rounded-md border border-border bg-input px-2 py-1 font-mono text-[10.5px] text-content placeholder:text-content-faint"
              />
              <button
                type="button"
                disabled={!draft.trim()}
                onClick={() => void submit()}
                className="shrink-0 rounded-md border border-border bg-input px-2.5 py-1 text-[11px] text-content transition-colors duration-75 hover:bg-content/10 disabled:opacity-40"
              >
                Add
              </button>
            </div>
            {error && (
              <p className="pt-1.5 text-[10px] leading-snug text-error">
                {error}
              </p>
            )}
          </label>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
