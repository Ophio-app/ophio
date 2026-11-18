# How controls answer a tap

Every control in the phone app that changes something on the computer belongs
to one of three kinds, and each kind behaves the same way on every screen. The
point is that a tap is never met with a frozen control while the computer
answers, and that the app never claims a safety change took effect before it
did.

| Kind | Examples | On the tap | When the computer refuses |
| --- | --- | --- | --- |
| **Instant** | Notification choices, appearance, read and unread, pin, Next up order, agent defaults | The control shows the new choice at once and the request goes behind it. It stays usable meanwhile. | The control goes back to what the computer has, and a toast in the stop tone says what didn't change and why. |
| **Safety** | What a conversation's agent may reach and what new chats in a project start with, a permission this device lacks, permission and skip-prompt defaults, GitHub on or off, voice control, app lock, taking over and handing back | The control keeps showing what is in force. A change that takes more than a moment shows as **Changing…** beside it, until the computer says the change is in force. | The control never showed the change, so nothing goes back; a toast says what didn't change and why. |
| **Destructive** | Delete a Library file (it goes to Recently deleted, so its toast has Undo), close an issue, archive | Where the computer can undo it, the change happens at once with **Undo** in a toast. Otherwise a confirmation names what will be lost. | As for instant. |

## In the code

`useCommandControl` (`client/src/app/commandControl.ts`) is the instant and
safety kinds. A screen gives it the computer's value, how to send a change
and how to describe a refusal, and shows what it returns:

- `value` is what the control shows as chosen. For an instant control it is
  the person's latest choice until the computer's own state shows it (or
  `SETTLE_MS` passes after the computer accepts, whichever is first). For a
  safety control it is always what is in force.
- `changing` is a safety control's choice still on its way, once it has taken
  longer than `CHANGING_AFTER_MS`, so a quick change doesn't flicker.
- `set(next, note)` makes a change. Taps made while one is on its way wait
  for it and then send only the latest choice, so a setting saved against a
  revision is never sent twice against the same one. `note` names the change
  for the computer's command history (`label`), for a refusal ("Couldn’t
  change *what*"), for a screen reader once it lands (`said`) and, where a
  toast should confirm it, for that (`done`). It resolves to whether the
  computer took the latest choice, for a screen with a next step.
- `waiting` is the choice on its way once it has taken a moment, for either
  kind. A control that mixes the two uses it: GitHub's settings are one
  record, so every choice on them goes through one instant control, one after
  another, each carrying the ones before; the on and merge switches show the
  mode in force and `Changing…` (`design/StateLine`) while `waiting` holds
  another.
- `pending` is the choice on its way from the tap. A safety control holding
  several choices in one record builds the next tap on it: voice control's
  two switches are one setting, so a second tap while the first is on its
  way carries the first.

Changes to the computer's shared state (archiving, closing an issue, moving
work in Next up) show at once through `HostStateStore.expect` and `settle`
(`client/src/host/expected.ts`) and `expectItem` for GitHub lists, with the
same rollback and stop toast. Next up's changes reach the computer one after
another, each against the revision the one before it leaves, so a quick run
of moves never trips over itself; a change queued behind a refused one goes
back too.

Action buttons (Send, Start, Pause, Save on a sheet, Confirm) aren't choice
controls: they may show their own request with a spinner until the computer
answers. Sheets that draft a change and save it with one button (project
defaults, a stage change, a connection) keep that shape.

Start on work a chat suggested is an action that flips at once: the card's row
says **Starting** on the tap, then **Started**, or **In Next up** when the
project's writer holds it, as soon as the computer creates the task. A
refusal puts the row back to Start and says why in a toast. The chip beside
it is an instant choice that lives on the phone: a pick changes the card at
once and is remembered, on this phone only, as the person's usual agent for
that kind of work. A card opened in Review and started there is still that
card's task: back in the chat its row is that task, and it has no Start left.
Start in the message box, for a plain Work task, behaves the same way as
Review's Start task.

What a project's new chats start with is a safety control. It is remembered
only once the computer has the chat's access in force, it is shown only as
the computer holds it, and **Set back to read-only** waits for the computer
before the line changes.

A control may still be disabled for a reason that holds before the tap: the
device lacks the permission, the computer is offline, or the computer is too
old for the feature. It is never disabled only because its own request is on
the way.

## Dialogs and navigation

Opening another screen or changing tabs dismisses dialogs on the screen being
left, including the Capture inbox picture viewer when **Draw on it** opens the
drawing screen. Back returns to the screen without reopening its old dialog.
Dismissing a dialog never approves, sends or confirms an action. The shared
sheet uses the shell's active-surface context so hidden, still-mounted screens
cannot leave an invisible browser modal intercepting taps.

A sheet holding something typed or chosen and not yet sent is the exception.
When another screen covers it, for example **Defaults in Settings** in its
agent sheet or a tapped notification, it steps out of the way without being
dismissed, and Back brings it back exactly as it was left. It still never
sends anything by itself. Three kinds of sheet stay dismissed instead:

- a sheet holding a key, password, sign-in code or revealed secret, which
  should not wait unseen behind another screen;
- a confirmation or a viewer, which Back should not bring back;
- a sheet whose choice the computer could overtake meanwhile, so that
  applying it on return would undo the newer state. Such a sheet is kept only
  while that state is unchanged (the access lists are kept until what the
  agent may reach changes), and a computer let go of meanwhile is not
  renamed.

The shared sheet does this for a sheet marked `keepWhenHidden`.

## Back returns to where you came from

An opened task is one visit. Both the on-screen Back control and Android Back
return to the screen that opened it, with that screen's list and filters kept.
A finished task may open on its receipt first. Continuing or following that task
in Chat replaces this entry view; Back does not make a second stop on the receipt.
This applies to Tasks, Live agents, task links and result notifications. A task
link opened from a chat returns to that chat, not the Tasks tab.

A task started from a chat's card opens from that card, and Back returns to
the chat. Wherever such a task shows, it says "From chat “…”": on its row in
Tasks, in Next up, and on its heading and receipt, where one tap on that line
opens the chat. The card's own line about the task changes in place as the
work moves and never alerts.

A receipt deliberately opened to inspect history or changes is different: it
remains a previous screen. Opening a file from a receipt still returns to that
receipt. Neither opening nor closing a task starts or restarts work.

## Checking it

- `client/src/app/commandControl.test.tsx` covers the hook: the instant
  flip, the rollback toast, queued taps, settling, and the safety kind's
  Changing… and refusal.
- Screen tests run the simulator with `latency(1500)` and expect an instant
  control to change within 100 ms of the tap and stay usable.
- The browser smoke (`client/scripts/smoke/plan.json`) does the same in a
  real browser with its `flips` step, in every screen shape, and its
  `inView` step checks the first Needs you control is on the screen Tasks
  opens to. A canary for each must fail, so neither check can pass by
  accident.
