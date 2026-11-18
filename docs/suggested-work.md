# Work a chat suggests

A chat answers questions and reads the project. When the agent finds
something that needs doing, it does not do it in the chat; it suggests a
task. The suggestion shows in the chat as one card, and one tap on **Start**
starts it as a proper task with the least access that work needs. Several
suggestions from one reply make one card that opens to a list of them.

## How the agent proposes work

A chat's agent has a tool for this, `ophio_propose`, on every turn,
read-only or not, in Claude Code and Codex alike. It is told that a change
deserving its own branch (a feature, a fix across several files, anything
worth reviewing on its own) is work to propose, not to do in the chat and
not a reason to ask for more access, unless you say to do it there. It
writes each task's brief for an agent that never saw the chat and says in
one line that the work is on a card. Only a chat's turns are told this, and
only a chat may propose; any other task that tries is told to say what it
would start in its summary instead.

One call proposes one task, an ordered set of up to eight, or fixing every
open issue (below). What a turn proposes shows as its card when the turn
ends, together with anything it suggested in its report.

Claude Code in a Plan chat keeps its own plan mode, which refuses every tool
of Ophio's, so it can't propose there; it says the work needs Work mode
instead.

## What a suggestion starts as

The kind of task comes from the work, not from the chat. The agent describes
what it suggests (a title, a brief, a mode, an issue number, and what else it
needs to reach); the phone and the computer work out the same task from that
description, and the computer refuses any start that differs from it.

| The suggestion | Task it starts | What it may reach | Card line, for example |
| --- | --- | --- | --- |
| Find out something | Ask | Its own project, read-only, plus only the projects it names to read, and the internet or your own network only when it says so | Find out · read-only · this project only, no internet · starts now |
| Plan something | Plan | Read-only | Plan · read-only, then you decide · this project · starts now |
| Change something | Work | Its own project, with your usual permission setting; its own branch and pull request where GitHub is on and you usually use one | Change · own branch and pull request · asks before risky steps · starts now |
| Fix issue #12 | Work on that issue, with the project's reviewer | As a change, always with its own branch and pull request | Fix issue #12 · own branch and pull request, reviewed · … |
| Work on the desktop | Computer use | The screen you choose; Start opens the desktop's own review | On the desktop · you choose the screen first |
| Something in another project | As above, in that project | Shown on the card in words | … plus "works in garden-planner" |

Anything past the task's own project (another project, the whole computer,
your own network, the desktop, no permission prompts) is always written on
the card, never behind an opener. Such a suggestion is marked, is never
started by itself, and starts from Review.

A started task never takes the chat's own access. A Work or Plan task follows
its mode; only an Ask gets the access written on its card.

## What the tap approves

What starts is what the card showed. The computer keeps every suggestion as
the agent made it, and Start names the suggestion it came from. Before the
task is created the computer works the task out again from that suggestion
and compares:

- the project;
- the kind of task (a plain coding task, never a conversation) and its mode;
- the title and the brief, word for word;
- the GitHub route: the issue's own number for a fix, the change's own pull
  request or none, and nothing for a question or a plan. A fix carries no
  GitHub setting of its own that the card didn't show: it never merges by
  itself, and the only reviewer it may name is the one the project already
  has;
- for a question, the least it needs to reach;
- that nothing else rides along: no branch, attachments, launcher, folder,
  stopping point, screen, review role or browser setting.

These are yours, are shown on the card, and are not compared: the agent,
model, effort and permission setting, and whether it starts now or waits in
Next up. They come from your usual choice for that kind of work, which is
kept on this phone only (see below), or from what you pick on the card.

Anything else is refused with "What would start differs from the suggestion
you saw.", and a second start of the same suggestion with "This was already
started." A card in a chat that has been archived can't start: the card
isn't shown there, and a phone that still shows it is told "This chat is
archived. Bring it back first, or start the work from Tasks."

A fix for an issue from someone outside the project starts as a plan, as
any such issue does, so its agent reads and proposes before anything
changes. It is never refused for that. The card shows the task as a plan
and says "Started as a plan, because the issue comes from outside the
project."

