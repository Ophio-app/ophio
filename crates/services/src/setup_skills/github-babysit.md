---
name: GitHub · babysit a pull request
description: Keep this task's pull request green and answered, until its checks pass on the latest commit and nothing is left open.
---
Look after this task's pull request until it is green.

1. Read what is new since your last push: `gh pr checks`, `gh pr view --comments`, and the review threads.
2. Treat every bot finding as a lead, not a verdict. Check it against the source. Fix the real ones, and answer a false positive in its thread with github_comment, giving the reason.
3. Once a bot's thread is fixed or answered, resolve it with github_comment's resolve. A person's thread is theirs to resolve: answer it and leave it open.
4. For a failing check, read its log, reproduce it locally where you can, and fix the cause rather than the symptom. Never skip, disable or loosen a check to make it pass.
5. Bring the branch up to date with `git merge origin/<base>`, not a rebase, and never force-push.
6. After each push, call github_pr so the description stays true.
7. Say nothing when nothing is new. Stop when every check passes on the latest commit and no review thread is open, and say so.
