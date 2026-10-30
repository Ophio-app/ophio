# Agents, skills and scratch pads

**Library**'s **Agents** and **Skills** segments start a task from one of
three things:

- an **agent**: a personality and instructions the model takes on;
- a **skill**: instructions for one kind of job, in a `SKILL.md`;
- a **scratch pad**: a model on a clean slate, with nothing added.

Tapping one opens the same Review sheet as any task, with the entry's own
agent, model, effort and flags filled in. Everything stays changeable there.
Its ⓘ opens what it is for, what it starts with and its file, with **Start
from this**. At **Home**, the scratch pad heads the agents, and **New agent**
ends them. With a project chosen, the lists hold that project's own agents
and skills first, then Øphio's (its built-in agents and skills, and the
agents made in it), which every project has as its defaults; see
[Built into Øphio](#built-into-øphio). A list longer than eight gets a search. In **Chat**, the composer's **Start from** chip opens
the same list, and the chip then reads, for example, "Agent: Video producer";
its X clears it.

A new conversation in **Chat** says under the project's name what its agent
may read, change and run, and whether it can reach the internet. Below that,
**Start from agent** and **Start from skill** open every agent or skill the
project's catalog holds, with a search that brings up the keyboard only when
tapped. Picking one starts the conversation at once, in Ask, with the entry's
instructions and "Start with *name*." as its first message. The entry's own
agent, model and effort are used; its flags are not, as what a conversation
may reach is what that sentence says. A start that is refused stays
on the new conversation with the reason, as any message does.

Once a task is started, its agent chip in Chat and Tasks shows what it started
from beside the model, effort and flags, as in "Claude Code · Opus · Skill:
imagegen".

Each agent and skill's ⓘ also has **Disable in Øphio**, which keeps it from
every agent Øphio runs without touching its file; a disabled one is listed
apart, below the others, and never started from. See
[Disable in Øphio](disable-in-ophio.md).

## Where the list comes from

The computer's own files are the list. It is read when Library asks for it,
and nothing of it is kept on the phone.

| Kind | Source | Folder |
| --- | --- | --- |
| Agent | Ophio | `~/Ophio/Agents` |
| Agent | Claude Code | `~/.claude/agents` (or `$CLAUDE_CONFIG_DIR/agents`) |
| Agent | Project | `<project>/.claude/agents` |
| Skill | Claude Code | `~/.claude/skills` (or `$CLAUDE_CONFIG_DIR/skills`) |
| Skill | Project | `<project>/.claude/skills` |
| Skill | Codex | `~/.codex/skills`, `~/.codex/skills/.system` (or under `$CODEX_HOME`), `~/.agents/skills` |
| Skill | Project (Codex) | `<project>/.agents/skills`, `<project>/.codex/skills` |
| Agent | Built into Øphio | Written to `setup-skills` in Øphio's data folder when a task starts from one; see [Built into Øphio](#built-into-øphio) |
| Skill | Built into Øphio | Written to `setup-skills` in Øphio's data folder when a task starts from one; see [Built into Øphio](#built-into-øphio), [Setting Øphio up](setup.md) and [GitHub](github.md) |

`$CLAUDE_CONFIG_DIR` and `$CODEX_HOME` count only as full paths. One given
relative to where the tool runs names a different folder in every project, so
that tool's own folder isn't listed, and the tool is given the saved copy of
every agent or skill rather than finding any itself.

An agent is a Markdown file with frontmatter: `name`, `description` and,
optionally, the agent, model, effort and flags it asks for. The text after the
frontmatter is its instructions. A skill is a folder holding a `SKILL.md`
whose frontmatter gives its `name` and `description`. A project's own entries
are listed only while that project is chosen, marked "This project", and in
Library they then come first, followed by Øphio's. Home lists the person's,
the tools' and Øphio's, and no project's. At most 500 entries are listed, a folder is looked through only so
far, and a file larger than 64 KB, or one that leads outside its project, is
not read.

**New agent** writes a file to `~/Ophio/Agents` in the format Claude Code
reads, so it also works if copied to `~/.claude/agents`. It needs the
`library-write` grant, and is written through Library: it is refused if that
folder is in a read-only root or leads into Ophio's own folders, and never
replaces an agent of the same name. Each entry's sheet opens its file in
Library, for editing when the file is inside a writable root.

## Built into Øphio

Øphio ships these with the app, marked **Built into Øphio**, so every project
has them from the start. An app update brings new versions.

**Agents.** Thirteen review the source, one for each of Find & fix's reviewer
profiles in [GitHub](github.md), with the same brief: **Senior engineer**
(Find & fix's Default), **Bug hunter**, **Security researcher**, **Test
engineer**, **Accessibility specialist**, **UI/UX developer**, **Performance
engineer**, **Reliability engineer**, **Code quality reviewer**, **Dependency
auditor**, **Privacy reviewer**, **API reviewer** and **Docs reviewer**. They
review what the request names, or else the whole project starting with its
latest changes, and answer in the chat, most serious first, with where each
problem is, how to see it, its severity and how sure they are. They change
nothing unless asked to fix what they found, and then fix each problem with a
test that fails first. Three more test the running app as a person would:
**Everyday user**, **Novice user** and **Power user**. Each follows the
live-testing skill below, in the persona's shoes, and says in its report how
each problem felt to that person.

**Skills.** The setup skills, listed in [Setting Øphio up](setup.md);
**Give the person a checklist**, which every agent uses for what the person
has to do themselves (see [Checklists](checklist.md)); and **Live-test an
app**, which teaches an agent to test a running app before
saying it works:

- bring the app up itself, from the project's README, manifests and CI
  configuration, in the background, waiting until it answers, and never
  against production or real accounts;
- drive it through what a person sees (a browser for the web, the computer
  for desktop apps, an emulator for phones, the terminal for command-line
  tools and APIs), and say what its tools can't reach rather than guess;
- look before and after every step, and watch the logs as well as the screen;
- do the app's main journey first, then what changed end to end, then explore
  from short charters with tried testing ideas;
- check each problem before reporting it: repeat it, narrow it, and rule out
  its own tools, a common source of false reports;
- leave no mark: no payments or messages to real people, test data named as
  such and removed, and the servers it started stopped;
- report what it saw, with steps, expected and actual results, severity,
  how often it happened and evidence, plus what worked and what it didn't
  test.

It draws on published experience of agents testing apps and on established
exploratory testing practice.

**Editing one.** An entry's sheet has **Edit it**, which copies it into Øphio's
own folders, an agent to `~/Ophio/Agents/<id>.md` and a skill to
`~/Ophio/Skills/<id>/SKILL.md`, and opens the copy in Library. The list then
shows the copy in its place, once, marked "your copy", and tasks start from
it. Øphio's updates never write over a copy. When a newer version ships, the
sheet offers **Keep mine** or **Use the new version**; the new version moves
the person's copy to Library's Recently deleted. A file the person made in
`~/Ophio/Agents` with a built-in agent's id counts as their copy of it, and
**New agent** won't make one with a built-in agent's name. Copying needs the
`library-write` grant.

## How a task starts from it

The phone sends only the entry's source, id and the digest of the file it was
shown. When the task starts, the computer reads the file again and refuses
the start if it is gone or has changed since it was shown, so the phone never
carries instruction text for files on the computer.

- **Claude Code agent** (from a Claude Code folder): `--agent <name>`.
- **Any other agent** (Øphio's own included): its instructions join the
  run's added instructions, after the task's mission and branch: Claude
  Code's appended system prompt, or Codex's developer instructions.
- **Claude Code skill on Claude Code:** the first message is
  `/<name> <request>`.
- **Codex skill on Codex:** the first message is `$<name> <request>`, with the
  skill attached, when the installed Codex accepts skill input.
- **A skill the chosen agent can't find itself** (a Claude Code skill on
  Codex, or the reverse): its instructions join the run's added instructions,
  with its folder named for the paths they mention. Claude Code is also given
  the skill's folder with `--add-dir`.
- **Scratch pad:** nothing is added.

A project's own agent or skill is used natively only when the run loads the
project's settings; otherwise its instructions are added as above. Plan and
Ask runs always add them.

The start is saved with the task, with the instructions as they were read.
Continued turns, retries, restarts and queued tasks keep it. Before each run
the computer looks again: if the file the agent would load by name has
changed, has gone, or now shares its name with another, the saved
instructions are added instead, so a run never follows text that wasn't
reviewed. Settings in an agent's frontmatter, such as its tools, then don't
apply. Only a skill's `SKILL.md` is saved; other files in its folder are read
as they are when the run uses them. A file in the agents folders that can't be
read sends every agent to its saved instructions, as the computer can't tell
whether it shares a name. The check and the agent's own read of the file are
moments apart; a change in between is not caught.

A **Next up** task starts as the task it follows did until another start is
chosen in Review (tap it, or **Start from** once it's removed) or that one is
removed. The computer copies the start saved with that task, never reading the
file again; Review says so. A skill is invoked by name only in a task's first
run; a continued session, Plan carried out in Work included, is given the
skill's saved instructions instead, as it may never have invoked the skill. A
Codex that can't be sent a skill is given its instructions in every run.

## On its own

Review's **Where** group offers **In a project** or **On its own**. On its
own makes a new folder, `~/Ophio/<yyyy-mm-dd>-<name>`, numbered if taken,
and registers it as a one-off. It has no branch. Set `one_offs_path` (an
absolute path) in the companion's `config.toml` to use another folder; it
must not overlap Ophio's own data, config or runtime folders, judged where its
links lead. A folder that does is refused before anything is made in it.

One-offs are listed under **One-offs** in the project sheet and in Tasks,
apart from projects. The one-offs folder and `~/Ophio/Agents` appear in
Library on their own unless a configured root already covers them. A
one-off's sheet has **Open its folder**, and **Make it a project**, which
moves it in among the projects.

**Delete it…**, in a one-off's sheet or beside it in the project sheet's
list, asks first, then deletes the one-off in full: it leaves Øphio's lists,
its tasks are archived, and its folder is deleted from the computer with
everything in it. It needs admin, waits until nothing a task in it started is
still running, and is never done from an agent's task. Øphio deletes only a
real folder directly in the one-offs folder: links in it are deleted, never
followed, and a one-off whose folder has become a link or been moved elsewhere
is refused and kept, to be removed as a project instead. A folder already gone
just leaves the lists. The task records stay in Øphio's own data, unlisted,
and agents keep their own session records.

A project's own agent or skill can start only in its project.

Downloading what a one-off made to the phone uses the `transfers` grant, as
for any Library file.

## Older companions

Library's agents and skills need a companion that advertises `launchers_v1`.
With an older one they say to update the companion, and nothing is sent that it
can't carry out. Its files work as before.
