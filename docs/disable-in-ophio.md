# Disable in Øphio

Library can keep a skill, an agent or an agent file from every agent Øphio
runs, without touching it. **Disable in Øphio** is in a skill's or agent's
About sheet, beside **Edit its file**, and on an agent file's own screen,
below **Send to this device**. The tap shows at once. If the computer refuses,
the item goes back and a toast says why.

What is disabled moves into **Disabled in Øphio**, a section of its own below
each of Library's segments (Agents, Skills and Files) and below the list of
each kind of agent file. Each row says what it is: "Skill · Claude Code",
"Agent · Yours", "Instructions · tidewater-api". Its **Enable** brings it back
at once, and tapping the row opens its file. One that is no longer on the
computer stays in the list, still disabled, so it is kept from agents again
if it comes back.

The computer keeps the list in Øphio's own state. The files are never
changed, and neither are Claude Code's or Codex's own settings. Claude Code,
Codex and anything else used outside Øphio still have everything that's
disabled here.

## What agents run through Øphio don't get

Every run Øphio starts is told what to leave out: tasks, conversations,
reviews, work an agent carries on by itself, and the subagents those runs
spawn. So is Claude Code or Codex started in a terminal from Control: what
applies there is worked out from the terminal's folder, every project whose
folder holds it, whatever project the request named. A
disabled item isn't in the agent's list of skills or agents, calling it
fails, and an instruction file isn't loaded. Each run is given only what
applies to it: its agent's items, the person's own and its project's, and
files that are still there, so a long list never makes a command line too
long.

