#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Authored protocol fixture; never imports or invokes a native harness."""
import json
import socket
import sys
import subprocess
import os
import time
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

if "--help" in sys.argv:
    with open(Path(__file__).with_name("help-calls"), "a") as calls:
        calls.write("--help\n")
    print("--agents --agent --forward-subagent-text --restricted --setting-sources --settings --allowedTools --disable-slash-commands --no-chrome")
    print("  --model <model> alias 'opus', 'fable', 'sonnet' or full name 'claude-fable-5'")
    print("  --effort <level> (low, medium, high, xhigh, max)")
    plan = "" if Path(__file__).with_name("no-plan").exists() else ', "plan"'
    print('  --permission-mode <mode> (choices: "manual", "acceptEdits", "bypassPermissions"' + plan + ')')
    if not Path(__file__).with_name("no-bypass").exists():
        print('  --dangerously-skip-permissions')
    print('  --tools <tools...>')
    print('  --safe-mode')
    print('  --append-system-prompt <prompt>')
    print('  --system-prompt-snapshot <on|off>')
    print('  --disallowedTools <tools...>')
    print('  --strict-mcp-config')
    print('  --mcp-config <configs...>')
    print('  --permission-prompts <target> (choices: "host", "none")')
    sys.exit(0)
if "--version" in sys.argv:
    print("2.1.280 (Claude Code fixture)")
    sys.exit(0)
if sys.argv[1:3] == ["auth", "status"]:
    send({"loggedIn": True, "authMethod": "api_key" if Path(__file__).with_name("api-auth").exists() else "claude.ai", "subscriptionType": "fixture"})
    sys.exit(0)
if "--settings" in sys.argv:
    settings_arg = sys.argv[sys.argv.index("--settings") + 1]
    if Path(settings_arg).is_file():
        Path("settings-content.json").write_text(Path(settings_arg).read_text())

if "--no-session-persistence" in sys.argv:
    # Ophio asking for the model list: answer the initialize request alone.
    with open(Path(__file__).with_name("list-calls"), "a") as calls:
        calls.write(json.dumps(sys.argv[1:]) + "\n")
    if Path(__file__).with_name("no-model-list").exists():
        sys.exit(1)
    request = json.loads(sys.stdin.readline())
    assert request["request"] == {"subtype": "initialize"}
    effort = ["low", "medium", "high", "xhigh", "max"]
    models = [
        {"value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default (recommended)", "supportsEffort": True, "supportedEffortLevels": effort},
        {"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus", "supportsEffort": True, "supportedEffortLevels": effort},
        {"value": "claude-fable-5-1[1m]", "resolvedModel": "claude-fable-5-1[1m]", "displayName": "Fable", "supportsEffort": True, "supportedEffortLevels": effort},
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "displayName": "Sonnet", "supportsEffort": True, "supportedEffortLevels": effort},
        {"value": "haiku", "resolvedModel": "claude-haiku-5-5", "displayName": "Haiku", "supportsEffort": False},
    ]
    model_path = Path(__file__).with_name("models.json")
    if model_path.exists():
        models = json.loads(model_path.read_text())
    send({"type": "system", "subtype": "hook_started"})
    send({"type": "control_response", "response": {"subtype": "success", "request_id": request["request_id"], "response": {"models": models, "commands": []}}})
    sys.stdin.read()
    sys.exit(0)
if os.environ.get("ANTHROPIC_API_KEY") == "ophio-relay-not-a-key":
    assert os.environ["ANTHROPIC_BASE_URL"] == "http://127.0.0.1:18080"
    for name in ["CLAUDE_CODE_SIMPLE", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "DISABLE_TELEMETRY", "DISABLE_ERROR_REPORTING", "DISABLE_AUTOUPDATER"]:
        assert os.environ[name] == "1"
    assert "PRIVATE_ACCOUNT_TOKEN" not in os.environ
    assert "ANTHROPIC_AUTH_TOKEN" not in os.environ
    Path("relay-checked").touch()
Path("argv.json").write_text(json.dumps(sys.argv[1:]))
Path("environ.json").write_text(json.dumps(sorted(os.environ)))
Path("no-model-fallback.json").write_text(json.dumps(os.environ.get("CLAUDE_CODE_NO_MODEL_FALLBACK")))
if "--mcp-config" in sys.argv:
    arg = sys.argv[sys.argv.index("--mcp-config") + 1]
    config = arg if arg.startswith("{") else Path(arg).read_text()
    Path("mcp.json").write_text(config)
initial = True

def complete():
    gate("turn.sock")
    result = json.loads(Path("result.json").read_text()) if Path("result.json").exists() else {"type": "result", "subtype": "success", "is_error": False, "usage": {"input_tokens": 12, "output_tokens": 4}}
    send(result)
    after_turn()

