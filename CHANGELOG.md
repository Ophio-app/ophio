# Changes

Each public release of Øphio is listed here, newest first. A release's version
matches the `vX.Y.Z` tag on the commit it was built from.

## 0.2.0 — several computers, calmer notifications, clearer approvals

The first release with a new phone app since 0.1.0. Update the computer
first: it then offers each paired phone the new app. This is the phone app's
first update of itself, so Android asks a little more than usual: allow
Øphio to install apps when Android asks, tap Try again if the app says the
update was cancelled, and confirm the update if Android asks.
[docs/upgrading.md](docs/upgrading.md#updating-to-020) walks through it.

### New

- **Several computers from one phone.** The status pill at the top of the
  app is now a menu that shows the computer you are on and lists every
  paired computer with its own status. With two or more paired, pick All
  computers to see Tasks, Needs you and To-Do from every computer at once,
  each item marked with its computer. Answering, Allow, ticking an item or
  Stop goes to the right computer without leaving the screen.
- **Computer names are shared.** Rename a computer in Settings and the name
  is kept on the computer, so every phone paired with it shows the same
  name. A phone without the Manage access permission, or with an older
  computer, keeps the name on that phone only, as before.
- **Choose which notifications make a sound.** Settings > Notifications lists
  each kind of notification in plain words and lets you set it to Alert,
  Silent or Off. By default only things that wait for you alert: questions,
  approvals, a signed-out agent, a task that needs a decision or stopped with
  a problem, a pull request that needs you, and a question about one of your
  problem reports. Each project can be muted or set kind by kind, and the
  bell on a task or chat ("Tell me when it’s done") makes that one alert
  whatever the settings say.
- **Sign an agent back in from the phone.** When Claude Code or Codex is
  signed out on the computer, the phone says so and offers Sign in again.
  You finish signing in in the phone's browser: Claude's sign-in page gives
  you a code to paste back into the app, and Codex shows a one-time code (this needs
  device-code sign-in turned on in ChatGPT's security settings; the app
  links there). Finishing on the computer's own screen still works.
- **Approvals you can read at a glance.** An approval card starts with the
  agent's one-line description, then a line Øphio works out from the exact
  request, such as "Changes files in src, makes a git commit, connects to
  github.com". Deleting, pushing, sudo, installing and downloaded scripts are
  highlighted, and the card says plainly when it can't tell. Allow and
  Decline sit right under that line and stay in view on short screens, and
  a card never shows a button that can't work.
- **Asking for more access in one step.** An agent that needs more than its
  chat or task allows asks with a single card. In a chat you allow it. In a
  task that may only plan or answer questions (Plan or Ask), you switch the
  task to Work, which lets it change files, and the same session carries
  on. A running Plan or Ask task can also be switched to Work, or a chat
  turned into a task, with one tap.
- **GitHub tasks can merge by themselves or wait for you.** Each task that
  works on an issue or pull request has its own choice, starting from the
  project's setting. Merging by itself is offered only where your account
  administers the repository, and a stranger's issue always waits for you.
- **A clear review verdict.** A change that touches something sensitive, or
  needs your approval, gets up to five rounds of review instead of three,
  and when nothing is left to fix Øphio says so under the pull request and
  on the phone, naming the commit it checked. Each fix must come with proof,
  such as a test that failed before it.
- **Questions written for a phone.** Agents are told to ask in plain words,
  naming the choices and the one they would pick, in a few short sentences.

### Improved

- When the phone's own Tailscale is off, the app says "Tailscale off" and how
  to fix it, instead of showing the computer as offline.
- A lost connection is retried calmly: Retry always works, coming back to
  the app retries at once, and otherwise the phone waits longer between
  attempts instead of retrying in a tight loop.
- Plainer words in several places: Codex steps read "Command", "File change"
  and "Web search"; "Your control ended" replaces "Agent paused" when your
  own control ends; and a desktop that can't be watched says why.
- Tailscale is found in its usual install folders even when Øphio runs as a
  background service that doesn't know your usual search path.
- A desktop that can't be watched is logged once, not several times a second.
- Telegram and Discord approval messages only offer Allow where the computer
  can accept it, and a notification you already handled on a phone is not
  posted to a chat late or twice.

### Fixed

- In a chat allowed to change files and run commands ("Change & run"), Allow
  on a command or file change failed with an error. It works now.
- A swiped notification no longer comes back after an app update or a phone
  restart, and swiping one no longer brings older ones back in its place.
- Piping an `ophio` command into another that stops reading early, such as
  `ophio update | head`, is no longer recorded as a crash and no longer stops
  an update part way.
- Working out an approval's description can no longer crash on unusual
  commands, and it now notices commands hidden behind encodings or
  downloads.
- The first approval card fits on screen at large text sizes.
- Claude Code's "commands changed" message no longer shows as an update
  Øphio can't read.
- The library that formats text in notes and messages is updated to fix two
  published problems that could make it very slow on crafted text. Øphio
  doesn't use the feature they affect.
- The phone's update screen no longer promises that Android installs updates
  without asking. Before an update starts, the app now opens Android's
  setting to allow Øphio to install apps and carries on when you come back,
  instead of Android cancelling the update part way through.

### Changed

- Notification settings from 0.1.x carry over to the new kinds the first
  time 0.2.0 runs, as [docs/notifications.md](docs/notifications.md)
  describes.
- Finished tasks no longer alert by default. Turn them on in Settings >
  Notifications, or ring the bell on a task to hear when that one is done.
- Going back to 0.1.1 with `ophio update --rollback` works and keeps your
  history, but notification choices don't go back with it: 0.1.1 alerts
  about every topic again until you turn them off there. In a project with
  Merge once checks pass on, 0.1.1 also merges pull requests you chose to
  merge yourself, so merge or close those first.
  [docs/upgrading.md](docs/upgrading.md#going-back-to-011) has the details.

### Desktop control and voice

Taking control of a Linux desktop from the phone was reworked.

- The pointer reaches the whole screen when you zoom in. Touches are now
  matched to the part of the desktop you can see.
- Shortcuts arrive as you press them. On the Hyprland desktop, Super+Enter
  used to arrive as Super+Escape. The computer now types with its real
  keyboard layout, so shortcuts that desktops bind by key work, and special
  characters reach Chrome, Chromium and apps built on Electron, such as
  VS Code.
- A live keyboard over the desktop: type with the phone's own keyboard, with
  keys for Esc, Tab, Ctrl, Alt, Super, Shift, the arrows and Paste.
- Voice control works on any desktop, not only where apps expose controls
  it can read. Say "open the browser", "open a terminal", "go to example.com",
  "press control shift T", "type hello" or "scroll down". Apps are matched
  against the computer's installed apps and, on systems that use systemd,
  open in their own session, so restarting Øphio doesn't close them.
  Shortcuts that could close or delete something are confirmed first. Voice
  control needs your own TypeSafe Jev access, as before; see
  [docs/voice-control.md](docs/voice-control.md).
- A request that needs several steps, such as "open Firefox and search for
  cats", can be handed with one tap to a computer-use agent, an agent that
  works the desktop by looking at the screen and using the pointer and
  keyboard. The phone shows
  the exact words the agent will get, and nothing starts until you tap.
- Computer-use agents can open installed apps by name while they hold
  control.
- If an app that was opened by voice or by an agent hangs and the computer
  can't confirm it stopped, Øphio says that an app may still open on that
  display and doesn't grant new control there until it can confirm it.

Windows, Mac and iPhone work continues in the source, but this release has
downloads for Android and Linux only. Known limits are in
[SUPPORT.md](SUPPORT.md).

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