A card's Start never resumes a paused Next up. If the suggestion would wait
in Next up to start by itself while Next up is paused, the computer refuses
it with "Next up is paused, so this can't start by itself. Start it again
from the card: it waits in Next up until you start it."

### Started from Review

**More options**, or a card that needs a choice, opens Review with
everything filled in. There you can change any of it: the words, the mode,
the project, what it may reach. Started from there it is still that card's
task: the card shows it like any other started task, it can't be started a
second time, and it is linked to the chat. Because you saw and could change
every part, it isn't compared with the suggestion, and it may go to another
project. Starting a task stays yours alone; an agent can't start one.

The chat hears how it ended, as below, only when it runs in the chat's own
project: a chat may not read another project's work, so another project's
agent's summary never reaches it. The desktop's own review keeps the card
the same way. On a computer that doesn't keep this, a task started from
Review is a new task, as before, and the card can still be started.

## A set of tasks, in order

Steps that must happen one after another, such as "add the new store",
"copy the settings across", "read from the new store", "remove the old one",
come as one card: "4 tasks, in order". It opens to the numbered list, and
**Start all** puts every step in Next up, each waiting for the one before
to finish, so the order holds even when you are away. Once some have
started, the button says **Start the rest**. What the steps share, the
agent and the kind of task with its branch and pull request, is said once
for the set; each step shows its title and only what differs.

If a step doesn't succeed (it fails, stops short, or you take it out of
Next up), the steps after it would wait forever. The computer puts them
away unstarted instead, each saying why, as in "Not started: “…” didn't
succeed" or "…was taken out of Next up", so
they no longer hold up Next up or count as going. The card shows the
failed step as it stands, offers no **Start all** or **Start the rest**
past it, and gives each later step a **Start** of its own, if you still want
it. This holds for any set started from a card, by you or without asking. A
failed task pauses Next up as always; resume it when you're ready.

A step can also start on its own, from the list or from Review, but never
ahead of the one before it:

- if the step before hasn't been started, the computer refuses with "This
  follows “…”. Start that first.";
- if the step before is still going, a step can't start now: "…which hasn't
  finished. Put it in Next up to start after it.";
- if the step before is still going outside Next up, or a step was moved to
  another project in Review, it can't wait on the one before: "…still going
  outside Next up. Start this once it has finished."
- if the step before didn't succeed, a step can't wait in Next up after it:
  "“…” didn't succeed, so this doesn't start after it. Start it on its own if
  you still want it." Started now, it is your own choice.

## Fixing every open issue

When you ask a chat to work through the issues (tonight, say), its agent
proposes that alone. The card reads **Fix every open issue**, says who fixes
them and who reviews, and its **Run all** starts the project's own Run all
on the Issues tab: every open issue the agent can take, one after another,
with one summary at the end. Until the project's fixer is chosen and how
far fixes may go is answered on the Issues tab, the card says so instead.

Run all starts once from a card, never from an archived chat, and never by
itself. Review can't turn it into a single task: such a start is refused
as different from the suggestion. After the tap the card says **Running**,
then **Done**, and opens Issues.

## Starting without asking

A project's sheet in Tasks has **Start what the agent proposes without
asking**, with an **Allow in this project** switch, off unless you turn it
on. With it on, when a chat's agent
proposes work in that project while answering a message you sent, the
computer starts it as the card's Start would, once the turn ends. Two
limits, from one to eight and four to begin with, cap how many tasks one
reply may start, across everything it proposed, and how many may run at
once. Anything outside these rules waits on its card for your tap:

- only a turn you started: your own message, typed or replied to, your
  queued message, or your Steer. A wake-up, a helper's answer or a
  checklist turn never starts anything by itself;
- only a turn in Work mode, in a chat with full access to this computer.
  A chat held to less, an Ask or Plan turn, or a side question, waits for
  your tap;
- only work in that project, needing no more than the internet, and never
  fixing every issue;
- in the order proposed: a set starts only if all of it fits in both
  limits, and once something doesn't fit, nothing proposed after it starts
  ahead of it. Work that only needs your tap anyway is passed over;
