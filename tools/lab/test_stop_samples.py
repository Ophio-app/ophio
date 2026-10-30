# SPDX-License-Identifier: Apache-2.0
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from stop_samples import rank, summarize


def sample(revoked, receipt, status="pass"):
    return {
        "status": status,
        "stop_tap_to_revocation_observed_ms": revoked,
        "stop_tap_to_receipt_observed_ms": receipt,
    }


class StopSampleTests(unittest.TestCase):
    def test_nearest_rank(self):
        values = list(range(1, 21))
        self.assertEqual(rank(values, 0.5), 10)
        self.assertEqual(rank(values, 0.9), 18)
        self.assertEqual(rank([7], 0.9), 7)
        # Ranks that fall between samples round up, never to the nearest.
        self.assertEqual(rank(list(range(1, 22)), 0.5), 11)
        self.assertEqual(rank(list(range(1, 7)), 0.9), 6)

    def test_all_passing_samples_are_summarized(self):
        result = summarize([sample(n, 2 * n) for n in (30, 10, 20)])
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["sample_count"], 3)
        self.assertEqual(
            result["stop_tap_to_revocation_observed_ms"],
            {"min": 10, "median": 20, "p90": 30, "max": 30},
        )
        self.assertEqual(result["stop_tap_to_receipt_observed_ms"]["max"], 60)

    def test_a_failed_sample_counts_and_fails_the_set(self):
        result = summarize([sample(10, 20), {"status": "fail"}])
        self.assertEqual(result["status"], "fail")
        self.assertEqual((result["sample_count"], result["failed"]), (2, 1))
        # Its timings are not mixed into the passing distribution.
        self.assertEqual(result["stop_tap_to_revocation_observed_ms"]["max"], 10)

    def test_no_samples_is_not_a_pass(self):
        result = summarize([])
        self.assertEqual(result["status"], "fail")
        self.assertIsNone(result["stop_tap_to_receipt_observed_ms"])

    def test_a_run_that_was_never_judged_fails_the_command(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            judged, unjudged = temp / "judged", temp / "unjudged"
            judged.mkdir()
            unjudged.mkdir()
            (judged / "stop-result.json").write_text(
                json.dumps(sample(40, 200)), encoding="utf-8"
            )
            output = temp / "summary.json"
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("stop_samples.py")),
                    str(output),
                    str(judged),
                    str(unjudged),
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
            )
            self.assertNotEqual(result.returncode, 0)
            summary = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(summary["status"], "fail")
            self.assertEqual(
                (summary["sample_count"], summary["passed"], summary["failed"]),
                (2, 1, 1),
            )
            self.assertEqual(
                summary["stop_tap_to_revocation_observed_ms"],
                {"min": 40, "median": 40, "p90": 40, "max": 40},
            )


if __name__ == "__main__":
    unittest.main()
