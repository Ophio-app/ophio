# Working with an agent

Ophio's features are meant to be used by the person and the agent together.
The agent knows it is talking through Ophio, what it can put in front of the
person, and what the person can send back; the person sees each of those where
it was said, with the button to act on it.

## What the agent can show

A Work run started from Ophio, and each turn of a Chat conversation, gets
three tools beside its computer tools, and instructions describing them and
the ways the person answers:

- **show_person** shows a picture or video from the project's folder or the
  task's scratch folder: a screenshot of a page it built (taken with a headless browser), a chart, a
  render or a screen recording. It can invite the person to **draw** on a
  picture or **talk** over a video.
- **show_preview** puts a page the agent serves on this computer in Chat as a
  live preview to open and draw on, and adds it to the project's previews in
  Control if it isn't there. Everything a run starts stops when it ends, the
  page's server too, so the agent gives **start**, the one command line that
  starts that server from the project's folder. When the person opens the
  preview and nothing answers, Ophio shows them that command, and if they
  tap Start server, it runs it in a terminal titled "Preview: *label*" in the
  project's folder (reused while it stays open, closed from Control like any
  other), waits up to a minute for the page to answer, then opens it. Only a
  device that may open terminals as well as run tasks can start it; another
  is shown the command to run itself. The command is refused if it holds a
  key or token the run was given, a control character or more than 1000
  characters. The agent can also ask up to six **questions** about the page,
  each with up to eight options to choose from or none, for the person to
  answer beside it as they try it (see below).
- **invite_person** offers a button to **capture** the computer's screen and
  draw on it, **watch** the screen live, **send files** from the phone,
  **start computer use**, where an agent tries an app or site on one of the
  computer's screens while the person watches (the agent can give the goal,
  which fills in Start computer use unless the person has typed one), or
  **control by voice** with Jev.

The agent is told to use these without waiting to be asked, and to suggest
them whenever one would serve better than words: a picture to draw on instead
of a description, a recording to talk over, a preview to try. It is told never
to screenshot or record the desktop itself: the person captures it through the
invitation. Nothing the agent shows waits for an answer; whatever the person
does comes back as their next message.

Files are read only from inside the run's project folder or its task's
scratch folder (links that lead out of them are refused, and so is a project
folder since replaced by a link), up to
20 MiB, and their type is taken from their bytes, not
their name. Previews are loopback only. Captions, labels and notes have the
keys, tokens and sign-in codes the run was given hidden, as everything else it
says does; a preview address holding one, even written with `%` escapes, is
refused, as is one over 8 KiB or escaped more than three times over. Ask and Plan runs, a question asked of a Chat
conversation while its agent works, and the computer-use specialist don't get
these tools; Ask and Plan runs are told to say what to look at instead.

### In a Chat held to what it may use

A conversation that isn't given the whole computer and the internet runs each
turn in a sandbox, and its tools keep to what the person gave it:

- A page the agent serves inside the sandbox can't be reached from the
  computer itself, so while the turn lasts, its preview is opened through the
  sandbox. Once the turn ends the server stops with it, and the preview's
  **Start server** runs the command in a terminal on the computer, outside the
  sandbox, as for any other preview.
- Filing an issue or starting a review goes to GitHub, so it needs the
  conversation's **Internet**.
- What it plans for the project's Next up waits for the person to start it,
  and a review it starts asks before fixing what it finds.
- The agent browser runs on the computer, outside the sandbox, so it opens
  pages only while the conversation has both **Internet** and **Local
  network**; until then the agent is told to fetch the page from inside.
- Someone the agent asks for help gets the conversation's access, read only.

## The task's scratch folder

Each task has a scratch folder for the files nobody needs to keep: the
screenshots and recordings it shows, renders, downloads, test builds and logs.
A Work run is given its path in `$OPHIO_SCRATCH` and told to put such files
there rather than in the project or `/tmp`, which on many computers is held in
memory. The folder is also the temporary folder (`TMPDIR`, or `TEMP` and `TMP`
on Windows) of everything the run starts, so what a tool leaves behind lands
there too, unless the folder's path is too long for the sockets programs make
in it. Claude Code may write to it as one of the task's folders; Codex already
may write to its temporary folder.