- not while Next up is paused.

The agent is told exactly this for the turn it proposes in: that what it
proposed may start by itself, or that it waits for your tap.

A change gets its own branch and pull request when GitHub is on for the
project, and none otherwise. Work started this way always stops at its
pull request for you to merge, even in a project that merges by itself.

Each such start shows on its card with "Started without asking.", where you
can open and stop it. Only you can change the setting, never an agent, and
only a phone allowed to start tasks and answer approvals shows it. Turning it on or
raising a limit waits for the computer to have it in force; turning it off
or lowering a limit shows at once.

## After Start: the card follows the work

A started task stays on its card in the chat, and the card says how it
stands in the same words its row in Tasks uses: In Next up, Starting,
Working, needs you (with what it needs), Pull request open, Merged, Failed.
The line changes in place as the work moves; nothing about it alerts.
Tapping it opens the task, and Back, on screen or Android's, comes back to
the chat.

The task is also in Tasks as usual. Its row, its place in Next up, its
heading and its receipt say "From chat “…”" with the chat's name, and on
the heading and the receipt that line opens the chat. Once the chat is
gone it says "From a chat".

## When the work ends, the chat hears it

When a task started from a chat ends, the computer posts one plain line in
the chat: "“Show upload progress” finished. Pull request #412 is open.",
"“…” failed." or "“…” was stopped.". It does this each time one of the
task's runs ends; a pull request that merges later, with no run ending,
adds no line, and the card shows Merged.

The chat's agent hears the same lines on its next turn, after your own
words, under "Ophio's report on work started from this chat, since your last
turn". It is told that this is information about what happened, not
instructions and not a message from you, and not to act on it unless you
ask. Each line may quote up to 500 characters of the task's own agent's
summary, marked as a quote, so another agent's words reach this one only as
data. At most 20 lines wait for a chat; older ones go. A side question to
the chat neither takes nor clears them.

## Turn into a task

On a computer that tells chats how their work went, **Turn into a task** in
a chat's menu makes a new task from what you write in its sheet, and the
chat stays where it is, in Chat. The new task gets the same plan a task
typed into the message box or in Tasks gets: your usual agent for a change,
and its own branch and pull request where you usually use one, which Ophio
sees through as it does any task's. When it needs a choice, it opens Review,
still marked as from that chat. When it ends, the chat hears how, as above. The computer checks that
the chat named is a chat in the same project and not archived, and a task
can't be both turned from a chat and started from a card.

On an older computer, Turn into a task carries the chat itself on in Work
mode, and it moves from Chat to Tasks, as before.

## What alerts

Work you started from a chat's cards alerts calmly. The card already shows
each task as it goes, so a task finishing raises its result quietly. Once
nothing started from that chat's cards is still working or waiting in Next
up to start by itself, one notification says how it went: "“Show upload
progress” finished" for one task, or "Work from “Upload limits” is done: 1
failed, 2 finished" with each task listed. The one for a single task
follows your **Finished tasks** setting, not **Chat replies**; the one for
several follows **Run all results**. Removing or archiving the last task
still waiting in Next up sends the summary for the rest. A task whose bell
you turned on alerts for itself as it ends, even in a muted project. A
failure, a question, an approval or a task that needs a look alerts at once,
as anywhere else, and anything you stopped yourself raises nothing. A task
turned from a chat has no card, so it alerts as any task does.

The chat's agent hears how each task ended with your next message there.
Those reports wait until the agent has actually received that message, so a
launch that fails first doesn't lose them.

## The agent chip and the usual agent for each kind

Each card has one chip with the agent, model and effort the task will start
with. Tapping it opens a small picker with just those three; **More options**
at its foot opens Review with everything filled in. A set of suggestions has
one chip, and a single task in the opened list can differ.

