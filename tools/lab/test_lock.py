# SPDX-License-Identifier: Apache-2.0
"""Reject contaminated and incomplete native-lock evidence."""

import json
from pathlib import Path
import subprocess
import unittest


class LockEvidenceTest(unittest.TestCase):
    def test_lock_observations_require_current_state_and_no_acknowledgements(self):
        module = (Path(__file__).parent / "lock_result.mjs").resolve().as_uri()
        script = f"""
          import {{ lockPassed }} from {json.dumps(module)};
          const good = {{lockConfirmed:true, sent:false, samples:Array.from({{length:4}},()=>({{
            acks:[], control:{{display:{{desktop_lock:'locked',input_state:'paused',lease:{{state:'revoked'}}}}}}
          }}))}};
          const evaluate = mutate => {{const value=structuredClone(good);mutate(value);return lockPassed(value)}};
          console.log(JSON.stringify([
            lockPassed(good),
            evaluate(v=>v.samples[0].acks.push({{outcome:'injected'}})),
            evaluate(v=>v.samples[0].control.display.desktop_lock='unlocked'),
            evaluate(v=>v.samples[0].control.display.lease.state='active'),
            evaluate(v=>v.samples[0].control={{}}),
            evaluate(v=>v.samples.pop()),
            evaluate(v=>v.lockConfirmed=false),
            evaluate(v=>v.sent=true)
          ]));
        """
        result = subprocess.run(
            ["node", "--input-type=module", "-e", script],
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        self.assertEqual(json.loads(result.stdout), [True] + [False] * 7)