for line in sys.stdin:
    with open("stdin.jsonl", "a") as log:
        log.write(line)
    value = json.loads(line)
    if value["type"] == "control_response":
        Path("plan-permission.json").write_text(json.dumps(value))
        send({"type": "result", "subtype": "success", "is_error": False})
        continue
    if value["type"] == "control_request":
        if value["request"]["subtype"] == "initialize":
            gate("model-check.sock")
            path = Path(__file__).with_name("models.json")
            models = json.loads(path.read_text()) if path.exists() else None
            send({"type": "control_response", "response": {"subtype": "success" if models is not None else "error", "request_id": value["request_id"], "response": {"models": models}}})
            continue
        if value["request"]["subtype"] == "get_settings":
            path = Path(__file__).with_name("applied-model.json")
            applied = json.loads(path.read_text()) if path.exists() else None
            send({"type": "control_response", "response": {"subtype": "success", "request_id": value["request_id"], "response": {"applied": {"model": applied}, "effective": {"switchModelsOnFlag": Path("force-fallback").exists()}}}})
            continue
        if Path("ignore-completion").exists():
            send({"type": "control_response", "response": {"subtype": "success", "request_id": value["request_id"], "response": {}}})
            gate("interrupt.sock")
            continue
        if Path("ignore-interrupt").exists():
            gate("interrupt.sock")
            continue
        if Path("late-interrupt-ack").exists():
            complete()
            send({"type": "control_response", "response": {"subtype": "success", "request_id": value["request_id"], "response": {}}})
        else:
            send({"type": "control_response", "response": {"subtype": "success", "request_id": value["request_id"], "response": {}}})
            complete()
        continue
    if initial:
        initial = False
        if value["message"]["content"][0]["text"].startswith("detach "):
            # Leave children behind the way backgrounded commands do.
            subprocess.run(["sh", "detach.sh"], check=True)
            value["message"]["content"][0]["text"] = value["message"]["content"][0]["text"].removeprefix("detach ")
        if value["message"]["content"][0]["text"] == "crash":
            sys.exit(7)
        model_path = Path("reported-model.json") if Path("reported-model.json").exists() else Path("applied-model.json")
        model = json.loads(model_path.read_text()) if model_path.exists() else "native-model"
        send({"type": "system", "subtype": "init", "session_id": "native-session", "model": model})
        Path("fixture.pid").write_text(str(os.getpid()))
        if value["message"]["content"][0]["text"].startswith("later "):
            # Go on only after a pause, as when the phone's app is closed meanwhile.
            _, seconds, rest = value["message"]["content"][0]["text"].split(" ", 2)
            time.sleep(int(seconds))
            value["message"]["content"][0]["text"] = rest
        if value["message"]["content"][0]["text"] == "outlive":
            # A helper in the agent's process group keeps its output open after it exits.
            helper = subprocess.Popen(["sleep", "30"])
            Path("helper.pid").write_text(str(helper.pid))
            os._exit(0)
        if value["message"]["content"][0]["text"] == "consent-declined":
            if os.environ.get("CLAUDE_CODE_NO_MODEL_FALLBACK") == "1":
                send({"type": "result", "subtype": "error_during_execution", "is_error": True,
                      "errors": ["CLAUDE_CODE_NO_MODEL_FALLBACK forbids model substitution"]})
            else:
                send({"type": "system", "subtype": "model_consent_fallback",
                      "choice": "cancelled", "original_model": model, "fallback_model": "claude-other"})
                Path("substituted-model-executed").touch()
                complete()
            continue
        if Path("after-init.json").exists():
            for frame in json.loads(Path("after-init.json").read_text()):
                send(frame)
        if value["message"]["content"][0]["text"] in ["complete", "wait"]:
            send({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "fixture-turn"}}})
            if value["message"]["content"][0]["text"] == "complete":
                complete()
            continue
        if value["message"]["content"][0]["text"] == "plan-reply":
            send({"type": "assistant", "message": {"id": "plan-message", "content": [{"type": "tool_use", "id": "plan-tool", "name": "ExitPlanMode", "input": {"plan": "1. Inspect the parser.\n2. Add regression coverage.\n3. Implement and verify in Work."}}]}})
            send({"type": "control_request", "request_id": "plan-approval", "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": {"plan": "fixture"}}})
            continue
        if value["message"]["content"][0]["text"] == "delegate-computer":
            agents = json.loads(sys.argv[sys.argv.index("--agents") + 1])
            parent = agents["computer-supervisor"]
            specialist = agents["computer-specialist"]
            assert parent["tools"] == ["Agent(computer-specialist)"]
            assert "mcpServers" not in parent
            assert json.loads(Path("mcp.json").read_text())["mcpServers"] == {}
            expected = ["observe_target", "pointer_action", "key_action", "type_text", "scroll", "request_human"]
            assert specialist["tools"] == ["mcp__computer__" + name for name in expected]
            assert "mcp__computer__observe_target" not in parent["tools"]
            send({"type": "assistant", "message": {"id": "parent-message", "content": [{"type": "tool_use", "id": "delegate-1", "name": "Agent", "input": {"description": "Computer specialist", "subagent_type": "computer-specialist"}}]}})
            server = specialist["mcpServers"][0]["computer"]
            with subprocess.Popen([server["command"], *server["args"]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as bridge:
                def rpc(method, params):
                    bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n")
                    bridge.stdin.flush()
                    return json.loads(bridge.stdout.readline())
                rpc("initialize", {"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"child-fixture","version":"1"}})
                bridge.stdin.write(json.dumps({"jsonrpc":"2.0", "method":"notifications/initialized"}) + "\n")
                bridge.stdin.flush()
                assert [tool["name"] for tool in rpc("tools/list", {})["result"]["tools"]] == expected
                send({"type":"system", "subtype":"init", "session_id":"child-session", "parent_tool_use_id":"delegate-1"})
                arguments = [{}, {"action":{"kind":"move"},"x":5,"y":5,"observation_revision":4}, {"keys":["ESC"]}, {"text":"fixture"}, {"x":5,"y":5,"dx":0,"dy":1,"observation_revision":4}, {"reason":"Review fixture"}]
                for name, args in zip(expected, arguments):
                    assert not rpc("tools/call", {"name":name,"arguments":args})["result"]["isError"]
                    send({"type":"assistant", "parent_tool_use_id":"delegate-1", "message":{"id":"child-"+name,"content":[{"type":"text","text":"Used "+name}]}})
                gate("child.sock")
                assert rpc("tools/call", {"name":"type_text","arguments":{"text":"late fixture"}})["result"]["isError"]
                bridge.stdin.close()
                bridge.wait(timeout=5)
            send({"type":"result","subtype":"success","is_error":False,"parent_tool_use_id":"delegate-1"})
            send({"type":"user", "message":{"content":[{"type":"tool_result","tool_use_id":"delegate-1","content":"Specialist stopped"}]}})
            send({"type":"assistant", "message":{"id":"parent-final","content":[{"type":"text","text":"Late call was fenced"}]}})
            continue
        if value["message"]["content"][0]["text"] == "bridge-permission":
            config = json.loads(Path(sys.argv[sys.argv.index("--mcp-config") + 1]).read_text())
            server = config["mcpServers"]["computer"]
            assert sys.argv[sys.argv.index("--permission-prompt-tool") + 1] == "mcp__computer__permission_prompt"
            # Claude Code adds a server's configured environment to its own.
            with subprocess.Popen([server["command"], *server["args"]], env={**os.environ, **server.get("env", {})}, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as bridge:
                def rpc(request):
                    bridge.stdin.write(json.dumps(request) + "\n")
                    bridge.stdin.flush()
                    return json.loads(bridge.stdout.readline())
                rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}})
                bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
                bridge.stdin.flush()
                # A test may ask for another tool, such as a question to answer.
                asked = json.loads(Path("permission-request.json").read_text()) if Path("permission-request.json").exists() else {"tool_name": "Bash", "input": {"command": "printf fixture"}}
                reply = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "permission_prompt", "arguments": {**asked, "tool_use_id": "fixture-permission"}}})
                decision = json.loads(reply["result"]["content"][0]["text"])
                Path("permission-result.json").write_text(json.dumps(decision))
                bridge.stdin.close()
                bridge.wait(timeout=5)
            send({"type": "assistant", "message": {"id": "permission-done", "content": [{"type": "text", "text": "Permission " + decision["behavior"]}]}})
            send({"type": "result", "subtype": "success", "is_error": False, "usage": {"input_tokens": 1, "output_tokens": 1}})
            after_turn()
            continue

        send({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m1"}}})
        send({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Hello"}}})
        send({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": "Hello"}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "pwd"}}, {"type": "tool_use", "id": "c1", "name": "Task", "input": {"description": "Fixture child"}}]}})
        send({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "fixture"}]}})
        send({"type": "future_native_type", "extra": 1})
    else:
        send(value)
        complete()

Path("stdin-closed").touch()
gate("boundary.sock")
sys.exit(int(Path("exit-code").read_text()) if Path("exit-code").exists() else 0)
