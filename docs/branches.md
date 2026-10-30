# Branches

In **Chat**, Review's **Where** group has a **Branch** row under the project.
It defaults to **All branches**, which names no branch: the agent works in the
project folder as it is, exactly as before. Tap it to read the project's local
branches from the computer, most recently committed first, with the checked-out
one marked along with its count of uncommitted changes. A filter appears once
there are more than ten.

Choosing a branch tells the agent which one the work is for. Ophio itself never
checks out, creates or switches a branch, so nothing it does can touch the
folder's uncommitted changes. The agent's instructions name the branch on every
run and continued turn. For an existing branch:

> The person wants this task's work done on the Git branch `NAME`. If another
> branch is checked out, switch only if that keeps every uncommitted change;
> otherwise tell them before changing anything.

Claude Code receives it in its appended system prompt after the task's mission
and mode; Codex receives it in the thread's developer instructions.

## New branch

The sheet's **New branch** row opens a name field. Spaces become hyphens as
you type, and a name Git won't accept, or one that already exists on the
computer, is explained under the field (a name still being typed, ending in
`/` or `.`, waits until you leave the field). When a search in the list matches
no branch exactly, a **New branch "…"** row offers the search as the name.

The agent makes the branch, not Ophio. Its instructions become:

> The person wants this task's work done on a new Git branch `NAME`. Before
> changing any file, create it from what is checked out and switch to it
> (`git switch -c NAME` keeps every uncommitted change). If it already exists
> and this task didn't make it, tell them before changing anything; if this
> task made it, switch to it only if that keeps every uncommitted change.

- It is made from whatever is checked out when the task actually starts, so a
  queued task branches from where things stand then, and the folder's
  uncommitted changes come with it.
- Ask and Plan runs stay read-only, so they don't make it; a Plan continued as
  Work does.
- Later runs of the same task are told the same instruction, which lets them
  return to the branch the task made.
- A Next up item from a task with a new branch names that branch as an existing
  one.

## Scope and persistence

- The branch is saved with the task and its runs, including continuations,
  restart recovery and lost-request retries. A task keeps its branch; start a
  new task for a different one. Tap the running task's agent chip to see it
  beside the agent and launch flags.
- A **Next up** item starts on its source task's branch; change it, or choose
  All branches, in Review.
- The branch list is read when the sheet opens, not stored. It is capped at the
  200 most recently committed branches. A name carried over from another task
  that no longer exists is still shown, and still sent.
- Only coding tasks take a branch. The companion refuses a name Git would not
  accept as a branch (`git check-ref-format --branch` rules), so a request can
  never pass an option or revision expression to Git.
- Ask and Plan remain read-only; naming a branch does not let them change one.
- A companion without `task_branch_v1` cannot pass a branch on. The Branch row
  is disabled and asks for an update, and the phone refuses to send a task
  naming a branch to it rather than silently dropping the choice.

## Protocol

- `CreateTask.branch` and `Task.branch` (both optional) are `TaskBranch { name,
  new }`. `new` is left off the wire unless true. The companion checks the name
  against Git's rules, plus `HEAD`, which Git refuses as a branch.
- `Query::ProjectBranches { project_id }` returns `BranchListing { current,
  branches, dirty_files, truncated }`, or no listing when the folder is not a
  repository Git can read within two seconds. It needs the device's task grant.
  The read runs Git with optional locks, filesystem monitors and the untracked
  cache turned off, and ignores Git environment variables that could point it
  at another repository.
