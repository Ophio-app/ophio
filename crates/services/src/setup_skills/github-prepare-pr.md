---
name: GitHub · prepare a pull request
description: Take this task's change to a draft pull request that says what it does and proves it, one concern at a time.
---
Take this task's work to a pull request the person would be glad to merge.

1. Find the repository's own rules first: `AGENTS.md`, `CONTRIBUTING.md` and the pull request template in `.github/`. They win over this list where they differ.
2. Keep to one concern. If describing the change needs the word "also", split it, and say what you left for another pull request.
3. Make the branch as this task's GitHub instructions say, commit, push, and open a draft pull request with github_pr as soon as the first commit is pushed. A draft is a checkpoint, not a handoff: keep working.
4. Title it as a conventional commit in plain words, like `fix(web): new threads no longer spike CPU`. In the description, say the problem in a sentence or two, then the change.
5. Prove each claim. Run the narrowest check that shows it, on the base and on your branch where that tells something, and write the exact commands and what they showed under Verification. Say what you didn't check beside the claim it affects.
6. For a visible change, take a screenshot before and after; for motion, a short recording. Commit them only to the `ophio/evidence` branch, never to this branch or the default one, and link them from the description.
7. After every push, call github_pr again so the description says what the branch does now.
8. When it is done, say so and stop. Being ready to merge is not permission to merge: merge only if this task's GitHub instructions say you may, and only with github_merge.
