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
