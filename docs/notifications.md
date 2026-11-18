# Notifications

Ophio notifies you on the phone when something needs you or a chat reply
arrives. By default, what waits on you makes a sound: an agent's question or
approval, an agent that isn't signed in, a task that needs a decision or
stopped with a problem, a pull request stuck on you. Chat replies also make a
sound unless that exact conversation is visible in the unlocked app. Other
things worth a look show silently in the shade. Routine progress, such as a
task finishing, waits in the app unless you ask to hear about it.

Each notification names the computer and the project. **Notification detail**
adds the task's title; titles can hold private details, and they show on the
lock screen. An agent's question shows in full whichever detail you choose,
so you can read and answer it; like a title, it shows on the lock screen
unless Android hides notification content there. With app lock on, a
notification shows only "Open the app to review." Tool requests use plain
names in notifications and linked chats (for example, "Search files" rather
than "Use Grep"). The original tool name remains in the stored request for permission matching;
notification wording does not change what the agent may do.

Tapping a notification opens that question, approval or result. A
notification goes away when it is answered anywhere: on this phone, another
device, in Telegram or Discord, or at the computer. Nothing else raises a
notification: no streamed text, no progress.

## What notifies you

Settings › Notifications lists every type of notification in plain English,
each with a line saying what it is for and a choice of **Alert** (sound and a
pop-up), **Silent** (in the shade only) or **Off** (in the app only).
These choices come before delivery setup. A warning above them stays visible
when permission or background delivery is limited; **Delivery settings** opens
**Background delivery and phone settings** below the choices. That section
starts collapsed, but its status keeps checking while closed. The layout does
not change any notification choices or defaults.

The available types are:

