# GitHub

With a project's GitHub workflow on, an agent working in it opens a draft pull
request for its branch, keeps its checks passing, and says on GitHub that it did the
work. Øphio uses the GitHub account signed in under **Accounts** (see
[Accounts](accounts.md)); there is no Øphio GitHub App, bot service or
webhook, and nothing needs a public address.

A project qualifies when its folder is a Git repository whose `origin` is on
github.com. A companion without `github_v1` has no GitHub settings; the phone
hides them and asks for an update where they would be.

## Requests from outside the project

Before each GitHub turn, Øphio checks the repository's visibility and the
authors of the content supplied to the agent: issue and pull request text,
changes, comments and reviews. Only an owner, member or collaborator is
trusted. Other associations, missing information and bots or apps are
outside content, in private repositories too. Øphio's own posts use the
signed-in person's account; a disclosure in a comment does not prove who
wrote it. One outside comment holds the whole turn, and the hold stays with
later turns even if that comment is removed. Older sessions and queued
handovers without a recorded trust decision are held too.

The computer holds these tasks to their project's folder. Work can change
that folder; planning and reviews can only read it. Other project folders
and the rest of the computer stay hidden. Internet and local network access
are off; only the agent's provider answers. The agent receives a snapshot
from GitHub and receives no other service accounts or host tool bridge.
It cannot post comments, open or close issues, push branches, resolve
threads, mark pull requests ready or merge through Øphio's GitHub tools.
It can inspect the supplied content, read the project and, in Work, prepare
changes locally. Findings and checks wait for you to review before they are
published or move a pull request on. Sorting can still keep its local
categories, but cannot close duplicates. A review using a screen cannot run
under project-only limits.

If conversation sandboxes are unavailable, the turn waits in **Needs you**
at **Start without project limits?** It explains that the request came from
someone outside the project and this computer cannot hold the agent to the
project. Only **Allow** starts that turn with the task's ordinary computer
and internet access; declining, stopping or leaving it unanswered starts
no agent. Later turns ask again. GitHub tools remain off.

With a sandbox, widen access through the task's existing access bar, just as
for a conversation: choose the folders and network access it needs. Only a
person can change that access; GitHub text and agent tool calls cannot.
Turns that have read only trusted content keep their usual behavior.

## Where it lives

A Git project on GitHub has two rows at the top of Tasks' **Project** section:

- **Issues**: the repository's open issues and pull requests.
- **Find & fix**: reviews that find problems, the fixes they queue, and sorting
  the open issues.

Each opens its own screen in Tasks, headed by the repository; the gear beside
it opens the settings, and back returns to wherever you opened it from. Issues
also has **Merge once checks pass**, a switch. Off, the agent opens draft pull
requests, keeps their checks passing and leaves the merging to you. On, it merges its own
once every check passes and no review comment is open.

The top row of Tasks is **New**. For a Git project on GitHub it
asks which first: **New task** (Review, as before), **New issue** or **New
review**, each on a screen of its own. Other projects keep **New task**.

While the project's workflow is off, **Set up GitHub** sits directly under
**New**. It opens **GitHub settings**, which says what's missing (no GitHub
account under Accounts, with a way there, or an `origin` that isn't on
github.com) and holds **Use GitHub for this project**, the one switch that
turns it on. The row goes once it's on. To turn it off again, **GitHub
settings** is the last of the project's GitHub rows in the **Project**
section, the same sheet the gear opens. Issues and Find & fix also say so with
a **Turn on** button while it's off. **Settings › GitHub** lists the
computer's Git projects and opens the same screen for each, and `ophio setup`
sets each one's workflow under Extras.

### Merge once checks pass

Turning the switch on asks first, in **Merge once checks pass?**: what merging by
itself means for the default branch, and what the computer found risky about
it, such as nothing protecting the branch, no checks on pull requests, or
pushes to it that deploy. Nothing changes until you confirm, and **Not now**
leaves the switch off. Turning it off takes one tap. It is offered only when
the signed-in account administers the repository.

The question is asked again each time it is turned on, and when the project's
`origin` names another repository. Turned on at the computer instead, the
screen shows **Before it merges by itself** until you answer it with **Merge
once checks pass** or **I'll merge them**; nothing merges by itself meanwhile. The
warning names the repository it is for, and is refused if `origin` has moved
since it was shown.

### Merge together

**Pull requests** has **Merge several together** once two or more are open.
Tap the ones to merge, then **Merge N together**; the phone's unlock is the
go-ahead for every merge, and needs the tasks and approvals grants on a
device, not a chat. Up to twenty at a time.

Øphio tests them together first, as the line of Merge once checks pass does:
it merges each into a branch made from the default branch and opens a draft
pull request of it, "Øphio merge check: …", for the repository's checks to
run on. Green, each merges in turn, as itself, so each keeps its own place in
GitHub's history; a draft is marked ready first. Red, they are halved and
each half tested alone until what breaks is found. One already up to date
with the default branch whose own checks passed, or a repository that runs
no checks, merges without a merge check. If something else merges into the
default branch meanwhile, what hasn't merged yet is tested again with it.
Øphio checks the default branch immediately before each merge, including
between requests in the same batch. If GitHub confirms a merge without
identifying its commit, the remaining requests are checked again from a
fresh base; the current default head isn't assumed to be what passed.

One that breaks the others, conflicts, gets a new commit after you picked
it, is closed, now merges into another branch, or that GitHub refuses to
merge five times in a row is left out with the reason, and the rest carry
on. **Merging together** at the top of Pull requests shows each one as
Waiting, Testing, Merged or Left out, with **See the merge check** while
there is one. **Stop** prevents further merge requests. A merge already sent
to GitHub can finish; its result stays visible and the remaining pull requests
are left out. The stop request and any unresolved merge survive a companion
restart. If the merge's response was lost, Øphio reads that pull request before
finalizing Stop. While GitHub still shows it open or cannot be reached, the phone
says it is checking the merge already sent; no further merge is sent. A confirmed
merge remains Merged, and a closed request is left out.
**Done** clears it once it has ended. While it runs, the line of Merge once
checks pass waits.

