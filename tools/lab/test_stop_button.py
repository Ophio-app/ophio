# SPDX-License-Identifier: Apache-2.0
"""The phone helper presses only its own task's Stop."""

import json
from pathlib import Path
import subprocess
import unittest


class StopButtonTest(unittest.TestCase):
    def test_only_the_named_task_and_its_shown_display_match(self):
        module = (Path(__file__).parent / "stop_button.mjs").resolve().as_uri()
        script = f"""
          import {{ isTaskStop }} from {json.dumps(module)};
          const t = 'Scripted Stop fence 1a2b3c4d';
          // The helper sends the function into the page as source.
          const sent = new Function('return ' + isTaskStop.toString())();
          console.log(JSON.stringify([
            isTaskStop('Stop · ' + t, undefined, t),
            isTaskStop('Stop · ' + t + ' · Desktop', ' · Desktop', t),
            sent('Stop · ' + t + ' · Desktop', ' · Desktop', t),
            isTaskStop('Stop · ' + t + ' · retry', undefined, t),
            isTaskStop('Stop · ' + t + ' · retry · Desktop', ' · Desktop', t),
            isTaskStop('Stop · ' + t + ' · Desktop', undefined, t),
            isTaskStop('Stop · ' + t, ' · Desktop', t),
            isTaskStop(null, undefined, t),
            isTaskStop('Force stop · ' + t, undefined, t),
            isTaskStop('Retry Stop · ' + t, undefined, t),
            isTaskStop('Stop · Scripted Stop fence', undefined, t)
          ]));
        """
        result = subprocess.run(
            ["node", "--input-type=module", "-e", script],
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        self.assertEqual(json.loads(result.stdout), [True] * 3 + [False] * 8)
