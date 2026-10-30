#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Manual, metadata-only qualification. Never submits a user/model prompt.

Run explicitly against a chosen native executable, outside the test suite.
Prints only sanitized model evidence; settings and account data are discarded.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import time


def inspect(binary, requested, expected, scratch):
    env = dict(os.environ)
    for key in (
        "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY",
        "OPENAI_API_KEY", "OPENAI_BASE_URL", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN",
        "OPHIO_BRIDGE_TOKEN", "OPHIO_BRIDGE_SOCKET",
    ):
        env.pop(key, None)
    if requested.startswith("claude-"):
        env["CLAUDE_CODE_NO_MODEL_FALLBACK"] = "1"
    version = subprocess.check_output([str(binary), "--version"], env=env, text=True).strip()
    args = [
        str(binary), "--print", "--input-format", "stream-json",
        "--output-format", "stream-json", "--verbose", "--model", requested,
        "--fallback-model", requested, "--safe-mode", "--tools", "",
        "--settings", '{"disableAllHooks":true,"switchModelsOnFlag":false}',
        "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
        "--no-session-persistence",
    ]
    with tempfile.TemporaryDirectory(dir=scratch) as cwd:
        process = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)

                def request(subtype):
                    frame = {"type": "control_request", "request_id": subtype,
                             "request": {"subtype": subtype}}
                    process.stdin.write(json.dumps(frame) + "\n")
                    process.stdin.flush()
                    deadline = time.monotonic() + 8
                    while time.monotonic() < deadline:
                        if not selector.select(max(0, deadline - time.monotonic())):
                            break
                        line = process.stdout.readline(32 * 1024 * 1024)
                        if not line:
                            break
                        value = json.loads(line)
                        response = value.get("response", {})
                        if (value.get("type") == "control_response"
                                and response.get("request_id") == subtype):
                            if response.get("subtype") != "success":
                                raise RuntimeError("native metadata request was refused")
                            return response["response"]
                    raise RuntimeError("native metadata was not received within eight seconds")

                listing = request("initialize")
                resolved = [m.get("resolvedModel") for m in listing.get("models", [])]
                settings = request("get_settings")
                applied = settings.get("applied", {})
                if (expected not in resolved or applied.get("model") != expected
                        or settings.get("effective", {}).get("switchModelsOnFlag") is not False):
                    raise RuntimeError("native exact model verification failed; no prompt sent")
                with binary.open("rb") as executable:
                    digest = hashlib.file_digest(executable, "sha256").hexdigest()
                return {
                    "checked_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    "native_version": version,
                    "binary_sha256": digest,
                    "requested_model": requested, "actual_model": applied["model"],
                    "actual_effort": applied.get("effort"),
                    "exact_id_in_native_catalog": True, "switch_models_on_flag": False,
                    "no_model_fallback_requested": requested.startswith("claude-"),
                    "user_prompts_sent": 0,
                }
        finally:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--claude", type=Path, required=True)
    parser.add_argument("--requested", required=True)
    parser.add_argument("--expected", required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(inspect(args.claude.resolve(), args.requested, args.expected,
                             args.scratch), indent=2))
