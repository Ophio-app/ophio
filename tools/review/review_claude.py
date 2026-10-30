#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Scripted Claude Code for reviewing the connected app; see README.md.

The companion launches this as `claude`. It speaks Claude Code's stream-json
print protocol, asks for permission through the companion's MCP permission
tool, and really reads, edits and tests the project it is started in. It never
contacts a model and never sends a network request.
"""

import difflib
import json
import os
import queue
import re
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from types import SimpleNamespace

VERSION = "2.1.280 (Claude Code review agent)"
HELP = """Usage: claude [options] [command]

Scripted Claude Code for reviewing the companion app. It never contacts a model.

Options:
  -p, --print                        Print the response and exit
  --output-format <format>           Output format (choices: "stream-json")
  --input-format <format>            Input format (choices: "stream-json")
  --verbose                          Report every step
  --include-partial-messages         Stream text as it is written
  --replay-user-messages             Echo each user message back
  --model <model>                    Model for the session: an alias such as 'opus', 'fable' or 'sonnet', or a full name such as 'claude-opus-5-5'
  --effort <level>                   Effort level (low, medium, high, xhigh, max)
  --permission-mode <mode>           Permission mode (choices: "manual", "plan")
  --permission-prompt-tool <tool>    MCP tool that answers permission prompts
  --permission-prompts <target>      Where permission prompts go (choices: "host", "none")
  --mcp-config <configs...>          MCP server configuration
  --strict-mcp-config                Use only the servers from --mcp-config
  --tools <tools...>                 Built-in tools to offer
  --disallowedTools <tools...>       Tools to refuse
  --safe-mode                        Refuse settings that widen permissions
  --system-prompt-snapshot <on|off>  Snapshot the system prompt
  --append-system-prompt <prompt>    Extra system prompt text
  -r, --resume <session>             Continue a session
  -v, --version                      Print the version

