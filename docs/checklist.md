# Checklists

Whatever an agent needs you to do yourself goes on the project's checklist,
not into the chat: signing up or in somewhere, granting access, making a key,
paying, approving, choosing, installing, or checking something only you can.
A chat scrolls away and a file gets lost; the checklist stays with the project
until it is done, and ticking off what an agent waits on starts that agent
again at once.

## Where it is

A project with a checklist shows it in a row right under **New**, in Chat and
in Tasks: its title, how many items are left, the next one, and how many are
blockers, wait on an agent or have a question still to answer. Those counts
and the next item are yours alone: an agent's own items (below) are counted
apart. A blocker gives the row its colour. Tapping it opens the checklist. On a screen short
for its text, such as a phone on its side or with large text, Tasks shows
what agents are asking right now first and the row just after it.

A project without one shows **Request checklist** there instead. It opens a
new task with Øphio's checklist skill and a request already written: "Make a
checklist of everything I have to do myself…". Change it before starting, or
start it as it is.

## Using it

Each item is a fold. The next item you can do opens on its own, at the top of
the screen with its steps below it, so nothing scrolls you away from what you
are doing. Tap any other item to open it there instead; only one is open at a
time.

An item has:

- a title that starts with what to do, and a line on what it unblocks;
- its priority, **Blocker**, **High priority** or **Low priority** (medium
  says nothing), and **An agent waits on it** when one does;
- **Aim for** a day, when the agent suggested one;
- its steps, one action each, in order. A step's page opens from the step
  itself, text to type or paste has **Copy**, and a file has its own button
  (below);
- what it comes after: "Do “…” first", with **Go to it**.

Tick a step's box as you go. Ticking the last one, or the item's own box,
ticks off the item: it folds away and the next one opens in its place. An item
that asks you something, such as a choice, a name or an ID, finishes when you
send the answer: tap one of the options the agent offered, or **Something
else** to write your own. **Open it again** or **Change the answer** puts a
finished item back.

**Remove the checklist**, at the bottom, clears it for the project. An agent
can make a new one.

## What comes first

Items go by priority first:

- **Blocker**: important work is stopped until it is done, such as an item an
  agent is waiting on for the work it is doing;
- **High**: should be done soon;
- **Medium**: the usual;
- **Low**: whenever suits you.

Within a priority, an item with a suggested day comes before one without, the
earlier day first. Once an item's day comes, it moves up to just under the
blockers, whatever its priority, and stays there once it's ticked off; an item
ticked off before its day never moves. An item that has to be done first sorts
with the one that needs it, just ahead of it. Otherwise the agent's order is
kept. Items stay in the sections the agent grouped them in, and the section
with the most pressing item comes first.

## When an agent waits on it

An agent that can't go on without an item marks it as waiting, says what it
will do once it is done, does everything else it can and ends its turn. The
task shows "Waiting for you to finish “…” on the checklist", and the project
is free for other work meanwhile.

The moment you finish that item, the agent starts again with what you did and
anything you answered, even with the rest of the checklist still open. When a
turn is already running, it gets the news as its next message. **Stop** on the
task ends the wait: the checklist keeps its items, and ticking them off no
longer starts that agent. With the item still open after 30 days, Øphio
starts the agent again anyway.

## An agent's own items

Besides what you have to do, the checklist can hold an agent's own work that
can't start until you have done something, such as deploying once you've
granted access. Such an item is marked as the agent's, names the items of
yours it comes after, and is the agent's to keep: you can't tick it off, but
you can send feedback on it under the item, and the agent answers there.

An agent puts its own work on the checklist only when it waits on you: Øphio
refuses one that names none of your open items. Work that nothing holds up,
it just does, and it doesn't use the checklist to plan what it is about to do.

It can also come after the agent's own items, but always after one of yours.
The moment nothing it comes after is open, because you ticked it off, ticked
its last step or answered its question, or an agent finished or took off
what it waited on, the agent that keeps it starts again to do it, and marks
it done itself once it has. Unticking yours before that reaches the agent
takes it back. Each item is told of on its own: when one tick readies several
things for one agent, such as what it waits on and its own item, the first
starts its turn and the rest reach it as messages in that turn.

A rewrite keeps an item's owner unless the agent gives another, and the
agent says so when it changes. Made the agent's, an item of yours has to come
after another open item of yours, as a new one does.

Your feedback reaches the agent that keeps the item: as its next message if a
turn is running, and as a new turn if not. **Stop** on the task, or archiving
it, lets its items go: nothing starts it for them any more, and feedback on
them stays under the item, not counted among unanswered questions, until
another agent takes the item over by rewriting it. An item put there with
`ophio checklist put` is kept the same way, by whoever put it.

## Asking about it

