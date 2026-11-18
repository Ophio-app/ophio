# Agent files in Library

Library's **Agent files** lists every Markdown file the agents on the computer
are given, wherever it is kept, by what it is for:

- **Instructions**, read at the start of every task: `CLAUDE.md`, `AGENTS.md`
  and the like;
- **Agents** a task can hand work to;
- **Skills**;
- **Commands**, prompts sent by name;
- **Rules and styles**;
- **Memory**, what agents wrote down to remember.

Each file's screen has **Disable in Øphio**, which keeps it from every agent
Øphio runs without changing it, or says why it can't be; disabled files are
listed apart, below the others. See [Disable in Øphio](disable-in-ophio.md).

Each kind opens a list grouped by where the files apply: an agent wherever it
works, a project, or a folder. Tapping a file opens it in Library, where it
reads and edits like any other. At Home every file is listed; with a project
chosen, only that project's, as Library's folders are.

## Finding a file

**Search files** is at the top of Library. Tapping it switches to **Files**;
searching names or words shows each matching file once, with its folder and
up to three matching lines. **Show more matching lines** expands the rest in
place. Tap the file to open it for reading or editing.

## Where they are found

| Agent | Folder | Files |
| --- | --- | --- |
| Ophio | `~/Ophio/Agents` | agents |
| Claude Code | `~/.claude` (or `$CLAUDE_CONFIG_DIR`) | `CLAUDE.md`, `agents/`, `skills/`, `commands/`, `rules/`, `output-styles/`, installed plugins' skills, agents and commands, and each folder's memory in `projects/<folder>/memory/` |
| Codex | `~/.codex` (or `$CODEX_HOME`) | `AGENTS.md`, `AGENTS.override.md`, `prompts/`, `skills/` |
| Codex | `~/.agents/skills` | skills |
| Hermes | `~/.hermes` (or `$HERMES_HOME`) | `SOUL.md`, `memories/`, `skills/` |
| Any | each project | `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `AGENTS.override.md`, `.hermes.md`, `HERMES.md`, `GEMINI.md`, `.claude/agents`, `.claude/skills`, `.claude/commands`, `.claude/rules`, `.claude/output-styles`, `.agents/skills` |
| Claude Code, Codex | folders inside a project | `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `AGENTS.override.md`, up to six folders deep, skipping `node_modules`, `target`, `dist`, `build` and other folders tools fill |
| Claude Code | folders above a project | `CLAUDE.md`, `CLAUDE.local.md` |

A skill or agent linked in from elsewhere is reached where it is kept.
Memory for a folder that is no longer there isn't listed. The computer looks
through these folders again when the list is asked for and the last look is
more than 30 seconds old, and when a project is added, removed or made from a
one-off.

## What stays out of reach

Each folder joins Library reaching only these files: in a skill's folder, only
its Markdown. Settings, credentials, past sessions and history kept beside
them, such as `settings.json`, `.credentials.json`, `history.jsonl`, session
transcripts or `auth.json`, are never listed, read or written. Nothing in a
skill's folder is run.

A folder Ophio keeps its own data in, or one around it, is never reached. A
file in a folder already in Library opens through that folder. These folders
aren't listed under **Folders on the computer**, and aren't offered as
somewhere to send files.

Reading uses the `library-read` grant and saving `library-write`, as for any
Library file.

On Windows, Library can list and read files on FAT32 and exFAT drives, but cannot
save, create, rename or remove files there. Those drive formats do not support
the safe saving Library needs. To edit through Library, keep the files on a drive
with the required features, such as a normal NTFS drive. A folder on removable
media stays protected from being moved while it is registered in Library.

## In a limited chat

On Linux and on a Mac, a chat kept to a project or to reading runs in a
sandbox with its own copy of each agent's settings folder. Your own
instructions and skills are shared into that copy read-only: Claude Code's
`CLAUDE.md`, `rules/` and `skills/`, and Codex's `AGENTS.md`,
`AGENTS.override.md` and `skills/`. The agent can read them but can't change,
remove or rename them. Nothing else in `~/.claude` or `~/.codex` is shared:
settings, plugins, MCP servers, hooks, agents, commands, history, sessions,
memory and Codex's `rules/` (which say what Codex may do without asking) stay
out, and only the agent's own sign-in file is shared to write. On a Mac, if
the agent renews its sign-in during a reply and Øphio stops before the reply
ends (a restart or an update, say), the next reply in that chat puts the renewed
sign-in back first, so you aren't signed out; if you signed in again yourself
meanwhile, yours is kept, and if you signed out it stays signed out. If it
can't be put back yet (the keychain is locked, say), the reply starts anyway
using the renewed sign-in, and the end of that reply tries again. Only the
next reply in the same chat does this: a reply in a different chat that starts
first is given the sign-in you had before, which may have to be signed in
again.
Claude Code
fetches the skills in `skills/synced/` from your account itself as it starts,
into the chat's own copy, so yours aren't shared. The project's own
instructions and skills are seen as part of the project, as always.
Windows has no limited chats, so none of this applies there.

Files you attach to a chat message are copied, read-only, into a folder Øphio
keeps for that conversation, outside the project. A limited chat can read its
own conversation's folder and no other; see [Files in Chat](chat-files.md).

A skill linked into `skills/` from elsewhere is shared from where it is kept
if that is a folder with a `SKILL.md`; a linked instruction file must be an
ordinary file. A link is left out if it leads through another link, or to the
home folder, a folder holding the home folder or the project, an agent's
settings folder, or anything else the chat is kept from. Links inside a skill's
own folder are not followed out of it: the place they name is not shared, so
they work only if the chat could already read it. Something left out is
skipped without stopping the chat, and the computer's log names it once.
Skills added or removed are picked up from the next turn. A shared skill is
read from wherever you keep it, so a chat that may change the project a skill
lives in can change what every later chat reads from that skill, as any chat
outside a limited one always could. Something the agent itself left in its
copy where one of yours goes is never deleted. On Linux a folder or file of
the same kind stays where it is, hidden under your shared one, and shows again
when sharing stops; anything else, and anything there on a Mac, is moved aside
under a name starting `.ophio-aside-`. Anything you
[disabled in Øphio](disable-in-ophio.md) isn't shared at all, since the agent
finds what is shared in the chat's own copy, where it would be listed under
another path; a folder of skills or rules holding something disabled is left
out whole.

A chat that can't change any files, because it was given no folder to change
and no write access to the whole computer, isn't shown your skills that say
they change files. A skill says so when its `allowed-tools` names `Write`,
`Edit`, `MultiEdit`, `NotebookEdit` or `apply_patch`, or when its front matter
has this, which both agents accept:

```yaml
metadata:
  ophio-access: write
```

A skill that says neither is shared, as is a folder of skills kept together
inside `skills/`. This filter does not cover a project's own skills or the
account skills Claude Code fetches into `skills/synced/`. Those may still cost
a read-only chat tokens even when it cannot use them. The sandbox, not a
skill's description or tool list, enforces what the chat may change or reach.

Claude Code gets its **Skill** tool when the sandbox confirms it placed at
least one shared user skill. This applies to ordinary limited conversations,
including read-only ones: those use Claude Code's Work tool list inside the
sandbox. Computer-use and relay runs do not gain this tool. Native Ask and
Plan runs keep their existing reading-only tool lists and settings restrictions;
runs outside the sandbox do not gain Skill through this change. No additional
settings, hooks, plugins, agent definitions or tool servers are shared.
