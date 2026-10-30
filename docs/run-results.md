# What a run's result records

When an agent's turn ends, Ophio keeps what came of it on the run: its
summary, the files it changed, the checks it ran, what is still running or
unknown, and the report the agent ends its last message with. Three facts are
kept apart, because a run can stop normally and still leave broken work:

- **Finished.** `state: completed` means only that the agent stopped normally.
- **Verified.** Whether its work was checked after its last change, read from
  the checks Ophio saw its tools run (`outcome.verification`).
- **Needed a follow-up.** Whether a later run on the same task had to change
  things after this one finished as done (`follow_up`).

None of the three comes from what the agent says about its work. Its report
(`outcome.report`) decides what Ophio offers next, such as Carry on or the new
work it found, never whether the work is verified.

## Checks

A check is a shell command the agent ran that runs a project's tests, build,
linter or type check: `cargo test`, `cargo clippy`, `cargo fmt --check`,
`npm test`, `npm run build`, `npx tsc`, `npx vitest`, `pytest`, `go test`,
`make check` and the like, including after `cd dir &&`, variable assignments,
`env` or `timeout`. A formatter or linter run that rewrites files
(`cargo fmt`, `--fix`, `--write`) is a change, not a check. Codex reports
commands wrapped in the shell that ran them (`/usr/bin/bash -lc '…'`); Ophio
records the command inside.

Each check has a status:

| Status | Meaning |
|---|---|
| passed | The command line exited 0, and that status is the check's, or the check's and the commands `&&` joins to it. |
| failed | The command line exited non-zero and that status is the check's own, or the check's own output shows it failed. |
| unknown | It ran, but its result can't be told: the status may be another command's, the tool reported none, it went on in the background or it was interrupted. `detail` says which. |
| not run | It never ran: the person or a policy denied it, Codex declined it, or a hook stopped it. |

A command line's exit status is the check's own when nothing after the check
can set it: the check is last, and no other command is joined to it.

- **Joined with `&&`.** In `cargo test && git commit -m x`, exit 0 means
  every command passed, the check too. A status other than 0 may be the other
  command's (nothing to commit, say), so the check is unknown unless its own
  output shows it failed. Checks joined to each other (`cargo fmt --check &&
  cargo test`) and `cd`, `export` or `set` before a check don't count as other
  commands.
- **Piped.** Piped into `tail`, `grep` or `head`, the status is the last
  command's, so the check is unknown, unless its own output shows it failed.
  With `set -o pipefail` earlier in the line (and no `set +o pipefail` after
  it), a check piped into `tail`, `cat` or `tee` keeps its own status.
- **Followed** by `;`, `||` or another line, or sent to the background, the
  status is another command's, so the check is unknown.

A check's output shows it failed when it holds a failure line of the runner
that printed it: `test result: FAILED` and `error: could not compile` from
Cargo, `error[E…]`, `error TS…` from TypeScript, `FAIL` and a `Tests … failed`
summary from Vitest and Jest, a pytest summary with `failed`, `npm ERR!`,
`make: ***` and the like. Output never shows that a check passed. Agents are
told to run checks so their status is their own. On Windows, where Codex runs
commands in PowerShell, they are told only not to pipe them.

How the status is read from each agent:

- **Codex** (`codex app-server`): the finished `commandExecution` item's
  `exitCode`, and its `status` (`declined` is not run). Its `aggregatedOutput`
  is the output.
- **Claude Code** (`stream-json`): a Bash tool result reports no exit status
  when the command exits 0. Its result is the output (`stdout`, `stderr`) and
  `is_error` is false. A non-zero exit comes back as an error whose text starts
  `Exit code N`. Claude Code reads some non-zero statuses as no error, such as
  `grep` finding nothing, and says so in `returnCodeInterpretation`; those are
  unknown. So are results with `backgroundTaskId` or `interrupted`. A
  `PreToolUse` hook error means a hook stopped the command, so it is not run.

`exit_code` is the whole command line's status. `output` keeps the end of what
the check printed, at most 20 lines and 2 KiB, without terminal colour codes.
Where that bound cuts into a line, the cut moves on to the next space and `… `
marks it. No secret Ophio hides holds a space, so no end of one is left
showing. Like the rest of the outcome, the output is then masked for the run's
secrets. The receipt shows each check's output under it, collapsed.

When the outcome has to fit its size limit, checks come before files.
Outputs go first, starting with the oldest checks that decide nothing. Then
long commands are shortened, then checks that decide nothing go. A check that
decides the verification goes only when nothing else is left. The run then
can't read as verified.