**Ask about this**, under an item, sends your question to the agent that
wrote the item, and its answer appears under the question. You can go on with
the checklist meanwhile. **Ask about the checklist**, at the bottom, asks
about the whole of it. When that agent's task has finished, your question
starts its next turn. When the task has been archived, Øphio starts a
read-only task in the project to answer instead, titled "Question about the
checklist": it reads the project and the checklist and answers under your
question, the same way, but changes nothing, so when an item is wrong its
answer says so and you can ask an agent to fix it.

## Getting a step's file

A step can carry a file, such as a settings file to put on a server, a form
or a picture. The computer keeps a copy, and the step's button gets it to you
the way you chose:

- **To the phone that asks**: saved where the phone keeps its downloads;
- **By Taildrop**, to one of your devices on Tailscale;
- **Into a folder on this computer**, such as your Downloads or the folder a
  Google Drive, Dropbox or OneDrive app keeps in sync;
- **Onto a drive set up in rclone**, into an `Ophio` folder there unless you
  name another.

Each project's files go in a folder of their own, named after the project.
Choose under **Settings › Where files go** in the app, or in `ophio setup`'s
Extras, which offers it until it has been asked once. The setting lists what
this computer has: Tailscale and rclone each show once they are set up here.
A folder can be given from your home folder, as `~/Documents/Checklists`.
Changing it needs the Manage access permission.

Pictures, videos and plain text files also have **Open**, which shows the
file inside Ophio without saving it; the viewer has its own **Save to this
phone**. Tap a picture to see it at full size, and again to fit it back to
the screen. Other kinds of file, such as PDFs, can only be saved or sent.

## Its look

An agent gives the checklist the project's own look when the project has one,
such as a logo, colours in its styles or a style guide: an accent for what you
tap, a background behind the heading, a font style (sans, serif, rounded or
mono), the project's name and its logo. The accent's text is white or
near-black, whichever reads. Without a brand, it has Øphio's look.

## For agents

The **Give the person a checklist** skill is built into Øphio and listed in
Library's **Skills**. Every run is also told to put what the person must do on
the checklist rather than in its reply. Øphio's tools:

| Tool | What it does |
| --- | --- |
| `checklist_read` | The checklist, with every id, tick, answer and question |
| `checklist_add` | Put items on it, or rewrite those with the same ids; mark the agent's own items done |
| `checklist_remove` | Take items off |
| `checklist_reply` | Answer a question the person asked under an item |

Outside Øphio, in a Claude Code or Codex session of your own, the same is
`ophio checklist`. `<project>` is a project's ID, as `ophio projects list`
shows it, or its folder:

```sh
ophio checklist show .                 # print it, with every id
ophio checklist put . items.json       # the JSON checklist_add takes
ophio checklist put <project> - < x.json
ophio checklist remove . make-key
```

Leave `owner` out of a rewrite to keep the item's owner. An item of an
agent's own work has `"owner": "agent"` and `after` naming the
person's open items it waits on; `"done": true` marks it done and `false`
opens it again. Only the person ticks off their own items, and an agent's own
item has no `waiting` or `ask`.

No agent waits on what is put there this way, and questions go to a new task
in Øphio. A step never holds a password, token or key: the person stores one
where it belongs, such as **Settings › Accounts**, and the step says where.

A checklist holds up to 24 sections, 160 items and 60 files of up to 25 MB
each; an item up to 30 steps.

## To-Do

**To-Do**, to the right of Library, brings your checklist and agents’ work
into one phone screen. The project picker works as on the other tabs. The
chips below it switch projects and show their open counts; **All** combines
every project, with each entry linking back to its project. All is remembered
on this phone until you choose a project, including in the header.

Every entry says **For you** or **For agent**. Your items keep their steps,
questions, notes, links, copied text and files. Ticks show immediately and
settle when the computer answers; a refusal puts the item back and says why.
The progress and **Next** count only your items; agent items have a separate
count. Blockers and things holding an agent up come first, then things whose
day has come. All puts finished items in a fold at the end.

Agent checklist items are work waiting on, or previously held up by, one of
your items—not a place to plan unrelated agent work. **Waiting on you** names
and opens the prerequisite. Agent items show whether their agent is working,
has finished, or is no longer on the task. Their steps cannot be ticked here;
**Send feedback** asks the item's writer, with its reply kept under the item.

**Agents’ work** also shows tasks needing you, tasks waiting on a checklist
item, and tasks not yet started in **Next up**. Feedback opens the task's usual
composer, keeping its existing send-or-follow-up behaviour. A task that has
not started offers **Open in Next up** instead. Completing a prerequisite
immediately clears its waiting entry on this phone. The To-Do badge counts
your open items holding agents up across all projects.

Checklist links and the old checklist destination now open To-Do on the
right project and item. An older computer without checklist support still
shows agents’ work and the update notice. This is shared web UI for Android
and iPhone; it needs no native plugin change.
