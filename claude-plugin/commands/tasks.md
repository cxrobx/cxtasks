---
description: Show CXTasks tasks — defaults to today, read-only
---

Show the user's CXTasks board. Argument (optional): `$ARGUMENTS` — a view name
(`inbox`, `today`, `waiting`, `open`, `all`, `in_review`, `done`), a project
name, or free text to search for. `open` is the actionable list — everything
NOT HELD, future deadlines included; `waiting` is the one "not yet" list (a
start date, an armed tripwire, an open blocker, or a written blocked reason);
`all` is every open task, held ones included.

Steps:

1. Route the argument:
   - empty → `mcp__cxtasks__list_tasks` with `view: "today"`
   - a known view name → `list_tasks` with that view
   - anything else → try `list_tasks` with `project` set to it; if that returns
     nothing, fall back to `mcp__cxtasks__search_tasks`

   If the user asks about a task that isn't in `today` or `open`, it is
   *held* — deferred, on an armed tripwire, or blocked — and deliberately
   hidden from the actionable views until it is released (deferred tasks
   reappear on their own). Held work lives in `waiting`. Check `waiting` or
   `all` before reporting a task missing, and say what is holding it rather
   than that it is gone. A future DUE date holds nothing: dated work is in
   `open` from the day it is filed.
2. Print a compact markdown table: `#`, `title`, `project`, `due`, `status`, `P`.
   The `#` column is the task's **T-number** (`T42`) — short and speakable, and
   accepted anywhere the CXTasks tools want a task id, so the user can say "close
   T42" from any session. Mark rows that carry execution context with `⎇` in the
   title column.
3. If any task is in `in_review`, call those out in a separate line underneath —
   those are finished background runs waiting on the user, and they're easy to miss.
   Do the same for anything `blocked`, printing its `blocked_reason` — a blocker
   nobody re-reads is the whole reason blocked work goes stale.
4. End with the counts per status.

This command is **read-only**. Do not complete, update, or file tasks from it —
if the user wants an action, they'll say so next.