## Verified

Ophio counts the run's possible file changes as they happen:

- native file edits: Codex `fileChange` items and Claude Code's Edit, Write,
  MultiEdit and NotebookEdit, when they succeed;
- every shell command, from Codex or Claude Code's Bash, that may have changed
  files and ran, whether or not it succeeded. One Codex declined or a hook
  stopped never ran. A command may have changed files unless every part of
  it is known only to read: listing, reading and searching files (`ls`, `cat`,
  `grep`, `rg`, `find` without `-delete` or `-exec`, `sed` without `-i`),
  Git commands that leave the working tree as it is (`status`, `diff`, `log`,
  `show`, `add`, `commit`, `push`, `fetch` and the like), and checks that only
  read (`cargo fmt --check`, not `cargo fmt`). Writing to a file with `>` or
  `tee` is a change, unless the file is in `/tmp`, `$TMPDIR` or `~/.cache`. A
  command Ophio can't read, such as one with a here-document, is a change.
  Claude Code's own report of what a command changed (`bashEditDiff`) isn't
  used: it is only sent in some modes.
- a command still running, or a tool with no answer, when the outcome is
  read.

A check's place among the changes is taken when it starts, so an edit that
lands while a long check runs is after it. On one command line, a change
before the check (`cargo fmt && cargo test`) comes before it, and one after it
(`cargo test && cargo fmt`) after. Each check records whether it started after
the last change (`after_last_change`).

The checks after the last change are the final checks. Each command line
counts once, by its latest result, so a flaky test that is run again counts by
its rerun. Checks that never ran are left out. Then:

| `verification.status` | When |
|---|---|
| verified | At least one final check, every one passed, every earlier failure was run again, and nothing was lost. |
| failed | A final check failed. |
| unknown | None failed, but one has no result of its own, a check that failed earlier wasn't run again, or evidence of a change or a check was lost (`incomplete` says which). |
| not checked | No check ran after the last change, or none ran at all. |

`passed`, `failed` and `unknown` count the final checks. `failed_not_rerun`
counts checks whose latest result failed before the last change.
`failed_before_changes` says that a check failed before the run changed any
file, so the work it started from was already failing.

Evidence is lost when Ophio couldn't read or keep it, and any loss stops a run
reading as verified:

- a Claude Code tool call over 64 KiB, or more than 128 under way at once;
- more than 128 checks, or a check command over 16 KiB;
- Codex steps after the first 1,024 of a run;
- a command that may change files sent to the background, whose changes may
  land after any later check.

A check counts among the run's doubts until a later run of the same check
replaces its result. A failure that the run fixed and checked again isn't a
doubt. A final failure reads as "N checks failed", and an earlier one never run
again as "N checks failed and weren't run again". The receipt, task rows and
notification say the same. The receipt says whether the run is verified beside
its outcome and above its checks. It says nothing for a run that changed no
files and ran no checks, such as an Ask.

## Needed a follow-up

A run finished as done when it was a Work run, ended normally, had no
outcome recorded by a person, and didn't report work left or a need for the
person. When the next run on the same task ends normally, Ophio records on the
run before it whether that one needed a follow-up:

- **check failed**: a check in the next run failed before that run changed
  any file;
- **changed files**: otherwise, the next run changed files.

A next run that was stopped, failed or was marked by a person says nothing of
the one before. Only the run just before is looked at. A run that left work,
needed the person, made a plan or answered a question expects what comes
next. The follow-up records the later run's id and when it ended. It is
written once, when the later run ends, with an indexed lookup of the task's
previous run.

## What it can't see

- Changes made by a command Ophio reads as only reading, for example a
  program that writes files though its name says it reads, or a script a
  check runs. Changes made by helper agents in Codex threads of their own
  aren't seen either.
- Read-only rules are conservative the other way too: a command Ophio doesn't
  know, such as a dev server, counts as a change, so a run that ends with one
  reads as not checked.
- A later run on the same task that changes files because the person asked
  for something new also counts as a follow-up. The reason and the later run
  are kept so that the two can be told apart by reading that run.
- Runs recorded before these facts were kept have no `verification`,
  `after_last_change`, `output` or `follow_up`. Their checks keep the status
  they were recorded with. All failed checks count among their doubts, as
  before.

Ophio has no statistics view of runs. To count results across runs, read
`outcome.verification.status` and `follow_up` rather than `state`.
