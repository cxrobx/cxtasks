# Alfred integration — `t`

Find a task from Alfred's bar and open it in CXTasks.

| Type | Gets |
|---|---|
| `t 69`, `t T69` | That task. A prefix lists the band: `t T11` → T11, T110…T119 |
| `t` + words | Keyword and semantic search, fused into one list |
| `t` alone | What is in motion: in progress, in review, then the most recently touched |

Each row shows `T69  title`, with status · priority · project · due · deferral
under it. Hold **⌘** to read the notes in the subtitle, **⌥** for tags, repo
and tripwire. **⇧** or **⌘Y** opens a Quick Look page with the whole task:
notes, what it waits on, subtasks, seed prompt, the latest comments. **⌘L**
shows the notes in large type, and **⌘C** copies the T-number.

**↩ opens the task in CXTasks** through `cxtasks://task/<id>`. The app handles
that link in `src-tauri/src/links.rs` and reveals the task the way a
notification click does, so it works when the app is minimized, hidden or
not running. The scheme exists only in an installed build, never in
`tauri dev`.

```bash
./install.sh     # copies the workflow into Alfred's preferences; re-run after editing
```

## The search

`cxtasks_search.py` opens the live database **read-only** and runs three
lanes:

- **T-number:** a lone `T69`, `t69` or `69`. This is the same query as the
  app's `short_id_ids`: exact first, then the prefix band. A reference is not
  text, so nothing else is searched.
- **Keyword:** the app's `tasks_fts` index, with `build_match_expr`'s rules
  (every word required, the last one prefix-matched) and its bm25 column
  weights.
- **Semantic:** cosine similarity against `nomic-embed-text` vectors from the
  local Ollama. The vectors are cached in `~/Library/Caches/cxtasks-alfred/`,
  never in the CXTasks database. They are keyed by task id and a hash of the
  text, so a task is embedded again only when its words change.

The keyword and semantic lanes are merged by reciprocal-rank fusion, so a task
both lanes find ranks above one that only a single lane finds. Trashed tasks
are left out and archived ones are kept, as in the app's search.

Two thresholds keep the semantic lane from padding every list. They were
measured on the live tasks: a paraphrase scores 0.64–0.72 against its task,
and unrelated tasks score 0.55–0.60 against almost any query. A semantic hit
must clear 0.57 **and** land within 0.06 of the best score.

The semantic lane can fail without breaking anything. If Ollama is down, slow
(1.5 s timeout) or missing the model, the list comes from the other two lanes.
The first run embeds at most 32 tasks per keystroke and asks Alfred to rerun
until the cache is full, so it never blocks typing.

Alfred runs the script with the system Python (`/usr/bin/python3`, 3.9), and
it uses only the standard library.
