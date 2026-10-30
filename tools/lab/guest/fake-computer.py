#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Authored child delegation exercising the real, run-bound MCP bridge."""

import base64
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import threading
import time

TOOLS = (
    "observe_target",
    "pointer_action",
    "key_action",
    "type_text",
    "scroll",
    "request_human",
)


def specialist_server(arguments, executable):
    """Validate the actual adapter configuration without logging its contents."""
    agents = json.loads(arguments[arguments.index("--agents") + 1])
    parent, child = agents["computer-supervisor"], agents["computer-specialist"]
    if parent["tools"] != ["Agent(computer-specialist)"] or "mcpServers" in parent:
        raise ValueError("Parent tool isolation missing")
    if child["tools"] != ["mcp__computer__" + name for name in TOOLS]:
        raise ValueError("Unexpected specialist tool set")
    config = json.loads(arguments[arguments.index("--mcp-config") + 1])
    if config != {"mcpServers": {}}:
        raise ValueError("Parent bridge configuration must be empty")
    server = child["mcpServers"][0]["computer"]
    if Path(server["command"]).resolve() != executable.resolve():
        raise ValueError("Unexpected bridge executable")
    return server


def main():
    if (
        Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
        != "disposable-desktop-lab"
        or os.geteuid() == 0
    ):
        raise SystemExit("Requires an ordinary disposable guest user")
    if (
        "--version" in sys.argv
        or "--help" in sys.argv
        or sys.argv[1:3] == ["auth", "status"]
    ):
        runpy.run_path(str(Path.home() / "bin/claude-probe"), run_name="__main__")
        return
    server = specialist_server(sys.argv, Path.home() / "bin/computer-mcp")
    output_lock = threading.Lock()
    record_lock = threading.Lock()
    stopped = threading.Event()
    evidence = Path.home() / "evidence"

    def send(value):
        with output_lock:
            print(json.dumps(value), flush=True)

    def record(value):
        value.update(monotonic_ns=time.monotonic_ns(), realtime_ns=time.time_ns())
        with (
            record_lock,
            (evidence / "computer-actions.jsonl").open("a", encoding="utf-8") as stream,
        ):
            stream.write(json.dumps(value) + "\n")

    def commands():
        for line in sys.stdin:
            value = json.loads(line)
            if value.get("type") == "control_request":
                stopped.set()
                record({"event": "native_interrupt"})
                send(
                    {
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": value["request_id"],
                            "response": {},
                        },
                    }
                )

    initial = json.loads(sys.stdin.readline())
    private = any(
        "Request the private fixture step." in part.get("text", "")
        for part in initial["message"]["content"]
    )
    record({"event": "fixture_started", "private_step": private})
    send(
        {
            "type": "system",
            "subtype": "init",
            "session_id": "desktop-fixture",
            "model": "scripted-fixture",
        }
    )
    send(
        {
            "type": "assistant",
            "message": {
                "id": "parent",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "desktop-child",
                        "name": "Agent",
                        "input": {
                            "description": "Desktop fixture",
                            "subagent_type": "computer-specialist",
                        },
                    }
                ],
            },
        }
    )
    send(
        {
            "type": "system",
            "subtype": "init",
            "session_id": "desktop-child-session",
            "parent_tool_use_id": "desktop-child",
        }
    )
    threading.Thread(target=commands, daemon=True).start()
    # Bridge credentials remain inherited and are never inspected or recorded.
    with subprocess.Popen(
        [server["command"], *server["args"]],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    ) as bridge:
        sequence = 0

        def rpc(method, params):
            nonlocal sequence
            sequence += 1
            bridge.stdin.write(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": sequence,
                        "method": method,
                        "params": params,
                    }
                )
                + "\n"
            )
            bridge.stdin.flush()
            line = bridge.stdout.readline()
            if not line:
                raise RuntimeError("MCP bridge closed")
            return json.loads(line)

        def tool(name, arguments):
            record(
                {
                    "event": "tool_started",
                    "name": name,
                    "after_interrupt": stopped.is_set(),
                }
            )
            result = rpc("tools/call", {"name": name, "arguments": arguments})
            failed = "error" in result or result.get("result", {}).get("isError", False)
            record(
                {
                    "event": "tool",
                    "name": name,
                    "after_interrupt": stopped.is_set(),
                    "failed": failed,
                }
            )
            return result.get("result", {}), failed

        rpc(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "desktop-fixture", "version": "2"},
            },
        )
        bridge.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        bridge.stdin.flush()
        if [t["name"] for t in rpc("tools/list", {})["result"]["tools"]] != list(TOOLS):
            raise RuntimeError("Unexpected bridge tool catalog")
        observation, failed = tool("observe_target", {"max_dim": 1280})
        if failed:
            raise RuntimeError("Observation refused")
        text = next(i["text"] for i in observation["content"] if i["type"] == "text")
        size = re.search(
            r"Image size: (\d+) x (\d+); observation_revision: (\d+)", text
        )
        if not size:
            raise RuntimeError("Observation geometry missing")
        image = next(i for i in observation["content"] if i["type"] == "image")
        extension = ".png" if image["mimeType"] == "image/png" else ".jpg"
        (evidence / ("computer-observation" + extension)).write_bytes(
            base64.b64decode(image["data"])
        )
        width, height, revision = map(int, size.groups())
        for action in (
            {"kind": "move"},
            {"kind": "click", "button": "left", "count": 1},
        ):
            _, failed = tool(
                "pointer_action",
                {
                    "action": action,
                    "x": width / 2,
                    "y": height / 2,
                    "observation_revision": revision,
                },
            )
            if failed:
                raise RuntimeError("Initial pointer action refused")
        send(
            {
                "type": "assistant",
                "parent_tool_use_id": "desktop-child",
                "message": {
                    "id": "desktop-started",
                    "content": [
                        {"type": "text", "text": "Scripted desktop actions started."}
                    ],
                },
            }
        )
        if private:
            tool("request_human", {"reason": "Take the private fixture step."})
            stopped.wait(90)
        else:
            deadline = time.monotonic() + 90
            while not stopped.is_set() and time.monotonic() < deadline:
                _, failed = tool("type_text", {"text": "x"})
                if failed:
                    stopped.wait(5)
                    break
                stopped.wait(0.25)
            if stopped.is_set():
                for _ in range(5):
                    tool("type_text", {"text": "AFTER_STOP"})
        bridge.stdin.close()
        bridge.wait(timeout=5)
    send(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "parent_tool_use_id": "desktop-child",
        }
    )
    send(
        {
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "desktop-child",
                        "content": "Specialist stopped",
                    }
                ]
            },
        }
    )
    send(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }
    )


if __name__ == "__main__":
    main()
