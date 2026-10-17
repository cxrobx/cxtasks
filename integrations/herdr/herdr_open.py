#!/usr/bin/python3
"""Shared desktop handoff: one Herdr space per scope, a fresh tab per click.

Only explicit app-generated launch scripts are accepted. The command travels as
argv through layout.apply; no prompt is typed into a shell or an existing agent.
"""

import argparse
import fcntl
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import time
import uuid


TITLE = "Herdr · CX Apps"
MAX_RESPONSE = 8 * 1024 * 1024


class RoutingError(Exception):
    pass


def request(socket_path, method, params=None):
    ident = uuid.uuid4().hex
    payload = json.dumps({"id": ident, "method": method, "params": params or {}})
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(5)
        sock.connect(str(socket_path))
        sock.sendall(payload.encode("utf-8") + b"\n")
        data = bytearray()
        while b"\n" not in data:
            chunk = sock.recv(65536)
            if not chunk:
                raise RoutingError("Herdr closed the connection before replying.")
            data.extend(chunk)
            if len(data) > MAX_RESPONSE:
                raise RoutingError("Herdr returned an oversized response.")
    response = json.loads(data.split(b"\n", 1)[0])
    if response.get("id") != ident:
        raise RoutingError("Herdr replied with a different request ID.")
    if "error" in response:
        error = response["error"]
        raise RoutingError(f"Herdr {method}: {error.get('message', error)}")
    return response["result"]


