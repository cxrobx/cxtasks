---
description: Decompose a goal into a dependency-ordered set of dispatchable CXTasks tasks
---

Break down what the user wants into a plan filed as real CXTasks tasks:
`$ARGUMENTS`

The output of this command is not a plan in the chat — it's a set of atomic tasks
that each carry enough context to be picked up cold, in an order that reflects
what actually depends on what. That is the whole value: a plan in the transcript
dies with the session, and a pile of untethered one-liners is just a list.

## 1. Explore first

Read the repo before proposing anything. Find the files the work will actually
touch, the patterns already in use, the tests that exist, whatever is already
half-built. A decomposition derived from the goal statement alone invents phases
that don't match the code, and every task filed off it is wrong in the same way.
Your cwd is the repo, and it is every task's `repo_path` — run `pwd` if you
aren't certain.

Resolve the project ONCE, up front, with `mcp__cxtasks__project_for_repo` on
that path. **Never use the directory's name** — `~/Projects/finance-app` is the
project "pocketbuddy", and filing under "finance-app" creates a second, empty
project beside the real one, then scatters the whole plan across it. If the
directory isn't registered, ask the user which project this belongs to rather
than guessing; you need the real name for `upsert_section`, which is what makes
the phases show up as milestones.

Budget this. Enough exploration to name real files and real failure modes, not a
full audit.

## 2. Propose, don't file

Print the plan and stop. Show:

- **The project name**, as resolved in step 1 — never the directory's name.
- **Whether this plan is its own PROJECT.** A multi-phase plan with an end is
  usually a *project*, not a pile of sections inside a standing area: a
  certification wave, a launch, a migration. If it is, say so, and name the area
  it belongs inside — the area's counts still roll up, so nothing gets hidden,
  but the wave stops diluting the area's day-to-day signal and can be closed when
  it's done. If the work is ongoing maintenance with no finish line, keep it in
  the area and use sections alone.
- **The phases**, in order. Give a phase a target date **only if the user gave you
  a timeline.** Don't invent one; an invented milestone date is indistinguishable
  from a real commitment a week later.
- **The tasks**, grouped under their phase, each with its title, its seed prompt,
  its acceptance condition, and what it's blocked by.

Every task must clear three bars:

- **Atomic** — roughly one session's work. "Build the auth layer" is a phase, not
  a task. If you can't state a single acceptance condition for it, it's still two
  tasks.
- **Seed-prompted** — written for a competent stranger with none of this
  conversation's context, same rules as `/cxtasks:task`: the concrete files and
  symbols involved, the observable failure mode or the behaviour to add, what you
  already ruled out, and what "done" looks like as a checkable condition (a test
  that passes, output that changes, a command that succeeds). Imperative. No
  "as discussed above" — there is no above.
- **Ordered** — dependencies stated explicitly, not implied by list position.
  A task that can start now must not be blocked by one that merely appears above
  it, or the board hands out nothing.

Then **wait for confirmation.** Filing 15 wrong tasks is worse than filing none:
each one now has to be found and trashed, and the wrong ones look exactly like
the right ones on the board a day later. Say that if the user pushes to skip the
review.

## 3. File on confirmation

In this order:

0. If the plan is its own project (step 2), `mcp__cxtasks__create_project` first
   — `kind: "project"`, `parent` set to the area it lives inside, and `repo_path`
   omitted, since a nested project inherits its area's repo for a task's
   workspace. Then use ITS name as `project` for everything below. Give it a
   `target_date` (via `mcp__cxtasks__update_project`) **only if the user gave you
   a timeline** — same rule as a phase's date, and for the same reason.
1. `mcp__cxtasks__upsert_section` once per phase — pass `project` and `name`, plus
   `target_date` only for a phase that has a real date (ISO-8601 UTC, trailing
   `Z`). A section carrying a target date **is** a milestone; that's how it shows
   up as one.
2. `mcp__cxtasks__file_task` per task, in topological order — blockers before the
   things they block, so the ids exist when you need them. Pass `title`, `body`,
   `prompt`, `project`, `section` (the phase name), `repo_path`, and
   `why: "chris-asked"` — correct here because Chris typed this command and
   confirmed the plan, and never anything else in this file. Set
   `blocked_by` to the ids returned by the earlier `file_task` calls; T-numbers
   (`T42`) work anywhere a task id is expected.

   **Dates: the phase's timeline lives on the section, not on every task.**
   `target_date` on the section is the milestone — do not copy it onto each
   task as `due_at`, and do not use `defer_until` to express phase order.
   `defer_until` is a start date that moves a task to Waiting and *hides* it
   from the actionable views (Open, Today, `next_task`) until it passes, which
   is not what "phase 2 comes after phase 1" means —
   ordering is `blocked_by`, which keeps later work visible on the board and
   releases it the moment its blockers close. Give a task `due_at` only for a
   real per-task deadline the user stated, and `defer_until` only when work on
   it genuinely cannot start before a calendar date ("after the conference"),
   independent of what it's blocked by.
3. Set `bg_allowed: true` **only** where the work is genuinely safe to run
   unattended and genuinely parallelizable — a mechanical, read-mostly,
   independently-verifiable task. It requires an explicit `repo_path` and will be
   rejected without one. Anything touching secrets, deploys, migrations, or a
   design decision the user should see stays foreground. Default to false.
   Then pick the narrowest tier that can do the work: investigation-only by
   default, `bg_shell_allowed: true` if it needs to run a command (git, tests,
   a CLI), `bg_writes_allowed: true` only if it must change files. The middle
   tier means "don't deliberately edit source", not "cannot write" — commands
   create caches and build output, which is why it is checkpointed.

   Leave `bg_executor` unset (it defaults to `claude`) unless the user asked
   for Codex, in which case pass `bg_executor: "codex"` on the tasks they meant.
   Under Codex the tiers become OS sandboxes: investigation-only is `read-only`
   and can still run read-only commands; the other two are `workspace-write`
   and checkpointed. Don't mix executors across a plan on your own judgement —
   a plan whose phases run under different agents for no stated reason is
   harder to reason about than one that doesn't.

   **Investigation-only phases are the ones worth marking remote-eligible.** A
   second host works from a mirror of the repo — a snapshot, not the live tree —
   so it can only take read-only work, and in exchange that work keeps moving
   with the laptop shut. `file_task` does not accept this; it is a routing
   decision about a task that already exists, so make it in a follow-up
   `mcp__cxtasks__update_task` with `bg_remote_allowed: true`, and only on the
   phases that are genuinely read-only. Skip it entirely if the user has not
   set up a second host — it is inert then, not harmful, but it is noise in a
   plan.
4. Optionally close with `mcp__cxtasks__post_project_update` — body along the
   lines of "Plan filed: N tasks across M phases", `health: "on_track"`. A
   project's health only ever moves by way of a written update, so this is also
   the record of when the plan landed.

## 4. Report

Print one compact markdown table: `T#`, `title`, `phase`, `blocked by`, `bg`.

Then stop. Filing the plan is the whole job — don't start on the first task. The
user asked for a decomposition, and `/cxtasks:next` is how work begins.
