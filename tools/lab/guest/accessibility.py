#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Inspect or activate a visible accessible control in the disposable guest."""

import argparse
import json
import os
from pathlib import Path

if (
    Path("/etc/companion-lab").read_text(encoding="utf-8").strip()
    != "disposable-desktop-lab"
    or os.geteuid() == 0
):
    raise SystemExit("Requires an ordinary disposable guest user")

import pyatspi
from gi.repository import GLib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("action", choices=("dump", "activate", "select"))
parser.add_argument("--name")
parser.add_argument("--role")
parser.add_argument("--app")
parser.add_argument(
    "--action-name", help="Exact accessible action for controls with several actions"
)
args = parser.parse_args()
matches = []


def walk(node, path=()):
    if len(path) > 20:
        return
    role, name = node.getRoleName(), node.name
    showing = node.getState().contains(pyatspi.STATE_SHOWING)
    actions = []
    try:
        action = node.queryAction()
        actions = [action.getName(i) for i in range(action.nActions)]
    except (NotImplementedError, GLib.Error):
        pass
    if args.action == "dump" and (showing or role == "application"):
        print(
            json.dumps(
                {
                    "path": path,
                    "role": role,
                    "name": name,
                    "actions": actions,
                    "checked": node.getState().contains(pyatspi.STATE_CHECKED),
                    "pressed": node.getState().contains(pyatspi.STATE_PRESSED),
                    "selected": node.getState().contains(pyatspi.STATE_SELECTED),
                    "enabled": node.getState().contains(pyatspi.STATE_ENABLED),
                }
            )
        )
    if showing and name == args.name and (args.role is None or role == args.role):
        matches.append(node)
    for index in range(node.childCount):
        child = node.getChildAtIndex(index)
        if child is not None:
            walk(child, (*path, index))


desktop = pyatspi.Registry.getDesktop(0)
for index in range(desktop.childCount):
    app = desktop.getChildAtIndex(index)
    if app is not None and (args.app is None or app.name == args.app):
        walk(app, (index,))
if args.action in ("activate", "select"):
    if len(matches) != 1:
        raise SystemExit(
            f"Expected one visible accessible control, found {len(matches)}"
        )
    node = matches[0]
    role = node.getRoleName()
    if args.action == "select" and any(
        node.getState().contains(state)
        for state in (
            pyatspi.STATE_CHECKED,
            pyatspi.STATE_PRESSED,
            pyatspi.STATE_SELECTED,
        )
    ):
        print(json.dumps({"already_selected": args.name}))
        raise SystemExit(0)
    action = node.queryAction()
    indices = [
        i
        for i in range(action.nActions)
        if args.action_name is None or action.getName(i) == args.action_name
    ]
    if len(indices) != 1:
        raise SystemExit("Specify exactly one accessible action")
    if not action.doAction(indices[0]):
        raise SystemExit("Accessible action refused")
    print(json.dumps({"activated": args.name, "role": role}))
