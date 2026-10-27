import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import unittest
import unittest.mock

import herdr_open as router


class FakeHerdr:
    def __init__(self, workspaces=None, panes=None):
        self.workspaces = workspaces or []
        self.panes = panes or []
        self.calls = []

    def __call__(self, method, params=None):
        params = params or {}
        self.calls.append((method, params))
        if method == "workspace.list":
            return {"workspaces": self.workspaces}
        if method == "pane.list":
            return {"panes": self.panes}
        if method == "workspace.create":
            return {"workspace": {"workspace_id": "created"}, "tab": {"tab_id": "initial"}}
        if method == "layout.apply":
            return {"layout": {"workspace_id": params["workspace_id"], "tab_id": "new-tab",
                               "focused_pane_id": "new-pane"}}
        return {}


class RoutingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cx-herdr-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.launch = self.root / "a 'quoted' $launch.command"
        self.launch.write_text("#!/bin/zsh\nexit 0\n")

    def pane(self, wid, cwd, **extra):
        return {"workspace_id": wid, "cwd": str(cwd), **extra}

    def test_reuses_exact_scope_and_leaves_existing_tabs_alone(self):
        rpc = FakeHerdr([{"workspace_id": "right"}], [self.pane("right", self.repo)])
        result = router.route(rpc, self.repo, self.launch, "Repo", None, {})
        self.assertEqual(result["workspace_id"], "right")
        methods = [m for m, _ in rpc.calls]
        self.assertNotIn("workspace.create", methods)
        params = next(p for m, p in rpc.calls if m == "layout.apply")
        self.assertNotIn("tab_id", params)  # Never replaces an existing tab.
        self.assertEqual(params["root"]["command"], ["/bin/zsh", "-l", str(self.launch)])

    def test_creates_space_when_another_repo_has_the_same_name(self):
        other = self.root / "other" / "repo"
        other.mkdir(parents=True)
        rpc = FakeHerdr([{"workspace_id": "wrong", "label": "Repo"}], [self.pane("wrong", other)])
        result = router.route(rpc, self.repo, self.launch, "Repo", None, {})
        self.assertTrue(result["created_space"])
        params = next(p for m, p in rpc.calls if m == "layout.apply")
        self.assertEqual(params["tab_id"], "initial")  # Only the newly created shell is replaced.

    def test_stale_cached_id_cannot_route_into_a_different_repo(self):
        other = self.root / "other"
        other.mkdir()
        rpc = FakeHerdr([{"workspace_id": "reused-id"}], [self.pane("reused-id", other)])
        cache = {"path:" + str(self.repo): "reused-id"}
        self.assertTrue(router.route(rpc, self.repo, self.launch, "Repo", None, cache)["created_space"])

    def test_mixed_repo_workspace_does_not_capture_a_scoped_handoff(self):
        other = self.root / "other"
        other.mkdir()
        rpc = FakeHerdr([{"workspace_id": "mixed"}],
                       [self.pane("mixed", self.repo), self.pane("mixed", other)])
        self.assertTrue(router.route(rpc, self.repo, self.launch, "Repo", None, {})["created_space"])

    def test_symlink_and_nested_checkout_directories_share_the_same_space(self):
        subprocess.run(["/usr/bin/git", "init", "-q", str(self.repo)], check=True)
        child = self.repo / "docs"
        child.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(child)
        rpc = FakeHerdr([{"workspace_id": "repo"}], [self.pane("repo", self.repo)])
        result = router.route(rpc, alias, self.launch, "Repo", None, {})
        self.assertEqual(result["workspace_id"], "repo")
        params = next(p for m, p in rpc.calls if m == "layout.apply")
        self.assertEqual(params["root"]["cwd"], str(alias))

    def test_linked_worktree_has_its_own_scope(self):
        subprocess.run(["/usr/bin/git", "init", "-q", str(self.repo)], check=True)
        subprocess.run(["/usr/bin/git", "-C", str(self.repo), "-c", "user.name=Test", "-c",
                        "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "test"], check=True)
        child = self.root / "worktree"
        subprocess.run(["/usr/bin/git", "-C", str(self.repo), "worktree", "add", "--detach", str(child)],
                       check=True, capture_output=True)
        self.assertEqual(router.repo_root(child), child)
        rpc = FakeHerdr([{"workspace_id": "main"}], [self.pane("main", self.repo)])
        self.assertTrue(router.route(rpc, child, self.launch, "Repo", None, {})["created_space"])

    def test_project_space_reuses_explicit_identity_across_task_scratch_dirs(self):
        rpc = FakeHerdr([{"workspace_id": "project"}],
                       [self.pane("project", self.root, tokens={"cx_scope": "cxtasks:project"})])
        result = router.route(rpc, self.repo, self.launch, "Project", "cxtasks:project", {})
        self.assertEqual(result["workspace_id"], "project")
        self.assertFalse(result["created_space"])

    def test_other_project_tokens_cannot_satisfy_the_cached_id(self):
        rpc = FakeHerdr([{"workspace_id": "project"}],
                       [self.pane("project", self.root, tokens={"cx_scope": "cxmail:other"})])
        result = router.route(rpc, self.repo, self.launch, "Project", "cxmail:me", {"cxmail:me": "project"})
        self.assertTrue(result["created_space"])

    def test_project_space_survives_loss_of_display_metadata_after_restart(self):
        rpc = FakeHerdr([{"workspace_id": "project"}], [self.pane("project", self.root)])
        cache = {"cxtasks:project": {"workspace_id": "project", "paths": [str(self.root)]}}
        result = router.route(rpc, self.repo, self.launch, "Project", "cxtasks:project", cache)
        self.assertEqual(result["workspace_id"], "project")

    def test_unknown_pane_directory_does_not_prove_a_workspace_is_scoped(self):
        rpc = FakeHerdr([{"workspace_id": "uncertain"}],
                       [self.pane("uncertain", self.repo), {"workspace_id": "uncertain"}])
        self.assertTrue(router.route(rpc, self.repo, self.launch, "Repo", None, {})["created_space"])

    def test_uncertain_launch_is_never_retried_or_sent_as_shell_input(self):
        rpc = FakeHerdr()
        def failing(method, params=None):
            if method == "layout.apply":
                rpc.calls.append((method, params))
                raise router.RoutingError("connection lost")
            return rpc(method, params)
        with self.assertRaises(router.RoutingError):
            router.route(failing, self.repo, self.launch, "Repo", None, {})
        self.assertEqual(sum(m == "layout.apply" for m, _ in rpc.calls), 1)
        self.assertFalse(any(m.startswith("pane.send") for m, _ in rpc.calls))

    def test_metadata_failure_does_not_report_a_successful_launch_as_failed(self):
        rpc = FakeHerdr()
        def no_metadata(method, params=None):
            if method == "pane.report_metadata":
                raise router.RoutingError("unsupported")
            return rpc(method, params)
        self.assertEqual(router.route(no_metadata, self.repo, self.launch, "Repo", None, {})["tab_id"], "new-tab")

    def test_handoff_metadata_produces_a_readable_tab_label(self):
        (self.root / "task.json").write_text(json.dumps({"short_id": 42, "title": "Fix\n routing"}))
        self.assertEqual(router.tab_label(self.launch), "T42 · Fix routing")

    def test_new_click_cannot_change_the_prompt_of_an_already_queued_tab(self):
        prompt = self.root / "prompt.txt"
        prompt.write_text("first prompt")
        quote = lambda path: "'" + str(path).replace("'", "'\\''") + "'"
        self.launch.write_text('#!/bin/zsh\ncat ' + quote(prompt) + '\n')
        frozen = router.freeze_launch(self.launch, self.root / "router-state")
        prompt.write_text("second prompt")
        self.launch.write_text("exit 1\n")
        output = subprocess.run(["/bin/zsh", str(frozen)], capture_output=True, text=True, check=True)
        self.assertEqual(output.stdout.strip(), "first prompt")

    def test_socket_transport_preserves_argv_and_checks_response_identity(self):
        socket_path = self.root / "server.sock"
        ready = threading.Event()
        seen = []
        def serve():
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(str(socket_path))
                server.listen(1)
                ready.set()
                with server.accept()[0] as client:
                    req = json.loads(client.makefile("rb").readline())
                    seen.append(req)
                    client.sendall(json.dumps({"id": req["id"], "result": {"ok": True}}).encode() + b"\n")
        worker = threading.Thread(target=serve)
        worker.start()
        ready.wait(2)
        argv = ["/bin/zsh", "-l", str(self.launch)]
        self.assertEqual(router.request(socket_path, "test", {"command": argv}), {"ok": True})
        worker.join(2)
        self.assertEqual(seen[0]["params"]["command"], argv)


class ClaudeTrustTests(unittest.TestCase):
    """A trusted ~/Projects doesn't reach a git repo inside it; the router carries that trust across."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cx-herdr-trust-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.trusted = self.root / "projects"
        self.repo = self.trusted / "new-repo"
        self.stray = self.root / "downloads" / "stray-repo"
        for folder in [self.repo / ".git", self.repo / "src", self.stray / ".git", self.trusted / "plain"]:
            folder.mkdir(parents=True)
        self.config = self.root / "claude.json"
        self.config.write_text(json.dumps({
            "numStartups": 7, "oauthAccount": {"emailAddress": "dana@example.com"},
            "projects": {str(self.trusted): {"hasTrustDialogAccepted": True, "allowedTools": ["Bash"]}},
        }), encoding="utf-8")
        self.config.chmod(0o600)

    def saved(self):
        return json.loads(self.config.read_text(encoding="utf-8"))

    def launch_script(self, agent, folder):
        script = self.root / "launch.command"
        script.write_text(f"#!/bin/zsh -l\nset -e\ncd '{folder}'\nexec '{agent}' \"$(cat '/tmp/prompt.txt')\"\n", encoding="utf-8")
        return script

    def test_repo_under_a_trusted_folder_is_trusted_like_accepting_the_prompt(self):
        self.assertEqual(router.extend_claude_trust(self.repo / "src", self.config), self.repo)
        saved = self.saved()
        entry = saved["projects"][str(self.repo)]
        self.assertIs(entry["hasTrustDialogAccepted"], True)
        self.assertEqual(entry["allowedTools"], [])
        self.assertEqual(saved["numStartups"], 7)
        self.assertEqual(saved["oauthAccount"], {"emailAddress": "dana@example.com"})
        self.assertEqual(saved["projects"][str(self.trusted)]["allowedTools"], ["Bash"])
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        self.assertFalse(Path(str(self.config) + ".lock").exists())
        before = self.config.read_bytes()
        self.assertIsNone(router.extend_claude_trust(self.repo, self.config))
        self.assertEqual(self.config.read_bytes(), before)

    def test_nothing_is_written_where_claude_code_should_still_ask_or_already_trusts(self):
        before = self.config.read_bytes()
        self.assertIsNone(router.extend_claude_trust(self.stray, self.config))
        self.assertIsNone(router.extend_claude_trust(self.trusted / "plain", self.config))
        self.assertEqual(self.config.read_bytes(), before)

    def test_waits_out_claude_codes_lock_and_never_takes_it(self):
        lock = Path(str(self.config) + ".lock")
        lock.mkdir()
        self.addCleanup(lambda: lock.exists() and lock.rmdir())
        with unittest.mock.patch.object(router.time, "sleep"):
            self.assertIsNone(router.extend_claude_trust(self.repo, self.config))
        self.assertTrue(lock.is_dir())
        self.assertNotIn(str(self.repo), self.saved()["projects"])

    def test_only_a_claude_launch_names_a_folder(self):
        claude = self.launch_script(Path.home() / ".local/bin/claude", self.repo / "src")
        self.assertEqual(router.claude_folder(claude, self.root), self.repo / "src")
        versioned = self.launch_script(Path.home() / ".local/share/claude/versions/2.1.312", self.repo)
        self.assertEqual(router.claude_folder(versioned, self.root), self.repo)
        codex = self.launch_script("/opt/homebrew/bin/codex", self.repo)
        self.assertIsNone(router.claude_folder(codex, self.root))

    def test_main_trusts_the_repo_before_the_session_starts(self):
        script = self.launch_script(Path.home() / ".local/bin/claude", self.repo)
        seen = {}

        def route(rpc, cwd, launch, label, scope, cache, tab=None):
            seen["entry"] = self.saved()["projects"].get(str(self.repo))
            return {"workspace_id": "w", "tab_id": "t"}

        home = self.root / "home"
        (home / ".local/bin").mkdir(parents=True)
        (home / ".local/bin/herdr").write_text("#!/bin/sh\n", encoding="utf-8")
        (home / ".local/bin/herdr").chmod(0o700)
        with unittest.mock.patch.object(router.Path, "home", return_value=home), \
                unittest.mock.patch.object(router, "CLAUDE_CONFIG", self.config), \
                unittest.mock.patch.object(router, "request", return_value={}), \
                unittest.mock.patch.object(router, "route", side_effect=route), \
                unittest.mock.patch.object(router, "show_terminal"), \
                unittest.mock.patch("builtins.print"):
            self.assertEqual(router.main(["--cwd", str(self.repo), "--launch", str(script)]), 0)
        self.assertIs(seen["entry"]["hasTrustDialogAccepted"], True)


if __name__ == "__main__":
    unittest.main()
