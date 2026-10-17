---
paths:
  - "crates/cxtasks-core/src/runner/**"
  - "crates/cxtasks-core/src/dispatch.rs"
  - "crates/cxtasks-core/src/executor.rs"
  - "crates/cxtasks-core/src/intent.rs"
  - "crates/cxtasks-core/src/checkpoint.rs"
  - "crates/cxtasks-core/src/launchd.rs"
  - "crates/cxtasks-core/src/bin/**"
---

# Background Execution & Dispatch Rules

The runner, both dispatch routes, the two executors, intent, and every budget
guard. Invariant and gotcha numbers are **global across the rules files** — the
index in `CLAUDE.md` maps every number to its file.

Three structural facts about these modules:

- `dispatch.rs` builds the prompt: the **task context block is byte-identical**
  whoever reads it; only the closing tail differs, by `PromptTarget`
  (foreground · background-Claude · background-Codex). It also holds
  `open_in_running_ghostty`, the AppleScript route (gotcha #25).
- `executor.rs` (`BackgroundExecutor` + the fail-closed per-executor gate) and
  `intent.rs` (`BackgroundIntent`) are read as a **pair**: executor.rs's module
  doc is the "why a second column" argument and intent.rs's is "why *not*"
  (invariants 15 and 19).
- `runner/` = `mod.rs` (drain loop, budget gates, `RunLimits`, `ToolTier`,
  preflight, checkpoint, the one-turn handoff salvage), `process.rs` (spawn/drain
  and **both** per-run kills — wall clock and token ceiling — provider-neutral),
  `providers/{claude,codex}.rs` (binary discovery, argv, prompt tail, output
  parsing, `live_total`), `tests.rs` (incl. the fake-Codex harness).

**Codex is a prerequisite, not a bundled dependency.** A Codex task is refused
at queue time when the CLI is missing or logged out. Note both Codex traps
before testing anything by hand: gotcha #19 (the node shim vs the LaunchAgent's
PATH) and gotcha #20 (`codex` is a shell function that injects an approval
policy — `command codex` is the real one).


## Completion evidence

Task scope comes from the requested outcome, notes, and relevant subtasks, not
from the runner's permissions. Research and plans can be complete on their own
when requested; a plan for an unavailable action leaves that action unfinished.
`dispatch::build_prompt_with_context` includes notes and subtask requirements
even when a separate seed instruction is present.

Ordinary background work ends with the `CXTASKS_RESULT:` report specified in
`runner::completion`. Exit code zero establishes process completion only.
Evidence plus no remaining steps requests human review (`ready_for_review`);
remaining work is `needs_action`; missing, malformed, or unchecked evidence is
`unverified`. The latter two hold the task in Waiting with a concrete reason
and allow a foreground continuation carrying the full handoff. Never auto-mark
the task done. Review and tripwire verdicts keep their separate contracts.

Evidence is appropriate to the task: sources, constraints, saved artifact
contents, observed behavior, or confirmation of an action. Do not require
software tests or installation for unrelated tasks. For an explicitly requested
installation or other action, preparation and mocked checks do not prove that
the action happened. These reports are agent claims for human review, not an
independent certification of correctness.

## Critical Invariants (execution)

5. **Cost scales with TURNS, not with context size.** Measured, same repo, same
   model, `--strict-mcp-config`:

   | turns | cache_write | cache_read | total |
   |---|---|---|---|
   | 2 | 8,005 | 21,931 | ~30k |
   | 4 | 8,184 | 51,875 | ~60k |

   `cache_write` is flat — the one-time context load. `cache_read` grows ~13-15k
   per extra turn because every turn re-reads the whole accumulated conversation.
   Consequences: (a) **a warm/persistent session makes this WORSE**, not better —
   carrying a prior conversation lengthens the context every subsequent turn
   re-reads (meeting-copilot recycles its warm sessions every 5 turns for exactly
   this reason, and excludes tool-using calls entirely); (b) `--strict-mcp-config`
   only trims the flat part, measured at ~1% of a real run — keep it for blast
   radius and CPU, not for tokens; (c) the real lever is a **precise seed prompt
   that needs fewer tool calls**, then `max_turns` as the ceiling.
6. **Background cost is measured in TOTAL tokens, not output tokens.** A run that
   emitted 141 output tokens touched **233,660** — the reloaded `CLAUDE.md` context
   is re-sent every turn, so cache reads dominate. Measured range is ~46k (single
   turn, no tools) to ~234k (a few tool-using turns), a 50x spread, which is why the
   primary guard is `daily_token_cap` (default **50M** since v15 — this said 2M
   for a long time and was simply wrong) and the run cap is only a
   secondary runaway check. Background runs default to **Sonnet 5**, not the session
   default — the token floor is model-independent but subscription limits are
   weighted by model, and Opus drains them far faster for the same mechanical work.
