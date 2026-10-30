#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Authored v2 schema fixture. No native tool, account, or inference access."""
import json
import os
import socket
import subprocess
import sys
from pathlib import Path


def gate(name):
    """Test-owned Unix socket handshake; no polling, timers, or live services."""
    if Path(name).exists():
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(name)
            connection.sendall(b"ready")
            connection.recv(1)

def after_turn():
    with open("turns.jsonl", "a") as log:
        log.write("completed\n")
    if Path("exit-after-turn").exists():
        sys.exit(int(Path("exit-code").read_text()) if Path("exit-code").exists() else 0)

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

def notification(method, params):
    send({"method": method, "params": {"threadId": "thread-fixture", **params}})

def complete(status):
    gate("turn.sock")
    last = json.loads(Path("token-usage.json").read_text()) if Path("token-usage.json").exists() else {"inputTokens": 12, "outputTokens": 4}
    notification("thread/tokenUsage/updated", {"turnId": turn_id, "tokenUsage": {"last": last, "total": {"inputTokens": 20, "outputTokens": 9}}})
    turn = json.loads(Path("result.json").read_text()) if Path("result.json").exists() else {"status": status}
    notification("turn/completed", {"turn": {"id": turn_id, **turn}})
    after_turn()

if sys.argv[1:3] == ["app-server", "generate-json-schema"]:
    # Authored subset of the inspected v2 schema; never reads a real tool/config.
    directory = Path(sys.argv[sys.argv.index("--out") + 1]) / "v2"
    directory.mkdir(parents=True)
    for name in ["ThreadStartParams", "ThreadResumeParams", "TurnStartParams"]:
        turn = name == "TurnStartParams"
        sandbox = "SandboxPolicy" if turn else "SandboxMode"
        properties = {key: {"anyOf": [{"$ref": "#/definitions/" + definition}, {"type": "null"}]} for key, definition in [("sandboxPolicy" if turn else "sandbox", sandbox), ("approvalPolicy", "AskForApproval"), ("approvalsReviewer", "ApprovalsReviewer")]}
        if not turn and not Path(__file__).with_name("no-instructions").exists():
            properties["developerInstructions"] = {"type": ["string", "null"]}
        definitions = {
            "SandboxMode": {"type": "string", "enum": ["workspace-write", "danger-full-access"] + ([] if Path(__file__).with_name("no-read-only").exists() else ["read-only"])},
            "SandboxPolicy": {"oneOf": [{"properties": {"type": {"enum": ["readOnly"]}, "networkAccess": {"type": "boolean", "default": False}}}]},
            "AskForApproval": {"oneOf": [{"enum": ["on-request", "never", "untrusted"]}]},
            "ApprovalsReviewer": {"enum": ["user", "auto_review"]},
        }
        if turn and Path(__file__).with_name("skill-input").exists():
            definitions["UserInput"] = {"oneOf": [
                {"properties": {"type": {"enum": ["text"]}, "text": {"type": "string"}}},
                {"properties": {"type": {"enum": ["skill"]}, "name": {"type": "string"}, "path": {"type": "string"}}},
            ]}
        (directory / (name + ".json")).write_text(json.dumps({"properties": properties, "definitions": definitions}))
    sys.exit(0)
if "--version" in sys.argv:
    print(Path(__file__).with_name("version").read_text() if Path(__file__).with_name("version").exists() else "codex-cli 0.153.2 fixture")
    sys.exit(0)
if sys.argv[1:3] == ["features", "list"]:
    print("shell_tool                       stable             true")
    if not Path(__file__).with_name("no-questions").exists():
        print("default_mode_request_user_input  under development  false")
    sys.exit(0)
if sys.argv[1:4] == ["mcp", "list", "--json"]:
    # The person's own servers, from the run's directory.
    if Path("mcp-list-fails").exists():
        sys.exit(1)
    print(Path("mcp-servers.json").read_text() if Path("mcp-servers.json").exists() else "[]")
    sys.exit(0)
if sys.argv[1:3] == ["login", "status"]:
    print("Logged in using ChatGPT", file=sys.stderr)
    sys.exit(0)
if os.environ.get("OPHIO_RELAY_DUMMY_KEY") == "ophio-relay-not-a-key":
    assert "PRIVATE_ACCOUNT_TOKEN" not in os.environ
    assert "ANTHROPIC_AUTH_TOKEN" not in os.environ
    assert "OPENAI_API_KEY" not in os.environ
    Path("relay-checked").touch()