The folders are in `scratch/` in Ophio's data folder, named for the end of the
task's id. Once an hour, and when Ophio starts, it deletes the folder of a task
that is archived, or that finished more than two days ago. While all of them
come to more than 10 GiB, finished tasks' folders go sooner, the longest
finished first. A task that is running, waiting on the person or queued keeps
its folder whatever its size. This includes a finished, failed or interrupted
task with a continuation waiting in Next up, even while the queue is paused
or waiting for permission to start. Deleting never follows a link or goes into a
drive mounted inside, and a folder whose task Ophio doesn't know is left an hour
first. What the person was shown is kept in Chat and the capture inbox for a
week after they last open it (see below); anything worth keeping belongs in
the project. Ask and Plan runs and the computer-use specialist aren't told
about it.

## Answering in Chat

A picture opens in the drawing with **Draw on it**. A video plays with **Talk
over it**: while it plays, each comment is given the moment of the video it was
made at and a still from then, so the agent hears what was said where. Speech
is timed from when the phone's microphone started, not from the tap, so a
permission prompt doesn't shift it. While the video waits to load or seeks it
isn't playing, so nothing said meanwhile is placed later in it, and a seek
places what is said at the frame shown until the frame sought appears. Sending waits
for each comment's still. A comment that can't get a still from its own moment,
as when the message already carries all the pictures it can, goes as a general
comment without a moment, rather than with a still from another, and says so;
taking a picture out makes room for its still. A
preview card opens the page; an invitation's button goes to the capture
choices, the live screen, the file sender, Start computer use or Jev.

When the agent asked about a preview, its questions sit under the page while
it is open, to hide or show: options to tap, and a box for the person's own
words. **Save answers** keeps them and closes the preview; nothing goes to
the agent yet. The card can also take them without opening it (**Answer
here**). Back in Chat the person sees what they answered and chooses:

- **Put in Next up** for a task that has stopped: the answers wait in the
  project's Next up and start the task's next turn when the project is free,
  where they can be edited or withdrawn like anything else there.
- **Send after this turn** for a task that is still working: the agent reads
  them when its turn ends.
- **Leave in Needs you**: the answers stay on the phone, unsent, and the
  task's row in Needs you says so until they are sent from the card.

The agent reads each question as it asked it, then the answer, or "(no answer)"
for one the person skipped. Answers are kept on the phone as they are written,
so closing the app loses nothing.

Once a turn ends, its card under the conversation, and the top of its receipt,
hold what that turn showed under **What it showed you**, next to what it asks:
the latest four pictures, videos, previews and invitations, each once, with a
line saying how many more it showed earlier. They are answered there as in the
conversation. However long the turn went on after showing them, the phone reads
back through the conversation to where the turn started to find them.

**Pictures and videos** in the Chat bar lists the task's pictures and videos,
newest first, to open, answer over or delete, each marked with where it came
from (what the agent showed says so however far back its message is, and
only the agent can mark a picture as shown). A picture can't be deleted once a
message carries it; what the person sent stays with their message.

What an agent made for the person to look at goes a week after they last
open it: the pictures and videos it showed, the screenshots it took of the
computer and the pages it showed. The week counts from the latest of when it
was made, when its task finished and when the phone last fetched it, which
is whenever its picture shows in Chat or the capture inbox, or it is opened
to look at, draw on or play. A page's week counts from when an agent last
showed it or the person last opened it. Once an hour Ophio forgets each
picture, video and page whose week is up, and a picture's bytes go once
nothing else has them. Chat still shows where it was, with a note that it was
removed, and nothing to do over it.

Tasks' capture inbox lists, for the project picked in Tasks, what agents
showed in it, newest first, from every task however long ago it ended, with
the pages agents showed or the person added. Opening a page whose server is
stopped offers to start it again on a phone allowed to open terminals.

What the person kept stays: a picture they drew on or a video they talked
over and sent stays with that message, and one saved to the phone is the
phone's copy. What the person sent or captured, the after-evidence a receipt
rests on, and the pages they added themselves never expire. A running,
waiting or queued task keeps all of its pictures. This includes a stopped task
whose next message is waiting in Next up, even while the queue is paused or
waiting for permission.

A finished task's receipt keeps the composer in its footer, so another prompt
continues the same conversation. A button that says what it sends sends it as
it's tapped, and what the person was writing stays as it was. **Retest** asks
the task to retest its changes. **Carry out this plan** continues a finished
Plan in Work mode. **Start** on new work the agent found starts it as a task
of its own: its brief replaces the new-task draft, and words the person had
there are set aside, not added to. These last two go through Review, which
stays open only when something there needs choosing, such as an agent or
model that is no longer available, or when the person's own words would go
with the plan. Touching the screen before it sends leaves the send to the
person.

