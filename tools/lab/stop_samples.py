# SPDX-License-Identifier: Apache-2.0
"""Summarize repeated phone Stop checks, each already judged by stop_result.py."""

import argparse
import json
import math
from pathlib import Path

TIMINGS = ("stop_tap_to_revocation_observed_ms", "stop_tap_to_receipt_observed_ms")


def rank(values, share):
    """Nearest-rank percentile: the smallest value at least `share` of samples reach."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(share * len(ordered)) - 1)]


def summarize(results):
    """Every sample counts; a failed one is reported, never dropped from the count."""
    passed = [result for result in results if result["status"] == "pass"]
    summary = {
        "status": "pass" if results and len(passed) == len(results) else "fail",
        "sample_count": len(results),
        "passed": len(passed),
        "failed": len(results) - len(passed),
        "method": "One stop_result.py judgement per run; timings are taken from passing runs only, nearest-rank percentiles.",
    }
    for name in TIMINGS:
        values = [result[name] for result in passed]
        summary[name] = (
            {
                "min": min(values),
                "median": rank(values, 0.5),
                "p90": rank(values, 0.9),
                "max": max(values),
            }
            if values
            else None
        )
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="where to write the summary JSON")
    parser.add_argument("samples", type=Path, nargs="+", help="sample folders")
    arguments = parser.parse_args()
    results = []
    for folder in arguments.samples:
        path = folder / "stop-result.json"
        # A run that stopped before judging is a failed sample, not a missing one.
        results.append(
            json.loads(path.read_text(encoding="utf-8"))
            if path.is_file()
            else {"status": "fail"}
        )
    summary = summarize(results)
    arguments.output.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    if summary["status"] != "pass":
        raise SystemExit("Not every Stop sample passed")


if __name__ == "__main__":
    main()