7. **A background run's tool grant is enforced by `--disallowedTools`, never by
   omission from `--allowedTools`.** Three tiers, `runner::ToolTier`: `ReadOnly`
   (Read/Glob/Grep) · `Shell` (+Bash) · `Write` (+Edit/Write), selected by
   `bg_shell_allowed` / `bg_writes_allowed`, with writes outranking shell.
   `denied()` is computed as the **complement of `allowed()` over
   `DANGEROUS_TOOLS`**, so the two can't drift and a new entry in that list is
   denied by every tier that doesn't name it.

   This is written this way because the obvious version was wrong and shipped.
   `--allowedTools` is **additive on top of `~/.claude/settings.json`** — the
   runner spawns `claude` as Chris, with his `$HOME`, so his 70 `permissions.allow`
   rules (`Bash(bash:*)`, `Bash(ssh:*)`, `Bash(curl:*)`, `Bash(git push:*)`) applied
   to unattended runs too. Measured against the real binary: `--allowedTools
   "Read,Glob,Grep"` and nothing else still ran `git status`. So every "read-only"
   run was shell-capable, and `bg_writes_allowed` was gating a checkpoint for runs
   that could already write without one. Adding the deny list fixes it — verified
   the same way, all three tiers.

   Two traps that cost real time here. **A deny rule naming a tool that doesn't
   exist is a silently absent guard** — the CLI prints `matches no known tool` on
   stderr and continues (`SlashCommand` was in the first draft of the list and
   isn't a tool). And **`claude` in Chris's interactive shell is a function that
   injects `--dangerously-skip-permissions`** (`~/.zshrc:58`), so any permission
   test run from a terminal proves nothing — it passes whatever you pass it. Test
   the resolved binary (`zsh -lc 'command -v claude'` → `~/.local/bin/claude`),
   which is what `find_claude_cli` returns, since `zsh -lc` is non-interactive and
   never sources `.zshrc`.

   **A shell is mutation-capable**, so `Shell` and `Write` are treated identically
   by `ToolTier::needs_checkpoint`: both require a git repo and both capture a
   **checkpoint** first (`src-tauri/src/checkpoint.rs`) — the whole working tree
   snapshotted onto a hidden ref under `refs/cxtasks/` via a throwaway
   `GIT_INDEX_FILE`. It does **not** require a clean tree: the diff against the
   checkpoint is exactly the agent's changes even on top of your WIP, and restoring
   it undoes them. Technique borrowed from pingdotgg/t3code. `preflight` and
   `queue_background` both read the tier so they can't disagree about which runs
   are mutation-capable, and a non-repo shell task is **refused, not downgraded** —
   a silent downgrade still returns `needs_review` with an answer assembled from
   whatever the run could reach instead.

   The tier exists because there was no honest middle: "list stale branches" needs
   `git`, and when the only way to get Bash was to tick "let it write files", the
   safe default came off for a reason unrelated to writing.

   **Codex enforces the same three tiers with `--sandbox` instead** —
   `read-only` for `ReadOnly`, `workspace-write` for both `Shell` and `Write` —
   which is an OS boundary rather than a name-matched list, so it needs no
   complement and no `DANGEROUS_TOOLS` upkeep. What does NOT change is the
   checkpoint: see invariant 15.

   **The per-run token ceiling is the opposite kind of guard: one
   implementation, in the shared poll loop.** It lives in `process.rs` beside
   the wall-clock kill rather than per provider, so there is a single copy to
   keep correct. The only per-provider part is reading the numbers,
   `Provider::live_total`, and that exists because a naive four-bucket sum is
   *wrong* for Codex: it reports `input_tokens` including `cached_input_tokens`,
   and the subtraction that makes those buckets mean what Claude's mean happens
   in `codex::finalize`, at the end of the run. A live sum would over-count a
   Codex run by its whole cache-read volume — firing early (safe) while
   recording a `spent` the daily counter later disagreed with (not safe, per
   gotcha #30). So `live_total` reproduces `finalize`'s arithmetic exactly,
   clamp included, and is pinned against it. Do **not** move the normalization
   into `codex::absorb_line` instead: `finalize` must run on every exit path,
   and per-event subtraction diverges from once-at-the-end the moment an event
   carries one field without the other.

   **What this ceiling does NOT do is bound a Codex run, and that has to be
   said out loud.** Measured against CLI 0.140.0: `codex exec --json` emits
   exactly ONE `turn.completed` — the only event carrying usage — at the *end*
   of the run, even for a multi-step one. So `live_total` returns 0 for a Codex
   run's entire life and the kill can never fire; a real 170-second Codex run
   reported 1.22M in a single event and sailed past a 300k ceiling untouched,
   while Claude was killed at 327,875 against the same ceiling. **Claude is
   bounded per-run; Codex is bounded only by the wall-clock timeout and the
   daily cap.** Keep the Codex arm of `live_total` anyway — it is what the
   ceiling would read the moment that CLI reports incrementally, and being
   wrong then would be silent — but do not write "background runs are capped at
   25M" without naming the agent. Exactly the discipline gotcha #21 already
   applies to the Codex network claim.

   The turn cap is no longer a cost guard at all — it is a **loop detector**,
   and the ceiling is why it can afford to be. See gotcha #31 for what 30 cost.

   **Since v18 both the wall clock and the ceiling can be raised per task** —
   `tasks.bg_timeout_minutes` / `tasks.bg_run_token_cap`, resolved by
   `runner::timeout_for` / `runner::token_cap_for` in exactly the shape
   `model_for` already had (task override, else the global), clamped to the same
   bounds `budget_settings` uses so a stored value cannot mean two things.
   Settable from the MCP on `file_task` / `update_task`; `-1` clears an override
   back to the global, because a plain `Option<i64>` collapses "absent" and
   "null" and would otherwise leave a session able to set one but never remove
   it.

   **They are raised together or the exercise is pointless**, and that is a
   measurement, not a preference: T24 touched **24.6M of the 25M ceiling in
   14m16s**, so a task handed 60 minutes on the default cap dies on tokens
   around minute 15 and never reaches its new clock. The inverse holds for
   Codex — the ceiling cannot bound it at all (a Codex run reports usage once,
   at the end), so for Codex the wall clock is the only per-run bound there is
   and raising it alone is exactly right.

   Two limits kept deliberately: the **daily** cap is not overridable, so a
   per-task budget can never spend more than the day allows; and an
   out-of-range value is **refused with the legal range named** rather than
   clamped — silently storing 120 for a requested 60000 reads as success to an
   agent that will never look again.

   Whatever `timeout_for` returns must also be **recorded on the run row**, or
   `sweep_abandoned` reclaims healthy long runs. That is invariant 20's
   territory — full text in `database.md`.

12. **A missing `repo_path` means "run at $HOME", not "refuse to dispatch" — but
    only for foreground.** `dispatch::resolve_repo_or_home` (used only by
    `dispatch_foreground` / `get_dispatch_prompt`) falls back to the user's home
    directory when `repo_path` is unset; `dispatch::resolve_repo` (used by
    `queue_background`, `preflight`, `run_diff`, `restore_checkpoint`) stays
    strict and errors instead. The split exists because a human is present for
    foreground dispatch — Claude's own cwd-scoped permission prompts still gate
    anything outside `$HOME` — while an unattended background run at `$HOME`
    would get unsupervised `Read`/`Glob`/`Grep` reach over the *entire* home
    directory (every client repo, `~/.ssh`, the notes vault), with no human to
    catch a bad read. So `bg_allowed` keeps requiring an explicit `repo_path`,
    enforced independently in three places that must not drift apart:
    `db::tasks::list_queued_for_background`'s SQL filter, `file_task`'s
    validation, and the UI checkbox's `disabled` state. Do not "simplify" by
    routing background dispatch through `resolve_repo_or_home` too — that
    collapses the one boundary this invariant depends on. As defense-in-depth,
    every `ToolTier` **denies** `WebFetch`/`WebSearch` — an unattended run has no
    legitimate need for network egress. Note the limit of that claim: above
    `ReadOnly` the run still has `curl` and `ssh` through Bash, so denying the two
    tools narrows egress rather than sealing it. Believing otherwise is the same
    mistake invariant 7 documents.


15. **A second background agent must fail CLOSED, and the tier means the same
    thing to both.** Three v8 rules.

    **The enabled flag is stored per-executor, and that is the whole safety
    property** — not a normalization mistake to tidy up. Three binaries share
    this DB and don't upgrade in lockstep, so after v8 ships there is a window
    where a pre-v8 `cxtasks-runner` is still draining `bg_allowed = 1`. Storing
    a Codex task as `bg_allowed = 1, bg_executor = 'codex'` gets it claimed and
    executed with Claude. So `bg_allowed` keeps its exact legacy meaning ("may
    run unattended **under Claude**") and Codex's flag lives in a column that
    runner cannot see. The failure mode becomes "doesn't run until the new
    runner ships", which is recoverable, instead of "ran as the wrong agent",
    which isn't. `db::tasks::Task.bg_allowed` folds the two back into one
    boolean so no caller has to know. Consequences: `NewTask`/`TaskPatch` write
    executor **and** flag together (`update` resolves them jointly — flipping
    the executor has to *move* the flag), and both queue queries use
    `BackgroundExecutor::enabled_sql()` rather than naming a column, so a third
    executor can't be added to one and forgotten in the other. Pinned by
    `db::schema::tests::a_legacy_runner_query_cannot_claim_a_codex_task`, which
    runs the verbatim pre-v8 filter.

    **Switching executor clears the model** unless the same call supplies a
    replacement. A model name belongs to one vendor; carrying `claude-sonnet-5`
    into a Codex run is a spawn failure with nothing to do with the task.
    Re-selecting the *same* executor is not a switch and must not wipe it.

    **The three tiers are one user-facing choice enforced two different ways** —
    for Claude by `--disallowedTools`, for Codex by `codex --sandbox`. What must
    NOT diverge is `ToolTier::needs_checkpoint`: Codex's `workspace-write` is
    exactly as mutation-capable as Claude's `Bash`, so both non-read-only tiers
    are repo-gated and snapshotted identically. There is no Codex exemption.
    Relatedly, the middle tier's old label **"shell, no writes" was false and is
    gone**: a shell writes, which is precisely why it's checkpointed. It now
    reads "commands and tests" everywhere (UI, MCP `format_detail`,
    `TIER_LABELS`) and the prompts say "don't *deliberately* edit source".


18. **A delegated run must leave a handoff, and the next agent must be handed
    it.** The three halves of that sentence are three separate mechanisms, and
    the feature is worthless if any one of them is missing.

    **Ask for a handoff, not a summary.** The background tail used to end "End
    with a short summary", which optimised the *one* artifact that has to carry
    everything the run learned. `providers::claude::handoff_tail` — shared
    verbatim by both providers, because what makes a good handoff has nothing to
    do with which agent wrote it — asks instead for what was done, what was
    **not** done and why, the next concrete step, and the assumptions made. A
    read-only run gets an extra instruction that **the plan IS the deliverable**:
    that tier cannot change anything, so a markdown brief an implementer can
    follow top to bottom is the entire output rather than a note attached to one.

    **Write it where someone will look — the artifact and the pointer are
    different records.** `runner::finish_run` writes the **full handoff** to
    `task_runs.result_text` and a **one-line summary** to the task's comment
    log. Both used to hold the whole thing, which rendered the same
    several-thousand-character brief twice in one pane. They answer different
    questions: the run row is the artifact (tokens, exit code, session id,
    checkpoint, and the complete text), the comment is the task's readable
    history saying a run happened and how it went.

    The summary is **derived, not generated** (`runner::handoff_summary`): it
    takes the handoff's opening line, reaching past a leading markdown heading
    to the sentence under it, because agents habitually open with `## Handoff:
    <the task title>` — a title restates what you already know. `handoff_tail`
    therefore asks for an outcome sentence up front, which is what makes the
    derivation worth doing.

    **Shortening the comment is only safe because `history_block` replays the
    run's `result_text` directly** (see below). Do one without the other and a
    second delegate is handed a single sentence and re-derives everything.

    `complete_with_recurrence` writes to comments too instead of appending to
    `tasks.body` — the pattern `db::comments` warns about, which v12 already
    removed from the runner, and which had a second-order bug: every completion
    of a *recurring* task grew the body that the next instance copies.

    **A foreground session leaves the same handoff, through the same split.**
    The background half was right and the foreground half was not: everything a
    watched session wrote back — `complete_task`'s `result`, a free
    `comment_task` call — landed as an unbounded comment, and `history_block`
    truncates a comment at `HISTORY_COMMENT_CHARS` (1200) while replaying a
    run's `result_text` in full. So a 3,000-character foreground result reached
    the next agent as a mid-sentence fragment where a background one of the same
    size arrived whole. Measured on the real DB when this was found: 61 agent
    comments / 118k chars against 1 human comment of 40, typical entries 2–4k.
    **The bound held; the content rotted.**

    `db::runs::record_foreground_handoff` closes it with **no schema change** —
    v18 is spent, and a foreground handoff is an ordinary `task_runs` row.

    * **Attach iff the newest run is this session's own dispatch row** —
      foreground, finished, and holding an empty `result_text`. Everything else
      creates. A row that already holds text is never appended to (it garbles
      the artifact, breaks `handoff_summary`'s opening-line derivation and
      poisons the dedup probe), and a newer **background** run forces a create
      too: attaching to the older foreground row would rank the new handoff
      *below* the background one in `list_for_task`'s newest-first order, so the
      replay would hand the next agent the stale one and call it current. No age
      bound — a session left open all day is legitimate, which is the same
      reason `sweep_abandoned` exempts foreground runs.
    * **The created row is terminal at `FOREGROUND_STATUS`**, byte-parallel to
      `commands/dispatch.rs` (invariant 20).
    * **`history_block`, `reviewable_run` and `push_handoff` needed zero
      changes**, because they key on `finished_at` plus a non-empty
      `result_text` and never on status — gotcha #31's "don't tidy a status
      filter into either of them", collecting its second dividend.
    * **The split happens in `complete_with_recurrence`, not in the tool.** Both
      the Tauri command and the MCP's `complete_task` route through it, and
      gotcha #18's lesson is that a rule enforced at one of two entry points is
      enforced at neither. If the run-row write fails it falls back to the old
      full-text comment and warns: a summary pointing at a run row that doesn't
      hold the text is worse than a long comment.
    * **Mid-way handoffs ride on `comment_task` as a `handoff: bool`, not a new
      tool** — invariant 18's own argument ("a third makes the choice a coin
      flip"), the `search_tasks` facets precedent one level up. One text in; the
      summary is derived, the full text goes to the run row, and the comment is
      headed `Foreground handoff` / `Foreground review` by **the returned run's**
      recorded intent rather than the task's, since the dispatch row is what the
      session was actually framed as.
    * **A background run is refused `handoff: true`,** and the reason is
      ordering, not neatness: its handoff is captured from the CLI's final
      message when the run ends, so a mid-run foreground row would carry a
      *later* `started_at` than the background run's own — and `history_block`
      replays `finished.first()`, so that note would become "the handoff",
      permanently. Gated in the handler because `CxTasksMcp` keeps a
      `background: bool`: **a router removes tools, never parameters**, so
      `a_background_run_gets_three_tools_and_no_way_to_close_a_task` stays at 3
      and this needs its own test.
    * The cap below does **not** apply to a handoff — `handoff: true` IS the
      escape hatch the refusal points at, and capping both would make the advice
      circular.

    The write-side cap itself (`mcp::server::COMMENT_MAX_CHARS`, 2,000) is the
    MCP rules file's territory.

    **Replay it into the next dispatch.** `dispatch::history_block` puts recent
    comments, the last runs' outcomes, and **the most recent run's full
    handoff** into the prompt, **after** the seed prompt (which is the
    definition of the work) and before the tail. Without it the loop never
    closed: a second delegate got the original seed prompt back and re-derived
    everything at full cost while the careful summary sat unread.

    **The `result_text` replay is the only channel carrying the detail
    forward** — the comment log holds a summary now — so deleting the
    `HISTORY_RUN_CHARS` block silently restores the pre-handoff behaviour one
    level down, with every test still green. Its **dedup guard** exists for
    tasks worked before that split, whose comment holds the entire handoff:
    replaying both would send it twice in one prompt. The guard probes for the
    handoff's first `HISTORY_DEDUP_PROBE` chars in the replayed comments, which
    new-style summaries never match because markdown is stripped out of them.

    Everything is bounded — `HISTORY_COMMENTS` entries, `HISTORY_COMMENT_CHARS`
    each, `HISTORY_RUN_CHARS` for the handoff — and it **says what it dropped**;
    silent truncation reads as "that's all there is". The bound is not about
    this prompt: per invariant 5 it lands in the one-time `cache_write`, but an
    unbounded block makes a task's cost grow with how often it has been worked,
    forever.

    One ordering trap, fixed once already: `db::runs::list_for_task` is
    `ORDER BY started_at DESC`, so it is **already newest-first**. It used to be
    `.rev()`ed here, which reported the three *oldest* runs as "most recent" —
    and now would also replay the wrong run's handoff.

    **The write-back MCP is a REDUCED router, not a deny list.** A background
    Claude run gets `--mcp-config` naming one server, with `--strict-mcp-config`
    still on, and that server serves exactly `mcp::server::BACKGROUND_TOOLS`
    (`comment_task` · `file_followup` · `get_task`) because `with_mode` calls
    `remove_route` on everything else. Measured on the real binary: **3 tools /
    ~717 tokens** against the full surface's **15 / ~5,329**, and `complete_task`
    answers `tool not found`. Removal rather than denial buys both halves —
    an absent tool costs no context and tells an unattended agent nothing about
    a capability being nearby (the same argument `--strict-mcp-config` already
    makes about the user's *other* servers, applied one level down), and it makes
    the self-completion that was observed live structurally impossible rather
    than merely forbidden. A deny rule would not: the CLI prints `matches no
    known tool` and continues (invariant 7).

    Four things that follow, each of which fails silently if dropped:
    * **The grant and the router must name the same tools.** An MCP tool in
      neither `--allowedTools` nor the user's settings is a permission *prompt*,
      and an unattended run has nobody to answer it — the tool just never fires.
      Hence `allowed_with_writeback()` at **every tier**: leaving a handoff must
      not depend on whether the run could edit files.
    * **`file_followup` forces `bg_allowed = false` in code, not by default.**
      A follow-up that could itself run unattended closes the loop — the runner
      drains it, that run files more, forever. It also inherits project and repo
      rather than naming them (invariant 17) and lands as a separate `inbox`
      task with a `relates_to` link to its source, so a human can see and triage
      every one in the main list. The task and link commit together. Out-of-scope
      work must not be hidden under, or archived with, the completed investigation.
    * **Codex gets the same reduced MCP without a global registration.** Its
      command injects a per-run `mcp_servers.cxtasks` config override beside
      `--ignore-user-config`, with the three names repeated in `enabled_tools`,
      their calls pre-approved for the unattended run, and `required = true` so
      a binary that exists but cannot initialize fails loudly instead of
      dropping the channel. Its tail passes `has_tools: true` only when the
      sibling `cxtasks-mcp` exists. Same reason the tail is parameterised rather
      than hardcoded: a tree where it was never built, or a bundle missing the
      copy (gotcha #5), still degrades to "no tools", not to an instruction to
      call something that isn't there.
    * **`file_followup` is hidden from interactive sessions** for the mirror
      reason — `file_task` is richer, and two tools that both file a task is a
      coin flip the narrower one sometimes wins.

19. **A run has an INTENT, continuation is DERIVED, and intent must never
    decide a capability.** The v16 rule, and the three halves that fail
    silently apart.

    **Continuation is derived; only review is chosen.** A second dispatch of an
    already-worked task used to be handed a finished plan and a fresh
    assignment in the same prompt — the history block said "this task has been
    worked on already" while the line above the seed prompt still read `What to
    do:` and the tail still said "Implement the work and verify it". The
    assignment is the more salient of the two, so the agent started over.
    `dispatch::brief_label` and `dispatch::framing_block` now reframe the prompt
    whenever the history block is non-empty — no user choice, no stored state,
    and **a first dispatch is byte-identical to what it was before**. The label
    is the fix, not decoration on it: a paragraph forty lines down saying "that
    was the original brief" loses to the one line that survives skimming, which
    is why `(Work, false)` is the *only* case still emitting `What to do:`.
    Derived from `history.trim().is_empty()` — `history` ends in `'\n'`, so
    `is_empty()` alone tells **every** first dispatch it has prior work.

    **One column, where `bg_executor` needed two.** Invariant 15 forced a second
    column because the divergence was *executable*: a pre-v8 runner claimed a
    Codex task and spawned Claude. Intent is not that — traced through a
    pre-v16 runner draining `bg_intent = 'review'`, the drain query, `preflight`,
    `ToolTier::for_task`, the checkpoint decision and the argv are all
    identical, and only ~400 characters of English in `prompt.txt` differ. The
    failure is "you got work when you asked for a critique", and since the old
    `finish_run` doesn't reset either, the arming survives and the review lands
    one cycle late against a *fresher* handoff. Graceful degradation.

    **The condition is load-bearing.** That argument holds only while **no Rust
    code derives a `ToolTier`, the repo gate, or the checkpoint decision from
    `bg_intent`**. Review preselects read-only **in the UI and nowhere else** —
    the frontend writes `bg_writes_allowed = false` as a real patch on the
    arming gesture, so what reaches the DB is an ordinary tier the user can
    override. The next reader will see "preselects read-only" and reach to
    harden it in Rust; that would make a pre-v16 runner compute a *different*
    tier than a v16 one, which is invariant 15's exact shape. Pinned by
    `db::schema::tests::a_legacy_runner_still_claims_a_review_task_and_would_run_it_as_work`,
    which deliberately asserts the OPPOSITE of its `..._cannot_claim_a_codex_task`
    twin — the two side by side are what make the asymmetry legible rather than
    looking like an inconsistency to tidy up.

    **`handoff_tail` must branch on intent, and this is the part that would
    have shipped broken.** `investigating = !bg_writes_allowed &&
    !bg_shell_allowed` is true for **every default review**, because read-only
    is what arming one preselects. So without the branch a reviewer is told
    "this run cannot change anything, so the plan IS the deliverable" — i.e. to
    redo the planning work it was dispatched to audit, the exact failure the
    feature exists to prevent, one level down. Matched on the *pair* so the
    review arm covers every tier: a write-capable review still holds `Edit`.
    The opening-line ask stays **one** paragraph with its noun swapped
    (outcome → verdict), because `runner::handoff_summary` lifts that line
    verbatim into the comment log and a second competing "START WITH…" makes
    every review's comment-log line a preamble. Codex inherits all of it
    through the shared `claude::handoff_tail`.

    **A review must not displace the work handoff it is about.**
    `history_block` replays `finished.first()`; once a review is the newest
    finished run, the work handoff stops reaching the next continuation and the
    agent reads a verdict about an artifact it was never shown. So a newest-run
    review replays the most recent **work** run too, under its own label. Each
    run is also rendered as `intent · executor · status (mode)` — the old
    `"{status} ({mode})"` made a critique and its subject identical on the page.

    **The queue guard and the replay must name the same run**, or the queue
    accepts a review of something the prompt won't show — the
    `list_queued_for_background` / `claim_for_background` lesson one level up.
    Hence one `dispatch::reviewable_run`, called from both: the most recent
    finished run with a non-empty handoff, deliberately **not** filtered to
    `succeeded` (a `timeout` run's partial handoff is genuinely reviewable) and
    **not** to `intent == Work` (reviewing a review is a second opinion).

    **The reset is one-shot, inside `should_advance`, and success-only.**
    `runner::after_run_patch` is pure and split out for exactly this: the three
    rules all look like omissions. Outside the guard, the reset would override
    the mid-run human decision the guard exists to protect — and `bg_intent` is
    the field a human is most likely to have just set. On the failure branch, a
    review that died at spawn would consume the arming and send the user back to
    re-queue a review they never got.

    **Foreground honours intent too.** The Review control sits next to *both*
    "Delegate to X" and "Open in Claude", so one honouring it and the other
    silently ignoring it is the half-wired divergence gotcha #18 is about. The
    foreground review tail asks for the verdict via `comment_task` and says
    **not** to call `complete_task` — closing the task would claim work finished
    that nobody has done.

    **Intent is deliberately absent from MCP writes.** Nothing is reviewable at
    filing time, and a background run able to arm a review on its own task
    closes the loop invariant 18 exists to prevent. `get_task` renders it
    read-only (`next run: review`, and `(review)` per run) because that one *is*
    a background tool.

    One ordering trap fixed on the way: `db::runs::list_for_task` needed
    `rowid DESC` as a tiebreaker. `started_at` is `datetime('now')` —
    second-resolution — so two runs of one task inside the same second tied and
    fell through to insertion order, the exact inverse of "newest first". Same
    shape as gotcha #12, and it matters more here because `.first()` is what
    picks the handoff to replay.


## Cancelling a run

A run has **three** ways to be killed, and only one of them is a decision:
`KillReason::{Timeout, TokenCeiling, Cancelled}` in `runner/process.rs`. The
first two are guards and report as `timeout` / `budget_stopped`; the third
reports as `cancelled`, which had been a valid `task_runs` status since the
table existed with nothing writing it.

**The request is a FILE, not a column, and that is load-bearing three times
over.** The app and the runner are separate processes, so the app cannot signal
the child. A `cancel_requested` column would put a SQLite read on the poll loop
every 250ms per live run, and `process.rs` is deliberately free of DB access —
its own comment warns that a busy DB must never stall the path draining the
child's pipes, because a full pipe deadlocks the child. And a `stat` needs no
migration, which is why this shipped without touching `CURRENT_VERSION`.

Three details worth keeping:

- **The global switch and the per-run flag stop different things.**
  `STOP_BACKGROUND` is checked *between spawns* (`runner/mod.rs`), so it stops
  the next run and lets a live one finish. Only `CANCEL_<run_id>` reaches a
  process already running. Conflating them is the easy mistake — the global one
  reads like a kill switch and is really a queue pause.
- **The flag is cleared on EVERY run end**, in `run_one_recorded` rather than
  inside `run_one_inner`, because every early return in the latter is a path
  where the run ended. Run ids are unique so a stale flag could never fire
  again, but it would leave one file per cancellation forever — and it covers
  the race where a cancel lands between the last poll and the process exiting
  on its own.
- **`cancel_run` returns whether the request was TAKEN, not whether the run
  died.** It refuses a terminal run and a foreground one (terminal at
  `dispatched`, invariant 20 — the session is a Ghostty window this process
  owns no child for). Writing the flag anyway would address it to a run id that
  will never poll again while telling the caller it worked.


## The heartbeat, and why the progress writer may not fail quietly

`progress.rs`'s writer refreshes `task_runs.heartbeat_at` every 30s, **on both
branches of its loop and ungated on `dirty`**. The gating is the part that
matters: a run that is thinking rather than calling tools produces no batches
for minutes at a time, and the old 60s idle wait was fine for notifications and
would have been fatal for liveness. `runs::start` seeds the first beat at insert
so a run that dies in its opening seconds is judged by liveness instead of
stranding its slot forever under sweep rule 3.

**`ProgressReporter::start` returns a `Result` and a failure refuses the run.**
It used to swallow a DB-open failure and continue without a live trace — an
acceptable degradation when the connection only carried the trace. Now that the
same connection carries liveness, continuing means the run stops heartbeating
and gets reclaimed as `abandoned` *while it works*, with its output unreported.
Refusing costs nothing by comparison: a run that cannot reach the DB could not
have recorded its outcome either.

Heartbeat write failures themselves are logged and swallowed, deliberately. A
transient `SQLITE_BUSY` while another process records an outcome is not a reason
to tear down a working run, and the next beat is 30s away — well inside the
staleness window.

## Gotchas (execution)

### 18. Queueing into a runner that isn't running used to look identical to queueing into one that is
**Symptom:** right-click → "Run in Background", the badge flips to **Queued**,
and nothing ever happens. No error, no timeout, no explanation — the sidebar
reads `BACKGROUND  Off` two panes away and nothing connects the two.
**Cause:** `queue_background` only ever set `status = 'queued'`. Draining is the
LaunchAgent's job, and it had never been installed, so the queue had no reader.
Both facts were true and neither was on screen.
**Fix:** `queue_background` returns `QueueResult { task, runner_installed,
paused }` and the UI says which case it's in, with two ways out: **Run it now**
(`run_queue_once` → `cxtasks-runner --once`, detached, installs nothing) and
**Turn the runner on** (the LaunchAgent). Deliberately not auto-installed:
clicking a per-task menu item shouldn't quietly also decide to run unattended
agents at every login from now on. Doing *nothing*, though, was the worst of the
three.
**The native menu is a separate path and needs its own wiring** — it has no
return value for the caller to read, so `lib.rs`'s `dispatch_bg` arm emits
`background-idle-warning`, `useMcpActivity` listens, and both routes render the
same notice from `taskStore.backgroundIdle`. Fixing only the button leaves the
right-click route silently dead-ending, which is the route this was reported
from.

### 19. `codex` on PATH is a **node script**, and the runner's PATH can't find node
**Symptom:** every Codex background run fails instantly with
`env: node: No such file or directory` — while running `codex` by hand works
perfectly. Reads as "Codex is broken"; is really "the interpreter isn't on
PATH".
**Cause:** the installed `codex` is a `#!/usr/bin/env node` shim (the real
binary is vendored at
`@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/…/bin/codex`).
`find_cli` resolves the shim's path correctly via a login shell, but the runner
is a **LaunchAgent**, whose PATH is about `/usr/bin:/bin:/usr/sbin:/sbin`, and
Chris's `node` is under nvm. `execve` honours the shebang, then `env` looks up
`node` in the *inherited* PATH and misses.
**Fix:** `providers::codex::spawn_path` prepends the shim's own directory (nvm
puts `node` and `codex` side by side) and, only if that doesn't contain the
interpreter, resolves it through a login shell. Applied to Codex alone —
Claude's spawn env is deliberately left byte-identical to what already works.
**Generalizes:** any future CLI provider installed via npm has this problem.
Check for a `#!` first line before assuming a resolved path is spawnable.

### 20. `codex` in Chris's shell is a FUNCTION that injects an approval policy
**Symptom:** you test a Codex flag combination in a terminal, it behaves fine,
and the runner does something else entirely.
**Cause:** `~/.zshrc` defines `codex()` as a wrapper that adds
`--ask-for-approval on-request` unless you already passed one. So an
interactive-shell test never exercises the argv this code builds — it exercises
that argv plus an approval policy the runner would never use.
**Exactly the same shape as gotcha #7's `claude` wrapper**, and the same fix:
test `command codex`, or the path `find_cli` returns
(`zsh -lc 'command -v codex'`, which is non-interactive and never sources
`.zshrc`).
**Also worth knowing:** `--ask-for-approval` and `--sandbox` are **global**
flags — `codex exec --help` doesn't list them, and they must precede the `exec`
subcommand or clap rejects the call. `runner::providers::codex::tests::
global_flags_precede_the_exec_subcommand` asserts on position, not presence,
because "the flag is in the argv" is exactly the assertion that passes on a
command the CLI refuses to parse.

### 21. Codex's network-disable override is defence in depth, NOT a seal
`sandbox_workspace_write.network_access=false` is a real field (it's in the
binary's `SandboxWorkspaceWrite` struct) and is passed on every tier. What could
not be demonstrated without spending a paid run is that *this override* is what
blocks egress: `codex sandbox` — the free probe — denies DNS with the value set
to either `true` or `false`, so it is **not an oracle** for `codex exec`.
Don't write "Codex background runs cannot reach the network" anywhere on the
strength of it. Same class of claim as invariant 12's note about `WebFetch`:
denying the obvious route is worth doing and is not the same as closing the
network.


### 25. Foreground dispatch has TWO routes, and the good one is silent when it fails
**Symptom (the old behaviour):** every "Open in Claude" spawned an entire
second Ghostty *application* — its own Dock icon, its own ⌘-Tab entry. A day of
dispatching left a row of them.
**Cause:** `open -na` explicitly forces a new instance, and the comment above it
said that was unavoidable because "there is no CLI to run a command in a tab of
an existing one." That was true of the CLI and is still true (`ghostty
+new-window` answers *"not supported on this platform"* on macOS). It was never
true of **AppleScript**: Ghostty 1.3 ships a scripting dictionary
(`Contents/Resources/Ghostty.sdef`, `NSAppleScriptEnabled`, gated on the config
key `macos-applescript`, default `true`) whose `new window with configuration`
takes `initial working directory`, `command` and `wait after command` — exactly
the three things `-e` was being used for.
**Fix:** `dispatch::open_in_running_ghostty` tries the AppleScript route first
and `commands::dispatch` **falls back to the old `open -na` path on any
failure**. Fail-open is the design, not laziness: Ghostty quit,
`macos-applescript = false`, declined automation consent, and a pre-1.3 install
must all still dispatch.

Six things that cost time here, the first of which shipped broken:
1. **`command` is SHELL-SPLIT; `initial working directory` is not.** Ghostty
   does not exec `command` — it runs it through
   `login -flp <user> /bin/bash --noprofile --norc -c exec -l <command>`, so
   bash word-splits it. **Every** dispatch hits this, because the launch script
   lives under `~/Library/Application Support/…` and that space is not
   optional: unquoted, bash execs `~/Library/Application` and Ghostty paints a
   red *"failed to launch the requested command"*. `dispatch::ghostty_command`
   shell-quotes it; `initial working directory` is applied directly by Ghostty
   and must stay RAW, or the quotes land in the path literally. Two adjacent
   fields in one record, opposite rules. Pinned by
   `the_ghostty_command_is_one_word_after_the_shell_splits_it`.
   **How it got shipped is the reusable lesson:** the probe used while
   developing this sat in a scratch path with no spaces, so it passed
   repeatedly against a string that could never work in production. A probe for
   a path-handling change has to use the shape of the real path, not a
   convenient one.
2. **`new tab` does not work — only `new window`.** In 1.3.1 `new tab` answers
   `errAEEventNotHandled` (-1708) *with no arguments at all*, so it isn't a
   matter of passing it differently. `new window`, `new surface configuration`,
   `close window` and the standard properties all work fine, which makes the
   one broken verb read like a syntax error on your side.
3. **Cold start belongs on the FALLBACK, deliberately.** The script guards on
   `if application id "com.mitchellh.ghostty" is not running then error` — and
   `is running` is answered by Launch Services rather than by an Apple Event, so
   it does not launch the app to ask (verified against a quit app). This matters
   because launching Ghostty cold *restores its saved windows*, which is the
   3-windows-from-one-launch mess `--window-save-state=never` exists to avoid;
   the fallback already handles it and leaves exactly one clean instance for
   every later dispatch to reuse. Dropping the guard doesn't error — it just
   quietly reintroduces gotcha #24's cousin.
4. **`NSAppleEventsUsageDescription` is required, and its absence is invisible.**
   TCC refuses the Apple Event outright without a purpose string, and a refusal
   is indistinguishable from Ghostty not answering — you'd get the fallback
   forever and conclude the AppleScript route "doesn't work". It lives in
   `src-tauri/Info.plist`, which the Tauri CLI merges into the generated one, so
   **it only exists in a real `tauri build`** — never under `tauri dev`.
   If the consent dialog is ever declined, the decision sticks until
   `tccutil reset AppleEvents com.cxtasks.app`.
5. **No entitlement is needed only because the install is re-signed WITHOUT
   hardened runtime — and `tauri build` now turns it on.** Hardened runtime
   blocks Apple Events unless `com.apple.security.automation.apple-events` is
   granted, and CXTasks grants nothing. It used to be safe to say the app
   "signs with `flags=0x0`, so the question doesn't arise"; that is no longer a
   property of the build. Tauri CLI 2.10.1 emits `flags=0x10000(runtime)`, and
   what restores `0x0` is the "Shipping a build" re-sign (CLAUDE.md) — which omits
   `--options runtime` and was, until this was noticed, doing so by accident.
   Ship the bundle as `tauri build` leaves it and this route dies **silently**,
   into the `open -na` fallback, one new Ghostty app per dispatch. Check with
   `codesign -dv --verbose=2 /Applications/CXTasks.app | grep flags` — the
   symptom otherwise looks like Ghostty misbehaving, not like a signature.

   **Notarization and this route are currently in direct conflict**, and nothing
   in the config records a decision. `tauri build` already warns it is skipping
   notarization (no `APPLE_ID`/`APPLE_API_KEY`), and notarizing *requires*
   hardened runtime — so taking that path means granting the apple-events
   entitlement in the same change, or knowingly giving up in-place Ghostty
   dispatch. Stripping the flag is the status quo, not a considered answer.
6. **Paths go in as `argv`, never interpolated into the AppleScript source.**
   AppleScript string literals escape nothing the way a shell does, so a
   `format!`-ed path holding a quote or backslash is a *compile* error inside
   osascript. `on run argv` + trailing arguments sidesteps it entirely, pinned
   by `osascript_arguments_arrive_verbatim_however_they_are_quoted`. Note this
   is a **different layer** from #1 and the two are easy to conflate:
   `shell_quote` is the wrong tool here (osascript's own parser) and the right
   one there (the shell Ghostty hands `command` to). One value, quoted for the
   shell, passed unquoted as an argument.

`wait after command:false` **matches the fallback on purpose**: the window
closes when the session ends, exactly as it always has. Setting it true would
make a dispatch behave differently depending on which branch it took.


### 31. A stopped run has to say WHICH guard, and a turn cap can't be a cost guard
**Symptom:** a background run that did 30 turns of real work on a real repo
returns `failed`, five lines of narration, a byte-identical working tree, ~10M
tokens spent, and `blocked_reason = "Background run failed — see the latest run
for details"`. Diagnosing it took a dig through
`~/.claude/projects/…/<session>.jsonl` to find
`{"type":"max_turns_reached","maxTurns":30,"turnCount":31}` — a line that never
reaches the runner at all.

**Cause, three separate defects stacked.**

1. **The reason was dropped.** `claude::absorb_line` read only `result.result`
   from the terminating event. An `error_max_turns` result carries **no `result`
   field** — it announces itself through `subtype`/`is_error`, neither of which
   was parsed — and stderr on that path is empty, so the `--- stderr ---` branch
   never fired either. This was general, not specific: *every* `error_*` subtype
   was invisible. The fix reads `subtype` and routes `error_max_turns` to a
   `StopReason`, every other `error_*` to `error_text`.
2. **The cutoff destroyed the run's whole value.** Nothing asked for a handoff
   on the way out, so the run was neither reviewable nor replayable. Now a turn
   cutoff spends **one more turn** — `--resume`, `--max-turns 1`, and a deny
   list naming *every* tool the runner knows, because with one turn available
   any tool call consumes it and the salvage produces nothing. Salvage is
   **turn-cutoffs only**: after `error_max_turns` the loop ended cleanly between
   turns so the transcript is well-formed, whereas a wall-clock kill can leave a
   `tool_use` with no matching `tool_result` and make `--resume` a 400. And
   `budget_stopped` never salvages — spending a turn straight after a spend
   guard fires is incoherent. That decision is `runner::should_salvage`, a named
   function precisely because both of its `false` cases are invisible omissions.
3. **The ceiling was the wrong unit at the wrong number.** Turns are a poor
   proxy for spend — context grew 93k→149k *within* one of these runs and
   75k→260k in another (3.4×), so late turns cost several times what early ones
   do. And 30 sat *below* the cost of an honest run: **both** of that day's
   background runs died at turn 31. A guard that fires on healthy runs is a tax.

**Fix:** tokens became the real per-run guard (25M, live-enforced in the shared
poll loop — invariant 7), turns rose 30 → 150 as a loop detector, and
`budget_stopped` finally got a producer, having sat in `db::runs::RUN_STATUSES`
and been labelled in Mission Control since it was written with **no Rust code
anywhere able to emit it**.

**Verified against the real CLI, not just in tests**, because the two things
that matter here are shapes only the binary knows. A run capped at 3 turns came
back `turns_exhausted` with `blocked_reason = "Background run hit its 3-turn
ceiling at turn 4 — a handoff was salvaged; see the latest run"` — so
`error_max_turns` does carry `num_turns`, and it reports `cap + 1`. The salvage
spawned exactly as designed (`--resume <uuid> --max-turns 1 --disallowedTools
Task,…,Read,Glob,Grep --strict-mcp-config`) and returned a **13.5k-character**
handoff that opened "Turn ceiling hit after only 5 of the repo's 19 source files
were read", listed confirmed-vs-unverified findings, and gave a file-by-file
reading order. Re-dispatching then replayed that handoff and reframed the prompt
as "This is a CONTINUATION, not a fresh start" — the loop invariant 18 and
invariant 19 exist to close, closed. `-r, --resume [value]` takes an
**optional** value in the CLI's own help, so the id must stay a separate argv
entry that does not begin with `-`; a UUID never does.

Two things fall out that are easy to undo by accident:

* **`turns_exhausted` and `budget_stopped` render as warnings, not errors, and
  from ONE shared map.** `RUN_STATUS_LABELS` / `RUN_STATUS_CLASS` in
  `src/lib/format.ts` — because the two surfaces disagreed: Mission Control had
  a local label map while `TaskDetail` printed `{r.status}` **raw**, so a new
  status showed up there as underscored snake_case. Same argument as
  `db::tasks::LIVE` a layer down; one constant is what makes the call sites
  greppable. A run we stopped on purpose is not a run that broke.
* **Nothing downstream needed a status filter.** `dispatch::reviewable_run` and
  `dispatch::history_block` key on `finished_at` plus a non-empty `result_text`,
  never on status, so a salvaged cut-off run becomes replayable and reviewable
  automatically. That is the entire point of salvaging — don't "tidy" a status
  filter into either of them.

**The accepted limits, written down so they aren't rediscovered as bugs.** The
ceiling is deliberately **not** clamped to the day's remaining budget (that
would start runs with an allowance too small to finish, wasting a checkpoint and
a spawn), so overshoot past the daily cap is bounded at `ceiling ×
max_concurrent` = 50M — for Claude. **It does not bound Codex at all**, because
`codex exec --json` reports usage once at the end (invariant 7); that was found
by verification, not by reasoning, and the plan this came from had it the other
way round. And it is **model-blind by
decision** — asked and answered, so don't reopen it. The two cut-off runs spent
9.89M on Sonnet and 9.29M on Opus: near-identical token counts costing very
different amounts of subscription, which a token ceiling cannot see. Chris's
call is that **selecting Opus IS the decision to spend more**, and the guard
should not second-guess it; `app_meta['bg_model'] = claude-opus-5` while
`meta::DEFAULT_MODEL` stays Sonnet (invariant 6) is deliberate, not drift.

**The obvious fix is measurably wrong, which is why this is worth writing
down.** Dividing the 25M by Opus's ~5× price weight yields an effective 5M —
*below* the 9.29M an ordinary Opus run already reached — so it would kill
healthy runs, exactly the tax this whole gotcha is about, one level down. Real
history says both families land in the same per-run token range (Sonnet max
14.57M, Opus max 9.29M over 7 runs); the 5× appears in **daily** subscription
burn, not in the size of one run. So weighting belongs on the daily budget or
nowhere — and nowhere is the standing choice.


---

# Multi-host execution (Phase 3) and repo-less runs (Phase 4)

## Invariant 24 — a host row means "a machine that runs tasks"

`hosts` is a registry of machines that execute, and **only the runner writes a
row**, on the machine it describes, on every tick. Nothing else may — not the
app, not `cxtasks-server`, not a read verb that happens to be convenient.

The failure that rule prevents is concrete. `cxtasks-server` calls `admit`, and
`admit` used to register the calling machine. The server cannot execute
anything, so its row claimed `Live` + `Write` + both executors and drained
nothing — and a task only that phantom could run would be admitted and then sit
forever, with a registry entry that looked healthy. `admit` now falls back to
this machine's declared capability **without writing** when the registry is
empty, which is what keeps a fresh install able to queue before any runner has
ticked.

**Declared vs observed.** Declared is policy — `max_tier`, `enabled`,
`headroom_pct` — set by a person. Observed is what the host reported after
looking: `last_seen_at`, `paused`, `gate`, `executors_ready`. Effective
capability is the intersection, and that split *is* the preflight split landed
in data rather than in control flow.

Two consequences that are easy to get backwards:

* **A restart never re-asserts declared policy.** `register_self` is
  `ON CONFLICT DO NOTHING`. A container comes back twice a day; a NAS somebody
  downgraded to read-only must not silently return to whatever its image says.
* **Absence of an observation is not a negative observation.** A host that has
  never heartbeated has an empty `executors_ready`, and intersecting against it
  unconditionally means a fresh install can run nothing until its runner has
  ticked once — the app refuses to queue, naming an executor that is in fact
  installed. An unobserved host is taken at its word until its first heartbeat,
  which is also the first moment anybody looked.

`hosts.mirrors` is the one field a host does not write about itself. A bare
mirror is named by `repo_key` — a hash of the source path — so the holder can
enumerate its mirrors and still not know that `web-a1b2c3d4.git` is
`~/Projects/artisttracker/web`. The side that knows is the side that made it,
and a repo lives on exactly one machine, so there is exactly one candidate
writer.

## Invariant 25 — a detached run is bounded by its TOOL SURFACE

Invariant 12 is satisfied differently, not weakened: background execution is
refused outside a **declared workspace**. A repo is one. A container mount set
the app authored is another. `$HOME` is neither — which is why the MacBook does
not declare `Detached`, since an unattended run there would get unsupervised
`Read`/`Glob`/`Grep` over every client repo, `~/.ssh` and the notes vault.

What replaces the repo gate is `--strict-mcp-config`, which means "only what
`--mcp-config` names" — the user's own `~/.claude.json` is ignored entirely. So
the generated `mcp.json` is the COMPLETE set of servers a run can see, and its
default is the `cxtasks` write-back router and nothing else. An ungranted
detached run can read nothing, which is a useless run and a safe one; making it
useful is a named, per-task decision.

* **`bg_mcp_servers` holds NAMES**, validated against `mcp_registry::REGISTRY`.
  If it held command lines then "which servers may this task use" would be
  "what may this task execute", and a task row would be a place to write
  arbitrary argv for an unattended process.
* **An unknown name is an error, never a silent drop.** A task filed asking for
  the vault that quietly runs without it reports confidently on nothing, and
  the only evidence is an absence.
* **There is no "is detached" column.** A detached task is one with no
  `repo_path`, and every drain query that predates Phase 4 — including the one
  generated for any host not declaring `Detached` — already carries
  `repo_path IS NOT NULL`. The existing guard is the skew guard. The next
  reader will look for that flag; it is not there on purpose.
* `WebFetch`/`WebSearch` stay denied at every tier. Network egress from an
  unattended run is its own question.

## Invariant 26, the runner half — the FREE lane

The tick has two lanes, and the ORDER is the invariant:

```text
sweep_abandoned
gate(...)                     → Paused | HostPaused | HostDisabled : return (the switches)
tripwire::script_pass(...)    → the free lane: claim due scripts, hand them to
                                ONE worker thread that runs them sequentially
slots <= 0                    → return (the budget gates)
drain(slots)                  → agent runs
```

`runner::lanes(&gate)` is that ordering as a pure function, so it is pinned by
a test rather than by reading `tick` carefully: scripts run under
`TokenCapReached`, `DailyCapReached`, `AtConcurrencyLimit` and
`HostHeadroomReached`, and under none of the three switches. One switch, one
doctrine — `STOP_BACKGROUND` stops a shell script as surely as it stops an
agent — but a spent budget is a reason not to start an agent and no reason at
all not to run a script (gotcha #42).

**Lease expiry is the whole crash story.** `claim` takes a five-minute lease;
the timeout is sixty seconds; a healthy attempt always records well inside its
lease. A crash between claim and record leaves the lease to expire, and the
next tick's `list_due` reclaims it — a request issued before the crash is
still on the row, because `record` is the only thing that clears one.

**The pipes are drained concurrently, bounded, and the join is bounded too.**
`process.rs`'s lesson holds for a shell script: an undrained stderr deadlocks
the child once the buffer fills. Each pipe gets a reader thread that keeps the
first 4 KB and keeps READING past it. And the script is put in its own process
group at spawn, so the timeout kills the group — killing `sh` alone leaves a
`sleep 30` grandchild holding the stdout pipe for its full duration. Even so
the reader join waits at most two seconds: a script that backgrounds something
leaves a writer behind, and the tick must not wait on a process it did not
start.

**A script gets a task, not a shell.** cwd is the task's repo when it resolves,
else the data dir; env is `CXTASKS_TASK_ID`, `CXTASKS_TASK_REF`,
`CXTASKS_TASK_TITLE`, `CXTASKS_TRIPWIRE=1`; `$1` is the one argument, passed
through `Command::arg`. Exit 0 = tripped (stdout is the evidence), 1 = not yet,
anything else / a timeout / a spawn failure = error (stderr is the note).

`--status` prints `tripwires: N armed · M due now · K leased`, and says when the
switches are off. `queue_for_background` refuses an armed task by name — the
drain query skips it exactly as it skips a deferred one, so queueing it would
park it at `queued` where nothing would ever claim it.

### The AGENT lane (Release 2)

The tick's ordering grew one step, and its position is the point:

```text
gate(...)                     → the switches: return
tripwire::script_pass(...)    → the FREE lane (above the budget return)
slots <= 0                    → the budget gates: return
tripwire::agent_pass(slots.min(1))  → at most ONE slot, BEFORE the drain
drain(slots - checks)         → agent runs
```

An `agent:` check is a real `claude -p`, so it lives BELOW the budget return
(it costs tokens and a slot) and takes at most one slot before queued work is
drained — one so the tripwires cannot starve the work, first so the work
cannot starve the tripwires. `claim_due` leaves an `agent:` spec untouched;
"not mine" is not an outcome a tripwire has.

**What makes it a check and not a run** — every item below is pinned by the
LANE, never read off a row (invariant 19):

* **`RunKind::Check`** threads through `run_one_inner` → `RunRequest.kind` →
  `claude::command`: the tier is `ReadOnly` whatever the task's flags say,
  `--allowedTools` is `ToolTier::allowed_for_check()` (Read/Glob/Grep +
  `mcp__cxtasks__get_task`), `--disallowedTools` the whole dangerous list, the
  MCP router starts in `CXTASKS_CHECK_RUN=1` mode serving `CHECK_TOOLS =
  ["get_task"]`, the prompt is `check-prompt.txt` (task context + notes +
  the CONDITION + the verdict tail — no history, no seed prompt, because the
  seed prompt is the work and a check that reads it starts doing it), and a
  turn cutoff never salvages.
* **Bounds** (`runner::tripwire::check_limits`): 12 turns · 3M tokens · 10
  min, each `tripwire::min_nonzero(task-or-global, constant)` — a task whose
  work runs uncapped (`bg_run_token_cap = 0`) must not hand "no ceiling" to a
  run nobody asked for today.
* **Admission** (`admit_check`): the check's WHOLE ceiling is reserved
  against `tripwire_daily_token_cap` before it starts, and a refusal is
  logged, never recorded — a spent sub-cap is a fact about the day, not the
  condition, so the tripwire stays due for a later tick. A missing Claude CLI
  IS recorded (as an error, through a fresh lease with no run row), because
  that is a fact about whether the condition can be evaluated here.
* **`preflight_for` / `ready_for`** with the purpose: a check skips the
  `bg_allowed` opt-in (arming the tripwire is its own opt-in to a read-only
  run), never needs git, runs on Claude whatever `bg_executor` says (Codex
  reports usage once at the end, invariant 7, so a Codex check could not be
  bounded), and refuses any extra MCP server not `read_only: true` in the
  registry — a check's tool surface is the whole of its bound.
* **The verdict** (`tripwire::parse_verdict` → `verdict_outcome`): the LAST
  `VERDICT:` line wins (a model reasoning aloud writes the word on the way to
  its conclusion); markdown dressing is tolerated; `EVIDENCE:` is the note.
  Any non-`succeeded` status, no verdict line, or `UNSURE` is an error.
* **The model** is the task's own override when its executor is Claude
  (that is how "haiku for this tripwire" is spelled), else the global Claude
  default.

`scripts/agent-check-sim.sh` drives all of it through the real runner binary
against a fake `claude` — NOT_YET, back-off, "check now", TRIPPED with the
comment and banner path, the sub-cap refusal, UNSURE — at a throwaway data
dir with zero tokens. Gotcha #46 is what its first version hit.

## Routing

`RunRequirement::for_task` is derived and stored nowhere; `HostCapability` is a
row; `routing::satisfies` is pure. `queue_sql(cap)` generates a host's drain
predicate from its own capability, exactly as `enabled_sql` is generated from
`ALL` — so a read-only host's query says `bg_writes_allowed = 0 AND
bg_shell_allowed = 0` forever, including for task shapes invented after it
shipped. `queue_sql_for_the_local_host_is_byte_identical_to_the_pre_v23_filter`
is a blunt string comparison and exactly right: it makes any change to the Mac's
drain query a deliberate act.

`satisfies` keeps the **most specific** refusal, not the last one tried — a host
declaring both Mirror and Detached would otherwise report "no workspace kind
fits" where "has no mirror of /repo yet" was available.

`gate()` applies host ceilings AFTER the global ones, in that order
deliberately: a host that says "I am at my share of a cap that is already gone"
sends the reader to fix the wrong number. A ceiling narrows and never widens,
and every counter still spans every host — a per-host budget is the
per-executor trap one level up.

## The claim

`db::runs::claim_and_start` re-checks the concurrency count inside
`BEGIN IMMEDIATE`. The gate is still what decides whether to bother, but it is
no longer the guarantee: before this, the count was read in `gate()` and the
claim happened in a loop afterwards with no lock in between, so two hosts could
both see one free slot.

Making it one transaction forces the run row to exist before the checkpoint,
which fixed two bugs that were invisible while it was backwards:
`checkpoint::capture` was called with `task.id` (so two runs of one task wrote
the same ref and the second overwrote the first's snapshot), and a checkpoint
failure parked the task with no run row at all.

## Mirrors

A checkpoint commit **is** the uncommitted tree as a real git object, so the
mirror pushes a commit and gets deltas, integrity and dedup for free. Never
`--mirror`: it deletes refs the near end lacks, which is exactly the snapshot
refs. `--shared` on materialize is what lets `/mirrors` be mounted read-only.

`MIRROR_CONTEXT` is the load-bearing part, not an afterthought. Three
differences an agent cannot see, each of which reads as a finding: gitignored
files absent, files untracked on the Mac arriving TRACKED so `git status` reads
clean, submodules absent. It is passed as a parameter rather than inferred from
the path, because inferring means a heuristic decides whether an unattended
agent is told the truth about its own filesystem.

**Gotcha #40:** the fingerprint that gates the background refresh must hash
content. `git status --porcelain` names changed *paths*, so editing a file that
was already dirty — which is what working in a repo looks like — produced an
identical fingerprint. It gates only the refresh; the queue-time push is what
makes a run correct.

## Cancel across hosts

The file stays the interface (`process.rs` is unchanged) and the row is the
transport. `task_runs.cancel_requested_at` replicates, and each host's control
thread turns rows addressed to it into the local flag. That column is gotcha
#36 in miniature: `task_runs` is swept on
`MAX(started_at, heartbeat_at, finished_at)`, so a column written on its own
would never move the watermark — the sweep expression had to grow to include
it.

The control loop runs on its own thread and connection (being prompt is its
whole value) and beats once a **minute**, not on its 3-second tick: a heartbeat
moves `hosts.updated_at`, which puts the row on the wire.

## The container

**Gotcha #41.** Its `$HOME` must grant no `permissions.allow` rules.
`--allowedTools` is additive over `~/.claude/settings.json` and Chris's holds
**175**, including `Bash(bash:*)` and `Bash(git push:*)`. The image ships an
explicit empty block and `--health` exits 1 if it ever grows one — which is how
a home mounted over during an incident gets caught. **On macOS the same count
prints as a `note:` and exits 0** (`home_rules_verdict`): the Mac runner is
Chris's own login by design and its tiers hold by `--disallowedTools`, and a
check that failed on every Mac run was one you learn to ignore. The split is by
OS, not an env var, so a container can't lose the strict branch by losing a
setting.

It shares the canonical node's data directory rather than running a sync
client. "SQLite over a network filesystem is not an option" is about SMB/NFS;
two containers on one host over one local volume is the ordinary multi-process
case, and the same one the app, the MCP and the runner have shared on the Mac
since the first version.

`--dry-run` claims nothing and spawns nothing: host, generated drain SQL,
workspace, CLI, and the real seed prompt. Deliberately not "`--once` with a
flag" — a run that spawns and is then told to stop has already cost money and
already touched the tree.

## Verification

`scripts/two-host-sim.sh`: two runners, one throwaway database, a local-path
"remote". 20 checks, zero network, zero tokens. `KEEP=1` keeps the scratch root.

Three harness gotchas it cost to learn, all of which fail as though the feature
were broken:

* **#37** `cmd | grep -q x` under `set -o pipefail`: grep exits at the first
  match and closes the pipe, the producer takes SIGPIPE, and the pipeline
  reports failure. Nine checks "failed" against working code. Capture into a
  variable.
* **#38** `timeout(1)` is GNU coreutils and is not on a stock macOS.
* **#39** `remove_dir_all(dir.parent())` on a directory created directly under
  `$TMPDIR` names the whole temp directory.

### 42. The drain tick returns early when the gate is closed — the free lane must run BEFORE that return
**Symptom:** the daily token cap is spent, or both concurrency slots are busy,
and no tripwire is checked all day. Nothing errors. The Waiting badge goes red
("unchecked — is the runner running?") while the runner is demonstrably
running.
**Cause:** `tick` matched the gate and `return`ed on anything but `Open`, and
that was correct for as long as everything a tick did cost tokens. A script
tripwire costs nothing — no agent, no slot — but it lived below the same return,
so a closed BUDGET gate silently stopped zero-token checks.
**Fix:** `runner::lanes` splits the decision: the three switches stop
everything, the budget gates stop only agent runs, and `tripwire::script_pass`
sits between the two. Pinned by
`runner::tripwire::tests::the_free_lane_runs_under_a_spent_budget_and_stops_with_the_switches`.
The general rule: anything a tick does that costs no tokens goes ABOVE the
budget return, and anything that does goes below.
