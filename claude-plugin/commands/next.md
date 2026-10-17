---
description: Pull the next CXTasks task and start working on it
---

Get the most pressing open task and start on it. Optional argument: `$ARGUMENTS` —
a project name to scope the pick to. With no argument the pick is scoped to the
repo you are sitting in, not the whole board.

Steps:

1. **Resolve the scope first.** If the user named a project, use that. Otherwise
   call `mcp__cxtasks__project_for_repo` with your current working directory and
   pass what it returns as `project`. Only fall back to a global pick — calling
   `next_task` with no `project` — when this directory isn't registered to one.
   Do not guess the project from the folder name; the two often differ
   (`~/Projects/finance-app` is `pocketbuddy`).
2. Call `mcp__cxtasks__next_task` with that scope.
3. If nothing comes back, say so and stop. Note the pick deliberately skips
   blocked and deferred tasks (a `defer_until` in the future means "don't start
   yet"), so an empty answer can coexist with a full `waiting` or `all` view —
   mention that if the board obviously isn't empty.
4. Print the task: title, project, due date, and its seed prompt if it has one.
5. **Check the repo** — a backstop now that step 1 scopes, but still load-bearing:
   one project may own several directories (the web and iOS halves of a product),
   and the global fallback isn't scoped at all. If the task has a `repo_path` and
   it isn't your current working directory, say so and stop — offer to have the
   user reopen a session there rather than working across repos from the wrong
   place.
6. If the repo matches (or the task has none), call `mcp__cxtasks__update_task` to
   set `status: "in_progress"`, then start on the work using the seed prompt.
7. When the work is genuinely finished, call `mcp__cxtasks__complete_task` with a
   `result` written as a **handoff**, not a one-paragraph summary: open with one
   sentence saying the outcome, then what you did and found (name the files and
   symbols, so nobody repeats the search), what you did NOT do and why, and the
   next concrete step. That's what makes the outcome survive this session — the
   task record is the only durable trace. Length is fine: the full text is kept
   on a run row and replayed to whoever picks the task up next, and the task's
   comment log gets a one-line summary derived from your opening sentence, so
   don't write the summary yourself.

If you stop part-way without finishing, leave the same thing through
`mcp__cxtasks__comment_task` with `handoff: true` — it takes the identical split
(full text on the run, summary in the log) and is how the next session starts
from where you stopped rather than from the brief. Plain comments are for notes
as you go and are capped at 2,000 characters.

If the work turns out to be blocked, set `blocked_reason` saying what it's
waiting on, instead of completing it — leave the status alone. A reason is a
HOLD, not a status: it takes the task out of Open, Today and `next_task` and
into Waiting until it is cleared. It belongs there rather than in the body,
which is the task's definition and not its history.
