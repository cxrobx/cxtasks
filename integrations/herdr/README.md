# Desktop handoffs to Herdr

`install.sh` installs `cx-open-in-herdr` into `~/.local/bin`. CXTasks, CXMail
(including Continue in terminal), and Onyx's Claude Open session call it when
installed. Without it they retain their standalone terminal handoff.

The router always targets the **local default Herdr session**, independent of
the app's inherited terminal or remote environment. It starts/attaches Herdr
in Ghostty when needed and focuses the existing attached client when possible.
It uses Herdr's documented newline-delimited socket API (0.9.3 or later).

- Code work shares a space by canonical checkout path. Nested directories and
  symlinks resolve to that checkout; linked worktrees remain separate. Existing
  checkout/scope matches win. Otherwise a matching space name is reused, ignoring
  capitalization, spaces, and punctuation (for example, Quick Ask and QuickAsk).
  A different checkout or explicit project/account identity blocks that name
  match. Among matching names, a space already running an agent is preferred.
  A fresh space is created only when no compatible existing space is found.
- Each click opens a fresh tab with the original app-generated launch script
  passed as argv. Existing agents never receive shell input or a new prompt.
  On a new space, `layout.apply` targets only its initial `tab_id`; on a reused
  space it targets only `workspace_id`. Herdr rejects requests containing both.
  The script and prompt are frozen locally so a later click cannot change a
  queued tab's instructions.
- Repo-less CXTasks work uses a per-task scratch directory, grouped by project
  (or Inbox). Unmapped CXMail work remains in its message scratch directory,
  grouped by account. Claude's existing flags and MCP setup are preserved.
- Local routing state lives in `~/Library/Application Support/cx-herdr`. A file
  lock serializes simultaneous launches. Saved space IDs are checked against
  live paths or explicit scope metadata, including after Herdr restarts.
- Before a Claude Code launch starts, the router saves Claude Code's folder
  trust for the git repository it opens in, when a folder above that repository
  is already trusted. Claude Code stops looking for a trusted parent at a repo
  root, so a trusted `~/Projects` never covered a repo inside it, and each
  never-opened repo asked "Do you trust this folder?". A repository with nothing
  trusted above it still asks. The write takes Claude Code's own
  `~/.claude.json.lock`; if that is busy or the file is unreadable, nothing is
  written, the prompt shows, and the launch goes ahead.
- A failed or unconfirmed handoff is surfaced to the app. It never retries the
  launch in a different terminal. After a confirmed launch, a display/focus
  failure is a warning, so the same click cannot launch two sessions.

Verify without touching a live Herdr session:

```sh
python3 -m unittest discover -s integrations/herdr -v
```

The socket test needs permission to create a disposable local Unix socket.
The tests cover matching/creation, name variations, existing-space preference,
same-named repos, worktrees, symlinks, stale IDs, explicit project/account identity,
exclusive layout targets, launching the original Claude script, argv transport,
and no launch retries.

Protocol reference: <https://herdr.dev/docs/socket-api/>.