| Kind | Where it's kept | Claude Code | Codex |
| --- | --- | --- | --- |
| Skill | `~/.claude/skills`, `<project>/.claude/skills` | `skillOverrides: {name: "off"}` | — |
| Skill | `~/.codex/skills` (and `.system`), `~/.agents/skills`, `<project>/.agents/skills`, `<project>/.codex/skills` | — | `skills.config` entry, by its `SKILL.md`, `enabled = false` |
| Command | `~/.claude/commands`, `<project>/.claude/commands` | `skillOverrides`, as Claude Code lists commands as skills (Library still lists and labels it a command) | — |
| Agent | `~/.claude/agents`, `<project>/.claude/agents`, a plugin's agents | `permissions.deny: ["Agent(name)"]` | — |
| Agent | Øphio's own, `~/Ophio/Agents` | No agent lists them itself; Øphio never starts from one | same |
| Instructions | `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, the folders above, nested ones, `~/.claude/CLAUDE.md` | `claudeMdExcludes` | — |
| Rules | `.claude/rules`, `~/.claude/rules` | `claudeMdExcludes` | — |
| Rules | an output style in `~/.claude/output-styles` or `<project>/.claude/output-styles` | `outputStyle: "default"` while it's the one in effect for the run: the run folder's `.claude/settings.local.json`, then `.claude/settings.json` when the run reads project settings, then the person's own. Claude Code 2.1.284 reads project settings only in the folder it runs in, not above it | — |
| Memory | `MEMORY.md` for a folder | `autoMemoryEnabled: false` for runs whose memory it is: Claude Code keeps memory for a repository's main working tree, so this covers runs in its subfolders and its worktrees | — |
| Instructions | `AGENTS.md`, `AGENTS.override.md`, or a file Codex is set to read instead (`project_doc_fallback_filenames`; Library lists those too) in a project | `claudeMdExcludes`: Claude Code reads a folder's `AGENTS.md` when that folder's `CLAUDE.md` isn't loaded | Codex's own project docs are turned off (`project_doc_max_bytes=0`) and Øphio gives the run the rest of the chain from the repository's root to the project as Codex would: in each folder the override, `AGENTS.md` or a fallback file that isn't disabled, within Codex's own size limit. Set to 0 in Codex's settings, Codex reads no project docs and Øphio adds none |

Claude Code's settings go in the one `--settings` Øphio already passes,
merged with what it holds and written to an owner-only file. `claudeMdExcludes`
entries are globs, so each path is written with its special characters in
brackets (`[[]`, `[*]`, `[(]`…), and a file reached through a link is given
both where it was found and where it leads. A skill reached through a link is
named as it is listed: `~/.claude/skills/foo` leading to `~/src/kit/bar` is
`foo` to Claude Code. Every other link to the same skill folder (or command
file) in the folders a run reads is blocked with it, for each agent that
reaches it, whichever link was listed when it was disabled. Codex's are `-c` overrides for that process only, so a
resumed thread and the agents it spawns have them too; its `skills.config`
entries join the person's own rather than replace them. A Codex terminal is
given the rest of the AGENTS.md chain as `developer_instructions`, after the
person's own `developer_instructions` from Codex's settings, kept under the
size one command-line argument can hold (100 KiB, 24 KiB on Windows) by
cutting the chain at a character boundary. All of this
was checked against Claude Code 2.1.284 and Codex 0.153.2.

Øphio itself keeps them too:

- No task starts from a disabled agent or skill, from Library, Chat's
  **Start from** or another task's start. A task that started from one before
  it was disabled isn't given its instructions again.
- Øphio's own command line, run from inside a task, doesn't list a disabled
  item (in the catalog, agent files, Library folders and search, Recently
  deleted, recovery copies or the instructions a task loads), and refuses to
  read it, read or discard its deleted or recovery copy, save, rename, delete or create
  it or a file in a disabled skill's folder, restore it from Recently deleted,
  or download it from Library or from its project. Only the person can read or
  change what's disabled.
- The instructions a task loads show a disabled file as not applied.

## What can't be disabled in Øphio

Some items can't be hidden without changing the file or the agent's own
settings, or aren't given to agents in Øphio at all. Their sheet or screen
says **Can't be disabled in Øphio** with the reason:

- Øphio's built-in skills and agents, and your copies of them: Øphio's own
  setup and GitHub work start from them.
- A Claude Code plugin's skills and commands: Claude Code hides those only by
  turning the whole plugin off. A plugin's agents can be disabled.
- Codex's own `AGENTS.md` in `~/.codex`: Codex always reads it from its own
  folder.
- Codex prompts: agents never see them; only you can send one.
- A memory file beside `MEMORY.md`: Claude Code finds it through `MEMORY.md`,
  so disable that.
- A file inside a skill's folder: disable the skill, from its `SKILL.md`.
- Files no agent Øphio runs reads, such as Hermes's and `GEMINI.md`.

## Limits

- A conversation keeps what its earlier turns were already given. Disabling
  takes effect from its next run, but a resumed session still has its history.
- An agent with a shell can still open a disabled file if it's told the path.
  Øphio keeps the file out of the agent's instructions and listings, and out
  of Øphio's own tools.
- Claude Code hides skills by name, so a skill of the same name kept somewhere
  else is hidden from Claude Code too.
- Claude Code reads a folder's `AGENTS.md` when it doesn't load that folder's
  `CLAUDE.md`, so disabling only `CLAUDE.md` lets Claude Code read the
  `AGENTS.md` beside it, as it would if there were no `CLAUDE.md`. Disable
  both to keep both away.
- An agent already running in a terminal keeps what it started with; one
  started after the change gets it.

## Older computers and phones

A computer advertises this as `disable_in_ophio_v1`. Without it, the phone
shows no Disable in Øphio and no Disabled section. An older phone talking to a
newer computer lists disabled agents and skills as before; the computer
refuses to start from one and says why.

## Checking for agents, skills and files

While Library reads the computer for the first time it shows a turning ring
and "Checking for agents, skills and files…", never an empty list. When it
reads again with a list already shown, the list stays and a small "Checking
again…" line sits above it. A read that fails says so, with Try again. The
line is announced politely to screen readers, and with reduced motion the
ring stands still.
