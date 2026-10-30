# Native launch flags

In **Chat**, open the agent picker beside model and effort, then choose **Launch
flags**. Review shows the selected policy before starting. These are bounded
native permission choices, not a free-form command line. Once a task has
started, Chat keeps its flags off the page: tap the task's agent chip, beside
how a message is sent, to see them. A task keeps its flags; start a new task to
use different ones.

**Skip permissions is on by default for new coding Work tasks**, independently
for Claude Code and Codex. In **Settings → Launch flags**, turn either default
off to use native approval prompts instead. Defaults are saved on this phone
across computers; a task-specific selection overrides them. Bypass permits
native tools to act without approval, and Codex bypass also removes its command
sandbox. Use it only for trusted work.

| Choice | Claude Code | Codex |
|---|---|---|
| Skip permissions | `--dangerously-skip-permissions` with `--permission-mode bypassPermissions` | Equivalent to `--dangerously-bypass-approvals-and-sandbox` |
| Ask for approval | `--permission-mode manual` | `--sandbox workspace-write --ask-for-approval on-request` |
| Accept edits | `--permission-mode acceptEdits` | Not offered |
| Sandbox, no prompts | Not offered | `--sandbox workspace-write --ask-for-approval never` |

Manual mode still respects actions Claude Code's own settings already allow.
Sandbox, no prompts refuses actions needing approval rather than waiting for a
person. The workspace-write Codex policies keep network access off in the
command sandbox; they are not whole-process network confinement.

Ophio uses Codex's **app-server**, not its interactive command line. It maps the
shown flags to process-local `approval_policy`/`sandbox_mode`, thread
`approvalPolicy`/`sandbox`, and every turn's `approvalPolicy`/`sandboxPolicy`.
Codex must acknowledge the selected thread policy before the prompt is sent.
No global Claude or Codex configuration is edited.

## Scope and persistence

- **Ask and Plan remain read-only**, even with bypass selected. Their saved
  launch choice applies only when doing Work, including carrying out a plan.
  When the agent asks to run a command, change files or widen its access
  there, the request is declined on the spot and the task notes it; only the
  agent's own questions wait for you.
- The choice is saved with the task and its runs, including Next up,
  continuations, restart recovery and lost-request retries. A retry uses the
  original request, not subsequently edited flags.
- Changing defaults does **not** change existing tasks or explicitly selected
  drafts. Tasks created before this feature keep their earlier native behavior
  when continued; a Next up made from one is a new task, so it starts with this
  phone's defaults. Start a new task to choose different launch flags.
- **A new task made from a Plan or Ask task starts in Work**, through Next up
  or Start a new task, with the source task's flags or, when it has none, this
  phone's defaults. Right above Review's start button it says it is switching
  from Plan to Work and names the flags; choose Plan there to keep it
  read-only. New work a Plan found says the same right above its **Start**. The source task stays in
  Plan. Next up from a stranger's work stays in Plan, as the computer keeps it.
- The project-default toggle saves agent/model/effort only. Launch defaults
  belong to this phone's Settings.
- These flags apply to coding tasks, not computer-use delegation. Pairing,
  device grants, Stop, writer admission and private-desktop boundaries are
  unchanged.
- A companion without `launch_policy_v1` cannot accept selected launch flags.
  The phone asks for an update instead of silently omitting the choice.
  Unsupported native versions or policies are refused rather than substituted.

Run records describe the **requested** policy, not an independent attestation
of every native tool's effects. This feature does not turn bypassed work into a
sandboxed or approval-gated session.
