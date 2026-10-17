---
description: File a task into CXTasks from this session, with repo context and a ready-to-work seed prompt
---

File what the user just described as a CXTasks task: `$ARGUMENTS`

Two things make a filed task worth more than a line in a notes app, and you are
the only one who can supply either — a future session won't have this
conversation.

## 1. Pass your current working directory as `repo_path`

The MCP server is a separate process and cannot see where you are. Without
`repo_path` the task loses its project-scoped execution context: "Open in Claude"
still works (it falls back to the user's home directory), but the task can NOT be
run unattended in the background, and a fresh agent gets no repo to start in. Run
`pwd` if you aren't certain. The one exception is a task that genuinely isn't
about this codebase (an errand, a call to make) — then omit it deliberately.

(There is a second, much rarer exception — a `detached` task, step 9 — but it is
for work that has no codebase, never for work whose repo you didn't look up.)

## 2. Write a real `prompt` — this is the important part

**Always write one when `repo_path` is set.** The seed prompt is what a fresh
agent reads when the task is picked up, and it has none of this conversation's
context. "Fix the webhook retry" is not enough to act on. A good seed prompt
means the next session starts working instead of starting over.

Include whatever of these you actually know — don't pad it with guesses:

- **Where.** Concrete files and symbols: `lib/audit/retry.ts`, the
  `backoffDelay()` helper. Save the next agent the search you already did.
- **The failure mode.** What's observably wrong, and how to reproduce or see it.
  Exact error text if you have it.
- **What you already ruled out.** This is the highest-value part and the easiest
  to lose. "It isn't the timeout — that's already 30s" stops the next agent
  spending a third of its budget rediscovering that.
- **What done looks like.** The acceptance check: a test that should pass, output
  that should change, a command that should succeed.
- **Constraints.** Anything that must not change; a pattern to follow; a file
  that's load-bearing.

Write it as instructions to a competent stranger, in the imperative. Don't write
"we discussed…" or "as mentioned above" — there is no above.

If you genuinely have nothing beyond the title (the user tossed you a one-liner
about code you haven't looked at), say so in the prompt: state that the task is
unscoped and the first job is to investigate. That's honest and still useful; a
vague prompt that pretends to be specific is worse than no prompt.

## Steps

1. Determine `repo_path` (your cwd). **Leave `project` unset** — CXTasks stores
   which project owns each directory and fills it in for you.

   **Do not derive `project` from the folder name.** A repo's directory very
   often isn't its project's name: `~/Projects/finance-app` *is* PocketBuddy,
   `~/Projects/artisttracker` *is* Artist Advisory. Passing the basename doesn't
   just mislabel the task — filing creates the project, so you get a second,
   empty "finance-app" alongside the real one, and the work is split across two
   things that look like projects and neither is complete.

   Pass `project` only when the user names one, or when `file_task` reports the
   directory isn't registered and you know where the work belongs. If the reply
   says the task was filed into a project you didn't expect, that's the stored
   mapping being right and the folder name being misleading — don't "correct" it.
2. Write a short imperative `title` — "fix the intake webhook retry", not "the
   webhook is broken".
3. Write the `prompt` per above.
4. Add `tags` for cross-cutting labels if any apply (`billable`,
   `waiting-on-someone`, `quick-win`). Use `project` for where the work lives.
5. Set `blocked_by` if the user named something this waits on — pass those task
   ids. Check with `mcp__cxtasks__list_tasks` if you need to find them.
6. Dates: **two different fields, and when the user gives a date you must decide
   which one they mean** — don't default every date to `due_at`.
   - **`due_at` is the deadline** — "by Friday", "due EOD", "before the demo".
     A future due date does *not* hide the task; it stays actionable now and
     sits in Open, then moves to Today once due.
   - **`defer_until` is the start date** — "next week", "after the launch",
     "don't start until Monday", "snooze this". The task moves to Waiting and
     hides from the actionable views (Open, Today, Inbox, `next_task`) until
     that instant, then reappears on its own. There is **no "planned" status**
     — deferral *is* this date; the task keeps `todo`/`inbox` while it waits.
   - Both together is normal and correct: "start Monday, due Friday" =
     `defer_until` Monday + `due_at` Friday.

   Set dates / `recurrence` / `priority` **only if the user actually said so** —
   don't invent deadlines. Both dates are ISO-8601 UTC with a trailing `Z`.
   **Priority is P0 = highest, P3 = lowest** (Meta-style), and defaults to P3.
7. Leave `bg_allowed` false unless the user explicitly asked for unattended
   background execution. If they did, pick the **narrowest tier that can
   actually do the job**: default investigation-only; add
   `bg_shell_allowed: true` when the task needs a command — anything using
   `git`, a test runner, or a CLI, which investigation-only genuinely cannot do;
   add `bg_writes_allowed: true` only when it must change files. Both of the
   latter require a git repo. Filing a shell-needing task as investigation-only
   doesn't fail loudly — the run "succeeds" having reached for a tool it never
   had.

   Note what the middle tier is and isn't: it means *don't deliberately edit
   source*, not *cannot write*. Commands create caches, lockfiles and build
   output, which is exactly why it is checkpointed like a write run.
8. Leave `bg_executor` unset — it defaults to `claude`. Set
   `bg_executor: "codex"` **only when the user explicitly asked for Codex**;
   never infer it from the task's subject matter. Under Codex the same three
   tiers map onto OS sandboxes rather than tool grants: investigation-only is a
   `read-only` sandbox and *can* still run read-only commands, while the other
   two are `workspace-write` and are checkpointed. Codex must be installed and
   logged in (`codex login`); the task is refused at queue time if it isn't.
   Don't pass a `model` for a Codex task unless the user named one — the Codex
   CLI's own default is almost always right, and a Claude model name there is a
   run that dies at spawn.
9. **`detached` is for work with no codebase at all** — research over the notes
   vault, a question that has no repo to answer it in. It is rare, and it must
   never be a way to file a task whose repo you happen not to know: if the work
   is about code, find the repo. A detached task carries no `repo_path` and can
   only run on a host that declares a detached workspace, which this Mac
   deliberately does not — an unattended run here would get unsupervised read
   over every client repo, `~/.ssh` and the vault.

   A detached task's whole boundary is its **tool surface**, so it also needs
   `bg_mcp_servers` — a comma-separated list of MCP server names, currently just
   `vault`. Empty is the default and means the run can read *nothing* beyond
   writing back to its own task, which is safe and useless. An unknown name is
   refused rather than ignored, so don't guess: if the server you want isn't
   `vault`, say so to the user rather than inventing one.
10. Call `mcp__cxtasks__file_task` with `why: "chris-asked"`. That value is
    correct here because Chris typed this command — the filing is his ask, not
    your initiative — and it is never anything else in this file. (`why` is
    required; a self-initiated filing elsewhere answers "why not now?" instead.)
11. Print one line back: the title, the project, and whether a seed prompt and
    execution context were attached. If the task is background-enabled, say
    which agent it will run under. Don't dump the whole tool output.

Do not start working on the task. Filing it is the whole job — the user is
deliberately deferring it.
