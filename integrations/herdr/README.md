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

## Running sessions on another Mac

The client says where Herdr runs by setting `CX_HERDR_HOST` when it starts this
helper: unset or empty means this Mac, anything else is an SSH target such as
`user@host`. Nothing here names a machine, so one build works on every Mac. A
GUI app doesn't see your shell's environment, so the app has to supply the value;
Quick Ask keeps it in Settings as **Herdr host**. Only clients that set it move.

With a host set, the helper copies the launch script's folder to the same
absolute path there, runs its own copy on that host with `--headless`, then
attaches a local Ghostty with `herdr --remote`. Routing, spaces and the folder
trust all happen on the host, against its own files.

- The host needs this helper in `~/.local/bin`, `python3` on a non-interactive
  SSH `PATH`, and a running Herdr. SSH must work without a prompt.
- Both Macs must reach the same absolute paths. If the home folders differ, link
  one to the other.
- Launch scripts must name the CLI so it resolves on the host: Quick Ask writes a
  bare `claude` or `codex` when a host is set.
- There is no fallback to local Herdr. A session that opened on the wrong Mac
  would split the work this exists to keep in one place, so an unreachable host
  or a folder missing there is an error, and nothing opens.

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
