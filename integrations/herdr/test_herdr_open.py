import json
import os
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
            if ("workspace_id" in params) == ("tab_id" in params):
                raise router.RoutingError("layout.apply requires exactly one target")
            return {"layout": {"workspace_id": params.get("workspace_id", "created"), "tab_id": "new-tab",
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
        self.assertEqual(params["workspace_id"], "right")
        self.assertEqual(params["root"]["command"], ["/bin/zsh", "-l", str(self.launch)])

    def test_creates_space_when_another_repo_has_the_same_name(self):
        other = self.root / "other" / "repo"
        other.mkdir(parents=True)
        rpc = FakeHerdr([{"workspace_id": "wrong", "label": "Repo"}], [self.pane("wrong", other)])
        result = router.route(rpc, self.repo, self.launch, "Repo", None, {})
        self.assertTrue(result["created_space"])
        params = next(p for m, p in rpc.calls if m == "layout.apply")
        self.assertEqual(params["tab_id"], "initial")  # Only the newly created shell is replaced.
        self.assertNotIn("workspace_id", params)

    def test_reuses_named_space_with_spacing_and_case_differences(self):
        rpc = FakeHerdr([{"workspace_id": "existing", "label": "QUICK ASK"}],
                        [self.pane("existing", self.root)])
        result = router.route(rpc, self.repo, self.launch, "QuickAsk", None, {})
        self.assertEqual(result["workspace_id"], "existing")
        self.assertFalse(result["created_space"])
        self.assertNotIn("workspace.create", [method for method, _ in rpc.calls])
        params = next(p for method, p in rpc.calls if method == "layout.apply")
        self.assertEqual(params["workspace_id"], "existing")
        self.assertNotIn("tab_id", params)  # Add a tab; preserve every existing tab/process.
        self.assertEqual(params["root"]["cwd"], str(self.repo))

    def test_reuses_matching_named_space_without_pane_directory_metadata(self):
        rpc = FakeHerdr([{"workspace_id": "existing", "label": "Quick-Ask"}],
                        [{"workspace_id": "existing"}])
        result = router.route(rpc, self.repo, self.launch, "Quick Ask", None, {})
        self.assertEqual(result["workspace_id"], "existing")
        self.assertFalse(result["created_space"])

    def test_general_handoff_reuses_existing_project_space(self):
        rpc = FakeHerdr([{"workspace_id": "code", "label": "QuickAsk"}],
                        [self.pane("code", self.repo, tokens={"cx_scope": "path:" + str(self.repo)})])
        result = router.route(rpc, self.root, self.launch, "Quick Ask", "quickask:general", {})
        self.assertEqual(result["workspace_id"], "code")
        self.assertFalse(result["created_space"])

    def test_named_space_never_overrides_a_different_checkout(self):
        other = self.root / "different-project"
        other.mkdir()
        subprocess.run(["/usr/bin/git", "init", "-q", str(other)], check=True)
        rpc = FakeHerdr([{"workspace_id": "wrong", "label": "Quick Ask"}],
                        [self.pane("wrong", other)])
        result = router.route(rpc, self.repo, self.launch, "QuickAsk", None, {})
        self.assertTrue(result["created_space"])

    def test_named_space_never_overrides_a_different_explicit_account(self):
        rpc = FakeHerdr([{"workspace_id": "wrong", "label": "Inbox"}],
                        [self.pane("wrong", self.root, tokens={"cx_scope": "cxmail:other"})])
        result = router.route(rpc, self.repo, self.launch, "Inbox", "cxmail:requested", {})
        self.assertTrue(result["created_space"])

    def test_known_checkout_wins_over_a_named_space(self):
        rpc = FakeHerdr([{"workspace_id": "named", "label": "Quick Ask"},
                         {"workspace_id": "exact", "label": "Development"}],
                        [self.pane("named", self.root, agent="claude"), self.pane("exact", self.repo)])
        result = router.route(rpc, self.repo, self.launch, "QuickAsk", None, {})
        self.assertEqual(result["workspace_id"], "exact")
        self.assertFalse(result["created_space"])

    def test_duplicate_names_prefer_the_space_already_running_an_agent(self):
        rpc = FakeHerdr([{"workspace_id": "empty", "label": "Quick Ask"},
                         {"workspace_id": "active", "label": "QuickAsk"}],
                        [self.pane("empty", self.root), self.pane("active", self.root, agent="claude")])
        result = router.route(rpc, self.repo, self.launch, "Quick Ask", None, {})
        self.assertEqual(result["workspace_id"], "active")
        self.assertFalse(result["created_space"])

    def test_new_space_executes_the_original_claude_launch_script(self):
        prompt = self.root / "prompt.txt"
        prompt.write_text("Fix the requested QuickAsk issue.\nPreserve café and quotes: ' \".")
        captured = self.root / "claude-started.json"
        fake_claude = self.root / "fake claude"
        fake_claude.write_text(
            "#!/usr/bin/python3\nimport json,sys\n"
            + "open(" + repr(str(captured)) + ", 'w').write(json.dumps(sys.argv[1:]))\n"
        )
        fake_claude.chmod(0o700)
        quote = router.shlex.quote
        self.launch.write_text(
            "#!/bin/zsh -f\nset -eu\nexec " + quote(str(fake_claude))
            + ' -- "$(cat ' + quote(str(prompt)) + ')"\n'
        )
        rpc = FakeHerdr()

        def execute_layout(method, params=None):
            result = rpc(method, params)  # Validate the real target exclusivity rule first.
            if method == "layout.apply":
                subprocess.run(params["root"]["command"], cwd=params["root"]["cwd"],
                               check=True, capture_output=True, text=True)
            return result

        result = router.route(execute_layout, self.repo, self.launch, "Quick Ask", "quickask:general", {})
        self.assertTrue(result["created_space"])
        self.assertEqual(json.loads(captured.read_text()), ["--", prompt.read_text()])

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
        rpc = FakeHerdr([{"workspace_id": "main", "label": "Repo"}], [self.pane("main", self.repo)])
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


# Stands in for /usr/bin/ssh: runs the remote command in a shell, with the local session
# folder swapped for a "host" one so a real copy happens and real quoting is exercised.
FAKE_SSH = """#!/usr/bin/python3
import json, os, subprocess, sys
state = os.environ["FAKE_SSH_STATE"]
with open(state, "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if os.environ.get("FAKE_SSH_EXIT"):
    sys.exit(int(os.environ["FAKE_SSH_EXIT"]))
command = sys.argv[-1].replace(os.environ["FAKE_FROM"], os.environ["FAKE_TO"])
sys.exit(subprocess.run(["/bin/sh", "-c", command]).returncode)
"""


class RemoteHostTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cx-herdr-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.home = self.root / "laptop"
        (self.home / ".local/bin").mkdir(parents=True)
        (self.home / ".local/bin/herdr").write_text("#!/bin/sh\n", encoding="utf-8")
        (self.home / ".local/bin/herdr").chmod(0o700)
        # A space in the path, as ~/Library/Application Support has.
        self.session = self.root / "App Support/Sessions/one"
        self.session.mkdir(parents=True)
        (self.session / "prompt.txt").write_text("the prompt", encoding="utf-8")
        self.launch = self.session / "launch.command"
        self.launch.write_text("#!/bin/zsh\nexec claude\n", encoding="utf-8")
        self.launch.chmod(0o700)
        self.host_home = self.root / "mini"
        self.remote_helper = self.host_home / ".local/bin/cx-open-in-herdr"
        self.remote_helper.parent.mkdir(parents=True)
        self.helper_reply("{\"workspace_id\": \"w\", \"tab_id\": \"t\"}", 0)
        self.state = self.root / "ssh.log"
        fake = self.root / "fake-ssh"
        fake.write_text(FAKE_SSH, encoding="utf-8")
        fake.chmod(0o700)
        self.mirror = self.root / "on-host"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.host_home), "FAKE_SSH_STATE": str(self.state),
                    "FAKE_FROM": str(self.session), "FAKE_TO": str(self.mirror)}
        for patch in (unittest.mock.patch.object(router, "SSH", [str(fake)]),
                      unittest.mock.patch.object(router.Path, "home", return_value=self.home),
                      unittest.mock.patch.dict(router.os.environ, self.env)):
            patch.start()
            self.addCleanup(patch.stop)

    def helper_reply(self, reply, status):
        self.remote_helper.write_text(f"import sys\nprint({reply!r})\nsys.exit({status})\n", encoding="utf-8")

    def calls(self):
        return [json.loads(line) for line in self.state.read_text(encoding="utf-8").splitlines()]

    def open(self, host="cx@mini"):
        with unittest.mock.patch.dict(router.os.environ, {router.HOST_VARIABLE: host}), \
                unittest.mock.patch.object(router, "request", side_effect=AssertionError("local Herdr touched")) as local, \
                unittest.mock.patch.object(router, "show_terminal") as terminal, \
                unittest.mock.patch("builtins.print") as printed:
            status = router.main(["--cwd", "/Users/you/Projects/only-on-host", "--launch", str(self.launch),
                                  "--tab", "Quick Ask · Claude", "--space", "Quick Ask"])
        self.assertFalse(local.called)
        return status, terminal, printed

    def test_a_named_host_gets_the_session_and_never_the_local_herdr(self):
        status, terminal, printed = self.open()
        self.assertEqual(status, 0)
        self.assertEqual((self.mirror / "prompt.txt").read_text(encoding="utf-8"), "the prompt")
        self.assertTrue(os.access(self.mirror / "launch.command", os.X_OK))
        command = self.calls()[-1][-1]
        self.assertIn("--headless", command)
        self.assertIn("'Quick Ask · Claude'", command)
        self.assertEqual(terminal.call_args.args[3], "cx@mini")
        self.assertEqual(json.loads(printed.call_args.args[0])["tab_id"], "t")

    def test_no_host_keeps_the_local_route(self):
        with unittest.mock.patch.object(router, "open_on_host") as forwarded, \
                unittest.mock.patch.object(router, "request", side_effect=OSError("stop here")), \
                unittest.mock.patch.object(router, "show_terminal"), \
                unittest.mock.patch.object(router.time, "monotonic", side_effect=[0, 99]):
            for value in ("", "   "):
                with unittest.mock.patch.dict(router.os.environ, {router.HOST_VARIABLE: value}), \
                        self.assertRaises(router.RoutingError):
                    router.main(["--cwd", str(self.root), "--launch", str(self.launch)])
                router.time.monotonic.side_effect = [0, 99]
        self.assertFalse(forwarded.called)

    def test_the_host_itself_never_forwards_again(self):
        with unittest.mock.patch.dict(router.os.environ, {router.HOST_VARIABLE: "cx@mini"}), \
                unittest.mock.patch.object(router, "open_on_host") as forwarded, \
                self.assertRaises(router.HostError):
            router.main(["--headless", "--cwd", str(self.root / "missing"), "--launch", str(self.launch)])
        self.assertFalse(forwarded.called)

    def test_an_unreachable_host_opens_nothing_anywhere(self):
        with unittest.mock.patch.dict(router.os.environ, {"FAKE_SSH_EXIT": "255"}), \
                self.assertRaisesRegex(router.HostError, "Nothing was opened"):
            self.open()
        self.assertEqual(len(self.calls()), 1)

    def test_a_host_without_the_helper_says_so(self):
        self.remote_helper.unlink()
        with self.assertRaisesRegex(router.HostError, "isn't installed on cx@mini"):
            self.open()
        self.assertEqual(len(self.calls()), 1)

    def test_the_hosts_own_sentence_reaches_the_client(self):
        self.helper_reply('{"message": "/x doesn\'t exist on the Herdr host."}', 1)
        with self.assertRaisesRegex(router.HostError, "doesn't exist on the Herdr host"):
            self.open()

    def test_a_lost_reply_never_claims_nothing_opened(self):
        self.helper_reply("not json", 1)
        with self.assertRaises(router.HostError) as caught:
            self.open()
        self.assertIn("Check Herdr there", str(caught.exception))
        self.assertNotIn("Nothing was opened", str(caught.exception))
        self.assertEqual(len(self.calls()), 2)  # One copy, one launch: never a retry.

    def test_a_target_cannot_become_an_ssh_option(self):
        with self.assertRaisesRegex(router.HostError, "isn't an SSH target"):
            self.open("-oProxyCommand=touch /tmp/x")
        self.assertFalse(self.state.exists())

    def test_the_terminal_attaches_to_the_host(self):
        state = self.root / "state"
        state.mkdir()
        script = router.terminal_script(state, Path("/bin/herdr"), "cx@mini").read_text(encoding="utf-8")
        self.assertIn("/bin/herdr --remote cx@mini", script)
        script = router.terminal_script(state, Path("/bin/herdr")).read_text(encoding="utf-8")
        self.assertIn("/bin/herdr --session default", script)

    def test_a_bare_claude_command_still_earns_trust_on_the_host(self):
        self.launch.write_text("cd /repo\nexec 'claude' --model opus -- \"$(cat p)\"\n", encoding="utf-8")
        self.assertEqual(router.claude_folder(self.launch, self.root), Path("/repo"))


if __name__ == "__main__":
    unittest.main()