### Issues

**Run all** first, then **New issue**, then the repository's open issues in
three lists:

- **Agent can take it**: what the last sort said an agent can fix, and
  anything an agent is on or that waits in Next up, whatever the sort said.
  What is being worked comes first, then what waits, then the rest.
- **Needs you**: only an issue whose task has a question for you (a
  decision, a plan to OK, a failure or a capped review), one whose fixer
  couldn't reproduce it or found it already fixed, one an agent checked
  before (it carries `agent-not-reproduced` and no task is on it), one the
  Sort says needs a decision, or one you put there. The Sort writes what it
  needs as a question, in plain words for someone who hasn't read the
  issue, naming the choices and the one it would pick; the issue's sheet
  shows it under **The Sort asks**, and what you write before **Start** is
  the answer the agent works from. A fix that is only
  waiting its turn in Next up is not in it, and Run all leaves all of these
  alone.
- **Not sorted**: what no sort has read yet.

Each row shows how its task stands (Working, In Next up, Stopped, Needs you,
Checks pass) from the moment a task starts on it, without waiting for the list to
be read again. Under Needs you, the row says instead what happened ("The
agent found this already fixed", or the Sort's question) and its pill the
next step: **Close it?**, **Answer**, **Stuck**, **Failed**, **Look**,
**Your call** or **On hold**. Øphio's own labels (`agent`, `agent-found`,
`agent-not-reproduced`) aren't shown.

#### Run all

One card, to sort, start, pause and stop all of the project's issue work.
The first time, **Run all** opens **Run all defaults** (the gear opens it
again):

- **Fixed by**: the agent, model, effort and launch flags each fix starts
  with.
- **Reviewed by**, optional: once a fix's pull request passes its checks, this agent
  reviews the change, without editing files, before it merges or is handed
  to you. It also sorts new issues. Off, the project's **Check before
  merging** setting decides, and the fixer sorts.

**Save and run all** keeps them on the computer for the project; after that,
**Run all** asks nothing more. It sorts what isn't sorted yet, then queues a
fix in Next up for each issue in Agent can take it, one after another, and
takes on any fix already waiting there. The tap is the go-ahead: its fixes
start by themselves in their turn, however long they wait, and are not asked
about again when Next up is edited. What a sort finds the agent can take
joins the run. It ends when nothing is left, and says how the last one went
("Last run: 3 done").

While it runs, the card says what it is fixing and how far it has got, with
a bar and a chip per issue (working, waiting, needs you, done or out) that
opens its task. **Sort** sorts new issues without starting anything.

- **Pause** holds Next up and stops the fixes under way; **Resume** carries
  them on where they stopped, then Next up.
- **Stop** asks first ("Don't show me again" turns that off). It stops the
  fixes under way and takes the rest out of Next up; Next up itself is left
  as it was. Tapping **Run all** again takes up what is left.

If something that isn't Run all's own waits at the head of Next up for your
go-ahead, or is blocked there, nothing behind it starts: the card says
**Held up in Next up** and offers **Review and start** or **Open it**.
While another task is changing the project's files, or something ahead of it
still waits, its Review offers **Start when it's free** instead of a start: the
go-ahead is given now and it starts by itself in its turn.
If another task is changing files in the project, the fixes wait for it to
finish; when that task waits on you, the card says **Held up by another
task** and **Open it** takes you to it.
Issues someone who can't write to the repository wrote are still fixed in
Plan, and wait for your OK of the plan.

#### An issue

One under Needs you opens on a card that says what happened, what the agent
or the Sort said in its own words (a long finding is cut to a few lines until
**Show all**), and **What to do**, with the buttons that do it:

- Found already fixed, couldn't reproduce, or checked before: **Close issue**
  closes it on GitHub in your name (as completed when it was found fixed,
  else as not planned); a fix of it waiting in Next up leaves it, and the
  fixer's alert is answered. **It still happens** opens the start form with
  **How to make it happen**; starting that fix answers the old alert too.
  Where the computer can't close issues yet, **Close on GitHub** opens it
  there.
- A question: **Answer it** opens the task. A failure: **Open its task** or
  **Try again**. A stuck pull request or anything else the agent raised:
  **Open its task**.
- The Sort's call, or one you put there: the usual start form below, and
  **Close issue** on the card.

Otherwise, or after **It still happens**, it shows:

- **Listed under**: Agent can take it, Needs you or Not sorted. Moving it is
  yours, and a later sort keeps it. Moved to Needs you, a fix of it that
  hasn't started leaves Next up and Run all, and waits for you; moved to
  Agent can take it, Run all fixes it in turn.
- While a task is on it, the sheet says so and offers **Open its task**. If
  that task waits in Next up for a go-ahead, it says **Waiting for your
  go-ahead**, with **Review and start** or **Let it start by itself**;
  otherwise where it stands in Next up.
- **Worked by**: the agent, model and effort; Run all's **Fixed by** if it
  has one, else the last one you chose for issues in this project, else the
  project's default.
- **More direction**, optional. The agent is asked only "Fix #N: title" and
  reads the issue on GitHub; anything written here is added after it. Nothing
  from an earlier task is carried in.
- **Have another agent review the code**, off by default. On, **Reviewed by**
  picks the agent, a different one from the fixer unless you choose
  otherwise: once the pull request passes its checks, it reviews the change in Plan,
  without editing files, before it merges or is handed to you. Off, the
  project's **Check before merging** setting decides.
- **Start** begins now, in Work mode, on its own branch; it opens a draft pull
  request that says "Closes" only if the fix covers the issue. While another
  task is changing the project's files it says **Start when free** and starts
  by itself when that task ends.
