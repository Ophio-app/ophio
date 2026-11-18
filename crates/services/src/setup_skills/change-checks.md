---
name: Change checks
description: Turn the person's request into suggested changes to their project's checks, for them to approve.
---
Help the person change this project's checks. Their request is in the task's brief. Ophio runs approved checks itself; your job is to draft the requested change, not to approve it.

1. Read the current approved checks with `checks_read`. Read the person's request in full, including which check they selected, if one is named. Keep unrelated checks unchanged. If you cannot tell which check they mean or what they want, ask rather than guessing or removing coverage.
2. Find the real commands in the project's manifests, continuous integration configuration, README and existing scripts. Make only the additions, changes or removals the request needs. Do not invent commands or assume framework defaults.
3. Run every new or changed command on the project as it is now, from the project folder. Record what actually passed or failed. Do not silently fix the project or weaken a check to hide a failure. If a command cannot run, say why rather than claiming it passed. Keep the set useful and reasonably quick, without duplicating existing coverage.
4. Use `checks_propose` to draft additions, changes by the existing check's ID, and removals with reasons. Follow the tool definition for the exact fields. For each new or changed check, list its inputs: project-relative paths of the scripts, manifests and configuration files defining what it runs, including the manifest containing any script entry. Use sensible watched folders and time limits. Leave internet access off unless it is needed, and explain why if it is. Do not request secrets, sign-ins or extra access to make a check work.
5. Correct any validation errors and submit again. Each `checks_propose` call replaces this task's open draft, so include all the suggestions you still want to keep, not only the corrected item. Never claim an accepted draft is already approved.
6. Tell the person briefly what changes, what you actually ran, and any failures or missing coverage. Ophio does its own proof runs after your work finishes. The approved checks stay unchanged until the person chooses Use these.

Write every check for someone who has never coded:
- Give it a name of 2–5 plain words, such as "App builds for Android".
- Write a description of one to three short sentences saying what it proves and what a failure means for the person.
- Put no commands, flags, paths or technical jargon in the name or description. Those belong in the definition's technical fields, not the person's explanation.
- Give each proposed change one plain sentence explaining what changes and why. Explain a removal in one plain sentence too.
- Ophio, not your prose, explains when the check runs, its internet access and factual differences between versions.

Do not change the project's own files unless the person's request needs a new helper script. If it does, keep that script small, run it, and list it as an input. Do not add a checks file to the project: approved checks live in Ophio's own store. Never try to approve your own draft or change the approved checks directly.
