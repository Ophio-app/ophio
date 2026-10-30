# Notifications

Ophio alerts you on the phone when an agent needs you or a task ends. Which
alerts reach you is chosen on the computer, for every project and for each one
(see [What alerts you](#what-alerts-you)). There are two kinds, each an Android
notification category you can turn off under Settings › Notifications › On
this phone, or in Android's own settings:

- **Needs you** (pops up, with sound):
  - "Question from Claude Code" (or the task's agent): the agent asked something;
  - "Approval waiting";
  - "Task needs you": waiting for you, a conflict, or a result to check.
- **Finished tasks** (sound, no pop-up):
  - "Task finished";
  - "Task stopped with a problem".
  Several from one computer stack under its name. A task someone stopped
  raises nothing.

Each alert names the computer and the project. **Notification detail** adds
the task's title; titles can hold private details, and they show on the lock
screen. An agent's question shows in full whichever detail you choose, so you
can read and answer it; like a title, it shows on the lock screen unless
Android hides notification content there. With app lock on, an alert shows
only "Open the app to review."
Tapping an alert opens that question, approval or result. An alert goes away
when it is answered anywhere: on this phone, another device, in Telegram or
Discord, or at the computer. Nothing else raises a notification: no streamed
text, no progress.

## What alerts you

Settings › Notifications chooses which alerts reach you. The computer keeps
the choices, so they hold on every phone paired with it, for the app's own
alerts and the watcher's alike. A device with leave to start tasks, or an
admin, may change them; any paired device sees them.

Each alert has a topic:

| Topic | Alerts when |
|---|---|
| Questions | An agent asks you something. |
| Approvals | An agent waits for your leave to do something. |
| Tasks waiting for you | A task waits for you to go ahead, settle a conflict, or say how it ended. |
| Finished tasks | A task finished its work. |
| Tasks that stopped with a problem | A task couldn't finish. |
| Pull requests ready to merge | Checks passed and nothing blocks it. |
| Pull requests that need you | A check failed, a conflict, or a review asks for changes. |
| Merged pull requests | A pull request merged. |
| Issues | Issues found and filed, closed as duplicates, or not reproduced. |
| Slow or stalled work | A step is taking long, or work stopped for showing no progress. |

- **What alerts you** is the default for every project: each topic on or off.
  All are on until you turn one off.
- **One notification per project** (off by default): once a project has a
  second alert, its alerts fold into one note, "tidewater-api has 3
  notifications", that says how many need you and how many are to look at.
  It alerts again, with sound, only for something that needs you (a question,
  an approval, a task waiting for you, a pull request that needs you);
  anything else changes its count quietly. Answered alerts leave the count,
  and the note goes with the last. A note the background watcher raised and
  you swiped away counts no more; the next alert starts again.
- **Projects** lists each project, saying whether it follows the default.
  Its page, also opened from the project's sheet in Tasks, has:
  - **Mute this project**: no alerts from it at all;
  - **One notification for this project**: Default, One or Separate;
  - each topic: Default, On or Off, over the default for every project.
  **Reset to the defaults** clears the project's own choices.

Tapping a project's note opens that project's **Needs you**: what waits for
you, and under **To look at**, results nobody has opened yet.

Each open task shows in one place on Tasks: under **Needs you** while it waits
for you, otherwise under **Running** or **Next up**. What waits for you
leaves Needs you by itself once the task has moved on, however it moved:
archived, started again, put in Next up, its pull request merged or closed,
its question answered or expired, or the Next up it waited on resumed.

A finished task is never under Needs you: it is under **Finished**, in
**Unread** until you open it and in **Read** after, each with its own
**Archive all**. Anything it left waiting for you, such as a pull request
held for you, is part of its result and counts as unread. An approval or
question still waits under Needs you until it's decided.

An alert turned off counts as raised and is never shown, so turning its topic
back on raises nothing it held back. With **Alerts when the app is closed**
on, turning a topic off also clears the alerts of that topic already showing.

These choices cover the phone's notifications only, not messages in Telegram
or Discord chats.

### What alerts once

Work Ophio carries on by itself alerts when it is through, not at every step.
Each step is still listed (under Finished, in Needs you's counts and in the
project's note); only its alert is held. Whatever needs you still alerts at
once: a question, an approval, a task that stopped with a problem, a pull
request held for you.

- **Run all**: a fix finishing, or its pull request ready or merged, raises
  no alert of its own. Once the run has ended and every fix is through
  (merged, waiting for you, failed or stopped), one alert says how it went,
  such as "Run all done: 2 merged, 1 needs you", listing the issues under
  each. Pull requests still being seen through 12 hours after the run ended
  are counted as still going ("Run all so far: …"), and what comes of them
  later alerts as usual. A run you stopped before anything came of it
  raises nothing.
- **The line of pull requests in Through merge**: a batch merged together
  alerts once, as the last merges: "PRs #7 and #8 merged". If the line ends
  part way, the alert says what merged. A pull request GitHub keeps refusing
  is handed to you as ready, saying what merged before it.
- **Merge together**: one alert when it ends: "PRs #7 and #8 merged", or
  "Merge together: 1 merged, 1 left out", which waits for you and says why
  each was left out. It alerts on a task in Ophio that has one of the pull
  requests; with none, it raises nothing. Stopping it raises nothing.
- **Ophio's own turns on a pull request**: when Ophio wakes an agent that
  asked to be woken, or hands it news from GitHub such as a failing check or
  a review, that turn ends without an alert while Ophio still sees the pull
  request through; what comes of the pull request alerts instead. If you
  sent a message into that turn, or it ends needing you or with something
  to look at, it alerts as usual. News waits while a writer or its helpers
  hold the project, without repeatedly trying to resume the agent. Uncharged
  news is refreshed before the next round; a round already saved for delivery
  keeps its original news across a restart.
- **Watching**: a turn that ends asking Ophio to wake it raises nothing; the
  task shows as watching. If its project is busy, its helpers are still
  finishing, or its next turn is already in Next up, the wake stays saved
  rather than repeatedly trying to start another turn. Only the latest wake
  for each task is kept, including across a computer restart or a wait longer
  than a day. It runs once the task can safely start again; an uncertain writer
  must be settled first. If you resume the task yourself, the wake waits until
  your turn finishes. Stop watching or archive the task to cancel it.
  A question it went on without was raised when it was asked, and stays open.
- **Next up's go-ahead**: you are asked once for the item waiting at the
  front. Put away while the project is busy or Next up is paused, it comes
  back without another alert. Taken out and put back, or another put in
  front of it, it asks again.

In Telegram and Discord chats, a go-ahead asked again isn't posted again
either. Messages there for each finished task, if you turned them on, still
come for each task.

## Answering from the notification

A question's notification shows the question itself, and takes your answer:

- One or two short choices show as buttons, from Android 12. Android asks you
  to unlock the phone before a button sends.
- **Answer** opens a reply box for your own words. When there are more than
  two choices, or on Android 10 and 11, the choices are offered there instead.
- While it sends, the notification says "Sending your answer…". Once the
  computer takes the answer, the alert goes away, as one answered anywhere
  does. When it couldn't be sent, the alert comes back with "Not sent:" and
  why, such as the computer being out of reach or the question being answered
  or changed elsewhere, so you can answer again.
- The phone sends the answer as the app would: on a short connection of its
  own, signed in with its device key, for the version of the question it
  showed. The computer refuses an answer to a question that changed since,
  and applies the same checks as for the app.
- Some questions are answered only in the app, and their alert just opens it:
  several questions at once, and a secret one. A question that lets you pick
  several, or whose choices are many or long, shows no choices; answer it in
  words.
- With app lock on, alerts have no answer actions.
- Only the background watcher's alerts take answers. While **Alerts when the
  app is closed** is off, the app's own alerts open the question instead.

## Alerts when the app is closed

One switch, **Alerts when the app is closed**, covers every paired computer.
The phone asks once, right after pairing (or after the update that added it):
"Get alerts when an agent asks something or finishes?", with Turn on chosen.
Turning it on walks through only the Android steps still needed, each
explained first:

1. Android's notification permission.
2. Battery use set to Unrestricted. Android lets Ophio keep waiting for alerts
   once the app leaves the screen only with it; without it, alerts arrive only
   while the app is open.

Closing the question without choosing asks again the next time the app opens.
Off means alerts arrive only while the app is open.

Ophio keeps nothing in the status bar or the shade while it waits: only the
alerts themselves show.

### How it works

A small watcher in the app's Rust core keeps its own connection to each
paired computer, separate from the app's screen. A plain Android service runs
it, with no notice of its own, so swiping the app away or Android reclaiming the
screen doesn't stop it. Android keeps such a service running in the background
only for an app whose battery use is Unrestricted; for any other it stops the
service about a minute after the app leaves the screen.

- The watcher signs in with the phone's own device key at `/v1/alerts`. It
  receives alerts only: the kind, project, task title and time, and for a
  question the agent asked you, the question and its choices. Never other task
  text, terminal output or the screen. A device sees an alert only for an item
  it may see in the app, and a question only if it may answer approvals.
- Every connection starts with the computer's list of open alerts, oldest
  first. The phone remembers which it has raised, so alerts that came up while
  it was offline arrive once, in order, and ones already answered never show.
  Alerts asked after you turned alerts on are raised even if the computer was
  out of reach until now. A full list holds the newest 200. The phone names
  the alerts it still shows or keeps to show, and the computer's list says
  which of them were answered while the phone was away: they are cleared
  before anything kept rises. Alerts left out are listed as room frees. A
  phone that falls too far behind is let go, and reconnects to do the same.
- The phone records an alert only once Android has shown it. One Android
  refused is raised again on the next connection.
- The shade holds at most 24 alerts at once, the app's and the watcher's
  together. More wait behind one "N more alerts" note and rise as you clear
  the shown ones, with the detail chosen by then. Tapping the note opens the
  app.
- The computer pings every four minutes and the phone only answers, so the
  phone sets no timers of its own. Ten minutes of silence, or a network change,
  reconnects. After a failure it waits 5 s, 15 s, 1 min, 5 min, then every
  15 min; opening the app tries at once.
- While the app is on screen, unlocked and showing a computer it is connected
  to, the app shows that computer's alerts itself and tells the watcher which
  it showed. Another computer the app stays connected to, as for a hand-off,
  is left to the watcher.
  An alert the unlocked app hadn't shown yet when it left the screen, or was
  locked, is raised by the watcher about two seconds later. While the app is
  locked the watcher raises them all.
- An alert answered anywhere is cleared from the shade, by the watcher or by
  the open app. One answered before the app or the watcher posts it, as one
  waiting for room, is not posted. Turning background alerts off clears the
  ones the watcher raised, and a watcher switched off and on again leaves
  everything to the new one, which raises what still waits only once each
  computer says what was answered meanwhile. If the computer stops accepting this phone, its alerts are cleared
  too, so a task title doesn't stay on the lock screen.

With Unrestricted battery use it comes back on its own:

- after the app is swiped away. The app closes its own process, and Android
  restarts the service in a new one;
- after the phone restarts, and after the app is updated. Both clear Android's
  shade, so alerts still waiting are raised again;
- every 15 minutes a check restarts it if Android stopped it.

Without it, opening the app starts it again.

Force-stopping the app in Android's settings stops it until the app is opened.

### Status in Settings

Settings › Notifications shows, for the computer in use:

| Status | Meaning |
|---|---|
| Ready for alerts from *computer* | Connected. |
| Connecting to *computer*… | Starting up. |
| Can't reach *computer* right now | Alerts that came up meanwhile arrive once it's back. |
| Update Øphio on *computer* | Its Ophio predates background alerts. Alerts arrive only while the app is open. |
| *computer* no longer accepts this phone | Pair again. |
| Android stopped alerts | The service isn't running; **Start again**. |

When battery use isn't Unrestricted it warns that alerts stop when the app
closes, with **Allow** to change it. It also opens Android's settings for each
category.

## Checks

The emulator suite runs the closed-app cases against a real computer:
`background` (swipe away, open from the alert, quiet on screen, answered at
the computer, no notice of its own, off stops the service) and
`closed-lifecycle` (restart, update, Doze, battery use limited, airplane
mode). Results are in `client/src-tauri/E2E.md`. The Galaxy and the tablet are
checked by hand before release, including a day of normal use with battery use
measured.
