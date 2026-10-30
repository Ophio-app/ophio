# SPDX-License-Identifier: Apache-2.0
"""Exercise the host supervisor without registering a runner or using systemd."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest


HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "isolated-job.sh"
TOKEN = "obviously-fake-host-token"
JIT = "obviously-fake-one-job-config"


class IsolatedJobTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.install = self.root / "pristine"
        self.install.mkdir()
        self.addCleanup(self.make_writable)
        self.rootfs = self.root / "rootfs"
        (self.rootfs / "bin").mkdir(parents=True)
        (self.rootfs / "bin/sh").touch(mode=0o755)
        (self.rootfs / "ci-shared/passed").mkdir(parents=True)
        (self.rootfs / "ci-shared/passed/forged").touch()
        self.token = self.root / "host-token"
        self.token.write_text(TOKEN, encoding="utf-8")
        self.token.chmod(0o600)
        self.trace = self.root / "sandbox-args.json"
        self.state = self.root / "state"
        self.requests = []
        self.response_status = 201
        self.response = {
            "runner": {"id": 42, "labels": [
                {"name": "self-hosted"}, {"name": "ophio-isolated"},
            ]}, "encoded_jit_config": JIT,
        }
        case = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                case.requests.append((self.path, self.headers["Authorization"],
                                      json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(case.response_status)
                if case.response_status == 302:
                    self.send_header("Location", "/redirected")
                self.end_headers()
                self.wfile.write(json.dumps(case.response).encode())

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)
        self.fake_bwrap = self.root / "fake-bwrap"
        self.fake_bwrap.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
pathlib.Path(os.environ["FAKE_TRACE"]).write_text(json.dumps(args))
env = {}
runner = config = None
for i, arg in enumerate(args):
    if arg == "--setenv":
        env[args[i+1]] = args[i+2]
    if arg == "--bind" and args[i+2] == "/runner":
        runner = args[i+1]
    if arg == "--ro-bind" and args[i+2] == "/run/jitconfig":
        config = args[i+1]
assert runner and config and "--clearenv" in args
env["HOME"] = runner + "/home"
command = args[-1].replace("/run/jitconfig", config).replace("/runner", runner)
os.chdir(runner)
os.execve("/bin/sh", ["/bin/sh", "-c", command], env)
''', encoding="utf-8")
        self.fake_bwrap.chmod(0o755)
        self.env = dict(os.environ, ISOLATED_REPOSITORY="example/project",
                        ISOLATED_TOKEN_FILE=str(self.token),
                        ISOLATED_RUNNER_INSTALL=str(self.install),
                        ISOLATED_ROOTFS=str(self.rootfs),
                        ISOLATED_STATE_DIR=str(self.state),
                        ISOLATED_API_URL=f"http://127.0.0.1:{self.server.server_port}",
                        ISOLATED_RUNNER_GROUP_ID="7", ISOLATED_TIMEOUT_SECONDS="20",
                        ISOLATED_BWRAP=str(self.fake_bwrap), FAKE_TRACE=str(self.trace),
                        HOST_SECRET=TOKEN)
        self.write_runner('''#!/bin/sh
set -eu
test "$1" = --jitconfig
test "$2" = obviously-fake-one-job-config
test ! -e planted
test ! -e .credentials
test ! -e "$HOME/planted"
test -z "${HOST_SECRET:-}${ISOLATED_TOKEN_FILE:-}${FAKE_TRACE:-}"
echo "runner_folder=$PWD"
echo "runner_home=$HOME"
echo planted > planted
echo planted > .credentials
echo planted > "$HOME/planted"
echo "token_visible=$(test -e TOKEN_PATH && echo yes || echo no)"
echo "shared_visible=$(test -e /ci-shared && echo yes || echo no)"
echo "root_writable=$(test -w / && echo yes || echo no)"
'''.replace("TOKEN_PATH", str(self.token)))

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def make_writable(self):
        for directory, _dirs, files in os.walk(self.install):
            Path(directory).chmod(0o755)
            for name in files:
                (Path(directory) / name).chmod(0o644)

    def write_runner(self, text):
        self.make_writable()
        (self.install / "run.sh").write_text(text, encoding="utf-8")
        (self.install / "run.sh").chmod(0o555)
        self.install.chmod(0o555)

    def run_job(self):
        return subprocess.run(["bash", str(SCRIPT)], env=self.env, capture_output=True,
                              text=True, timeout=35, encoding="utf-8")

    def assert_clean(self, result, expected):
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        self.assertEqual(list((self.state / "jobs").iterdir()), [])
        log = (self.state / "jobs.log").read_text(encoding="utf-8")
        self.assertRegex(log.splitlines()[-1],
                         rf"^job_id=job\.[A-Za-z0-9]+ exit={expected} duration_seconds=\d+$")
        self.assertNotIn(TOKEN, result.stdout + result.stderr + log)
        self.assertNotIn(JIT, log)

    def assert_mounts(self):
        args = json.loads(self.trace.read_text(encoding="utf-8"))
        self.assertIn("--die-with-parent", args)
        self.assertIn("--unshare-pid", args)
        self.assertIn("--new-session", args)
        self.assertIn("--clearenv", args)
        self.assertIn("--remount-ro", args)
        self.assertNotIn("/ci-shared", args)
        self.assertNotIn(str(self.rootfs / "ci-shared"), args)
        self.assertNotIn(str(self.token), args)
        self.assertNotIn(TOKEN, json.dumps(args))
        writable = [(args[i+1], args[i+2]) for i, arg in enumerate(args) if arg == "--bind"]
        self.assertEqual(len(writable), 1)
        self.assertEqual(writable[0][1], "/runner")
        self.assertTrue(writable[0][0].startswith(str(self.state / "jobs") + "/job."))
        self.assertFalse(Path(writable[0][0]).exists())
        return writable[0][0]

    def test_fresh_copy_labels_environment_and_cleanup(self):
        folders = []
        for _ in range(2):
            result = self.run_job()
            self.assert_clean(result, 0)
            folders.append(self.assert_mounts())
            self.assertIn("runner_home=" + folders[-1] + "/home", result.stdout)
        self.assertNotEqual(*folders)
        self.assertEqual(len((self.state / "jobs.log").read_text(encoding="utf-8").splitlines()), 2)
        self.assertFalse((self.install / "planted").exists())
        self.assertEqual(len(self.requests), 2)
        for path, authorization, body in self.requests:
            self.assertEqual(path, "/repos/example/project/actions/runners/generate-jitconfig")
            self.assertEqual(authorization, "Bearer " + TOKEN)
            self.assertEqual(body["labels"], ["self-hosted", "ophio-isolated"])
            self.assertEqual(body["runner_group_id"], 7)
            self.assertEqual(body["work_folder"], "_work")
            self.assertTrue(body["name"].startswith("ophio-isolated-job."))

    def test_failure_cleans_directories_with_removed_permissions(self):
        self.write_runner('#!/bin/sh\nmkdir locked\nchmod 000 locked\nexit 19\n')
        self.assert_clean(self.run_job(), 19)

    def test_timeout_kills_descendants_and_cleans(self):
        self.env["ISOLATED_TIMEOUT_SECONDS"] = "1"
        self.write_runner('''#!/bin/sh
trap '' TERM
sleep 60 &
echo "child_pid=$!"
wait
''')
        started = time.monotonic()
        result = self.run_job()
        self.assert_clean(result, 137)
        self.assertLess(time.monotonic() - started, 20)
        child = re.search(r"child_pid=(\d+)", result.stdout).group(1)
        stat = Path(f"/proc/{child}/stat")
        self.assertTrue(not stat.exists() or stat.read_text(encoding="utf-8").split()[2] == "Z")

    def test_timeout_cleans_when_term_is_obeyed(self):
        self.env["ISOLATED_TIMEOUT_SECONDS"] = "1"
        self.write_runner('#!/bin/sh\nsleep 60\n')
        self.assert_clean(self.run_job(), 124)

    def test_signal_cleans_and_logs(self):
        self.write_runner('#!/bin/sh\necho ready\nsleep 60\n')
        process = subprocess.Popen(["bash", str(SCRIPT)], env=self.env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, encoding="utf-8")
        try:
            self.assertEqual(process.stdout.readline().strip(), "ready")
            process.send_signal(signal.SIGTERM)
            out, err = process.communicate(timeout=10)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        self.assert_clean(subprocess.CompletedProcess(process.args, process.returncode, out, err), 143)

    def test_api_errors_are_redacted_and_never_start_sandbox(self):
        self.response_status = 403
        self.response = {"message": TOKEN}
        self.assert_clean(self.run_job(), 1)
        self.assertFalse(self.trace.exists())

    def test_redirect_is_refused(self):
        self.response_status = 302
        self.assert_clean(self.run_job(), 1)
        self.assertEqual(len(self.requests), 1)
        self.assertFalse(self.trace.exists())

    def test_unexpected_labels_are_refused(self):
        self.response["runner"]["labels"].append({"name": "trusted"})
        self.assert_clean(self.run_job(), 1)
        self.assertFalse(self.trace.exists())

    def test_token_must_be_private(self):
        self.token.chmod(0o644)
        self.assert_clean(self.run_job(), 1)
        self.assertEqual(self.requests, [])

    def test_token_cannot_be_under_a_root_mount(self):
        token = self.rootfs / "token"
        token.write_text(TOKEN, encoding="utf-8")
        token.chmod(0o600)
        self.env["ISOLATED_TOKEN_FILE"] = str(token)
        result = self.run_job()
        self.assertEqual(result.returncode, 1)
        self.assertIn("token must be outside", result.stderr)
        self.assertEqual(self.requests, [])

    def test_pristine_install_cannot_be_writable_or_registered(self):
        self.install.chmod(0o755)
        result = self.run_job()
        self.assertEqual(result.returncode, 1)
        self.assertIn("must be read-only", result.stderr)
        (self.install / ".runner").touch(mode=0o444)
        self.install.chmod(0o555)
        result = self.run_job()
        self.assertEqual(result.returncode, 1)
        self.assertIn("must be unregistered and unused", result.stderr)
        self.assertEqual(self.requests, [])

    def test_read_only_install_can_contain_symlinks(self):
        self.install.chmod(0o755)
        (self.install / "helper").symlink_to("run.sh")
        self.install.chmod(0o555)
        self.assert_clean(self.run_job(), 0)
        self.assertEqual((self.install / "run.sh").stat().st_mode & 0o222, 0)

    def test_real_bubblewrap(self):
        bwrap = shutil.which("bwrap")
        if not bwrap:
            self.skipTest("bubblewrap is not installed")
        probe = subprocess.run([bwrap, "--ro-bind", "/", "/", "--unshare-user",
                                "--unshare-pid", "--die-with-parent", "/bin/true"],
                               capture_output=True, text=True, encoding="utf-8")
        if probe.returncode:
            self.skipTest("bubblewrap namespace unavailable: " + probe.stderr.strip())
        # A tiny merged-/usr root like Ubuntu's (bin -> usr/bin, so the
        # script's symlink path runs too), made from host tools, never the
        # host home or credentials.
        shutil.rmtree(self.rootfs / "bin")
        (self.rootfs / "usr/bin").mkdir(parents=True)
        (self.rootfs / "bin").symlink_to("usr/bin")
        for tool in ("sh", "sleep", "cat"):
            self.copy_with_libraries(shutil.which(tool), "usr/bin/" + tool)
        (self.rootfs / "etc").mkdir(exist_ok=True)
        (self.rootfs / "etc/resolv.conf").touch()
        for name in ("dev", "proc", "tmp", "run", "var/tmp"):
            (self.rootfs / name).mkdir(parents=True, exist_ok=True)
        self.env["ISOLATED_BWRAP"] = bwrap
        for _ in range(2):
            result = self.run_job()
            self.assert_clean(result, 0)
            self.assertIn("runner_home=/runner/home", result.stdout)
            self.assertIn("token_visible=no", result.stdout)
            self.assertIn("shared_visible=no", result.stdout)
            self.assertIn("root_writable=no", result.stdout)
        self.write_runner('#!/bin/sh\nsleep 60\n')
        self.env["ISOLATED_TIMEOUT_SECONDS"] = "1"
        self.assert_clean(self.run_job(), 124)

    def copy_with_libraries(self, tool, destination):
        libraries = subprocess.run(["ldd", tool], capture_output=True, text=True, check=True, encoding="utf-8").stdout
        copies = [(tool, destination)]
        copies += [(name, name.lstrip("/")) for name in re.findall(r"/[\w/+.\-]+", libraries)]
        for source, relative in copies:
            target = self.rootfs / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o755)


class RoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import yaml
        except ImportError:
            raise unittest.SkipTest("PyYAML is needed for workflow checks") from None
        cls.workflow = yaml.load((HERE.parents[1] / ".github/workflows/checks.yml").read_text(encoding="utf-8"),
                                 Loader=yaml.BaseLoader)

    def evaluate(self, expression, *, ref="main", head="", override="", windows="off"):
        # Evaluate this workflow's boolean/string expressions from the YAML,
        # including short-circuiting and JSON array results.
        expression = expression.removeprefix("${{").removesuffix("}}").strip()
        expression = expression.replace("&&", " and ").replace("||", " or ")
        expression = re.sub(r"!(?!=)", " not ", expression).strip()
        return eval(expression, {"__builtins__": {}}, {
            "startsWith": lambda value, prefix: value.lower().startswith(prefix.lower()),
            "fromJSON": json.loads,
            "github": SimpleNamespace(head_ref=head, ref_name=ref),
            "vars": SimpleNamespace(CI_LINUX_RUNS_ON=override, CI_WINDOWS_RUNS_ON=windows),
            "runner": SimpleNamespace(environment="self-hosted"),
        })

    def test_push_filter_and_runner_selection(self):
        self.assertIn("report/**", self.workflow["on"]["push"]["branches"])
        cases = [
            ({"ref": "report/x"}, ["self-hosted", "ophio-isolated"]),
            ({"ref": "report/x", "override": "trusted"}, ["self-hosted", "ophio-isolated"]),
            ({"ref": "12/merge", "head": "report/x", "override": "ubuntu-24.04"},
             ["self-hosted", "ophio-isolated"]),
            ({"ref": "main"}, ["self-hosted", "Linux", "X64"]),
            ({"ref": "main", "override": "trusted"}, "trusted"),
            ({"ref": "12/merge", "head": "fix/x", "override": "ubuntu-24.04"}, "ubuntu-24.04"),
            ({"ref": "report/base", "head": "fix/x", "override": "trusted"}, "trusted"),
        ]
        for job in ("plan", "linux", "advisories"):
            for context, expected in cases:
                with self.subTest(job=job, context=context):
                    self.assertEqual(self.evaluate(self.workflow["jobs"][job]["runs-on"], **context),
                                     expected)

    def test_report_does_not_request_hosted_container_or_native_windows(self):
        linux = self.workflow["jobs"]["linux"]
        windows = self.workflow["jobs"]["windows"]
        for context in ({"ref": "report/x"}, {"ref": "12/merge", "head": "report/x"}):
            self.assertEqual(self.evaluate(linux["container"], override="ubuntu-24.04", **context), "")
            self.assertFalse(self.evaluate(windows["if"], windows="windows-2025", **context))
        self.assertEqual(self.evaluate(linux["container"], override="ubuntu-24.04")["image"],
                         "ubuntu:24.04")
        self.assertTrue(self.evaluate(windows["if"], windows="windows-2025"))
        self.assertEqual(self.evaluate(windows["runs-on"], windows="custom-windows"), "custom-windows")

    def test_report_never_reads_or_writes_passed_marks(self):
        plan = next(step for step in self.workflow["jobs"]["plan"]["steps"] if step.get("id") == "plan")
        record = next(step for step in self.workflow["jobs"]["linux"]["steps"]
                      if step.get("name") == "Record the passed tree")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ci-shared/passed").mkdir(parents=True)
            tools = root / "bin"
            tools.mkdir()
            fake_python = tools / "python3"
            fake_python.write_text('#!/bin/sh\nprintf "%s\\n" "$*" > "$TRACE"\n', encoding="utf-8")
            fake_python.chmod(0o755)
            for ref, head, expected in (("report/x", "", False), ("12/merge", "report/x", False),
                                        ("main", "", True)):
                report = self.evaluate(plan["env"]["REPORT_BRANCH"], ref=ref, head=head)
                env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                           TRACE=str(root / "trace"), EVENT="push", REPORT_BRANCH=str(report).lower(),
                           GITHUB_OUTPUT=str(root / "output"))
                subprocess.run(["bash", "-c", plan["run"].replace("/ci-shared", str(root / "ci-shared"))],
                               env=env, check=True, capture_output=True, text=True, encoding="utf-8")
                self.assertEqual("--marks" in (root / "trace").read_text(encoding="utf-8"), expected)
                self.assertEqual(self.evaluate(record["if"], ref=ref, head=head), expected)


if __name__ == "__main__":
    unittest.main()
