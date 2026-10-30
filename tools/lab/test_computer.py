# SPDX-License-Identifier: Apache-2.0
"""Validate authored delegation and readiness without starting a real harness."""

import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "fixture", ROOT / "tools/lab/guest/fake-computer.py"
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class ComputerFixtureTests(unittest.TestCase):
    def test_help_covers_every_adapter_readiness_flag(self):
        adapter = (ROOT / "crates/harness-claude/src/delegation.rs").read_text(
            encoding="utf-8"
        )
        flags = re.findall(r'"(--[\w-]+)"', adapter.split("pub(crate) fn arguments")[0])
        # The fake records each help call beside itself, so run a copy.
        with tempfile.TemporaryDirectory() as temp:
            fake = shutil.copy(
                ROOT / "crates/harness-claude/tests/fake_claude.py", temp
            )
            help_text = subprocess.check_output(
                [sys.executable, fake, "--help"],
                text=True,
                encoding="utf-8",
            )
        for flag in set(flags) - {"--help"}:
            self.assertIn(flag, help_text)

    def arguments(self):
        return [
            "--agents",
            json.dumps(
                {
                    "computer-supervisor": {"tools": ["Agent(computer-specialist)"]},
                    "computer-specialist": {
                        "tools": ["mcp__computer__" + t for t in fixture.TOOLS],
                        "mcpServers": [
                            {
                                "computer": {
                                    "command": "/fixture/computer-mcp",
                                    "args": [],
                                }
                            }
                        ],
                    },
                }
            ),
            "--mcp-config",
            '{"mcpServers":{}}',
        ]

    def test_uses_only_the_child_scoped_bridge(self):
        server = fixture.specialist_server(
            self.arguments(), Path("/fixture/computer-mcp")
        )
        self.assertEqual(server["command"], "/fixture/computer-mcp")

    def test_refuses_parent_bridge_or_unexpected_executable(self):
        args = self.arguments()
        args[-1] = '{"mcpServers":{"computer":{}}}'
        with self.assertRaises(ValueError):
            fixture.specialist_server(args, Path("/fixture/computer-mcp"))
        with self.assertRaises(ValueError):
            fixture.specialist_server(self.arguments(), Path("/different/computer-mcp"))

    def test_refuses_extra_child_tool(self):
        args = self.arguments()
        agents = json.loads(args[1])
        agents["computer-specialist"]["tools"].append("Bash")
        args[1] = json.dumps(agents)
        with self.assertRaises(ValueError):
            fixture.specialist_server(args, Path("/fixture/computer-mcp"))
