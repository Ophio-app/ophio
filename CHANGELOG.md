# Changes

Each public release of Øphio is listed here, newest first. A release's version
matches the `vX.Y.Z` tag on the commit it was built from.

## 0.1.1 — fixes for a fresh install

Fixes for what a newcomer met installing the beta on a fresh Ubuntu Desktop.
Only the computer program changed: the phone app is the same as in 0.1.0,
and the release carries it unchanged as `ophio-0.1.0-arm64.apk`.

- Setup, sign-in, tasks and phone terminals now look for Claude Code and
  Codex in the same places: Øphio's PATH, then `~/.local/bin`,
  `~/.cargo/bin`, `~/.claude/local`, `~/.npm-global/bin` and nvm's default
  Node.js. Signing in straight after setup installs an agent no longer fails
  with "No such file or directory", and agents installed with npm or nvm are
  found without logging out and in.
- Setup no longer says "Installed" when an installer did nothing. Without
  curl, Claude Code's step says to run `sudo apt install curl` first; a
  failed download fails the step; and setup shows why an installer failed,
  such as npm's permission error with Ubuntu's own Node.js, on its own
  screen.
- A message to an agent that isn't signed in says which agent and how to sign
  in, in plain sentences.
- Codex tasks no longer show notes about updates Øphio didn't recognise, or
  the warning about a Codex feature Øphio turns on itself.
- Setup's screen is no longer garbled by systemd's "Created symlink"
  messages, the final check gives each fix as a sentence of its own, setup
  says GNOME's Remote Desktop dialog is coming and what "Allow Remote
  Interaction" is for, and the pairing screen keeps the fingerprint in view in
  a small terminal.
- The README and guides describe the published release, give the exact
  commands to unpack it, and cover curl, npm's permission error, finding a
  newly installed agent, and which installs chats kept to a project can use.

## 0.1.0 — first public beta

The first public release, for Android phones and Linux computers.

- Pair a phone with Øphio on a computer by scanning a code and confirming the
  same six digits at the computer.
- Chat with Claude Code or Codex on the computer, hand them tasks, and answer
  their approvals and questions from the phone.
- Watch the desktop live, take over input, and stop an agent at any time.
- Read and edit the Markdown files that shape your agents.
- Keep the steps only you can take in a to-do list next to the agent work
  waiting on them.
- Install with one script on Linux; `ophio setup` walks through the rest, and
  the computer offers the phone app and its updates.

Known limits are in [SUPPORT.md](SUPPORT.md).
