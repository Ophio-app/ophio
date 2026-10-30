# Security reports

Report suspected vulnerabilities privately, in either of two ways:

- **GitHub:** on the official public repository,
  [github.com/ophio-app/ophio](https://github.com/ophio-app/ophio), open
  **Security → Report a vulnerability**. Only the maintainers see the report.
  Forks and other copies may not have this enabled.
- **Email:** security@ophio.app.

Do not write to commit authors instead, and do not post an exploit, device
key, pairing offer, account token or private project content in a public
issue.

Include the affected version/platform, a minimal synthetic reproduction, expected
and observed behavior, and the scope of access required. Redact secrets, typed
text, screenshots and personal paths. No response time or coordinated disclosure
deadline is promised during the beta.

Pairing, TLS certificate pinning, device grants and desktop input leases are
security boundaries. Network reachability alone does not authorize control.
A terminal belongs to the device that opened it: no other device can list it,
attach to it or read its output, and revoking the device ends its shells. The
owner at the computer can still list and close every terminal.
For an exposed or compromised device, revoke its pairing on the computer; stop
Øphio if you cannot establish who still has access. A certificate change
requires verification and re-pinning, never automatic acceptance.

Release candidates and their locally generated test certificates are not
production-signed releases. Consult [SUPPORT.md](SUPPORT.md) for qualification
limits and [BUILDING.md](BUILDING.md) for distribution checks.

## The owner's controls on the computer

The `ophio` command reaches the running program through a socket (a named pipe
on Windows) that only your user account can open. Pairing devices, changing
their permissions and answering approvals all happen there.

Coding agents, the programs they run and terminals opened from a device also
run as your user, and Øphio's setup skills use this channel from a task to
check the computer, install voice, connect a chat or add a project. What only
you may decide is refused to them: confirming a pairing, changing what a device
may do or ending its access, answering an approval or a chat's request to link,
starting or instructing a task, letting queued tasks start without asking,
giving tasks an account, a login or a project's committed settings, editing a
login (its sites, which tasks may use it and whether you are asked first),
changing how a project's work reaches GitHub or merging a pull request,
unlocking the desktop, deleting a one-off's folder, revealing an account's key
or a login's password, and quitting Øphio.
Øphio judges each connecting program by what the operating system reports
about it, never by what it says:

- On Linux, a program is treated as started by Øphio when Øphio is among its
  ancestors, when it is in a control group Øphio made for an agent run or a
  device's terminal, or, when Øphio runs as its installed service, when it is
  in Øphio's control group. Detaching from its parent does not take a program
  out of any of these groups.
- On Windows, everything Øphio starts belongs to a job object that it cannot
  leave.

A terminal you opened yourself on the computer is not affected.

## Project files that change what an agent may do

Claude Code reads settings from a project's folder (`.claude/settings.json` and
`.claude/settings.local.json`). They can let the agent use tools without asking
and run commands (hooks) whenever it works, and in the non-interactive mode
Øphio uses, Claude Code applies them without asking whether you trust the
folder. Work runs started from a device therefore ignore them, and the project's
own skills and agent definitions with them, unless you turn them on for that
project at the computer with `ophio projects settings <PROJECT-ID> on`. The
CLAUDE.md files in the project folder apply either way. The app's instruction
list shows which of these files exist and whether runs use them.

## Accepted limits

These are known and deliberate. Reports that show one of them is weaker than
described here are welcome.

- **Programs running as your user are not isolated from one another.** A program
  Øphio started can still reach what is refused to it through a program it did
  not start: by asking the system to start one (`systemd-run`, a scheduled task,
  a terminal multiplexer that moves itself into its own control group), by
  editing your shell's startup files, or on Windows by starting a program under
  another of your processes. Keep agents in their sandboxes and review what they
  change outside the project.
- **On Linux, started from a shell, Øphio shares that shell's control group.**
  Agent runs and devices' terminals are still recognised by the control groups
  they get, but where the computer cannot give one (no cgroup v2, or a control
  group Øphio may not divide), a program they started is recognised only while
  it is still attached to Øphio. Run Øphio as its installed service to cover
  programs that detach there.
- **Files sent to the computer are kept.** Files a device sends to Øphio's own
  storage, screen captures and other task evidence are not removed
  automatically. A device with the transfers permission can send files until
  the computer has 512 MiB of space left; unfinished uploads are limited
  separately and expire after a day.
- **Before a device signs in, the computer may hold one message per connection.**
  It accepts up to 256 connections, 32 from one network address, and each may
  send up to 1 MiB before its size is checked. Nothing in such a message is
  interpreted until it passes that check.