- **Add to Next up** queues it to start by itself when its turn comes.
- **Edit first** opens Review with the issue linked; **Open on GitHub**.

An issue written by someone who can't write to the repository starts in Plan,
and its plan needs your OK before anything changes. Below the issues, **Pull
requests** lists the open ones, where you can take one over or have it
checked.

### New issue

A **Title** (one line, at most 256 characters) and a **Description**, filed
as written, in your name. **Then**:

- **Start work** (the default): an agent starts on it once it's filed, or
  when the project is free.
- **Add to Next up**: queued to start by itself when its turn comes.
- **Just file it**: filed, and nothing else happens.

**Worked by** and **Have another agent review the code** are as for an open
issue. If filing works but the start doesn't, the screen says the issue is
filed and the button only retries the start; it never files it twice. Issues
are filed from the phone only, not from a chat channel, since they go out in
your name. A computer older than `github_issues_v1` can't file them or take a
reviewer, and the phone says to update it.

### Find & fix

**New review** opens the review screen below. **Sort open issues** has an
agent read the open issues and sort what it can take from what needs you; it
opens in Review first. Under them, **Reviews** lists the latest reviews, and
**Fixes from reviews** the fixes they queued; older ones are in Tasks' **Finished** and **Archived** lists.

Without GitHub, a review writes FINDINGS.md. When it finishes, a verifier
in Work, run by the same agent, adds one `Verdict:` line to each finding
no verifier has judged as it now reads (confirmed, with how it was shown;
rejected; or uncertain) and changes nothing else. A `Verdict:` line the
review wrote itself, or one under a finding changed since a verifier left
it, doesn't count: Øphio remembers each finding as a verifier left it, and
the last `Verdict:` line wins. The verifier takes its turn in Next up first,
and a fix waiting there for FINDINGS.md waits on it, however long, across a
restart too. Once the verifier ends, Øphio reads the file: if every finding
it was asked about has its verdict, the fix starts and fixes only what was
confirmed. A finding is the `## [ ] 1. Title` heading and what follows it, up
to the next finding or the next `#` or `##` heading. Øphio finds line ends
and code blocks in the file as any Markdown viewer does, and as your phone
does, so both list the same findings. If the review or its verifier failed
or was stopped, the file can't be read, a finding was left without a
verdict, or a line reads like a finding's heading but isn't written as one
(in a quote or a list, with an odd box, or behind characters that don't
show, or in a code block that is set off only by its indent or never
closed), so no verifier would be asked about it, that fix no longer starts
by itself and you're told once. Next up pauses only when such a fix is
waiting in it. A review that wrote no findings starts no verifier. When
Øphio lost track of the review or its verifier and you mark it completed or
failed, all of this happens as if it had ended that way by itself. A
read-only question you ask the review afterwards is none of this: however
it ends, it starts no verifier and holds no fix.

### New review

One agent reviews the project; a second, starting fresh, tries to disprove
each finding, and only the ones it confirms are filed and fixed. The rest
are kept on the review with the reason, and never posted.

