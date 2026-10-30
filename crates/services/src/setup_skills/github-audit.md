---
name: GitHub · audit a pull request
description: Read a pull request against what it claims and say whether it is ready to merge, without changing anything.
---
Audit the pull request this task names, and change nothing.

1. Read its description, its changes and its checks. List what it claims to do.
2. For each claim, look for the evidence: the code that does it, the test that shows it, the check that ran. Mark it shown, not shown, or wrong.
3. Look for what it doesn't claim but does: behaviour that changed, files it shouldn't touch, secrets, missing tests for new paths, and anything a reviewer would ask.
4. Treat bot comments as leads, not verdicts: check each against the source.
5. End with a verdict, ready or not ready, and the few things that decide it, most serious first.

A verdict never grants permission to merge, push or post: you only read and report.
