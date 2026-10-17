import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import unittest

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


if __name__ == "__main__":
    unittest.main()