- **Reviewer**: a profile that tells the reviewing agent what to look for.
  **Default** is a careful senior engineer looking at anything that matters.
  The others that read the code are **Bug hunter**, **Security
  researcher**, **Test engineer**, **Accessibility specialist**, **UI/UX
  developer**, **Performance engineer**, **Reliability engineer**, **Code
  quality**, **Dependency auditor**, **Privacy reviewer**, **API reviewer**
  and **Docs reviewer**. Under **Uses the app** are three that use it
  instead; see below. Each is also one of Øphio's built-in agents in
  Library, to review or test in a chat or task that answers there instead of
  filing issues; see [Agents, skills and scratch pads](launchers.md#built-into-øphio).
- **What it reviews**: the whole project, a folder, or the commits since its
  last review.
- **Agents**: who **Reviews** (read-only, in Plan) and who **Fixes**. The
  phone remembers both per project.
- **Issues it files**: **Most issues to file** (1 to 20), most serious first;
  it stops at the limit. Ones already filed don't count. Then:

| After filing | What happens |
| --- | --- |
| Fix by itself | Each becomes "Fix #N" in Next up and starts by itself, one after another, once the project is free. Archiving, renaming or removing other tasks in Next up leaves that go-ahead in place; moving fixes, or editing one or what it waits on, asks again of it and of what waits on it. |
| Queue fixes | Each becomes "Fix #N" in Next up and waits for you to start it. |
| Only file | The issues are filed and nothing else happens. Start them from Issues. |

**Start review** starts it now; **Edit first** opens it in Review. A computer
older than `github_review_v1` offers four profiles and the project's own
limit and choice.

#### Reviewers that use the app

**Everyday user**, **Novice user** and **Power user** don't read the source:
they use the app on a screen, as that person would, and file what got in the
way. The everyday user is a typical person doing the main tasks. The novice
finds technology hard and is upset by anything that isn't simple. The power
user learns fast, doesn't mind complexity, and is annoyed by every extra
click, step or confirmation.

- The review runs as computer use. Claude Code hands the screen to its
  computer specialist; Codex uses the computer tools itself, with nothing
  else in its run. An agent that can't use a computer is shown but can't be
  chosen to **Use it**.
- The reviewer acts on the screen only while you watch it in Control, as any
  computer use does. Leave that screen and the agent pauses within seconds:
  its next action waits, and Øphio tells you "Agent paused" with **Resume
  agent**. Resuming from anywhere else opens the screen it works on. The
  action that waited isn't taken, since the screen may have changed: the
  agent gets the screen as it is then and carries on from it, rather than
  being told its action was refused. Nobody resuming it within 10 minutes
  ends its turn, saying what it couldn't try. Once the review ends, its
  screen no longer reads "Agent paused". Øphio gives it the screen only
  once the app is started, so the time you spend allowing the start doesn't
  count.
- **What it uses** is the **Whole app** or **What's new**, the screens the
  commits since the last review touch. There is no folder.
- **How to open it** is optional: a web address, or how you start it.
- Øphio starts the app before the review, when the computer has terminals:
  1. An agent reads the project and works out the command that starts it,
     from its README, manifests and build files. It runs as a read-only Ask
     task named "How to start" and the project, with the review's agent and
     model, and can't change or run anything. The review waits for it.
  2. Øphio asks you once whether to run that command, showing it, the
     project's folder and the address it answers at. A command longer than
     one line of 1000 characters, holding a key or token, or with an address
     that isn't a page on this computer isn't put to you.
  3. Once you allow it, Øphio runs it in a terminal named "App:" and the
     project, in Control, in the project's folder. It waits up to five
     minutes for the address to answer. With no address, it waits briefly.
     The reviewer is told how the app was started and where to open it, and
     not to start another copy. When no browser is open on the screen, it
     starts one from the app menu or opens the address from a terminal.
  4. The allowed command is kept for the project. Later reviews reuse it
     without asking, and reuse the terminal while it still runs the command
     and the app still answers when it has an HTTP address to check. A desktop
     app without an address keeps its running terminal and existing state;
     the reviewer is told to use its existing window. On Linux with a delegated
     cgroup, and on Windows, Øphio checks whether the terminal's process
     container still has anything besides its shell. If the app has exited
     and only the original shell remains, the next review starts the command
     again. A command that replaces the shell with `exec` is preserved. On
     Linux, an empty scan of nested cgroups cannot prove the app has exited:
     it may have moved between groups while they were read.
     When that check is uncertain, unavailable or fails, the terminal is preserved and
     the reviewer checks for an existing window before starting anything.
     A closed terminal or a changed command starts a new copy. When **How to
     open it** says something, the command is worked out and asked about again.

  When there is nothing to start (a phone app or a hosted site), you don't
  allow it, or nobody answers within 15 minutes, the review goes on without
  it. Stop while it waits ends the review.
- The reviewer brings the app up itself when Øphio didn't, or when the app
  stops answering. It opens the app when the task says how, finds it on the
  screen, in the app menu or the browser, or starts it in a terminal on the
  screen from the project's folder. It may build it or install what it needs
  there, but changes none of its files. It asks you only for what only you
  can give, like a sign-in or a device.
- **Screen it uses** picks the display or window to control, as computer use
  does; one that's held or can't take input says why and can't be chosen, and
  the only usable one is chosen for you.
- It starts now, in Work mode, since it controls the screen; it needs the
  project's writer free and computer use available, and changes no file.
  There is no **Edit first**.
- The agent in Chat can't start these: only the phone asks which screen.

Findings come only from what the specialist saw, with steps from opening the
app, and are filed and fixed like any other review's. A computer without
`github_uat_v1` doesn't offer them.

### Settings

Behind the gear, and **GitHub settings** in Tasks:

- The repository: private or public, the account it works as, and whether
  the default branch is protected. An admin can **Protect** it from there:
  GitHub then refuses any change that doesn't come through a pull request,
  admins' included, and requires the checks recent pull requests ran. It
  needs no approving review, so the agent's own pull requests can still
  merge once their checks pass. Change or remove it in the repository's settings on GitHub.
- **Use GitHub for this project**: off, nothing is linked or posted.
- **Branch prefix** (`ophio/`): branches the agent makes start with it.
- **Issues per review** (5, at most 20) and **After a review files issues**:
  what a review does when it doesn't say.
- **Agent label**: what the agent opens gets the `agent` label, and its
  findings `agent-found`. Only on repositories you own; the label is made the
  first time it's used.
- **Check before merging** (on): when the agent's pull request passes its checks, its
  changes are reviewed, in Plan, before it merges or is handed to you: by
  the reviewer whose issue it fixes, or else by the task's own agent. Once
  corrections are pushed, what changed is rechecked, three rounds at most
  (see [Watching a pull request](#watching-a-pull-request)).

The settings are kept on the computer, per project, with Run all's **Fixed
by** and **Reviewed by**; no other model is stored there. Issues anyone else
wrote never start by themselves.

## Planning from Chat

A Work run in Chat that isn't itself working a GitHub item can plan the
person's GitHub work, so "set up issues for these five things" works by
voice. Its bridge tools:

- `ophio_agents`: the agents on the computer, whether each is signed in, their
  models and efforts, the project's default agent, whether another task is
  working in the project, and its GitHub setup and review profiles.
- `github_new_issue`: file one issue, written so an agent that never saw the
  conversation can do it. Filing starts nothing; it is checked for
  duplicates as `github_issue` is, and a repository that isn't yours asks
  first.
- `github_start_issue`: put an agent on an issue, in Next up, waiting for you
  or, with `start`, starting once the project is free.
- `github_start_review`: start a review with a profile, folder, reviewer,
  fixer, most issues and what follows. The reviewers that use the app are
  refused here, since only the phone picks their screen.

The agent is told to use the last two only when you ask for the work to
start. The three GitHub tools are refused while the project's workflow is off.

## Agents asking each other

An agent's own subagent tool runs only its own models, so Claude Code can't
ask Codex to review its change that way, nor Codex ask Claude Code. Every Work
run, a GitHub item's included, has `ask_agent` instead: it names an agent from
`ophio_agents`, optionally a model and effort, and the whole question, since
the agent asked sees none of the conversation.

Ophio starts the agent asked as a task of its own in the same project, in Ask:
read-only, without the browser or your launch flags, beside whatever holds the
project. Its last message is passed back to the asking agent, in its turn if
it is still working, or as its next turn once the project lets it run. Until
then the asking task shows **Waiting** and the project is free; if no answer
comes within a day, Ophio wakes it anyway. A run may ask ten times.

The task asked says which task asked it and raises nothing for you, in Needs
you or as a notification: its answer is the other agent's to read. Stop
waiting on the asking task, or stopping or archiving it, leaves the answer in
the task asked, where it is yours like any finished task. Ask runs have no
bridge, so the agent asked can't ask in turn.

## Linking a task

In **Chat**, Review's **Where** group has a **GitHub** row under **Branch**
while the project's workflow is on. Its sheet offers:

| Link | What the task does | Mode |
| --- | --- | --- |
| Not linked | No pull request, nothing posted on GitHub. | Any |
| New pull request | Opens a draft pull request for its branch and keeps its checks passing. The default for Work. | Work |
| Fix an issue | Takes the issue's title, and a branch named for it. | Work |
| Take over a pull request | Continues its branch and keeps its checks passing. | Work |
| Stack on PR #N | Starts from that pull request's branch, and its pull request targets it. Offered in Review for a Next up item whose task has an open pull request. | Work |
| Find issues | A review, as in New review: reads the whole project, a folder, or the commits since its last run, and files an issue for each finding. | Plan |
| Sort issues | Sorts the open issues and pull requests. Nothing starts by itself. | Plan |
| Check a pull request | Reviews its changes and reports back. | Plan |

- An issue or pull request is chosen from the open ones, or typed as `#12`,
  `12`, or a link to it in this repository.
- Ask and Plan default to Not linked. A Next up item from a task with an open
  pull request defaults to that pull request.
- Find, Sort issues and Check choose Plan, and Review won't start them in Work.
  An issue or pull request written by someone who can't write to the
  repository starts in Plan too; the computer decides this when the task is
  created, whatever was chosen.
- Taking over a pull request uses its own branch, so the Branch row is fixed.
  A new pull request or a fixed issue gets a new branch the agent names, with
  the project's prefix, unless you choose one.
- A link belongs to the repository it was made on. If the project's `origin`
  later names another, the task posts nothing, its pull request isn't
  watched, and it won't start again in Work; link a new task.
- Find shows the **Reviewer** profile and two roles: **Find with**, the
  task's own agent, and **Fix with**, the agent, model and effort each "Fix
  #N" it queues will use. The phone remembers both per project.

## What the agent may do

A Work run of a linked task gets bridge tools, and only these post to GitHub:

- `github_pr`: open its pull request from the branch it pushed, or rewrite the
  title and description. Øphio adds the disclosure line, the agent label and
  the link to the task, and opens it as a draft.
- `github_comment`: comment on the pull request, or reply in a review thread.
  It may resolve a thread a bot started once that finding is fixed; a person's
  thread is theirs to resolve.
- `github_issue`: file one finding, once it isn't a duplicate (see
  [Duplicates](#duplicates)). On a public repository a security finding is
  reported privately instead.
- `github_cannot_reproduce`: say an issue couldn't be reproduced, with what
  was tried, or with `already_fixed` that the code already fixes it and by
  which commit or pull request. It comments on the issue, labels it
  `agent-not-reproduced`, and waits under Needs you with what it found,
  saved as the task's `finding`, for you to close the issue or say how it
  still happens.
- `github_ready` and `github_merge`, with Merge once checks pass on: mark its pull
  request ready, and squash-merge the head commit it checked. A stacked pull
  request waits until the one under it has merged and GitHub has moved it onto
  the default branch. With **Check before merging** on, both wait until Øphio
  has had the change checked and what the check found is fixed, so the agent
  stops once its checks pass. Each reads the settings and the task's pull request
  again just before it acts, and refuses if either changed. A pull request
  from a fork never merges by itself.

Each post goes as the signed-in account, carries a line naming the agent and
model "in Øphio, working for @you", and has the run's keys and GitHub tokens
removed first. On a repository that isn't yours, each post is asked first.

The run's own `gh` passes reads and refuses writes. With Git 2.54 or later
on the computer, its pushes go through a check that refuses force pushes,
deletions, tags, any repository but the task's, and the default branch, as it
was when the run began and as GitHub says it is at the push. Older Git doesn't
run the check, so there the default branch's own protection on GitHub is all
that holds. The run is told the project's rules alongside its task. A run that can't push to a
pull request's branch continues in a new pull request and says so on the old
one, which stays open for you to close.

## Duplicates

Agents working on one repository meet the same problems, and say them in
their own words. Before any issue is filed, Øphio reads the repository's
open issues and the ones closed in the last 30 days from GitHub's lists,
which have an issue the moment it is filed, where search may not yet, and
compares titles word by word, so "has a long path" and "path is long" read
the same. A name from the code that both mention, a file or an identifier,
counts toward it.

- An issue with the same title, give or take case and spacing, is never
  filed again.
- From `github_issue` and `github_new_issue`, the agent is shown up to five
  issues that read alike, as data, and nothing is filed or counted. It reads
  them; if its problem is different, it calls again with `distinct_from`,
  the numbers it read, and says in the body how it differs. The agent is
  also told to look with `gh issue list --state all --search` before
  filing.
- A finder's findings are filed after its run, with no agent left to ask,
  so only an issue that says the same thing stops one: its title reads the
  same, and the two don't point to different places (other files, or lines
  far apart in one). Pointing to the same lines doesn't make two issues the
  same, since two problems can sit a few lines apart; it only puts that
  issue among the ones an agent is shown. A file named without a line points
  to nowhere in particular. Within one run, two findings in the same file, a
  few lines apart, are one problem, and the second isn't checked or filed,
  only if what their titles say beyond the kind of problem ("panics",
  "missing test") reads nearly the same, or they tell what goes wrong in
  the same words; "panics on empty input" and "panics on a malformed
  header" are two, and so are "export of an empty note crashes" and
  "export of a large note crashes". Two findings wrongly kept cost a second look; two wrongly merged
  lose one.

What gets past that, a Sort finds: given the open issues, it marks one that
says the same problem as an older open issue, with the same cause, as a
duplicate of it. If an agent filed it, as your account, and no agent has
started on it, Øphio closes it on GitHub as a duplicate of the older one,
with a "Duplicate of #N" comment, and takes its fix out of Next up. One you
filed, or one an agent is already fixing, waits in Needs you with "Repeats
#N: close it as a duplicate?". The Sort's task says which it closed and
which it left.

## Review rules

A project can tell its reviews what matters to it in `REVIEW.md` files: one
at the root for the whole project, and one in a folder for what changes
under it. Every review Øphio runs is given them: a finder run, a check of a
pull request, the verifier that checks what a review found, and a review
started from Find & fix without GitHub. Ordinary tasks and conversations are
not. Claude and Codex are given them the same way, quoted at the end of the
first message of each new session.

They are read with Git from the branch the change goes into, as last
fetched: `origin/<base>` for a pull request, and otherwise the project's
default branch (`origin/HEAD`, else `origin/main`, `origin/master`, `main`,
`master`, then `HEAD`). Never from the working tree or the change itself, so a
change can't loosen its own review: one that edits a `REVIEW.md` is reviewed
under the base's rules, and the review is told so. The root's file comes
first, then the folders of the changed paths, shallower first; a review of
the whole project reads every one. At most eight files are quoted to the
reviewer, 8 KiB of each and 16 KiB in all, cut at a line end. Protected paths,
below, are read from every file that applies, up to 1,024 files of 1 MiB
each and 4,096 protected paths in all. A path in a folder
named like Git's own pattern syntax, such as `:!x`, is read as it is spelled.
Nothing they name is followed: no includes, links or hooks, and no filter the
repository's Git settings name is run, and no replacement object stands in
for what Git stored. `origin/<base>` always means origin's
branch as last fetched, never a tag or local branch that happens to share
its name. A verifier is given exactly the rules the review it checks was
given, and a finder looking at one pull request gets that pull request's,
as a check does.

The review is told the rules are the maintainers' guidance and grant
nothing: they can't grant permissions, choose agents or models, skip an
approval, change the form of the report, or tell the reviewer to approve
or say nothing. The reviewer still reports every real problem, and says so
if the rules asked for any of that. The task records the commit the rules
came from, the files read, whether any was cut or edited by the change, and
why none were used when none were; the phone shows it with the review.

A `## Protected paths` section names the project's own sensitive paths, one
to a line, a glob and a plain label: `- crates/billing/** — payments`. An em
dash, en dash or a hyphen between spaces separates them, and a glob with no
label is its own. Globs read as `.gitignore` reads them, ignoring case; one
in a folder's `REVIEW.md` applies within that folder. A change touching one
needs your approval, below; for that, they are read from GitHub rather than
this computer.

## Changes that need your approval

Some changes wait for you to approve their exact commit before they merge,
whatever any agent says: ones touching permissions or access control,
pairing or signing in, credentials or secrets, sandboxing, software updates,
signing, how the project is built or released (workflows and CI settings,
build scripts and files named `build`, container files, and the package
manifests and lock files of the common ecosystems: any `*.lock`, `*.lockb`
or `*.lockfile`; Cargo, npm, Yarn, pnpm (with `.pnpmfile.cjs` and its
workspace file), Bun, Deno, Go (with `go.work`), Python (`pyproject.toml`,
`setup.py`, `Pipfile`, requirement, constraint and `requirements*.in`
files, Conda's `environment.yml`), Ruby, PHP's Composer, Dart's `pubspec.yaml`,
Elixir's `mix.exs`, .NET project files, `Directory.*.props`,
`packages.config` and `nuget.config`, Haskell's Cabal and Stack files,
Nix files, Meson, vcpkg and Conan; Gradle and its wrapper, Maven, Swift
packages, CocoaPods and Carthage, the Xcode project and its build settings
and schemes; and `.gitmodules`),
installers or packaging, stored data that can't be
undone (migrations, SQL, database schemas), the review rules themselves, or
a path the project's rules protect. Øphio tells them from the changed
paths' folder and file names, for any project, and documentation only
where its exact name is listed, such as `REVIEW.md`. A
reviewer can add a reason in its own words, and can never take one away: a
reason it gave stays with the pull request's later commits, and with every
task about that pull request, whichever of them it was raised on, in any
project on the same repository.

Øphio works it out once for each commit of the pull request and of its base,
from GitHub's list of the pull request's files at that commit. The project's
protected paths come from the `REVIEW.md` files that apply at the commit
GitHub says the base branch is at now, read from GitHub itself: nothing in
the project's folder, where agents work, decides them, neither where its
branches point nor what its stored Git objects hold, and the base needn't
be fetched. When the base moves on, it is worked out again. If it can't
read the list, the pull request moved on while it was read or changes more
than 300 files, the rules can't be had whole (more than 1,024 folders to
look in, a binary or cut-short `REVIEW.md`, or one longer than is read),
it can't tell, and the pull request waits for you the same way; that is
what the commit is, so it is remembered. A failure that may pass, such as
GitHub not answering where the base is, its files or its rules, changes
nothing: what each task about the pull request had stays, no alert is
raised, nothing merges it on that pass, and it is worked out the next time.

While one waits, nothing merges it but you: not Merge once checks pass, the
line, an agent's own merge tool, or the later steps of Merge together. Its
pull request goes to Needs you with the reason, and the phone's merge
button reads **Approve and merge**. Your merge, pinned to the commit you saw,
is the approval, and covers that commit alone; a newer one is worked out
again. If you tap Merge on a commit Øphio hadn't yet worked out, or your
phone showed less than the commit needs, it says what it found and asks you
to look again first. Picking pull requests for Merge
together approves each at the commit you picked, for what its row showed (a
row whose review stopped names its approval too); one that needs more than
that by the time its turn comes is left out, for you to merge on its own. No
agent counts as you,
and there is no second approver to stand in for one.

## Watching a pull request

Once a linked Work task's turn is over, Øphio reads its pull request every
minute while something is under way and every ten minutes otherwise. New
failing checks, review threads and conflicts go back to the agent as data, in
rounds. After five rounds, or six hours without a new commit, it stops and
asks you. While it waits for you it still reads the pull request every ten
minutes, only to see it merged or closed, on GitHub or anywhere else: then it
no longer needs you and leaves Needs you. A check that was cancelled, went stale or ended any other way than
passing counts as failing; only a skipped or neutral one counts as neither.
GitHub gives the first hundred checks of a pull request; one with more shows
the rest as pending, so it doesn't merge by itself. A task that opens another
pull request leaves the old one's rounds and check behind, and what is read
of the old one afterwards isn't news to it. Turning the workflow Off holds a
round or check that hadn't started; it starts once the workflow is back on and
the pull request still needs it.

With Merge once checks pass on, a pull request whose checks pass waits in line
to merge, and the line moves only once nothing in the project is running or
waiting in Next up, so nothing merges under a run still working from the old
default branch. Øphio first tests the line together: it merges each pull
request into a branch made from the default branch and opens a draft pull
request of it, "Øphio merge check: …", for the repository's checks to run on,
and closes it itself. Green, each merges in turn; red, the line is halved and
each half tested alone until the one that breaks is found, and what failed goes
back to its agent as a round. One already up to date with the default branch,
or a repository that runs no checks, merges without a merge check. A merge
GitHub refuses five times in a row is left with you. Before every merge,
Øphio checks that the default head is still the tested base or the commit
from its preceding merge. An unexpected update, or a merge whose commit
GitHub didn't identify, sends the remaining requests back for checking
against the latest default branch. A moved base triggers that check even
when GitHub reports the next pull request as behind, without counting it
as a merge refusal. Øphio rechecks merge permission after reading the base,
so switching to Drafts or Off during that read prevents the merge.

Where the default branch's rules want branches up to date, a pull request whose
checks pass but is behind is brought up to date, as GitHub's **Update branch**
does, once a commit, and goes on once its checks pass again. That happens only
once the agent's turn is over, never for a branch in a fork or a pull request
held for you under Needs you, and whether Merge once checks pass is on or not.
If GitHub refuses, it stays behind, and the pull request says why.

With **Check before merging** on, a pull request whose checks pass is checked by a
Plan task before it is handed over or, with Merge once checks pass on, merged. A task that
moves on to another pull request has that one checked too. What the check
finds goes first to a verifier: a fresh read-only task, run by the same
agent, that tries to disprove each finding (is it intended, was it there
before the change, can it really happen) and says how it knows. Only what it
confirms goes on; a finding it rejects or can't settle stays with the check,
with its reason, and is never posted. The pull request waits in Checking
meanwhile, within the check's six hours, and a verifier that fails or can't
be read leaves it with you. What was confirmed is posted as one comment,
"🤖 Review by …", and all of it becomes the agent's next round, after any
earlier check's findings not yet handed over; if the computer stops
before that round starts, it starts once when it's back, and nothing merges
meanwhile. It never approves. A
check that fails, doesn't start, can't be read in full, finds more than
twenty things, or hasn't finished six hours after it was asked for leaves the
pull request with you, under Needs you, and it doesn't merge by itself. So does one that would start after `origin` was changed to
another repository. Such a check also takes back what an earlier check
allowed, or one still under way would, so once you pick the task up again it
is checked again before it merges. Only the newest check asked for can let a
pull request through: an older one that finishes late still hands over what
it found, and allows nothing. A check you start with **Check a pull
request** counts the same, whether Check before merging is on or not: the
agent's pull request doesn't merge, and isn't
handed to you as ready, until that check reports. What a check finds goes to
the task speaking for the pull request when it ends. If the computer stops as
a check ends, its report is acted on once when it's back, and a verifier
started or still to start goes on.

The review runs in rounds, kept on the pull request's task, with the commit
each round read. The first reviews the whole change. When the head moves
after a round, a recheck is given each finding still open, to say with
evidence whether it is corrected, and what changed since the last round: the
commits, and their changes to the pull request's own files. Work merged in
from the base branch, in files the change doesn't touch, isn't reviewed
again, unless GitHub's lists of changed files are as long as it gives, and
may leave some out. Every problem the verifier confirms is posted, kept and
handed over, even one that looks like a problem still open. A finding a
round didn't answer for, as when a whole review follows a recheck that
didn't report, is asked about again in a recheck of the same commit, not
taken for one the agent pushed nothing for. A pull request gets three rounds; it is left with you, under Needs
you, with a plain reason, when a problem marked corrected comes back, when
the last round ends with problems still open, when the change moves after
the last round, or when the agent was handed problems and pushed nothing for
them. Picking the task up again starts the rounds over and keeps what was
found. One review runs at a time: a commit pushed while one runs is the next
round's, and a check you ask for while a round reviews the same commit is
part of that round, not another. The verifier and the rechecks raise nothing of their own; you hear
when the pull request is ready, merged or needs you.

When more than one task is linked to the same pull request, the newest one
speaks for it: its rounds, its check and its holds count, and an older task
isn't watched, and can't merge, around a hold on the newer one. A round
charged to an older task that hasn't started by the time a newer one is
linked goes back. A round, or marking the pull request ready, never lifts a
hold put on while GitHub was being read. A round given back because the pull
request moved on before it started returns only what it changed, so a hold
put on meanwhile stays, and a newer check's findings follow the ones given
back.

## Finder and fixer

A Find run reads in Plan and answers in a structured block; one that uses the
app runs as computer use in Work, changes no file, and answers the same way. The block is
for Øphio alone: the task's conversation and summary don't show it. Its
findings go to a verifier first, as a check's do (see [Watching a pull
request](#watching-a-pull-request)); only confirmed ones are filed, and the
finder's alert says how many were set aside after checking and why. A
verifier that fails, or whose answer can't be read, files nothing and says
so. Øphio files the confirmed findings as issues itself, through the same path as `github_issue`, skips
ones already filed or said the same way, and, unless the review only files, queues a "Fix #N" in
Next up for each, with the Fix with agent. A finding it can't read is left in the task, and its alert says
so. If the computer stops part way through, nothing is filed or queued
twice; a private report it was sending isn't sent again, and stays in the
task. Those start by themselves only when the review says Fix by itself and Next
up isn't paused, only for that run's own findings, and only within a day of
the finder's end; acting on a run again never extends that. A fix that ends
up linked to another repository, because `origin` was changed while it was
being filed, never starts by itself, and the finder's alert says it waits for
you. A Sort issues run is given
the open issues and pull requests as data, and its sorting is kept for the
list; an issue an agent filed twice is closed (see [Duplicates](#duplicates)). What these runs report is never carried out in Work.

On GitHub, the issue names the finder, the pull request's last line names
both ("🤖 Found by Sol 6.1, fixed by Opus 5.5 in Øphio, working for @you"),
and commits carry the fixer's co-author trailer. On the phone, a finder run
raises one Finished alert naming what it filed, and nothing when it finds
nothing.

## On the phone

A linked task shows a chip under its title, such as "PR #214 · Ready · Checks
pass · 1 open thread". It opens the pull request's sheet: who found, fixed and
checked it, its checks, open review threads and description, what stops it
merging, and buttons to **Fix failing checks** or **Address comments**, which
open Review with the pull request linked and the task's agent.

**Mark ready** and **Squash and merge** (or the repository's allowed method)
are yours, done by the companion with your account. Both need the phone's
unlock, and need the tasks grant on a device, not a chat. Merge names the
commit it will merge, "Merges commit 5b7e2c9 into main, and nothing newer",
and the companion reads GitHub again and refuses if the head moved: "has a new
commit, … since you looked. Check it again before merging." Both name the
repository the sheet was read from, and are refused if the project's `origin`
names another one by then.

## Not built

- Starting work by itself on issues other people wrote, or posting reviews of
  other people's pull requests.
- An account per project: every project uses the one signed in under
  Accounts.
- An "Open PR" shortcut on a finished task that wasn't linked.
- GitLab, Bitbucket and other hosts.

## Protocol

- Capabilities `github_v1`, `github_review_v1` for the twelve review
  profiles and a review's own limit and follow-up, and `github_uat_v1` for
  the `everyday_user`, `novice_user` and `power_user` focuses. A `find` with
  one of those is a `computer_use` task with a `target_id`, scope `project`
  or `since_last_run`, and an agent with a computer specialist. `Query::GithubProject { project_id }`, `GithubItems {
  project_id, list, filter }` and `GithubPr { project_id, number }` read the repository,
  its open issues or pull requests with their link state, and one pull
  request's checks, threads, head and mergeability.
- `CreateTask.github` is a `GithubLink`: `issue`, `pr`, `new_pr`, `stack`,
  `find { scope, focus, fix_with, max_issues, fix_findings }`, `triage` or
  `check`. It is saved with the
  task like its branch.
- `Command::Github` carries `set_settings`, `answer_warning`,
  `protect_branch`, `mark_pr_ready`,
  `merge_pr { expected_head, method, approval_shown? }` (`approval_shown`
  is what the phone showed as needing approval, empty when nothing; a need
  with anything else asks the person to look again, and an older phone
  leaves it out),
  `run_issues { project_id, step }` (`start`, `pause`, `resume` or `stop`)
  `set_group { project_id, number, pr, group? }` (no group puts it back
  in Not sorted) and `close_issue { project_id, repo, number, done }`
  (`done` closes it as completed, else as not planned; refused if `origin`
  is no longer `repo`, or for a pull request). `mark_pr_ready`,
  `run_issues`, `set_group` and `close_issue` need the tasks grant, and `merge_pr` the approvals grant as well; the rest need
  admin. Ready and merge are refused for chats, and a program Øphio started
  can't merge, run all, move an issue or close one.
- Capability `github_run_v1`: the workflow's `fix_with` and `review_with`
  (Run all's defaults), `GithubProjectView.run` (the latest Run all: its
  fixes, the Sort it waits on, paused, ended, stopped), and the item status
  `queued` (waiting in Next up) and `stopped`. A computer without it can't
  run all, and the phone says to update it.
- Capability `github_close_v1`: `close_issue`, `TaskGithub.finding` and
  `already_fixed`, and `GithubItem.grouped_by_you`. Without it the phone
  offers **Close on GitHub** instead.
- Capability `github_together_v1`: `GithubItem.head`,
  `GithubProjectView.together` (the latest Merge together: each pull
  request's state and why it was left out, the merge check, ended, stopped),
  and the actions
  `merge_together { project_id, repo, prs: [{ number, head, approval_shown? }] }`
  (`approval_shown` as for `merge_pr`, for each pick)
  and `stop_together { project_id }`. `merge_together` needs the tasks and
  approvals grants and `stop_together` the tasks grant; both are refused for
  chats and for programs Øphio started. A computer without it doesn't offer
  Merge several together.
- Capability `review_rules_v1`: `CreateTask.review` (`find`, `verify` or
  `recheck`, set by whoever creates the task, never by an agent), which gives
  a task that isn't a conversation the project's review rules and saves
  `Task.review` (its role, the rules used, the review it judges or its
  verifier, and its findings with their verdicts). `TaskGithub.review` is a
  pull request's rounds of review (the commit covered, findings, agents,
  rules, the task doing the latest round, and why it stopped, if it did), and
  `TaskGithub.approval` what makes the change need the person's approval,
  for one exact commit.
- Events `github_changed` and `pr_changed` bump the phone's GitHub revision.