def repo_root(folder):
    """Canonical checkout, including a worktree's own root, or exact folder."""
    folder = Path(folder).resolve()
    result = subprocess.run(
        ["/usr/bin/git", "-C", str(folder), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, timeout=3,
    )
    return Path(result.stdout.strip()).resolve() if result.returncode == 0 else folder


def select_workspace(workspaces, panes, scope, cached, path_scope):
    """Validate cached IDs against live state; never match just a display name."""
    ids = {w["workspace_id"] for w in workspaces}
    candidates = {}
    for pane in panes:
        wid = pane["workspace_id"]
        folder = pane.get("cwd") or pane.get("foreground_cwd")
        if folder:
            candidates.setdefault(wid, set()).add(str(repo_root(folder)))
        else:
            candidates.setdefault(wid, set()).add("unknown")
    cached_id = cached.get("workspace_id") if isinstance(cached, dict) else cached
    cached_paths = set(cached.get("paths", [])) if isinstance(cached, dict) else set()
    # A live ID can be reused after a full server restart. Its paths must still
    # belong to this scope; a saved ID alone is never an identity check.
    def belongs(wid):
        if wid not in ids:
            return False
        if path_scope:
            return candidates.get(wid) == {path_scope}
        if wid == cached_id and candidates.get(wid) and candidates[wid] <= cached_paths:
            return True
        return any(
            p.get("workspace_id") == wid and p.get("tokens", {}).get("cx_scope") == scope
            for p in panes
        )
    if cached_id and belongs(cached_id):
        return cached_id
    return next((w["workspace_id"] for w in workspaces if belongs(w["workspace_id"])), None)


def tab_label(launch):
    for filename, field in [("task.json", "title"), ("email.json", "subject")]:
        try:
            meta = json.loads((launch.parent / filename).read_text(encoding="utf-8"))
            title = meta.get(field) or "Claude"
            if filename == "task.json" and meta.get("short_id"):
                title = f"T{meta['short_id']} · {title}"
            return " ".join(str(title).split())[:120]
        except (OSError, ValueError, AttributeError):
            pass
    return "Claude"


def route(rpc, cwd, launch, label, scope, cache, tab=None):
    path_scope = None if scope else str(repo_root(cwd))
    scope = scope or "path:" + path_scope
    workspaces = rpc("workspace.list")["workspaces"]
    panes = rpc("pane.list")["panes"]
    wid = select_workspace(workspaces, panes, scope, cache.get(scope), path_scope)
    created = wid is None
    initial_tab = None
    if created:
        result = rpc("workspace.create", {"cwd": str(cwd), "label": label, "focus": False})
        wid = result["workspace"]["workspace_id"]
        initial_tab = result["tab"]["tab_id"]
    # Replacing only the tab just created avoids an unused shell in a new space.
    # Existing workspaces get a new tab; no existing process is replaced.
    params = {
        "workspace_id": wid, "tab_label": tab or tab_label(launch),
        "focus": True,
        "root": {"type": "pane", "cwd": str(cwd),
                 "command": ["/bin/zsh", "-l", str(launch)]},
    }
    if initial_tab:
        params["tab_id"] = initial_tab
    layout = rpc("layout.apply", params)["layout"]
    # Tag the pane so project/inbox spaces with distinct scratch directories
    # have explicit identity. Metadata is display-only, so a local cache is
    # advisory and is checked against both live panes and their tokens.
    try:
        rpc("pane.report_metadata", {
            "pane_id": layout["focused_pane_id"], "source": "cx-apps",
            "tokens": {"cx_scope": scope},
        })
    except (RoutingError, OSError):
        pass  # The launch succeeded; never make the caller submit it again.
    previous = cache.get(scope)
    paths = set(previous.get("paths", [])) if isinstance(previous, dict) else set()
    paths.add(str(repo_root(cwd)))
    cache[scope] = {"workspace_id": wid, "paths": sorted(paths)}
    return {"workspace_id": wid, "tab_id": layout["tab_id"],
            "pane_id": layout["focused_pane_id"], "created_space": created,
            "working_dir": str(cwd), "scope": scope}


def terminal_script(state_dir, herdr):
    script = state_dir / "attach.command"
    script.write_text(
        "#!/bin/zsh -l\nexec " + shlex.quote(str(herdr)) + " --session default\n",
        encoding="utf-8",
    )
    script.chmod(0o700)
    return script


def freeze_launch(launch, state_dir):
    """Keep a queued tab's prompt stable when the app reuses its scratch dir."""
    destination = state_dir / "launches" / uuid.uuid4().hex
    destination.mkdir(mode=0o700, parents=True)
    text = launch.read_text(encoding="utf-8")
    prompt = launch.parent / "prompt.txt"
    if prompt.is_file():
        frozen_prompt = destination / "prompt.txt"
        frozen_prompt.write_bytes(prompt.read_bytes())
        frozen_prompt.chmod(0o600)
        # All three apps shell-quote this explicit absolute filename. Preserve
        # the invocation and flags; only the cat input becomes immutable.
        quote = lambda path: "'" + str(path).replace("'", "'\\''") + "'"
        text = text.replace(quote(prompt), quote(frozen_prompt))
    frozen = destination / "launch.command"
    frozen.write_text(text, encoding="utf-8")
    frozen.chmod(0o700)
    return frozen


def show_terminal(herdr, state_dir, cwd):
    """Focus an attached Herdr client, or attach in a new Ghostty window."""
    script = terminal_script(state_dir, herdr)
    apple = '''on run argv
if application id "com.mitchellh.ghostty" is not running then return "cold"
tell application id "com.mitchellh.ghostty"
    repeat with surface in terminals
        if name of surface is (item 1 of argv) then
            focus surface
            activate
            return "focused"
        end if
    end repeat
    new window with configuration {initial working directory:(item 2 of argv), command:(item 3 of argv), wait after command:false}
    activate
    return "opened"
end tell
end run'''
    try:
        result = subprocess.run(
            ["/usr/bin/osascript", "-e", apple, TITLE, str(cwd), shlex.quote(str(script))],
            capture_output=True, text=True, timeout=8,
        )
        if result.returncode == 0 and result.stdout.strip() in {"opened", "focused"}:
            return
    except (OSError, subprocess.TimeoutExpired):
        pass
    ghostty = next((p for p in [Path("/Applications/Ghostty.app"),
                               Path.home() / "Applications/Ghostty.app"] if p.exists()), None)
    if not ghostty:
        raise RoutingError("Ghostty is not installed; open Herdr in a terminal to view the session.")
    result = subprocess.run(
        ["/usr/bin/open", "-na", str(ghostty), "--args", "--window-save-state=never",
         "--working-directory=" + str(cwd), "-e", str(script)],
        capture_output=True, text=True, timeout=8,
    )
    if result.returncode:
        raise RoutingError("Could not open the Herdr terminal: " + result.stderr.strip())


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", type=Path, required=True)
    parser.add_argument("--launch", type=Path, required=True)
    parser.add_argument("--space", help="Display label; identity comes from the checkout or --scope")
    parser.add_argument("--scope", help="Explicit app/project identity for repo-less handoffs")
    parser.add_argument("--tab", help="Label for the fresh conversation tab")
    args = parser.parse_args(argv)
    cwd = args.cwd.resolve(strict=True)
    launch = args.launch.resolve(strict=True)
    if not cwd.is_dir() or not launch.is_file():
        raise RoutingError("The working directory or launch script is missing.")
    herdr = Path.home() / ".local/bin/herdr"
    if not os.access(herdr, os.X_OK):
        raise RoutingError("Herdr is not installed at ~/.local/bin/herdr.")
    # Desktop clicks target the local default session, even when the app was
    # started from a named/remote Herdr pane. Never inherit its focused target.
    config_dir = Path.home() / ".config/herdr"
    socket_path = config_dir / "herdr.sock"
    state_dir = Path.home() / "Library/Application Support/cx-herdr"
    state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (state_dir / "routing.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        rpc = lambda method, params=None: request(socket_path, method, params)
        try:
            rpc("ping")
        except (OSError, RoutingError):
            show_terminal(herdr, state_dir, cwd)
            deadline = time.monotonic() + 10
            while True:
                try:
                    rpc("ping")
                    break
                except (OSError, RoutingError):
                    if time.monotonic() >= deadline:
                        raise RoutingError("Herdr did not start; no Claude session was launched.")
                    time.sleep(0.1)
        cache_path = state_dir / "spaces.json"
        try:
            cache = json.loads(cache_path.read_text(encoding="utf-8"))
            if not isinstance(cache, dict):
                cache = {}
        except (OSError, ValueError):
            cache = {}
        tab = args.tab or tab_label(launch)
        frozen = freeze_launch(launch, state_dir)
        result = route(rpc, cwd, frozen, args.space or repo_root(cwd).name, args.scope, cache, tab)
        # Past layout.apply, errors are presentation errors, never a reason to
        # return failure and cause a second launch on the next click.
        try:
            cache_path.write_text(json.dumps(cache, indent=2), encoding="utf-8")
            rpc("client.window_title.set", {"title": TITLE})
            time.sleep(0.1)  # Let the attached client emit the title to Ghostty.
            show_terminal(herdr, state_dir, cwd)
        except (OSError, ValueError, RoutingError, subprocess.TimeoutExpired) as error:
            result["warning"] = "Session opened in Herdr: " + str(error)
        print(json.dumps(result))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RoutingError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
