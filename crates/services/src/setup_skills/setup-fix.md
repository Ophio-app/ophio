---
name: Fix what the readiness check found
description: Work through what `ophio doctor` says needs attention, one problem at a time.
---
Help the person fix what Øphio's readiness check found.

1. Run `ophio doctor`. For more detail, `ophio doctor --json` prints every check.
2. Take the problems one at a time, most important first: Øphio not running, then agents missing or signed out, then the network, then the rest. For each, say what is wrong in a sentence and what you propose to do.
3. Fix what Øphio's own commands fix. For an agent that isn't signed in, the person signs in themselves, in their own terminal (`claude` or `codex`), and you wait. For anything that needs a program installed, ask first; for anything that needs their password, give them the command.
4. Run `ophio doctor` again after each fix, and stop when it is clean or what is left needs the person.
5. Say what you fixed and what is left.