| Type | When | Recommended |
|---|---|---|
| Questions | An agent asked you something and is waiting for your answer. | Alert |
| Approvals | An agent wants your OK before it does something. | Alert |
| Agent signed out | An agent can't work because it isn't signed in on a computer. Tap it to sign in again from the phone; see [Accounts](accounts.md#when-an-agent-isnt-signed-in). | Alert |
| Tasks that need a decision | A conflict, an end Ophio can't confirm, or a screen to watch. | Alert |
| Tasks that stopped with a problem | A task or a review couldn't finish, or work stopped for showing no progress. | Alert |
| Pull requests that need you | Checks fail, a conflict, a review asks for changes, or some were left out of a merge. | Alert |
| Questions about your reports | A problem report you sent has a question for you. | Alert |
| Next up go-aheads | A task in Next up is waiting for your go-ahead to start. | Silent |
| Chat replies | An agent replied in a chat you aren't viewing. | Alert |
| Pull requests ready to merge | Checks passed and it's waiting for you to merge it. | Silent |
| Run all results | What a Run all came to, once it's through. | Silent |
| Issues that need your call | An issue couldn't be reproduced, or looks fixed already. | Silent |
| Problem report updates | A report you sent moved on, or someone sent you one. | Silent |
| Øphio updates | A computer has a newer Øphio for this phone. | Silent |
| New Øphio versions | A newer Øphio is out for one of your computers. Tap it to open Settings › Versions, which says what's new and how to update that computer; see [Upgrading](upgrading.md#finding-out-about-new-versions). Listed only while the computer in use checks for new versions. | Silent |
| Agent updates and new models | An agent has an update, a release announces new models, or an update adds models to the computer’s actual model list. Tap it to open Settings › Agent apps; an announcement does not guarantee your account can use a model. Available only on computers with [agent update policies](agent-updates.md). | Silent |
| Finished tasks | A task finished. | Off |
| Merged pull requests | A pull request merged. | Off |
| Issues filed | Issues were found and filed, or closed as duplicates. | Off |
| Slow steps | A step is taking longer than usual. | Off |

The recommendations make a sound when something waits on you or an agent
replies in a chat you aren't viewing. Your notification choices, project
settings and the phone's sound settings still apply. **Use the recommended
choices** puts the type choices back to their recommendations.

- **The choices belong to this phone.** They hold for every computer it is
  paired with, and a tablet keeps its own, so it can be quieter than the
  phone. They are kept in Android's own notification categories, one for
  each type under the same name, so Android's settings for Øphio list the
  same types. A change in the app changes Android's category at once; a
  change in Android's settings shows in the app when you come back to it.
  Both survive updates of the app.
- **Off** shows nothing, and the item waits in the app as before. Turning a
  type off clears its notifications already showing. Turning it on later
  raises nothing it held back.
- **Silent** shows in the shade with no sound, vibration or pop-up.
- A type from a newer computer that this phone doesn't know yet shows
  silently until the app is updated.
- Switching off one of the categories, or one of their headings, in
  Android's settings counts as Off.

### Each project's own choices

Settings › Notifications › Projects lists each project of the computer in
use, saying whether it follows your choices. Its page, also opened from the
project's sheet in Tasks, has:

- **Mute this project**: nothing from it notifies at all;
- for each type: **Default** (your choice on each phone), **Alert**, **Silent**
  or **Off**. Øphio updates, New Øphio versions and Agent updates belong to no project, so
  they are chosen only on each phone;
- **One notification for this project**: Default, One or Separate;
- **Reset to the defaults**.

The computer keeps a project's choices, so they hold on every phone and
tablet paired with it. A device with leave to start tasks, or an admin, may
change them. A project's choice wins over the phone's. Where a project, or a
task you asked about, makes a type louder than your phone's choice for it,
the notification shows under Android's "Projects and tasks you follow"
category.

### Tell me when it's done

The bell in a task's or a chat's header, **Tell me when it's done**, makes
its finish, its failure or, in a chat, the agent's reply alert on every
phone, whatever your choice for those types, even in a muted project: asking
about one task is the more particular choice. The computer keeps it with the
task. Starting a task asks once whether to turn it on. A task with its bell
on alerts for itself, without waiting for a chat's summary or a Run all.

### Grouping and bursts

A project's notifications from one computer stack together in the shade
under "project · computer", so a busy project takes one place. After one
notification from a computer makes a sound, the others from it in the next
minute show silently: a burst makes one sound, not one each. The shade holds
24 at most; when it is full, one that alerts takes the place of the oldest
showing silently, which waits in the app.

**One notification per project** (off by default) goes further: once a
project has a second notification, they fold into one note,
"tidewater-api has 3 notifications", that says how many need you and how
many are to look at. Tapping it opens that project's **Needs you**: what
waits for you, and under **To look at**, results nobody has opened yet.

### Stays gone

A notification is raised once for each thing that happened, and never again
for it.

- **Swiping it away, clearing the shade, tapping it or answering it** handles
  it. So does **seeing its item in the app**: opening its task, result or
  question counts at once. A row in the list where it lives (Needs you, a
  project's Needs you, Finished's Unread, or All) counts once it has been at
  least half on screen for two seconds, outside a closed group, while the
  app is in front and unlocked and you touched it in the last minute. A
  tablet left open on Tasks doesn't silence your phone.
- Only those count. Android also removes notifications for its own reasons:
  a force stop, a manufacturer's app sleeping, a category replaced. Those
  aren't handled and aren't told to your other devices; a notification
  removed that way, still open and still switched on, goes back silently
  about ten seconds later, unless the app cleared it because it was answered
  or seen. A snoozed one stays snoozed until its time, though it then comes
  back silently if it was put back meanwhile.
- A handled notification never comes back: not after a reconnect, a restart
  of the app, a restart of the phone, an update of the app or a change of
  computer.
- **Your other devices** clear it too: the phone tells the computer, which
  tells every other phone or tablet paired with it. Handling a notification
  doesn't deal with the item itself, which still waits in Needs you or
  Unread on every device.
- A restart of the phone or an update of the app clears Android's shade. Once
  the computer is reached, the notifications that were still showing just
  before, never swiped, opened or seen, are put back silently, if they are
  still open; nothing else comes back.
- A newer Øphio is told once for each computer that checks for it. Its
  notice clears once that computer runs the version, and the same version
  never notifies again; a later one does.
- Agent-update notices follow the same clearing rules; see [Agent updates](agent-updates.md) for their 14-day waiting period, 30-day reminder limit and when the computer clears them.
- Something new about the same task, such as another question, notifies as
  usual. New means: a new question or approval; a new lapse of an agent's
  sign-in; a new decision or failure, on a new run; a pull request handed
  back for a new reason; another task at the front of Next up, or the same
  one taken out and put back; a new reply; a report whose status changed. An
  answer that couldn't reach the agent asks again once, saying so.

A choice that hides a notification already showing clears it from this
device only, without handling it: muting its project, turning its type off
there, or turning its type off on this phone.

The update that brought this in counted everything open at the time as
already raised, so none of it came back. Choices made before it carried
over, where they were quieter than the new recommendations: the old
"Needs you", "Finished tasks" and "App updates" categories in Android's
settings to the types that used to show in them, and each computer's old
topic switches: one a project turned off became that project's choice
for the types under it; one turned off for every project became the
computer's own choice for those types, which every project without a choice
of its own follows, including projects added later, and which a project's
page shows as its default. After that the old switches no longer
apply; an older phone's own screen shows them as on. An older phone, which
can't choose by type, is told to hold back what the recommended choices and
each project's own turn off, so it shows what a newer phone left as
recommended would; saving its settings keeps each project's choices by type. It posted one silent note,
"Notifications are calmer now", saying how many things were waiting for you
in Needs you, under Android's "Notes from Øphio" category, which never makes
a sound.

### Where things wait in the app

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

These choices cover the phone's notifications only. Messages in Telegram or
Discord chats have their own settings, for each chat.

A computer whose Ophio predates notification types keeps its older choices of
which topics alert, for every project; a phone that predates them uses those.
This phone's own choices replace them.

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
- **Work started from a chat's cards**: the card shows each task as it goes,
  so a task finishing alerts nothing of its own unless its bell is on. Once
  nothing started from that chat's cards is still working or waiting in Next
  up to start by itself, one alert says how it went: "“Show upload progress”
  finished", or "Work from “Upload limits” is done: 1 failed, 2 finished",
  listing each task. Removing or archiving the last waiting task releases
  any summary held for it. A summary of one task follows **Finished tasks**,
  not **Chat replies**; a summary of several follows **Run all results**.
  A failure or anything that needs you alerts at once, as usual; work you
  stopped raises nothing. The chat itself gets one plain line for each task
  as it ends, which never alerts. Its agent gets those reports with your next
  message. They stay waiting until the agent confirms delivery, so a failed
  launch does not lose them.
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

## Usage-limit notes in a conversation

Claude Code and Codex usage-limit notices stay separate from the agent's reply.
The computer saves identical wording at most once per conversation per UTC
calendar day, across replies, native agent restarts and computer restarts. All
phones see the same saved notes; changing devices does not repeat a warning.
Different wording appears immediately, including reaching a limit after nearing
it or a different named limit. Other warnings are not suppressed, and the
account's usage-limit status and reset time still update normally.

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

## Notifications when the app is closed

This section describes Android. On iPhone, notifications currently arrive only
while Ophio is running; closed-app background delivery is not supported yet.
Agent update notices use the same platform limits as other notifications.

While the app runs, notifications come whatever you choose here. One switch,
**Notify me when Øphio is closed**, keeps them coming once the app is closed,
for every paired computer. The phone asks once, right after pairing (or after
the update that added it), with Turn on chosen. Turning it on walks through
only the Android steps still needed, each explained first:

1. Android's notification permission.
2. Battery use set to Unrestricted. Android lets Ophio keep waiting for alerts
   once the app leaves the screen only with it; without it, alerts arrive only
   while the app is open.

Closing the question without choosing asks again the next time the app opens.
Off means notifications arrive only while the app is running.

Ophio keeps nothing in the status bar or the shade while it waits: only the
alerts themselves show.

### How it works

A small watcher in the app's Rust core keeps its own connection to each
paired computer, separate from the app's screen, and posts every
notification on Android. It runs whenever the app does. With **Notify me when
Øphio is closed** on, a plain Android service keeps it running, with no notice
of its own, so swiping the app away or Android reclaiming the screen doesn't
stop it. Android keeps such a service running in the background only for an
app whose battery use is Unrestricted; for any other it stops the service
about a minute after the app leaves the screen.

- The watcher signs in with the phone's own device key at `/v1/alerts`. It
  receives alerts only: the kind, project, task title and time, and for a
  question the agent asked you, the question and its choices. Never other task
  text, terminal output or the screen. A device sees an alert only for an item
  it may see in the app, and a question only if it may answer approvals. Each
  alert says its type, its project's choice for that type, and whether you
  asked to hear about its task; the phone decides how it shows.
- The same connection is the one way the phone speaks: it names the
  notifications you handled (swiped, opened or seen in the app), and the
  computer records each on its item and tells every other device watching.
  What was handled while the computer was out of reach is named when the
  phone reconnects.
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
  together. A new one that alerts takes the place of the oldest showing
  silently; any more are counted in one "N more alerts" note instead of
  showing, and wait in the app. They don't rise later as you clear the shade,
  so swiping alerts away never brings older ones up in their place. The note
  changes its count silently, and alerts again only when one more needs you.
  Tapping it opens the app.
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
  the open app. One answered before the app or the watcher posts it is not
  posted. Turning **Notify me when Øphio is closed** off keeps what shows; a
  watcher stopped and started again leaves everything to the new one, which
  raises what still waits only once each computer says what was answered
  meanwhile. If the computer stops accepting this phone, its alerts are
  cleared too, so a task title doesn't stay on the lock screen.

With Unrestricted battery use it comes back on its own:

- after the app is swiped away. The app closes its own process, and Android
  restarts the service in a new one;
- after the phone restarts, and after the app is updated. Both clear Android's
  shade; what was still showing is put back silently, and nothing you swiped
  away, opened or saw comes back (see [Stays gone](#stays-gone));
- every 15 minutes a check restarts it if Android stopped it.

Without it, opening the app starts it again.

Force-stopping the app in Android's settings stops it until the app is opened.

### Status in Settings

Settings › Notifications shows, for each paired computer:

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
type's category.

## Checks

The emulator suite runs the closed-app cases against a real computer:
`background` (swipe away, open from the alert, quiet on screen, answered at
the computer, no notice of its own, off stops the service) and
`closed-lifecycle` (restart, update, Doze, battery use limited, airplane
mode). Results are in `client/src-tauri/E2E.md`. Physical Android devices still
need checks before release, including a day of normal use with battery use
measured.
