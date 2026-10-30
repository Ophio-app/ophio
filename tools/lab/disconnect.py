#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Exercise held-input release and truthful reconnection in the lab Android app."""

import argparse
import json
import os
import subprocess
import time

import lab

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("target", choices=lab.TARGETS)
args = parser.parse_args()
os.umask(0o077)
if os.environ.get("ADB_SERVER_SOCKET") not in ("tcp:5038", "tcp:5042"):
    raise SystemExit("Select the isolated Android build environment first")
record = lab.state(args.target)
if not record or not record["restricted"]:
    raise SystemExit("Qualification requires the running guest's restricted network")
folder = lab.EVIDENCE / args.target
folder.mkdir(parents=True, exist_ok=True, mode=0o700)


def phone(step):
    lab.run(["node", lab.ROOT / "tools/lab/phone.mjs", args.target, step])


def witness(name):
    result = lab.guest(
        args.target, ["cat", "evidence/input-state.json"], capture_output=True
    )
    state = json.loads(result.stdout)
    (folder / name).write_bytes(result.stdout)
    return state


def network(enabled):
    errors = []
    for kind in ("wifi", "data"):
        try:
            lab.run(
                [
                    "adb",
                    "-s",
                    lab.adb_serial(),
                    "shell",
                    "svc",
                    kind,
                    "enable" if enabled else "disable",
                ],
                timeout=10,
            )
        except subprocess.SubprocessError as error:
            errors.append(error)
    if errors:
        raise errors[0]


disconnected = False
try:
    phone("hold")
    before = witness("held-before-disconnect.json")
    network(False)
    disconnected = True
    time.sleep(5)
    after = witness("released-after-disconnect.json")
    phone("snapshot")
    (folder / "snapshot.json").replace(folder / "disconnected.json")
    (folder / "snapshot.png").replace(folder / "disconnected.png")
finally:
    try:
        if not disconnected:
            # Even a failed hold/check can have partially injected input. Force
            # lease expiry before restoring connectivity on that error path.
            network(False)
            time.sleep(5)
    finally:
        network(True)
recovery_started = time.monotonic()
for _ in range(15):
    time.sleep(2)
    phone("snapshot")
    snapshot = json.loads((folder / "snapshot.json").read_text(encoding="utf-8"))
    if snapshot["link"]["state"] == "online":
        break
(folder / "snapshot.json").replace(folder / "reconnected.json")
(folder / "snapshot.png").replace(folder / "reconnected.png")
offline = json.loads((folder / "disconnected.json").read_text(encoding="utf-8"))
online = json.loads((folder / "reconnected.json").read_text(encoding="utf-8"))
result = {
    "held_observed": bool(before["keys"] and before["buttons"]),
    "released": not after["keys"] and not after["buttons"],
    "observation_interval_ms": (after["monotonic_ns"] - before["monotonic_ns"]) / 1e6,
    "offline_link": offline["link"],
    "reconnected_link": online["link"],
    "recovery_observed_ms": (time.monotonic() - recovery_started) * 1000,
    "method": "Guest event timestamps, five-second outage sample, actual phone connection state",
}
(folder / "disconnect-result.json").write_text(
    json.dumps(result, indent=2) + "\n", encoding="utf-8"
)
if (
    not result["held_observed"]
    or not result["released"]
    or offline["link"]["state"] == "online"
    or online["link"]["state"] != "online"
):
    raise SystemExit("Disconnect/reconnect check failed; inspect its evidence")
print("Held input released; phone reported disconnection and reconnection")
