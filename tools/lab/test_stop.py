# SPDX-License-Identifier: Apache-2.0
import copy
import unittest

from stop_result import summarize


class StopEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.phone = {
            "timing": {
                "tap": 10,
                "activeBeforeTap": True,
                "revoked": 40,
                "receipt": {
                    "at": 100,
                    "observedAt": 90,
                    "receipt": {"facts": [{"status": "done"}]},
                },
            }
        }
        self.actions = [{"event": "fixture_started", "realtime_ns": 1}]
        self.actions += [
            {"event": "tool", "name": name, "after_interrupt": False, "failed": False}
            for name in ["observe_target", "pointer_action", "pointer_action"]
        ]
        self.actions += [
            {
                "event": "tool",
                "name": "type_text",
                "after_interrupt": True,
                "failed": True,
            }
            for _ in range(5)
        ]
        self.inputs = [
            {"event": "pointer", "buttons": [1], "text": "", "realtime_ns": 2},
            {"event": "text", "buttons": [], "text": "x", "realtime_ns": 3},
        ]

    def test_complete_observation_passes(self):
        self.assertEqual(
            summarize(self.phone, self.actions, self.inputs)["status"], "pass"
        )

    def test_late_success_or_witness_event_fails(self):
        actions = copy.deepcopy(self.actions)
        actions[-1]["failed"] = False
        self.assertEqual(summarize(self.phone, actions, self.inputs)["status"], "fail")
        inputs = self.inputs + [{**self.inputs[-1], "realtime_ns": 101_000_000}]
        self.assertEqual(summarize(self.phone, self.actions, inputs)["status"], "fail")

    def test_acknowledgements_without_witness_delivery_fail(self):
        self.assertEqual(summarize(self.phone, self.actions, [])["status"], "fail")

    def test_inflight_reply_is_distinct_from_five_deliberately_late_calls(self):
        actions = (
            self.actions[:4]
            + [
                {
                    "event": "tool_started",
                    "name": "type_text",
                    "after_interrupt": False,
                },
                {
                    "event": "tool",
                    "name": "type_text",
                    "after_interrupt": True,
                    "failed": True,
                },
            ]
            + self.actions[4:]
        )
        result = summarize(self.phone, actions, self.inputs)
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["late_calls"], 5)
        self.assertEqual(result["calls_completed_after_interrupt"], 6)

    def test_already_revoked_input_does_not_qualify_mid_action_stop(self):
        self.phone["timing"]["activeBeforeTap"] = False
        self.assertEqual(
            summarize(self.phone, self.actions, self.inputs)["status"], "fail"
        )