Commands:
  auth status --json                 Report how this session is signed in"""
PACE = float(os.environ.get("REVIEW_AGENT_PACE", "1"))
TEST = "python3 -m unittest"
# Tools Claude Code asks permission for in its manual mode.
GATED = {"Bash", "Edit", "Write", "WebFetch", "AskUserQuestion"}
DOCS = "https://docs.python.org/3/library/decimal.html"
CARRIED = re.compile(r"--- Plan from run (\d+), proposed by the agent ---\n.*?\n--- End of plan ---", re.S)
# The demo project's known bug and the improvements the agent makes, in order.
BUG = ("(1 + Decimal(str(tip_percent)))", "(1 + Decimal(str(tip_percent)) / 100)")
TEST_ANCHOR = '\n\n\nif __name__ == "__main__":'
GUARD_ANCHOR = "    total = "
IMPROVEMENTS = [
    {
        "title": "refuse to split a bill between zero people",
        "test": "test_zero_people_is_refused",
        "code": "    def test_zero_people_is_refused(self):\n"
        '        with self.assertRaisesRegex(ValueError, "at least one person"):\n'
        "            split_bill(50, 10, 0)",
        "guard": "    if people < 1:\n"
        '        raise ValueError("Split the bill between at least one person")\n',
    },
    {
        "title": "refuse a negative tip",
        "test": "test_negative_tip_is_refused",
        "code": "    def test_negative_tip_is_refused(self):\n"
        '        with self.assertRaisesRegex(ValueError, "negative"):\n'
        "            split_bill(50, -5, 2)",
        "guard": "    if tip_percent < 0:\n"
        "        raise ValueError(\"A tip can't be negative\")\n",
    },
]

OUT = threading.Lock()
INBOX = queue.Queue()  # user messages in arrival order; None once input closes
REPLIES = queue.Queue()  # control responses to this agent's own requests
STOP = threading.Event()


class Stopped(Exception):
    """The companion interrupted the turn."""


class Denied(Exception):
    """The person declined a permission request; the message is their reason."""


class Failed(Exception):
    """The turn ends as a failure."""


def option(name):
    args = sys.argv[1:]
    return args[args.index(name) + 1] if name in args[:-1] else None


def send(value):
    with OUT:
        sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def acknowledge(request_id):
    send({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": {}}})


class Interrupts:
    """An interrupt stops the running turn and is acknowledged once that turn has ended."""

    def __init__(self):
        self.lock = threading.Lock()
        self.busy = False
        self.pending = []

    def arrive(self, message):
        with self.lock:
            self.busy = True
            INBOX.put(message)

    def request(self, request_id):
        with self.lock:
            if not self.busy:
                acknowledge(request_id)
                return
            self.pending.append(request_id)
            STOP.set()

    def end(self):
        with self.lock:
            for request_id in self.pending:
                acknowledge(request_id)
            self.pending.clear()
            self.busy = not INBOX.empty()
            STOP.clear()


INTERRUPTS = Interrupts()


def read_input():
    for line in sys.stdin:
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if value.get("type") == "control_request" and value["request"].get("subtype") == "interrupt":
            INTERRUPTS.request(value["request_id"])
        elif value.get("type") == "control_response":
            REPLIES.put(value["response"])
        elif value.get("type") == "user":
            INTERRUPTS.arrive(value)
    INBOX.put(None)


def pause(seconds):
    if STOP.wait(seconds * PACE):
        raise Stopped


class Permissions:
    """The companion's permission tool, reached through the MCP server it configured."""

    def __init__(self):
        name = option("--permission-prompt-tool")
        config = option("--mcp-config")
        self.server = self.tool = self.process = None
        if name and config and name.count("__") == 2:
            _, server, self.tool = name.split("__")
            text = config if config.lstrip().startswith("{") else Path(config).read_text()
            self.server = json.loads(text)["mcpServers"].get(server)
        self.sequence = 0

    def ask(self, tool, tool_input, tool_id):
        if not self.server:
            return {"behavior": "deny", "message": "No permission prompt tool is configured"}
        if not self.process:
            self.start()
        reply = self.rpc(
            "tools/call",
            {"name": self.tool, "arguments": {"tool_name": tool, "input": tool_input, "tool_use_id": tool_id}},
        )
        if "error" in reply:
            return {"behavior": "deny", "message": reply["error"].get("message") or "The permission tool failed"}
        text = reply["result"]["content"][0]["text"]
        if reply["result"].get("isError"):
            return {"behavior": "deny", "message": text}
        return json.loads(text)

    def start(self):
        command = [self.server["command"], *self.server.get("args", [])]
        env = {**os.environ, **self.server.get("env", {})}
        self.process = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env
        )
        replies = self.replies = queue.Queue()
        output = self.process.stdout

        def read():
            for line in output:
                try:
                    replies.put(json.loads(line))
                except ValueError:
                    continue

        threading.Thread(target=read, daemon=True).start()
        self.rpc(
            "initialize",
            {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": VERSION}},
        )
        self.write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def write(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.sequence += 1
        try:
            self.write({"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params})
        except OSError:
            self.close()
            return {"error": {"message": "The permission tool is not running"}}
        while True:
            try:
                reply = self.replies.get(timeout=0.1)
            except queue.Empty:
                if STOP.is_set():
                    # Closing the server withdraws the pending request.
                    self.close()
                    raise Stopped
                if self.process.poll() is not None:
                    self.close()
                    return {"error": {"message": "The permission tool stopped"}}
                continue
            if reply.get("id") == self.sequence:
                return reply

    def close(self):
        if not self.process:
            return
        try:
            self.process.stdin.close()
            self.process.wait(timeout=2)
        except (OSError, subprocess.TimeoutExpired):
            self.process.kill()
            self.process.wait()
        self.process = None


class Session:
    def __init__(self):
        self.id = option("--resume") or str(uuid.uuid4())
        self.continuing = "--resume" in sys.argv
        self.model = option("--model") or "claude-opus-5-5"
        self.effort = option("--effort")
        if option("--permission-mode") != "plan":
            self.mode = "work"
        elif "ExitPlanMode" in (option("--disallowedTools") or ""):
            self.mode = "ask"
        else:
            self.mode = "plan"
        self.cwd = Path.cwd()
        self.permissions = Permissions() if self.mode == "work" else None
        self.steps = 0
        self.tokens = [0, 0]
        self.denials = []

    def path(self, name):
        return str(self.cwd / name)

    def read(self, name):
        return (self.cwd / name).read_text()

    def demo(self):
        return (self.cwd / "tips.py").is_file() and (self.cwd / "tests" / "test_tips.py").is_file()

    def files(self, pattern="**/*"):
        return sorted(
            p
            for p in self.cwd.glob(pattern)
            if p.is_file() and not {".git", "__pycache__"} & set(p.relative_to(self.cwd).parts)
        )


SESSION = None


def frame(kind, **fields):
    send({"type": kind, **fields, "parent_tool_use_id": None, "session_id": SESSION.id, "uuid": str(uuid.uuid4())})


def stream(event):
    frame("stream_event", event=event)


def assistant(message_id, content):
    usage = {"input_tokens": SESSION.tokens[0], "output_tokens": SESSION.tokens[1]}
    message = {"id": message_id, "type": "message", "role": "assistant", "model": SESSION.model}
    frame("assistant", message={**message, "content": content, "stop_reason": None, "usage": usage})


def begin_message():
    message_id = "msg_" + uuid.uuid4().hex[:24]
    message = {"id": message_id, "type": "message", "role": "assistant", "model": SESSION.model, "content": []}
    stream({"type": "message_start", "message": message})
    return message_id


def end_message(reason):
    stream({"type": "content_block_stop", "index": 0})
    stream({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": SESSION.tokens[1]}})
    stream({"type": "message_stop"})


def replay(message):
    send({**message, "parent_tool_use_id": None, "session_id": SESSION.id, "isReplay": True})


def heed():
    """Acknowledge messages that arrived during the turn, between steps."""
    while True:
        try:
            message = INBOX.get_nowait()
        except queue.Empty:
            return
        if message is None:
            INBOX.put(None)
            return
        replay(message)
        say(f"Noted: “{text_of(message)}”. I'll keep that in mind for the rest of this task.")


def say(text):
    """Stream one assistant message, a few words at a time."""
    message_id = begin_message()
    stream({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
    written = ""
    try:
        for chunk in re.findall(r"(?:\S+\s*){1,3}|\s+", text):
            pause(0.04)
            stream({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": chunk}})
            written += chunk
            SESSION.tokens[1] += max(1, len(chunk) // 4)
    finally:
        end_message("end_turn")
        if written:
            assistant(message_id, [{"type": "text", "text": written}])
    return text


def tool_result(tool_id, content, failed, data):
    block = {"type": "tool_result", "tool_use_id": tool_id, "content": content, "is_error": failed}
    fields = {"message": {"role": "user", "content": [block]}}
    if data is not None:
        fields["tool_use_result"] = data
    frame("user", **fields)
    SESSION.tokens[0] += len(content) // 4


def announce(tool, tool_input):
    tool_id = "toolu_" + uuid.uuid4().hex[:24]
    message_id = begin_message()
    stream({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": tool_id, "name": tool, "input": {}}})
    stream({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": json.dumps(tool_input)}})
    end_message("tool_use")
    assistant(message_id, [{"type": "tool_use", "id": tool_id, "name": tool, "input": tool_input}])
    SESSION.steps += 1
    return tool_id


def call(tool, tool_input, think=1.2):
    """One tool step: announce it, ask when Claude Code would, run it, report the result."""
    heed()
    pause(think)
    tool_id = announce(tool, tool_input)
    try:
        if tool in GATED:
            decision = SESSION.permissions.ask(tool, tool_input, tool_id)
            if decision.get("behavior") != "allow":
                reason = decision.get("message") or "Permission denied"
                SESSION.denials.append({"tool_name": tool, "tool_use_id": tool_id, "tool_input": tool_input})
                tool_result(tool_id, reason, True, None)
                raise Denied(reason)
            if isinstance(decision.get("updatedInput"), dict):
                tool_input = decision["updatedInput"]
        pause(0.6)
        content, data, failed = TOOLS[tool](tool_input)
    except Stopped:
        tool_result(tool_id, "Interrupted by user", True, {"interrupted": True} if tool == "Bash" else None)
        raise
    tool_result(tool_id, content, failed, data)
    return SimpleNamespace(output=content, failed=failed, data=data or {})


def patch(before, after):
    hunks = []
    for line in difflib.unified_diff(before.splitlines(), after.splitlines(), lineterm=""):
        header = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", line)
        if header:
            old, old_lines, new, new_lines = header.groups()
            hunks.append(
                {
                    "oldStart": int(old),
                    "oldLines": int(old_lines or 1),
                    "newStart": int(new),
                    "newLines": int(new_lines or 1),
                    "lines": [],
                }
            )
        elif hunks:
            hunks[-1]["lines"].append(line)
    return hunks


def run_read(tool_input):
    path = Path(tool_input["file_path"])
    try:
        text = path.read_text(errors="replace")
    except OSError as error:
        return f"File does not exist or cannot be read: {error.strerror}", None, True
    lines = text.splitlines()
    numbered = "\n".join(f"{number:>6}\t{line}" for number, line in enumerate(lines, 1))
    file = {"filePath": str(path), "content": text, "numLines": len(lines), "startLine": 1, "totalLines": len(lines)}
    return numbered, {"type": "text", "file": file}, False


def run_glob(tool_input):
    names = [str(p) for p in SESSION.files(tool_input["pattern"])]
    return "\n".join(names) or "No files found", {"filenames": names, "numFiles": len(names), "truncated": False}, False


def run_grep(tool_input):
    pattern = re.compile(tool_input["pattern"])
    hits = []
    for path in SESSION.files():
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            if pattern.search(line):
                hits.append(f"{path}:{number}:{line}")
    content = "\n".join(hits) or "No matches found"
    return content, {"mode": "content", "numFiles": len({h.split(":")[0] for h in hits}), "content": content, "numLines": len(hits)}, False


def run_bash(tool_input):
    process = subprocess.Popen(
        ["/bin/sh", "-c", tool_input["command"]],
        cwd=SESSION.cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        process_group=0,
    )
    while True:
        try:
            stdout, stderr = process.communicate(timeout=0.1)
            break
        except subprocess.TimeoutExpired:
            if STOP.is_set():
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
                raise Stopped
    output = (stdout + stderr).strip()
    code = process.returncode
    content = output if code == 0 else f"Exit code {code}\n{output}"
    data = {"stdout": stdout, "stderr": stderr, "interrupted": False, "isImage": False, "exit_code": code}
    return content, data, code != 0


def run_edit(tool_input):
    path = Path(tool_input["file_path"])
    try:
        before = path.read_text()
    except OSError as error:
        return f"File does not exist or cannot be read: {error.strerror}", None, True
    old, new = tool_input["old_string"], tool_input["new_string"]
    count = before.count(old)
    if count == 0:
        return "String to replace not found in file.", None, True
    every = tool_input.get("replace_all") is True
    if count > 1 and not every:
        return f"Found {count} matches of the string to replace, but replace_all is false.", None, True
    after = before.replace(old, new) if every else before.replace(old, new, 1)
    path.write_text(after)
    data = {
        "filePath": str(path),
        "oldString": old,
        "newString": new,
        "originalFile": before,
        "structuredPatch": patch(before, after),
        "userModified": False,
        "replaceAll": every,
    }
    return f"The file {path} has been updated.", data, False


def run_write(tool_input):
    path = Path(tool_input["file_path"])
    existed = path.exists()
    before = path.read_text() if existed else ""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(tool_input["content"])
    data = {
        "type": "update" if existed else "create",
        "filePath": str(path),
        "content": tool_input["content"],
        "structuredPatch": patch(before, tool_input["content"]),
    }
    done = f"The file {path} has been updated." if existed else f"File created successfully at: {path}"
    return done, data, False


def run_fetch(tool_input):
    # Scripted: the text below is what the page says; no request is sent.
    result = (
        "ROUND_HALF_UP rounds to the nearest value, with ties going away from zero: "
        "Decimal('0.125').quantize(Decimal('0.01'), rounding=ROUND_HALF_UP) is Decimal('0.13'). "
        "(Scripted review agent: no request was sent.)"
    )
    return result, {"url": tool_input["url"], "result": result}, False


def run_question(tool_input):
    """Claude Code takes the answers in the allowed input: the questions passed
    through, and either an answer to each keyed by its text, or `response`."""
    questions = tool_input.get("questions")
    asked = [question.get("question") for question in questions] if isinstance(questions, list) and questions else []
    answers = tool_input.get("answers")
    response = tool_input.get("response")
    if isinstance(response, str) and response.strip() and isinstance(answers, dict) and not answers and asked:
        return f"The user responded: {response}", {"response": response}, False
    if (
        not asked
        or not isinstance(answers, dict)
        or sorted(answers) != sorted(asked)
        or not all(isinstance(answer, str) and answer.strip() for answer in answers.values())
    ):
        return "AskUserQuestion needs the questions passed through with an answer to each, or a response", None, True
    chosen = ", ".join(f'"{question}"="{answers[question]}"' for question in asked)
    return (
        f"User has answered your questions: {chosen}. You can now continue with the user's answers in mind.",
        {"answers": answers},
        False,
    )


TOOLS = {
    "Read": run_read,
    "Glob": run_glob,
    "Grep": run_grep,
    "Bash": run_bash,
    "Edit": run_edit,
    "Write": run_write,
    "WebFetch": run_fetch,
    "AskUserQuestion": run_question,
}


def text_of(message):
    return "\n".join(b["text"] for b in message["message"]["content"] if b.get("type") == "text").strip()


def tally(output):
    total = re.search(r"^Ran (\d+) tests?", output, re.M)
    failing = sum(int(n) for n in re.findall(r"(?:failures|errors)=(\d+)", output))
    return (int(total[1]) if total else 0), failing


def outcome(run):
    total, failing = tally(run.output)
    return f"all {total} pass" if not run.failed else f"{failing} of {total} failed"


def edit(name, old, new):
    return call("Edit", {"file_path": SESSION.path(name), "old_string": old, "new_string": new})


def run_tests():
    return call("Bash", {"command": TEST, "description": "Run the test suite"})


def next_improvement():
    tests = SESSION.read("tests/test_tips.py")
    return next((item for item in IMPROVEMENTS if item["test"] not in tests), None)


def unfinished_improvement():
    tests, tips = SESSION.read("tests/test_tips.py"), SESSION.read("tips.py")
    return next((i for i in IMPROVEMENTS if i["test"] in tests and i["guard"] not in tips), None)


def survey():
    say("I'll take a look around first.")
    call("Glob", {"pattern": "*"})
    readme = next((p for p in SESSION.files("*") if p.name.lower().startswith("readme")), None)
    if readme:
        call("Read", {"file_path": str(readme)})
    return say(
        "This review agent only changes the tip-splitter demo project, so I looked around here without "
        "changing anything. Open a task in the demo project to see edits, checks and approvals."
    )


def work(text):
    """Fix the demo's bug, or make its next improvement test-first."""
    if not SESSION.demo():
        return survey()
    carried = CARRIED.search(text)
    if carried:
        say(f"Following the plan from run {carried[1]}. First I'll confirm where the tests stand.")
    elif SESSION.continuing:
        say("Picking up where we left off. I'll check where the tests stand first.")
    else:
        say("I'll look over the project, then run its tests to see where things stand.")
        call("Glob", {"pattern": "**/*.py"})
        call("Read", {"file_path": SESSION.path("README.md")})
    try:
        before = run_tests()
    except Denied as denied:
        return say(f"You declined running the tests ({denied}). I won't change code without seeing them run, so nothing changed.")
    if before.failed:
        return repair(before)
    item = next_improvement()
    if not item:
        return say(f"`{TEST}`: {outcome(before)}. There's nothing left on my list for this project, so I changed nothing.")
    say(f"Every test passes. Next on my list: {item['title']}. I'll write the test first.")
    call("Read", {"file_path": SESSION.path("tests/test_tips.py")})
    try:
        added = edit("tests/test_tips.py", TEST_ANCHOR, "\n\n" + item["code"] + TEST_ANCHOR)
        if added.failed:
            return say(f"`tests/test_tips.py` doesn't look the way I expected ({added.output}), so I stopped there.")
        failing = run_tests()
    except Denied as denied:
        return say(f"You declined that step ({denied}), so I stopped there. `{TEST}` passed before I started.")
    if not failing.failed:
        return say(f"I added `{item['test']}` to `tests/test_tips.py`, but it already passes, so `tips.py` needs no change.")
    say("The new test fails, as it should. Now the change itself.")
    return finish(item, failing)


def repair(before):
    """Tests fail: fix the known bug or finish a half-made improvement."""
    names = re.findall(r"^(?:FAIL|ERROR): (\w+)", before.output, re.M)
    say(f"`{TEST}`: {outcome(before)} ({', '.join(f'`{n}`' for n in names)}). Let me look at the calculation.")
    call("Read", {"file_path": SESSION.path("tips.py")})
    item = unfinished_improvement()
    if BUG[0] not in SESSION.read("tips.py"):
        if not item:
            return say("These failures aren't ones this review agent knows how to fix, so I changed nothing.")
        say(f"The test for the change to {item['title']} is in place but `tips.py` doesn't do it yet.")
        return finish(item, before)
    call("Grep", {"pattern": "tip_percent", "path": str(SESSION.cwd), "output_mode": "content", "-n": True})
    say("`split_bill` multiplies the bill by `1 + tip_percent`, so a 20% tip is treated as 2000%. The percentage needs dividing by 100.")
    try:
        changed = edit("tips.py", *BUG)
        if changed.failed:
            return say(f"The edit didn't apply ({changed.output}), so nothing changed.")
        after = run_tests()
    except Denied as denied:
        return say(f"You declined that step ({denied}). The bug is still there: a 20% tip multiplies the bill by 21.")
    if after.failed:
        return say(f"I changed `tips.py`, but `{TEST}` still fails ({outcome(after)}):\n\n{after.output}")
    return say(
        "Fixed the tip calculation: `split_bill` now treats the tip as a percentage, so 100.00 with a 20% tip "
        "split four ways is 30.00 each.\n\n"
        "- Changed `tips.py`: divide `tip_percent` by 100.\n"
        f"- `{TEST}`: {outcome(before)} before the change; {outcome(after)} now."
    )


def finish(item, before):
    call("Read", {"file_path": SESSION.path("tips.py")})
    try:
        changed = edit("tips.py", GUARD_ANCHOR, item["guard"] + GUARD_ANCHOR)
        if changed.failed:
            return say(f"`tips.py` doesn't look the way I expected ({changed.output}), so I stopped there.")
        after = run_tests()
    except Denied as denied:
        return say(f"You declined that step ({denied}), so `{item['test']}` still fails.")
    if after.failed:
        return say(f"I changed `tips.py`, but `{TEST}` still fails ({outcome(after)}):\n\n{after.output}")
    return say(
        f"Done: `split_bill` now {item['title'].replace('refuse', 'refuses', 1)}.\n\n"
        f"- Changed `tests/test_tips.py`: added `{item['test']}`.\n"
        "- Changed `tips.py`: check the input before splitting.\n"
        f"- `{TEST}`: {outcome(before)} with the new test alone; {outcome(after)} now."
    )


def question(text):
    say(
        "Before I change anything I need one decision from you: should each share be rounded to the nearest "
        "cent, or always up so the bill is fully covered?"
    )
    ask = {
        "questions": [
            {
                "question": "How should each share be rounded?",
                "header": "Rounding",
                "options": [
                    {"label": "Nearest cent", "description": "Keep today's rounding: 10.00 between three is 3.33 each."},
                    {"label": "Always up", "description": "Round up so the bill is covered: 3.34 each."},
                ],
                "multiSelect": False,
            }
        ]
    }
    try:
        reply = call("AskUserQuestion", ask)
    except Denied as denied:
        return say(f"No answer came back ({denied}), so I'll keep the recommended option, Nearest cent, and change nothing.")
    if reply.failed:
        return say("Your answer didn't reach me, so I'll keep the recommended option, Nearest cent, and change nothing.")
    if "response" in reply.data:
        return say(f"Thanks. You said: {reply.data['response']}. I'll use that for the rounding work; nothing changed yet.")
    return say(f"Thanks. You chose: {', '.join(reply.data['answers'].values())}. I'll use that for the rounding work; nothing changed yet.")


def fetch(text):
    say("I'll check how Python's decimal module rounds halfway values before touching the rounding code.")
    try:
        call("WebFetch", {"url": DOCS, "prompt": "How does ROUND_HALF_UP treat a value exactly halfway between two cents?"})
    except Denied as denied:
        return say(f"You declined the fetch ({denied}). From memory, `ROUND_HALF_UP` rounds halves away from zero; I changed nothing.")
    return say("The docs confirm `ROUND_HALF_UP` rounds halves away from zero, so 0.125 becomes 0.13. `tips.py` already uses it; nothing needs to change.")


def changelog(text):
    if not SESSION.demo():
        return survey()
    say("I'll record what's been done in a changelog.")
    call("Glob", {"pattern": "*.md"})
    entries = []
    if BUG[0] not in SESSION.read("tips.py"):
        entries.append("Treat the tip as a percentage.")
    tips = SESSION.read("tips.py")
    entries += [item["title"].capitalize() + "." for item in IMPROVEMENTS if item["guard"] in tips]
    lines = "\n".join(f"- {entry}" for entry in entries or ["Nothing released yet."])
    try:
        call("Write", {"file_path": SESSION.path("CHANGELOG.md"), "content": f"# Changelog\n\n## Unreleased\n\n{lines}\n"})
    except Denied as denied:
        return say(f"You declined writing `CHANGELOG.md` ({denied}), so nothing was written.")
    return say(f"Wrote `CHANGELOG.md` with {len(entries) or 'no'} entr{'y' if len(entries) == 1 else 'ies'}.")


def slow(text):
    say("This will take a while: I'll go through every file, one at a time.")
    files = SESSION.files()
    for number, path in enumerate(files, 1):
        call("Read", {"file_path": str(path)}, think=3)
        if number % 3 == 0:
            say(f"{number} of {len(files)} files read so far; nothing wrong yet.")
    return say(f"Read all {len(files)} files. Nothing needed changing.")


def give_up(text):
    say("Let me take a look.")
    call("Glob", {"pattern": "*"})
    raise Failed("I can't finish this task: the prompt asked me to give up.")


def plan(text):
    """Investigate, then propose what Work would do next."""
    say("I'll read through the project before proposing anything.")
    if not SESSION.demo():
        call("Glob", {"pattern": "*"})
        return propose(
            "Look around",
            ["Read the README and the main source files.", "Report what the project does. This review agent only changes the tip-splitter demo."],
        )
    call("Glob", {"pattern": "**/*.py"})
    call("Read", {"file_path": SESSION.path("tips.py")})
    call("Read", {"file_path": SESSION.path("tests/test_tips.py")})
    call("Grep", {"pattern": "def test_", "path": SESSION.path("tests"), "output_mode": "content", "-n": True})
    if BUG[0] in SESSION.read("tips.py"):
        return propose(
            "Fix the tip calculation",
            [
                f"Run `{TEST}` to confirm `test_tip_is_a_percentage` fails.",
                "In `tips.py`, divide `tip_percent` by 100 so a 20% tip adds a fifth of the bill, not twenty times it.",
                f"Rerun `{TEST}` and confirm every test passes.",
                "Report the changed file and both test runs.",
            ],
        )
    item = unfinished_improvement() or next_improvement()
    if not item:
        return propose("Nothing to change", [f"Run `{TEST}` to confirm every test still passes.", "Report back: nothing is left on the list."])
    return propose(
        item["title"].capitalize(),
        [
            f"Add `{item['test']}` to `tests/test_tips.py`.",
            f"Run `{TEST}` and confirm the new test fails.",
            "Add the check to `split_bill` in `tips.py`.",
            f"Rerun `{TEST}` and confirm every test passes.",
        ],
    )


def propose(title, steps):
    text = f"## {title}\n\n" + "\n".join(f"{n}. {step}" for n, step in enumerate(steps, 1))
    pause(1.5)
    tool_id = announce("ExitPlanMode", {"plan": text})
    request_id = str(uuid.uuid4())
    send(
        {
            "type": "control_request",
            "request_id": request_id,
            "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": {"plan": text}, "tool_use_id": tool_id},
        }
    )
    while True:
        try:
            reply = REPLIES.get(timeout=0.1)
        except queue.Empty:
            if STOP.is_set():
                raise Stopped
            continue
        if reply.get("request_id") == request_id:
            break
    note = reply.get("error") or "The plan is ready for review."
    tool_result(tool_id, note, reply.get("subtype") != "success", None)
    return text


def ask(text):
    """Answer from reading only."""
    say("Let me read the code before answering.")
    if not SESSION.demo():
        call("Glob", {"pattern": "*"})
        return say("This review agent only knows the tip-splitter demo well. Here I can see the files listed above; nothing was changed.")
    call("Read", {"file_path": SESSION.path("tips.py")})
    call("Read", {"file_path": SESSION.path("tests/test_tips.py")})
    tests = len(re.findall(r"def test_", SESSION.read("tests/test_tips.py")))
    state = (
        "One problem: the tip is used as a whole number, so `split_bill(100, 20, 4)` returns 525.00 instead of "
        "30.00 and `test_tip_is_a_percentage` fails."
        if BUG[0] in SESSION.read("tips.py")
        else "The calculation matches what the tests expect."
    )
    return say(
        "This is a small tip-splitting web app. `split_bill` in `tips.py` adds the tip, divides by the number "
        f"of people and rounds to the cent; `app.py` serves it as a page and a JSON endpoint. `tests/test_tips.py` "
        f"has {tests} tests. {state}"
    )


# Checked in order against the person's own words; the default is `work`.
WORDS = [
    (("give up",), give_up),
    (("question", "ask me"), question),
    (("fetch", "url", "look up"), fetch),
    (("changelog", "new file"), changelog),
    (("slow", "take your time", "every file"), slow),
]


def choose(text):
    if SESSION.mode != "work":
        return plan if SESSION.mode == "plan" else ask
    own = CARRIED.sub("", text).lower()
    return next((s for words, s in WORDS if any(re.search(rf"\b{re.escape(w)}\b", own) for w in words)), work)


def turn(message):
    replay(message)
    started = time.monotonic()
    SESSION.tokens[0] += 2400
    SESSION.denials = []
    try:
        final = choose(text_of(message))(text_of(message))
        heed()
        result = {"subtype": "success", "is_error": False, "result": final}
    except Stopped:
        result = {"subtype": "success", "is_error": False, "result": "Stopped at your request."}
    except Failed as failure:
        say(str(failure))
        result = {"subtype": "error_during_execution", "is_error": True, "errors": [str(failure)], "result": str(failure)}
    INTERRUPTS.end()
    elapsed = int((time.monotonic() - started) * 1000)
    usage = {"input_tokens": SESSION.tokens[0], "output_tokens": SESSION.tokens[1]}
    frame("result", **result, duration_ms=elapsed, duration_api_ms=elapsed, num_turns=SESSION.steps + 1, usage=usage,
          permission_denials=SESSION.denials)
    SESSION.continuing = True


def main():
    global SESSION
    if "--help" in sys.argv:
        print(HELP)
        return
    if "--version" in sys.argv or "-v" in sys.argv:
        print(VERSION)
        return
    if sys.argv[1:3] == ["auth", "status"]:
        print(json.dumps({"loggedIn": True, "authMethod": "claude.ai", "subscriptionType": "review"}))
        return
    SESSION = Session()
    threading.Thread(target=read_input, daemon=True).start()
    first = True
    while (message := INBOX.get()) is not None:
        if first:
            first = False
            mcp = [{"name": "computer", "status": "connected"}] if SESSION.permissions and SESSION.permissions.server else []
            frame(
                "system",
                subtype="init",
                cwd=str(SESSION.cwd),
                model=SESSION.model,
                **({"effort": SESSION.effort} if SESSION.effort else {}),
                permissionMode="plan" if SESSION.mode != "work" else "default",
                tools=sorted(TOOLS) + ["ExitPlanMode"],
                mcp_servers=mcp,
                apiKeySource="none",
                claude_code_version=VERSION.split()[0],
            )
        turn(message)
    if SESSION.permissions:
        SESSION.permissions.close()


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        sys.exit(0)