An agent in Plan or Ask has no Ophio tools, so it can't start or queue
anything itself: the new work it finds waits on its receipt, and it is told
to ask the person to start what should come next. Where starting something
changes mode, a lime line right above the button says so, such as
"Switching from Plan to Work. This new task can change files, and starts
with “Skip permissions”." It is on each new work card whose mode differs
from its task's, above **Carry out this plan**, and above Review's own start
button.

## Projects and unread results

The project switcher shows each project's unread results, and the switcher
button is marked when another project has any. Projects are listed by when one
of their tasks last moved: it was made, a turn of it started or ended, or it
asked for the person. What an agent does in between, each step it reports,
doesn't move its project, so the list stays still while agents work.

Control's header is the same switcher, with the computer's name under it.
Its Terminals and Previews show the chosen project's, and their counts are
that project's; Home also holds terminals opened outside any project. **Show
N in other projects**, under the list, shows the rest there, each project's
under its name. New terminal and Add a preview start in the chosen project.
Screens are the whole computer's, whichever project is chosen.

## Permissions from the phone

When something is unavailable because this device lacks a permission, and this
device may manage access (the **Manage** permission), the notice offers a
switch to allow it. A device without Manage is told the `ophio devices allow`
command to run on the computer instead.

## System

Control's **System** section shows how busy the computer is: processor (overall
and per core), memory, swap, network, graphics, disks, temperatures, battery,
uptime and the busiest processes. It needs the **View** permission. The
computer is read only while a device is looking, at most once a second however
many look; a few minutes of history are kept for the graphs and dropped after
30 seconds without anyone looking. Processes are named by their short program
names only, never their command lines. A disk that takes longer than 400 ms
to say how full it is is left out and not asked again until it answers, so a
stuck FUSE disk or network drive can't hold the reading.

On Windows, swap, network, temperatures and graphics aren't read, and Control
leaves them out.

## Previews found by themselves

A site served on this computer by a program working in one of the projects'
folders, such as a dev server an agent or the person started, joins that
project's previews in Control by itself, named by its page title, with any
key, token or sign-in code Ophio holds hidden in it, however the page writes
its characters (`&#49;` is read as `1` and `&eacute;` as `é`, as a browser
reads them) or spaces them, and kept hidden after the run that had it ends;
one Ophio holds only later is hidden in it then, even where the name is cut
short and while its server has stopped or works outside the project: the
title it was named from is kept, hidden, as long as its preview. A
title longer than 1 KiB, or made so by what is hidden in it, isn't used; the
preview is named by its address. Only servers on this
computer are found, and only programs whose working folder is inside a project,
and still there, are looked at; in nested projects, the deepest one owns the server. A
project folder that was replaced by a link to somewhere else is not looked in.
A server is asked for its page at the address it listens on, a few at a time
and new ones first; one that answers with something other than a web page (an
API, a debugger or a database) is left out, and one that doesn't answer yet is
asked again a few times. A server is not asked when its project has no room
for another preview, or when the person removed its preview. Ophio keeps the
answers of at most 256 running servers; a new one waits until one of them
stops.

When the server stops, its preview leaves Control's list at once and is
removed after 30 seconds unless it starts again. Removing a found preview keeps the server it
shows away while that server runs (once it has stopped, the one found on its
port in its place), not another found on its port meanwhile; started again, it
is found again, at the address and page
title it now has. A server runs while it listens, even while its program works
outside the project for a time, and what it answered and whether its preview
was removed are kept that long, through Ophio starting again until the
computer restarts. A preview the person or the agent added of a page on this
computer follows whether anything listens on its port: it is marked reachable
while something does, and is removed 30 seconds after that stops, or after ten
minutes if nothing was seen listening since it was added or Ophio started,
unless an agent gave the command that starts its server: that one stays,
marked unreachable, until the person removes it.
Its name and address are never changed, and a preview of another computer is
left alone. Pages served on the same port at different
loopback addresses (`127.0.0.1`, `[::1]`, `127.0.0.2`) are different previews;
`localhost` is `127.0.0.1`. Each project still holds at most its usual
number of previews.

Finding previews works on Linux only. Windows doesn't say which folder a
program works in, so add previews there by hand.
