# SPDX-License-Identifier: Apache-2.0
"""Compare the phone's Stop receipt with guest witness and bridge timestamps."""

import argparse
import json
from pathlib import Path


def summarize(phone, actions, inputs):
    timing = phone["timing"]
    receipt = timing["receipt"]
    start = max(
        i for i, event in enumerate(actions) if event["event"] == "fixture_started"
    )
    actions = actions[start:]
    started = actions[0]["realtime_ns"]
    inputs = [event for event in inputs if event.get("realtime_ns", 0) >= started]
    tools = [event for event in actions if event["event"] == "tool"]
    # RPCs are serial. A call already in flight at interrupt can finish after
    # it; classify deliberately late calls by invocation, not reply arrival.
    late = []
    started_after_interrupt = None
    for event in actions:
        if event["event"] == "tool_started":
            started_after_interrupt = event["after_interrupt"]
        elif event["event"] == "tool":
            if (
                event["after_interrupt"]
                if started_after_interrupt is None
                else started_after_interrupt
            ):
                late.append(event)
            started_after_interrupt = None
    typed = [event for event in inputs if event["event"] == "text"]
    clicks = [
        event for event in inputs if event["event"] == "pointer" and event["buttons"]
    ]
    after_receipt = [
        event for event in inputs if event["realtime_ns"] >= receipt["at"] * 1_000_000
    ]
    facts = receipt["receipt"]["facts"]
    passed = (
        timing.get("activeBeforeTap") is True
        and len(late) == 5
        and all(event["failed"] for event in late)
        and any(
            event["name"] == "observe_target" and not event["failed"] for event in tools
        )
        and sum(
            event["name"] == "pointer_action" and not event["failed"] for event in tools
        )
        == 2
        and bool(typed and clicks)
        and not after_receipt
        and not any("AFTER_STOP" in event["text"] for event in inputs)
        and all(fact["status"] == "done" for fact in facts)
        and timing["revoked"] is not None
        and timing["revoked"] >= timing["tap"]
    )
    return {
        "status": "pass" if passed else "fail",
        "sample_count": 1,
        "agent_input_active_before_tap": timing.get("activeBeforeTap") is True,
        "stop_tap_to_revocation_observed_ms": timing["revoked"] - timing["tap"],
        "stop_tap_to_receipt_observed_ms": receipt["observedAt"] - timing["tap"],
        "companion_receipt_at_ms": receipt["at"],
        "last_witness_event_ns": inputs[-1]["realtime_ns"] if inputs else None,
        "witness_text_changes": len(typed),
        "witness_clicks": len(clicks),
        "calls_completed_after_interrupt": sum(
            event["after_interrupt"] for event in tools
        ),
        "late_calls": len(late),
        "late_calls_refused": sum(event["failed"] for event in late),
        "witness_events_after_receipt": len(after_receipt),
        "facts": facts,
        "method": "Phone performance.now immediately before Stop click to first revoked lease plus withdrawn observation; witness realtime_ns and companion event timestamp share the guest clock. Receipt arrival is measured separately. One run, five deliberate post-interrupt calls, 3.5-second post-tap observation.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    folder = parser.parse_args().evidence
    result = summarize(
        json.loads((folder / "computer-stop.json").read_text(encoding="utf-8")),
        [
            json.loads(line)
            for line in (folder / "stop-actions.jsonl")
            .read_text(encoding="utf-8")
            .splitlines()
        ],
        [
            json.loads(line)
            for line in (folder / "stop-input-events.jsonl")
            .read_text(encoding="utf-8")
            .splitlines()
        ],
    )
    (folder / "stop-result.json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(result, indent=2))
    if result["status"] != "pass":
        raise SystemExit("Stop evidence did not pass")


if __name__ == "__main__":
    main()