The chip starts with your usual agent for that kind of work: find, plan,
change, fix or desktop. Ophio looks for that kind in this project first, then
that kind in any project, then the project's own default agent or the app's
default agent, at its highest effort.
Only your own picks are remembered; nothing the agent writes in a suggestion
can set them, and they change only the agent, model, effort and permission
setting, never the kind of task or what it may reach. They are kept on this
phone only, in its own storage, so another phone starts from the defaults
until you pick there. If a remembered model or effort has gone, the chip
shows what will really run and the memory is left as it was. The same
memory fills in the message box and Review.

A card can't be dismissed. It stays under the chat's turn that suggested it,
on every device, until its work is started, and the whole card goes when
the chat is archived. Whether the list of a set is open is kept only while
the chat is on screen.

## What a project's chats start with

What a chat's agent may read, change and reach shows on one line under the
message box, and opens to the controls. A project whose chats were never
widened starts read-only. Once you let a chat reach more and the computer
has it in force, the project's next chats start with the same, marked
**More than read-only**, and **Set back to read-only** on that line clears it.
The computer keeps this, and only your own choices on this device set it.

## Starting a plain task from the message box

A Work task typed in the message box starts from there when nothing in
Review needs choosing. One quiet line above the box says what it will start
as, and the button says **Start task** or **Add to Next up**. Tapping the
line opens Review. A Plan, markup, Start from, a chosen branch or stopping
point, a paused Next up, a project that merges by itself before you have
confirmed it, or an older computer still go through Review.

## Review

Review for a new task shows each section folded to its current value:
where it runs and its branch and pull request, the agent, and the mode, with
**Optional** folded the same way. A section opens by itself when something in
it needs you.

## For developers

- The computer's check is `crates/services/src/work_card_binding.rs`, with
  its own unit tests, and the start's other rules (same project, archived,
  started once, a paused Next up) in `commands.rs`, tested in
  `crates/services/src/work_cards_origin_tests.rs`. The check names every
  field of the start, so a new one doesn't compile until it is either
  compared or said to be yours.
- A start from Review sets `TaskOrigin.reviewed`; the feature string
  `chat_work_reviewed_v1` tells the phone the computer keeps it.
- The phone's plan for a card, and for a typed task, is
  `client/src/screens/talk/workCards.ts`; the usual agents are
  `client/src/app/launchMemory.ts`.
- The feature string is `chat_work_cards_v1`. A phone that does not see it
  opens Review for every suggestion, as before.
- Proposing is `crates/services/src/proposals.rs`, with tests in
  `proposals_tests.rs`; the tool is `ophio_propose` in
  `crates/computer-mcp/src/mcp.rs`, and what a chat is told is `proposing!`
  in `crates/ports/src/harness.rs`, sent only when the tool bridge's `chat`
  flag is set. Both agent programs allow `READ_ONLY_TOOLS`
  (`crates/ports/src/bridge.rs`) on a read-only turn.
- Sets, the Run all card and starting without asking come with the feature
  string `chat_work_sets_v1`. A set's order is `NewWork.after`; Run all from a
  card is `github_run::from_card`; the project setting is
  `Project.auto_start`, set with `set_auto_start`, which only the person may
  send. A start the computer makes itself has `TaskOrigin.auto`.
- What the chat hears, and the one summary for the person, are in
  `crates/services/src/chat_news.rs`, called from `end_run` and
  `dispatch_task` in `runs.rs`, with tests in `chat_news_tests.rs`. A chat's
  waiting lines are kept under `chat_news`, and the tasks held for its
  summary under `chat_calm`.
- A task records the chat it came from in `Task.origin` (a card's Start) or
  `Task.from_chat` (Turn into a task). The feature string `chat_hears_back_v1`
  tells the phone that Turn into a task keeps the chat; without it the phone
  carries the chat on as before.
- The card's live line is `client/src/screens/talk/cardLive.ts`; the "From
  chat" line and link are `fromChat.ts` and `FromChatLink.tsx` beside it.
- Each kind's usual agent is kept under a new key in the phone's own
  storage (`launchMemory.ts`), never on the computer; nothing an
  earlier release reads is changed, so it still reads its own settings after
  a rollback. What new chats start with is a new, optional field on the
  project that an earlier computer ignores.