Path("argv.json").write_text(json.dumps(sys.argv[1:]))
turn_number = 0
turn_id = None
computer_turn = False
for line in sys.stdin:
    with open("stdin.jsonl", "a") as log:
        log.write(line)
    value = json.loads(line)
    method = value.get("method")
    params = value.get("params", {})
    if method == "experimentalFeature/list":
        send({"id": value["id"], "result": {"data": [{"name": "shell_tool", "enabled": not Path(__file__).with_name("no-shell").exists(), "stage": "stable", "defaultEnabled": True}], "nextCursor": None}})
    elif method == "initialize":
        if Path(__file__).with_name("sandbox-warning").exists():
            send({"method": "configWarning", "params": {"summary": Path(__file__).with_name("sandbox-warning").read_text()}})
        send({"id": value["id"], "result": {"userAgent": "fixture"}})
    elif method == "model/list":
        send({"id": value["id"], "result": {"data": [
            {"id": "picker-a", "model": "gpt-6-astra", "displayName": "GPT-6-Astra", "hidden": False, "isDefault": True, "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [{"reasoningEffort": e, "description": e} for e in ["low", "medium", "high", "xhigh"]]},
            {"id": "picker-b", "model": "gpt-6-sol", "displayName": "GPT-6-Sol", "hidden": False, "isDefault": False, "defaultReasoningEffort": "low", "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "Low"}]}], "nextCursor": None}})
    elif method == "config/read":
        send({"id": value["id"], "result": {"config": {"model": "gpt-6-astra", "model_reasoning_effort": "high"}}})
    elif method == "account/read":
        send({"id": value["id"], "result": {"account": {"type": "apiKey" if Path("api-auth").exists() else "chatgpt", "email": None, "planType": "fixture"}}})
    elif method == "thread/resume" and Path("resume-missing").exists():
        send({"id": value["id"], "error": {"code": -32600, "message": "no rollout found for thread id resume-fixture"}})
    elif method in ["thread/start", "thread/resume"]:
        send({"id": value["id"], "result": {"thread": {"id": "thread-fixture"}, "model": "actual-native-model", "modelProvider": params.get("modelProvider", "openai"), "reasoningEffort": "medium", "sandbox": {"type": "dangerFullAccess"} if Path("full-access-response").exists() or (params.get("sandbox") == "danger-full-access" and not Path("unsafe-response").exists()) else {"type": "readOnly" if Path("read-only-work-response").exists() or (params.get("sandbox") == "read-only" and not Path("unsafe-response").exists()) else "workspaceWrite", "networkAccess": False}, "approvalPolicy": params.get("approvalPolicy"), "approvalsReviewer": params.get("approvalsReviewer")}})
        if Path("after-thread.jsonl").exists():
            # Recorded notifications that follow a thread's start.
            for line in Path("after-thread.jsonl").read_text().splitlines():
                print(line, flush=True)
    elif method == "turn/start":
        turn_number += 1
        turn_id = "turn-" + str(turn_number)
        if params["input"][0]["text"].startswith("detach "):
            # Leave children behind the way backgrounded commands do.
            subprocess.run(["sh", "detach.sh"], check=True)
            params["input"][0]["text"] = params["input"][0]["text"].removeprefix("detach ")
        if params["input"][0]["text"] == "crash":
            sys.exit(9)
        if params["input"][0]["text"] == "outlive":
            # A helper in the server's process group keeps its output open after it exits.
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
            Path("fixture.pid").write_text(str(os.getpid()))
            helper = subprocess.Popen(["sleep", "30"])
            Path("helper.pid").write_text(str(helper.pid))
            os._exit(0)
        if params["input"][0]["text"] == "usage-limit":
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
            notification("turn/started", {"turn": {"id": turn_id, "status": "inProgress"}})
            send({"method": "account/rateLimits/updated", "params": {"rateLimits": {"primary": {"usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1790000000}, "secondary": {"usedPercent": 62, "windowDurationMins": 10080, "resetsAt": 1790400000}}}})
            error = {"message": "You've hit your usage limit.", "codexErrorInfo": "usageLimitExceeded"}
            notification("error", {"turnId": turn_id, "error": error, "willRetry": False})
            notification("turn/completed", {"turn": {"id": turn_id, "status": "failed", "error": error}})
            after_turn()
            continue
        if params["input"][0]["text"] == "turn-error":
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
            notification("turn/started", {"turn": {"id": turn_id, "status": "inProgress"}})
            error = {"message": "stream disconnected before completion"}
            notification("error", {"turnId": turn_id, "error": error, "willRetry": True})
            notification("error", {"turnId": turn_id, "error": {"message": " "}, "willRetry": False})
            notification("turn/completed", {"turn": {"id": turn_id, "status": "failed", "error": error}})
            after_turn()
            continue
        if params["input"][0]["text"] == "computer":
            # A computer-use turn: a bridge tool, a message, and the requests
            # Codex may still make, before it completes.
            computer_turn = True
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
            notification("turn/started", {"turn": {"id": turn_id, "status": "inProgress"}})
            call = {"id": "look", "type": "mcpToolCall", "server": "computer", "tool": "observe_target", "status": "inProgress", "arguments": {}}
            notification("item/started", {"turnId": turn_id, "item": call})
            notification("item/completed", {"turnId": turn_id, "item": {**call, "status": "completed"}})
            notification("item/completed", {"turnId": turn_id, "item": {"id": "msg", "type": "agentMessage", "text": "It shows the settings page."}})
            send({"id": 71, "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread-fixture", "turnId": turn_id, "itemId": "cmd", "command": "pwd"}})
            send({"id": 72, "method": "item/tool/requestUserInput", "params": {"threadId": "thread-fixture", "turnId": turn_id, "itemId": "ask", "questions": [{"id": "choice", "question": "Which one?"}]}})
            continue
        late = params["input"][0]["text"] == "late-ack"
        if not late:
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
        notification("turn/started", {"turn": {"id": turn_id, "status": "inProgress"}})
        notification("item/agentMessage/delta", {"turnId": turn_id, "itemId": "msg", "delta": "hello"})
        notification("item/started", {"turnId": turn_id, "item": {"id": "cmd", "type": "commandExecution", "command": "pwd", "status": "inProgress"}})
        notification("item/started", {"turnId": turn_id, "item": {"id": "file", "type": "fileChange", "status": "inProgress", "changes": [{"path": "/etc/profile.d/agent.sh", "kind": {"type": "update", "move_path": None}, "diff": "+alias ls=true"}]}})
        notification("item/completed", {"turnId": turn_id, "item": {"id": "msg", "type": "agentMessage", "text": "hello"}})
        notification("item/commandExecution/outputDelta", {"turnId": turn_id, "itemId": "cmd", "delta": "/work\n"})
        notification("future/method", {})
        notification("future/method", {})
        notification("remoteControl/status/changed", {})
        send({"method": "configWarning", "params": {"summary": "unknown key `fixture`"}})
        if late:
            complete("completed")
            send({"id": value["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
        elif params["input"][0]["text"] in ["later", "complete"]:
            complete("completed")
        elif params["input"][0]["text"] == "questions":
            send({"id": 73, "method": "mcpServer/elicitation/request", "params": {"threadId": "thread-fixture", "turnId": turn_id, "serverName": "fixture", "mode": "form", "message": "Pick one"}})
            send({"id": 72, "method": "item/tool/requestUserInput", "params": {"threadId": "thread-fixture", "turnId": turn_id, "itemId": "ask", "questions": [{"id": "choice", "question": "Which one?"}]}})
        else:
            send({"id": 71, "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread-fixture", "turnId": turn_id, "itemId": "cmd", "command": "pwd"}})
            send({"id": "71", "method": "item/fileChange/requestApproval", "params": {"threadId": "thread-fixture", "turnId": turn_id, "itemId": "file"}})
        if params["input"][0]["text"] == "resolved-elsewhere":
            notification("serverRequest/resolved", {"requestId": 71})
    elif method == "turn/steer":
        assert params["expectedTurnId"] == turn_id
        send({"id": value["id"], "result": {"turnId": turn_id}})
        complete("completed")
    elif method == "turn/interrupt":
        if Path("ignore-completion").exists():
            send({"id": value["id"], "result": {}})
            gate("interrupt.sock")
            continue
        if Path("ignore-interrupt").exists():
            gate("interrupt.sock")
            continue
        send({"id": value["id"], "result": {}})
        complete("interrupted")
    elif method is None and "id" in value:
        notification("serverRequest/resolved", {"requestId": value["id"]})
        if value["id"] == 72 and computer_turn:
            complete("completed")

Path("stdin-closed").touch()
gate("boundary.sock")
sys.exit(int(Path("exit-code").read_text()) if Path("exit-code").exists() else 0)
