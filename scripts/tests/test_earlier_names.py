# SPDX-License-Identifier: Apache-2.0
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import earlier_names
from earlier_names import EARLIER, inspect

UPPER, TITLE = EARLIER.upper(), EARLIER.capitalize()


def rules(path: str, line: str) -> list[str]:
    return [hit["rule"] for hit in inspect(path, line)]


class EarlierNameTests(unittest.TestCase):
    def test_each_earlier_value_is_found(self):
        for line, rule in [
            (f'applicationId = "dev.{EARLIER}.phone"', "android-namespace"),
            (f"Scan {EARLIER}://pair?d=abc", "link-scheme"),
            (f"{EARLIER}-object://open/v1/host/task/id", "link-scheme"),
            (f"export {UPPER}_BRIDGE_TOKEN=1", "variable"),
            (f"VITE_{UPPER}_DOWNLOAD_URL", "variable"),
            (f'format!(".{EARLIER}-save-{{id}}-old")', "library-prefix"),
            (f"rm -r ~/.config/{EARLIER}", "directory"),
            (f'"$HOME/.local/share/{EARLIER}/db"', "directory"),
            (f"~/.local/opt/{EARLIER}/bin", "directory"),
            (f"Run `{EARLIER} pair` on the computer.", "command"),
            (f".\\{EARLIER}.exe doctor --json", "command"),
            (f"{EARLIER} rotate-certificate --confirm-repin", "command"),
        ]:
            self.assertEqual(rules("docs/guide.md", line), [rule], line)

    def test_current_names_and_role_names_pass(self):
        for line in [
            "ophio pair",
            'applicationId = "app.ophio"',
            "ophio://pair?d=abc and ophio-object://open/v1/",
            "OPHIO_BRIDGE_TOKEN and ~/.config/ophio and .ophio-save-",
            f"use {EARLIER}::config::APP_NAME;",
            f"cargo build -p {EARLIER} -p computer-mcp",
            f"crates/{EARLIER}/src/cli.rs",
            f'host.{EARLIER}_version and {{ kind: "{EARLIER}" }}',
            f'b"host-{EARLIER}.identity.dpapi.v1"',
            f"    {EARLIER} object {{",
            f"bash tools/review/connected.sh {EARLIER} pause",
            f"migrating `{EARLIER}.sqlite` to schema 1",
            f"The {EARLIER} starts the agent.",
        ]:
            self.assertEqual(rules("crates/example/src/lib.rs", line), [], line)

    def test_the_display_name_counts_everywhere_but_generated_bindings(self):
        line = f"<dt>{TITLE}</dt>"
        self.assertEqual(rules("client/src/screens/Versions.tsx", line), ["display-name"])
        self.assertEqual(rules("client/src-tauri/tauri.conf.json", f'"productName": "{TITLE}"'), ["display-name"])
        self.assertEqual(rules("docs/ARCHITECTURE.md", line), ["display-name"])
        self.assertEqual(rules("crates/example/package/NOTICE", f"{TITLE} Windows build"), ["display-name"])
        self.assertEqual(rules("tools/example.sh", f'echo "{TITLE}  stopped"'), ["display-name"])
        self.assertEqual(rules("client/src/protocol/generated/EventSource.ts", line), [])
        self.assertEqual(rules("client/src/screens/Welcome.tsx", f"<{TITLE}Download />"), [])
        self.assertEqual(rules("crates/store/src/lib.rs", f"source: EventSource::{TITLE},"), [])

    def test_only_needed_exceptions_are_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {
                "README.md": f"Run `{EARLIER} status`.\n",
                "upgrade.md": f"Ophio renames ~/.config/{EARLIER}.\n",
                "stale.md": "Nothing earlier here.\n",
                "icon.bin": f"\0{EARLIER} pair".encode(),
            }
            for name, content in files.items():
                path = root / name
                if isinstance(content, bytes):
                    path.write_bytes(content)
                else:
                    path.write_text(content, encoding="utf-8")
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
            subprocess.run(["git", "add", "--all"], cwd=root, check=True)
            allowed = {"upgrade.md": "explains upgrading", "stale.md": "no longer needed"}
            with mock.patch.object(earlier_names, "ALLOWED", allowed):
                hits = earlier_names.audit(root)
        self.assertEqual(
            hits,
            [
                {"path": "README.md", "line": 1, "rule": "command"},
                {"path": "stale.md", "line": 0, "rule": "unneeded-exception"},
            ],
        )

    def test_every_exception_names_its_reason(self):
        for path, reason in earlier_names.ALLOWED.items():
            self.assertTrue(reason.strip(), path)


if __name__ == "__main__":
    unittest.main()
