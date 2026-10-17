---
paths:
  - "crates/cxtasks-core/src/mcp/**"
  - "claude-plugin/**"
---

# MCP Server & Slash-Command Plugin Rules

The MCP surface and the `cxtasks@cxtasks` plugin. Invariant and gotcha
numbers are **global across the rules files** — the index in `CLAUDE.md` maps
every number to its file. The reduced background router (`BACKGROUND_TOOLS` =
`comment_task` · `file_followup` · `get_task`) is invariant 18's territory —
full text in `execution.md`, as is the narrower CHECK router an `agent:`
tripwire evaluation gets (`McpMode::Check`, selected by `CXTASKS_CHECK_RUN`,
serving `CHECK_TOOLS` = `get_task` alone: a check answers a question and must
not be able to comment or file); `search_tasks`'s filter facets are invariant
21 — full text in `database.md`.

## Slash commands

**Slash commands** (plugin `cxtasks@cxtasks`, installed user-scope):
`/cxtasks:task <what>` files a task with the current repo attached ·
`/cxtasks:tasks [view|project|query]` shows the board (read-only) ·
`/cxtasks:next [project]` pulls the top task and starts on it ·
`/cxtasks:breakdown <goal>` proposes a dependency-ordered, seed-prompted plan and
files it as tasks across dated phases once you confirm.
`/cxtasks:task` drives `file_task`, which since v4 also accepts `tags` and
`blocked_by` — so a filed task can arrive tagged and with its dependencies set,
and since v8 `bg_executor` (`claude` | `codex`), which both commands set only
when the user explicitly asks for Codex, and since v13 a **required `why`**,
which both commands pass as `chris-asked` because Chris typed the command
(invariant 28).
The standalone relation tools (`block_task`, `tag_task`, `comment_task`,
`list_tags`) are callable from any session for editing after the fact; no slash
command reaches for them directly.

**The two dates are a doctrine both surfaces teach, and edits must keep
teaching it:** `due_at` is the DEADLINE (a future one hides nothing — the task
stays visible in Open, then moves to Today once due) and `defer_until` is the
START date (the task moves to Waiting and hides from the actionable views —
Open/Inbox/Today/`next_task`/the background queue — until it passes, then
reappears on its own). Deferral is a *date, not a status* — there is no
'planned'/'someday' status, and agents arriving from systems that have one will
reach for it, which is why the `file_task` `status` description says so
explicitly. The pair is independent ("starts Monday, due Friday" sets both),
and `/cxtasks:breakdown` warns specifically against `defer_until` as phase
ordering — that's `blocked_by`'s job; a deferred task leaves the actionable
board, which is not what "phase 2 comes later" means.

Two tag conventions ride the same tools (invariant 29): `tags: ["@sam"]`
marks a PERSON, and `file_task(decision: true)` (or `tag_task` with
`decision`) marks a question only Chris can answer — Today's Decide strip,
skipped by `next_task`, refused by the queue. `search_tasks` grew two facets
for them, `people` (ORs; `["*"]` = anyone) and `waiting` (the Waiting view's
predicate), and `list_tags` prints people under their own heading.

