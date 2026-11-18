# Changes

Each public release of Øphio is listed here, newest first. A release's version
matches the `vX.Y.Z` tag on the commit it was built from.

## 0.3.0 — chat across projects, checks after each task, split view

This release is for Android and Linux, with an experimental,
unsigned Windows 11 x86_64 ZIP; Windows is not a beta support promise and
paid signing is not required for that download. The final files have limited
lab checks, not complete physical-device qualification; see the scope below.
It has a new phone app: update the computer first, then the
paired phone from Settings › Versions ›
**Update now**. Most new features need both the new phone app and the new
computer; [docs/upgrading.md](docs/upgrading.md#updating-to-030) says what
works when only one end is updated, and what happens if you go back to 0.2.0.

### New

- **Install the computer app with one command.** There is one command for
  Linux or a Mac, and one for Windows. They download the right release,
  check its checksum and install it for your user account. See
  [Get started](README.md#get-started).
- **An experimental Mac download.** Macs with Apple silicon get their own
  archive, `ophio-0.3.0-aarch64-macos.tar.gz`. It is not a beta support
  promise: the final archive passed checksum, signature and clean version
  checks on macOS 27.2, not installation or full workflows. Screen watching
  and control have not been tested. Its programs have no Apple Developer ID
  signature and are not notarized by Apple,
  so read [the Mac install guide](packaging/macos/INSTALL.md) before
  downloading, especially if you use a browser. Screen and accessibility
  permissions have to be given again after each update.
- **Checks after every task.** Each project can have checks, written in plain
  English in Library › Checks, such as "the app still builds for Android".
  When a task finishes changing the project, Øphio runs the checks that apply
  itself and shows the results on the task and its receipt, so you don't have
  to take the agent's word that the work works. **Set up checks** and **Add a
  check** ask an agent to write them; agents can only suggest checks, and
  Øphio runs only the ones you approved. On Linux, checks run in a sandbox
  with no more access than the agent had, and reach the internet only if you
  allowed it for that check and the task had it too. While checks run, the task shows as
  checking, and its notification waits until they finish.
- **Split view on tablets and foldables.** On a tablet held sideways, or an
  unfolded foldable phone, **Split view** opens two copies of the app side by
  side. Each side has its own screen and can show a different computer, or
  All computers. On a foldable the divider sits on the crease, using the
  fold position Android reports. Earlier builds were checked on Android
  tablet and foldable emulators. The final app passed partial foldable checks;
  its tablet run stopped after emulator crashes. Neither was tested on physical
  hardware. A chat's draft stays in sync across both sides, and each
  side warns about its own unsaved edits. Open sheets keep their edits and
  scroll position when you fold or unfold the phone.
- **Build the experimental iPhone app yourself.** Developers with a Mac can
  follow the [iPhone build guide](packaging/ios/INSTALL.md). There is no App
  Store or TestFlight download yet. Only simulator runs have been tested,
  not signing or installation on a real iPhone.
- **Chat across all projects.** Chat lists the conversations from every
  project on the connected computer in one place.
- **Switch agent, model and effort between messages.** Pick another agent,
  model or effort for the next message without starting a new conversation.
  The new agent is given a summary of the recent conversation, not the other
  agent's whole memory. Øphio remembers which agent you prefer for each
  project and each kind of work, and keeps the model list short, with the
  full list one tap away.
- **Attach files to messages.** Send ordinary files, not only pictures, with
  a chat message, and download them again later.
- **Suggested work as cards.** When an agent in a chat suggests work, it
  shows as a card you can start with one tap, with the right task type and
  access. Ordered sets of tasks start in order, waiting for the step before;
  a set stops at a failed step. Work started from a chat reports back to that
  chat in one combined summary. **Turn into a task** carries a chat on as a
  task that can change files, and plain tasks can be started from the
  message box. Starting suggested work by itself is an optional setting with
  limits, and is off unless you turn it on.
- **Steer Claude Code while it works.** **Steer now** sends a message that
  Claude Code reads while it is still working, instead of after it finishes.
- **Replies survive interruptions.** Replies are saved while they stream, so
  a restart or a lost connection keeps what had arrived. After a reply is
  confirmed cut off, writing another message carries the conversation on.
  Unfinished actions are not silently repeated or reported as done.
- **Chats and tasks take turns.** A chat and a task can work in the same
  project: they take turns changing it, so a busy task doesn't stop a chat
  from answering.
- **Your instructions and skills in limited chats.** A chat kept to its
  project can now read your own agent instructions and the skills that suit
  its access, without being able to change them.
- **New versions of Øphio.** Once a day the computer asks GitHub whether a
  newer Øphio has been released, and tells your phone once per new version.
  Settings › Versions shows what's new and how to update. This check is on
  by default in the public download and can be turned off there; it sends
  nothing about you or your projects.
- **Agent updates you choose.** Claude Code and Codex updates are now
  **Update automatically**, **Tell me when there's an update** (the new
  default) or **Don't check**, with **Check now** and a one-off **Update
  now**. Øphio also tells you, quietly, when an update brings new models.
  See [docs/agent-updates.md](docs/agent-updates.md).

### Improved

- Setup explains Android's download and unknown-app warnings. The phone's
  Windows instructions point to the installed app, and Windows setup text
  no longer shows garbled characters.
- The phone's welcome and pairing screens no longer describe every computer
  download as a Linux archive.
- A task that is watching for something (Watching) says in plain words what
  it is waiting for, on the task and at the top of the chat.
- Notifications: a chat reply makes a sound unless that chat is on screen;
  work started from chat cards gives one combined notice; an interrupted
  reply gives one "write to carry on" notice.
- **Allow** still counts when you tap it after the agent stopped waiting: the
  agent is told it may go ahead, and if it asks for exactly the same thing
  again within half an hour, that is allowed once without a second card.
- Approval and question cards are shorter: short requests sit beside Allow
  and Reject, a request is declined in one tap, and a question card shows
  just the question.
- Library search groups matches by file.
- Review is folded down to the choices you made, chat steps show plain tool
  names and whether each step finished or was stopped, and finished tasks
  return you to the screen you opened them from.
- Typed text in a sheet is kept when another screen covers it, and a
  double tap sends a message only once.
- A phone and computer whose versions can't work together offer an update
  instead of retrying the connection.

### Fixed

- Running the Windows one-command installer again when that version is
  already installed now finishes cleanly without changing the installation.
- The agent's browser keeps tab titles and addresses up to date when a page
  finishes loading, even in the background, and gives screenshots up to 15
  seconds to finish on a busy computer.
- Opening a web address by voice gives a slow site the full time allowed
  for that action, instead of cutting it off early.
- Terminal controls stay at the top of the phone screen, including while
  typing, so **Detach**, **Attach again** and **Close terminal** stay within
  reach.
- On a Mac, uninstalling the background service also removes a finished
  update's background job. An update still in progress is left alone.
- On a Mac, the warning before deleting Øphio's data names the shared
  settings and data folder once, rather than twice.
- A failed Claude Code or Codex update could remove the copy tasks were
  using. A failed update now keeps the working copy and the previous one,
  including on the first try after a restart.
- Pull requests from a task use that task's own branch, not whatever branch
  the project folder happens to be on.
- On Linux, an app outside your home folder could be taken for one you
  installed yourself. Øphio now checks that it really is in your home folder.
- Shutting Øphio down lets a Claude Code reply finish cleanly instead of
  stopping it part way.
- Øphio's own background checks of Claude Code no longer run at the same
  time or get cut off part way, either of which could leave Claude Code
  signed out.
- `ophio update` retries when a just-copied program is briefly busy, instead
  of failing the update.
- Notification settings that name a kind this version doesn't know no longer
  stop the others from being read.
- Dependency security fixes: the bundled SQLite (RUSTSEC-2026-0459), rustix
  (RUSTSEC-2026-0486), the quinn-proto and quinn-udp network libraries
  (RUSTSEC-2026-0523, RUSTSEC-2026-0524 and RUSTSEC-2026-0538), and the zip
  library that the speech-recognition code brings in (RUSTSEC-2026-0545 to
  RUSTSEC-2026-0548), now at a version with its archive-extraction fixes. Øphio's speech code never
  extracted archives, so the zip problems could not be reached before
  either.

### Changed

- Agent updates default to **Tell me when there's an update**. A computer
  that updated automatically before keeps doing so.
- The public download checks GitHub for new Øphio releases once a day, as
  described above. Builds made from source with `OPHIO_PERSONAL_SIGNING=1`
  start with the check off.

### Known issues

- A pinch outside a picture can zoom the whole phone app. Pinch in on the
  chat or reopen the app to fix it. This also affects 0.2.0. It is fixed on
  the development line, but not in this 0.3.0 release.
- On Windows, a script that starts the background service and waits for its
  output can hang. Typing the commands yourself and the one-line installer
  are unaffected.
- When updating the phone from 0.2.0, the first try just after allowing
  installs from Øphio can fail. Tap **Try again**.

### Scope and known limits

- Windows 11 x86_64 has an experimental unsigned ZIP, not a beta support
  promise; read [Windows warnings and how to continue](packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
  for **Windows protected your PC** and Smart App Control before installing.
  The initial host-only candidate passed fresh installation and
  connected checks with a signed Android 0.2.0 emulator, including stand-in
  chat/task approval, a real terminal, transfers and desktop input/Stop.
  Native tests and packaged executable-sharing-error recovery also passed;
  a complete aggregate service-suite run did not finish within its time limit.
  Normal upgrade from an old flat ZIP installation is not qualified: inherited
  permissions are intentionally refused rather than weakened. A separately
  tested clean reinstall retained data without changing old program permissions,
  and corrected stable-launcher startup passed native checks. Real providers,
  matching 0.3.0 Android, physical hardware, Windows 10 and power-loss recovery
  remain unverified. Limited chats/tasks and resource-limited containers remain
  unavailable. See [Windows qualification](docs/qualification/windows-macos.md).
- The beta is for Android and Linux, as in 0.2.0. The Mac download is
  experimental; iPhone code is in the source but has no download
  and no support promise.
  On iPhone the app can only tell whether a foldable is folded, not where
  its crease is, and has only run in the simulator.
- With a 0.2.0 phone and a 0.3.0 computer, the phone can't show attached
  files, set up checks, start a suggested-work card as work linked to its
  chat, or show the three agent-update choices. With a 0.3.0 phone and a
  0.2.0 computer, the features that need the computer are hidden. Update both.
- Going back to 0.2.0 keeps your history, setup and Øphio's own agent
  copies, but 0.2.0 starts updating the agents automatically again, and
  your Tell me or Don't check choice is not kept when you come back to
  0.3.0. Read [going back to 0.2.0](docs/upgrading.md#going-back-to-020)
  first.
- The final Android and Linux files were tested on an Android 15 emulator
  and an Ubuntu 24.04.5 virtual machine with glibc 2.39. Installation, pairing,
  phone self-update, computer upgrade/rollback and Chat/Task use with either
  end on 0.2.0 passed using synthetic agents, not real providers. Permissions,
  attachment bytes, writer scheduling and interrupted-reply recovery also
  passed. Managed-agent lookup failures retained the saved programs; separate
  source tests covered deeper update failures. Reminder timing used synthetic
  saved metadata, not a real update feed.
- The final Windows ZIP passed limited installation and lifecycle checks in
  a Windows 11 24H2 virtual machine, including update and retained rollback
  from a synthetic managed baseline. The final Mac archive passed limited
  archive, signature, version and synthetic quarantine checks on macOS 27.2,
  not installation, service or update checks. Earlier-build results above do
  not qualify the final files.
- Physical phones, real provider sign-in/conversations, audible reply sounds,
  vibration, Do Not Disturb and battery-managed background delivery were not
  qualified. Full settings retention, sound eligibility, locked-screen taps,
  dismissal/reconnection and focus/selection checks remain incomplete.
  Tablet qualification stopped after emulator crashes; foldable checks were
  partial. See [Linux qualification](docs/qualification/linux.md#030-release-file-checks)
  and [Windows and Mac qualification](docs/qualification/windows-macos.md).

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
