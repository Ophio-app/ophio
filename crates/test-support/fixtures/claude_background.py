#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""A non-inference Claude stream fixture with independently lived native tasks."""
import json
import select
import sys
from pathlib import Path


def send(value):
    print(json.dumps(value), flush=True)


if sys.argv[1:3] == ["auth", "status"]:
    send({"loggedIn": True, "authMethod": "claude.ai"})
    sys.exit(0)

live = []
ignore = False


def tasks():
    send({"type": "system", "subtype": "background_tasks_changed", "tasks": live})


for line in sys.stdin:
    value = json.loads(line)
    if value["type"] == "user":
        mode = value["message"]["content"][0]["text"]
        ignore = mode == "ignore"
        live = [{"task_id": "job-1", "task_type": "bash", "description": "Build assets"},
                {"task_id": "job-2", "task_type": "agent", "description": "Check styles"}]
        if mode == "resumed":
            live = []
        send({"type": "system", "subtype": "init", "session_id": "fixture-background"})
        tasks()
        if mode in ("complete", "ignore"):
            send({"type": "result", "subtype": "success", "is_error": False})
        elif mode == "exit":
            sys.exit(0)
        elif mode == "resumed":
            # Claude Code 2.1.284 on --resume: a watcher the last process lost is
            # reported in a turn of its own, then the queued message runs unless
            # stdin has closed meanwhile.
            send({"type": "system", "subtype": "task_notification", "task_id": "old-watch",
                  "status": "stopped", "summary": "Background shell command didn't finish"})
            send({"type": "result", "subtype": "success", "is_error": False, "num_turns": 0,
                  "result": "", "duration_ms": 17, "origin": {"kind": "task-notification"}})
            if select.select([sys.stdin], [], [], 0.5)[0] and not sys.stdin.readline():
                break
            send({"type": "assistant", "message": {"id": "reply", "content": [{"type": "text", "text": "Carrying on"}]}})
            send({"type": "result", "subtype": "success", "is_error": False, "num_turns": 1,
                  "result": "Carrying on", "duration_ms": 900, "usage": {"input_tokens": 12, "output_tokens": 4}})
        elif mode == "natural":
            live = []
            tasks()
            send({"type": "result", "subtype": "success", "is_error": False})
    elif value["type"] == "control_request":
        request = value["request"]
        with open("controls.jsonl", "a") as log:
            log.write(json.dumps(request) + "\n")
        send({"type": "control_response", "response": {"subtype": "success", "request_id": value["request_id"], "response": {}}})
        if request["subtype"] == "interrupt":
            send({"type": "result", "subtype": "success", "is_error": False})
        elif request["subtype"] == "stop_task":
            if not (ignore and request["task_id"] == "job-2"):
                live = [task for task in live if task["task_id"] != request["task_id"]]
            tasks()
Path("closed").touch()