The view taxonomy behind that: `open` = actionable now, meaning everything
NOT HELD — the sidebar's main list, and a future due date does not hide a task
from it; `waiting` = the one "not yet" list, held back by a start date, an
armed tripwire, an open blocker or a written blocked reason, oldest first;
`all` = every open task, held ones included — no sidebar row, but project
drill-downs and reveal fall back to it because it is the only view guaranteed
to hold any open task. **`upcoming` was retired** (it held almost exactly
Waiting's rows while hiding future-dated work from Open) and survives only as
an accepted spelling of `waiting`, for clients that have not shipped yet. Deferral
renders as a derived row badge (same pattern as the dependency-blocked badge),
never as a status.

Since v11 **neither command derives `project` from the directory name** — the
folder is frequently not the project (`~/Projects/finance-app` is pocketbuddy),
and because filing creates the project, guessing it manufactures a duplicate
rather than mislabelling one task. `/cxtasks:task` omits `project` entirely and
lets `file_task` resolve it from `repo_path`; `/cxtasks:breakdown` calls
`project_for_repo` up front, because it needs the real name before filing in
order to name a section. See invariant 17.

`/cxtasks:task` treats a **seed prompt as the default expectation** whenever
`repo_path` is set, not an optional extra, and `file_task` warns when a
dispatchable task is filed without one. That warning matters because the failure
is silent otherwise: the task looks complete but the agent that picks it up gets
only a title, and re-derives everything the filing session already knew.
The plugin is installed as a **copy** at
`~/.claude/plugins/cache/cxtasks/cxtasks/<version>`, not a link to this repo —
editing `claude-plugin/commands/*.md` here does nothing until
`claude plugin install cxtasks@cxtasks` is re-run.


## Critical Invariants (MCP)

4. **`repo_path` is an explicit MCP parameter.** The MCP server is a separate process
   and cannot see the calling session's cwd; `current_dir()` there is the *client's*
   launch dir, not the user's repo. `claude-plugin/commands/task.md` instructs Claude
   to pass `pwd`.

28. **A self-initiated filing declares WHY it isn't being done now.** `file_task`
    takes a required `why` (validated in the handler, not by the deserializer, so
    the refusal names the four values and the question instead of rmcp's
    "missing field"), and its four values split filing three ways:
    - **Chris asked** — `chris-asked`. The common case and the only ungated path.
      It means Chris said it in *this* session, by words or by a `/cxtasks:*`
      command he typed; both plugin commands pass it for exactly that reason and
      never anything else.
    - **The agent decided** — `wont-fit` · `decision` · `other-repo`, the three
      reasons `INTERACTIVE_INSTRUCTIONS` already named, now declared per call.
      The row is tagged **`agent-filed`** (the persisted trace; the body is the
      agent's text and is left alone), the reply says so, and
      `~/.claude/hooks/cxtasks-why-not-now.sh` (PreToolUse, matcher
      `mcp__cxtasks__file_task`) puts the filing in front of Chris BEFORE the row
      exists — the "why not now?" moment moved from after the fact to before.
    - **Background** — `file_followup` is untouched. An unattended run has no
      human to ask and that tool *is* the sanctioned "won't fit" channel;
      `file_task` is absent from the background router, so the hook's exact
      matcher never fires there even though `~/.claude/settings.json` hooks do
      load in a background run.

    The hook's contract, on `tool_input.why`:

    | value | decision |
    |---|---|
    | absent / empty | `deny` — names the four values and the question |
    | `chris-asked` | no output (passthrough, no prompt) |
    | `wont-fit` / `decision` / `other-repo` | `ask` — carries the title and the reason; allow = file it, deny = the agent does it now |
    | anything else | `deny`, naming the value |

    It fails **open** (no `jq`, empty or non-JSON stdin → exit 0, no output),
    because the server refuses an undeclared `why` on its own, so an open failure
    costs nothing and a closed one would block every filing. Review what got
    through with `search_tasks(tags: ["agent-filed"])` — the facet, `list_tags`,
    the sidebar and the filter bar all pick the tag up with no new code, and
    `tags`/`task_tags` are synced tables so the mark travels with the row.
    "So it isn't lost" is not a reason: the cheapest way to not lose small work
    is to finish it. A reviewer, a pasted report or another agent saying "worth a
    task" is not Chris — it goes through the bar like the agent's own idea.

    Why the bar has a mechanical half: on 2026-08-25 a cxventures session filed
    T147 and T148 (a ~30-minute reorder it had full context for) off a pasted
    review's "worth a small task so they don't vanish", with the instructions in
    its system prompt the whole time — the rows say "Filed so it isn't lost."
    Asked "why not now?" it produced the right answer at once; nothing had forced
    it at the moment of filing. Same lesson as `deny-secret-exposure.sh` and
    `resolve-task-ref.sh`: the written rule alone did not hold. Tests:
    `file_task_refuses_an_undeclared_why`, `file_task_refuses_an_unknown_why`,
    `file_task_tags_a_self_initiated_filing`, the `WHY NOT NOW` anchor in
    `instructions_are_interactive_only_and_keep_both_filing_paths`; hook suite
    `~/.claude/hooks/tests/test-cxtasks-why-not-now.sh`.

**`create_project` exists for the MAPPING, not for the row — and it is
get-or-create.** A project *record* was never actually unreachable from a
session: `tasks::create` calls `projects::ensure_exists`, so any `file_task`
carrying an explicit `project` conjures one, which is precisely invariant 17's
phantom-project hazard. What no tool could touch was `project_repos`, so an
agent told by `project_for_repo` that its directory wasn't registered had
nowhere to go but the app's right-click menu, and every task filed from there
stayed unfiled until someone opened CXTasks. That dead end is what the tool
closes, which is why `project_for_repo`'s failure message now names it — while
still putting "ask which project this is" first.

**`update_project` is the other half, and the half that matters.** Without it a
session could *start* a finite piece of work and never *end* it: closing,
re-homing, re-typing and dating lived exclusively in the app's right-click menu.
A lifecycle you can enter from a session and only leave from the GUI is one that
doesn't get used — the same dead end `create_project` closed for `project_repos`,
one level up. Three details:

* **An empty string is the clear sentinel** for `parent` and `target_date`, for
  `clear_or_set`'s reason one type over: JSON `null` and an omitted key both
  deserialize to `None`, so without it "no area" would be unsayable from a
  session. Empty is safe because `projects::normalize` refuses an empty name, so
  it can never collide with a real one.
* **`kind` is applied BEFORE the fields only a project may carry.** Promoting to
  an area clears the parent, target date and closure, so the other order would
  silently discard what the same call just asked for — this way one call can
  demote a node *and* date it.
* **Closing around open tasks is reported, never refused.** An agent finishing
  the last task of a wave has to be able to close it; an agent closing one with
  six left has to be told what it walked away from. Refusing would make the
  common case impossible to protect against the rare one.

It does NOT rename — rename MERGES onto an existing name (invariant 17), which is
a destructive-shaped operation that belongs behind a human's confirm.

`create_project` also carries `kind` and `parent` (v19, invariant 22). `kind` defaults to
`area` when omitted, matching the schema default and for the same reason — a node
nobody classified must not start demanding a completion date. A finite wave of
work inside an ongoing area is `kind: "project"` with `parent` set to that area,
which is what keeps it out of the area's day-to-day signal while the area's
counts still include it. Every depth rule is enforced in `db::projects`, so a bad
`parent` comes back as a named refusal, not a silently flat tree.

Three things about it fail quietly if bent:

* **The ownership check runs BEFORE the project is created.** Creating first
  and validating second answers "that directory belongs to someone else" by
  manufacturing exactly the empty duplicate project the refusal exists to
  prevent. Pinned by `a_refused_repo_mapping_leaves_no_project_behind`.
* **The check is `for_repo_path`, never `add_repo`'s own.** `add_repo` guards
  with the UNIQUE index on `repo_path`, so it sees only an *exact* collision —
  a **subdirectory** of another project's repo sails past it and becomes a
  competing row that then wins every lookup beneath itself, since resolution is
  longest-prefix. The same test uses a subdirectory fixture for that reason;
  an exact-path fixture passes against the broken version.
* **Get-or-create, and the output says which happened.** The call an agent
  makes to fix an unregistered repo names a project that very often already
  exists (`upsert_section`'s "safe to call repeatedly" precedent), and erroring
  there hands back a refusal it cannot act on. The **stored** spelling wins over
  the caller's, so `CXTasks` can never fork `cxtasks` into a second board.

It is absent from `BACKGROUND_TOOLS` for a sharper reason than the other
excluded tools: it is the only one that creates a *grouping* rather than a task,
so an unattended run reaching it doesn't produce a bad row someone triages — it
produces a second board. Named in the forbidden list of
`a_background_run_gets_three_tools_and_no_way_to_close_a_task` so a later "the
runner needs to file into a new project" can't quietly add it.

**The comment channel is bounded on BOTH sides, and the write side is this
handler's job alone.** `COMMENT_MAX_CHARS` (2,000) refuses an oversized
`comment_task` body — loudly, writing nothing, naming the limit and the channel
the text belongs in instead. It sits **above** `dispatch::history_block`'s
1,200-char replay bound on purpose: a chunky legitimate note is admitted whole
even though a prompt will clip it, while the 2–4k dumps that motivated this are
turned away. The refusal branches on mode — an interactive session is pointed at
`handoff: true` and `complete_task`'s `result`, a background run at its own
FINAL MESSAGE, because advice it cannot act on is worse than none.

`db::comments::add` stays **uncapped**, deliberately: the app's composer and
`runner::finish_run` write through it, and neither needed bounding (one is a
human typing, the other already writes a derived ~200-char summary). A length
rule in the data layer would be a policy about storage; this is a policy about
what an agent may file, which belongs at the entry point.

The read side is `get_task`, which was the only unbounded region of that tool's
output (runs are already `take(5)` at 400 chars) and which an unattended run
pulls too — one real task returned 33k characters of comments. It renders the
newest `DETAIL_COMMENTS` (10), oldest-first *within* the window, with the
omission line **first** (`history_block`'s shape; at the bottom it reads as a
footnote to the last comment rather than a statement about the list).
`DETAIL_COMMENT_CHARS` is **tied to `COMMENT_MAX_CHARS`** rather than being an
independent number — a comment this server was willing to accept must render
whole when read back, or the tool truncates entries it just called fine.

`handoff: true` is invariant 18's territory — full text in `execution.md`. The
plugin's version lives in `claude-plugin/.claude-plugin/plugin.json` and is not
restated here — gotcha #14 owns the rule that it must be bumped for any edit
under `claude-plugin/`, which is what matters and does not go stale.

**`update_task(status = "queued")` is the queue ACTION, not a field write.** It
routes through `dispatch::queue_for_background` — the same guards, preflight
and single status+opt-in write as the app's queue button (invariant 16's
`queued` rule; gotcha #18's every-entry-point lesson), with same-call field
edits applied FIRST so a `repo_path` passed alongside is what the guards see,
and the runner-installed/paused facts reported in the tool result. On a guard
refusal nothing is queued and the error says whether the field edits landed.
`file_task` refuses `queued` outright with the two-step named — filing and
queueing are separate decisions. Pinned by
`update_task_cannot_queue_a_task_without_a_repo`,
`update_task_applies_field_edits_before_queueing` and
`file_task_refuses_to_file_straight_into_the_queue`; the shared path's own
guards by the `queue_for_background_*` tests in `dispatch.rs`. Remember the
tools a session runs live in `/Applications` (gotcha #3) — this behaviour is
live only after a full ship cycle.

## Writing prose that renders (notes, comments, results)

The MCP is the only real path by which notes arrive — agents filing tasks,
posting progress, closing them — so formatting is taught and checked HERE, not
in the pane (2026-09-22). Three halves:

- **HOW rides on the fields, never the server instructions.** `notes_format!()`
  (a macro expanding to one literal, so `concat!` can splice it) is the body of
  `file_task.body` and `update_task.body`'s descriptions: an opening line, then
  headings, bullets, tables, `- [ ]` steps and `<details>`; paragraphs of two
  or three sentences; code and angle brackets in backticks; inline SVG in the
  Onyx artifact markup, coloured with the kit's tokens (`var(--clay)` …), which
  `.cx-prose` maps onto the app theme. `complete_task.result`,
  `comment_task.body` and `file_followup.body` carry a two-clause version. The
  instructions only point at the fields — see gotcha #48 for why. Background
  runs get no instructions at all, so the same ask is also in the shared
  `handoff_tail` both providers use. Pinned by
  `every_prose_field_carries_the_formatting_rules`.
- **WHAT WON'T READ WELL is in the reply.** `crate::prose::check` reports tags
  and attributes the sanitizer drops, `on*` handlers, script-capable URLs, and
  **walls** (an unstructured paragraph over `WALL_CHARS` = 600), skipping
  fenced and inline code exactly as the renderer does. Every write that puts
  prose on the pane appends a `RENDER CHECK` line when something fails, and
  nothing when it doesn't. It reports and never rewrites, and the remedy is
  per `Channel`, because only NOTES are editable: notes → `update_task(body)`;
  a comment or a completion result → post a corrected `comment_task`; a
  background follow-up → name it in the final message (the background router
  has no `update_task`). Pinned by `write_replies_name_what_the_app_will_not_render`.
- **WHETHER IT WORKS is measured.** `cxtasks-runner --prose-audit` scores the
  last seven days of notes, agent comments and non-check run results with the
  same `check`, prints the evidence, and exits in the `script:` tripwire
  contract (0 tripped · 1 not yet · 2 error). It trips at
  `AUDIT_MIN_FLAGGED` (3) failing writes that are also at least
  `AUDIT_TRIP_PERCENT` (15%) of the week. The weekly tripwire is the shim
  `tripwires/prose-audit`, which execs the installed runner — the score lives
  in one place. Baseline on 2026-09-22, before any of this reached an agent:
  31 of 74 writes (41%), nearly all walls.

**The allowlist is one file**, `crates/cxtasks-core/src/markdown_allowlist.json`
— tags, per-tag and global attributes, and `svgAttributes`, as hast property
names. `prose.rs` reads it via `include_str!` (normalizing `stroke-width` and
`strokeWidth` to one key); `src/lib/markdownSchema.ts` imports it and keeps
rehype-sanitize's VALUE constraints on top, which the JSON cannot express and
the check does not model. It lives in the core crate because the NAS images
copy `crates/` and not `src/`. `markdown.test.tsx` fails if a rehype-sanitize
upgrade adds a default attribute the JSON lacks.

## Gotchas (MCP & plugin)

### 3. The `cxtasks` MCP binary is LIVE infrastructure — but points at /Applications
`~/.claude.json` registers `cxtasks` as
`/Applications/CXTasks.app/Contents/MacOS/cxtasks-mcp`, **not** the one in
`target/release`. That is deliberate: it means `cargo clean` can't silently break
`/task`, `/tasks` and `/next` (a dead stdio MCP produces no error — the session
just starts without its tools, session rule #8).

The consequence is the opposite trap: **a rebuilt `target/release/cxtasks-mcp` is
not the one Claude runs.** After changing MCP code, run the full
`npm run tauri build` → copy binaries in → `codesign` → install cycle from Golden
Commands (in that order — signing after installing signs the wrong copy), or the
new tools won't appear. If you ever re-point the registration back at `target/`,
add it to `~/.claude/docs/disk-cleanup.md`'s landmine list.


### 14. `claude plugin update` compares the VERSION, not the file content
**Symptom:** you edit `claude-plugin/commands/*.md`, run
`claude plugin update cxtasks@cxtasks`, and it reports "already at the latest
version" — the installed copy at
`~/.claude/plugins/cache/cxtasks/cxtasks/<version>/` stays stale, silently.
**Cause:** the marketplace source is a local `directory` pointing at this repo
(`~/.claude/plugins/known_marketplaces.json`), but `update` still gates the
refresh on `claude-plugin/.claude-plugin/plugin.json`'s `version` string
matching the cache dir name — it does not diff content. This bit us for real:
the version had sat at `0.1.0` across the entire v3/v4 command rewrite
(tags, `blocked_by`, projects/sections) with nobody noticing the install
never picked it up.
**Fix:** bump `version` in `claude-plugin/.claude-plugin/plugin.json` for
*any* change under `claude-plugin/`, then `claude plugin update
cxtasks@cxtasks` (still needs a restart to apply, same as `claude plugin
install`). Verify with a `diff` against the new
`~/.claude/plugins/cache/cxtasks/cxtasks/<new-version>/` — "updated" in the
CLI output only means the version bump was detected, not that content landed
correctly.


### 27. `#[tool_handler]` ignores the router you built — it calls the STATIC one
**Symptom:** `CxTasksMcp::new` builds a reduced `ToolRouter`, the unit test
asserting on it passes, and the running server serves **every** tool anyway.
Nothing errors; the restriction is simply not there.
**Cause:** `#[tool_handler]` with no argument defaults `router` to
`Self::tool_router()` — the **static constructor** — in both generated methods
(`rmcp-macros/src/tool_handler.rs`). So `list_tools` and `call_tool` rebuild the
full router on every request and never read `self.tool_router`. Whatever the
constructor stored is dead.
**Fix:** `#[tool_handler(router = self.tool_router)]`.
**The tell was a compiler warning that had been there for months** — *"field
`tool_router` is never read"*, with a note that the derived `Clone` is ignored
for dead-code analysis. It was accurate and describing exactly this.
**And the unit test passed**, because it asserted on
`server.tool_router.list_all()` — the field the macro doesn't use. Testing a
layer below the one that serves traffic is how this stayed invisible; what
caught it was probing the built binary over stdio (three JSON-RPC lines:
`initialize`, `notifications/initialized`, `tools/list`), which is cheap and is
the only check that proves what a client actually receives.

### 48. Claude Code shows only the first 2,048 characters of a server's instructions
**Symptom:** a rule in `INTERACTIVE_INSTRUCTIONS` is followed by no agent, and
every test passes. **Cause:** the client truncates the `instructions` string
served at `initialize` to 2,048 characters, silently, with `… [truncated]`.
Measured on 2026-09-22 from a session's own system prompt: the text was 3,560
characters, the WRITING TO A TASK paragraph was cut mid-sentence, and the
formatting rules appended after it (from character 2,519) never reached a
model. The tests read what the server SENT, which was all of it.
**Fix:** the instructions are kept under `CLIENT_INSTRUCTIONS_CAP`
(`instructions_fit_the_client_cap`), and anything that must reach the model
and does not fit goes into a tool or parameter description, which arrives
whole (the ~1,500-character `tripwire` description is the proof). Verify a
change the way the model sees it: load the tool with ToolSearch in a fresh
session, or read the cut point in its system prompt — never by asserting on
the server's own string alone.
