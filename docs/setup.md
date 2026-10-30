# Setting Øphio up

`ophio setup` goes through everything Øphio needs on a computer, one step at a
time. The Linux installer starts it when it finishes in a terminal, and
`ophio` with nothing after it starts it on a computer that has not run Øphio
yet, or whose setup was left part way. On Windows, run `.\ophio.exe setup` in
PowerShell in the extracted folder.

Setup draws a full screen in a terminal that can show one. `--plain` shows
plain text, one screen after another, as a screen reader or a basic terminal
reads best; `--still` turns the animation off. Any step can be left for later,
and the rest of setup skipped. What it has done is kept in `setup.json` in
Øphio's data folder, so a setup left part way carries on where it was. Run
again after it has finished, it shows every step again and undoes nothing
unless something is changed.

## The steps

1. **Keep Øphio running.** Starts Øphio now and whenever you log in, or just
   for now.
2. **Reach.** How the phone reaches this computer: Tailscale, or this Wi‑Fi
   only, which adds this computer's address to `config.toml` once you allow
   it.
3. **Agents.** Finds Claude Code and Codex, offers to install one that is
   missing and to sign in to one that is signed out, and asks which one new
   projects start with. It then asks once whether Øphio keeps them up to
   date, and recommends it (see
   [Keeping Claude Code and Codex up to date](upgrading.md#keeping-claude-code-and-codex-up-to-date)).
4. **Phone.** Gets the app onto the phone and pairs it. Setup shows how to
   install it, or installs it from this computer over the phone's Wireless
   debugging (below), and asks whether the phone gets alerts while the app is
   closed. The phone then shows this computer's fingerprint and a code; pair
   only if it shows the same code as setup. Setup then asks what the phone may
   do.
5. **Extras.** Ticks for adding projects, choosing where a checklist's files
   go, watching and controlling the desktop, speaking instead of typing,
   Telegram and Discord bots, other accounts, the GitHub workflow, agent
   logins, and voice control. Each shows what it does and the command it runs
   before running it. Where files go is ticked until it has been offered once:
   it lists this phone, each Taildrop device, folders on this computer such as
   a synced Google Drive or Dropbox, and drives set up in rclone, or takes a
   folder of your own (see [Checklists](checklist.md#getting-a-steps-file)).
   The GitHub
   workflow goes through each Git project on GitHub and asks for its mode, as
   Tasks › GitHub in the app does, warning before it merges once checks pass (see
   [GitHub](github.md)); it needs the GitHub account signed in under
   Accounts first. Agent logins runs `ophio agent-logins enable`, which shows
   its notice and turns them on only when you type `yes` (see
   [Agent logins](agent-logins.md)).
6. **Check.** Runs the same checks as `ophio doctor` and says what needs
   attention, including whether agents can be held to what you let them reach,
   which Chat's access bar and GitHub requests from outside a project rely on.
   On Linux that needs bubblewrap at `/usr/bin/bwrap`.

## Installing the app from this computer

A download can carry the Android app as `ophio.apk`. On Linux the installer
puts it in `~/.local/opt/ophio/app`; on Windows it stays beside `ophio.exe`.
Setup installs it over Wireless debugging, which needs Google's `adb` from the
Android SDK Platform-Tools or the system's `android-tools` package:

1. On the phone, turn on Developer options (Settings › About phone, tap Build
   number seven times), then Settings › Developer options › Wireless
   debugging, on the same Wi‑Fi as this computer.
2. Tap **Pair device with QR code** and scan the code setup shows, or tap
   **Pair device with pairing code** and type that code into setup.
3. Setup installs the app, lets its alerts through if you want them, and
   opens it with this computer's pairing link. Once the phone is paired, setup
   offers to turn Wireless debugging off again.

The app is installed only if it is signed with Øphio's release key, and never
in place of an Øphio signed with another key: setup says so and leaves that
choice to you. The same steps are `ophio phone find`, `pair`, `connect`,
`install`, `link`, `alerts` and `off`; `ophio phone install --apk <file>`
installs another copy of the app. The app carries its own licence notices,
under Settings › Versions.

## Letting the agent do it

Once Claude Code or Codex is connected, steps that an agent can help with
offer **Let the agent do it**: Tailscale, the phone (installing `adb` too),
the extras other than where files go, the GitHub workflow, agent logins and
voice control, whose choices are yours, and finding
projects, after which setup lists the folders it found, to tick. When a check
fails, **Ask the agent to fix this** hands it what failed.

Each one starts an ordinary task from one of Øphio's built-in setup skills, in
a new one-off folder (see [Agents, skills and scratch pads](launchers.md#on-its-own)).
It shows in the app's Tasks like any other task. Setup follows it in a pane:
what the agent last said and the commands it ran since it was last told
something. From the pane you can reply, answer its questions, **Stop it**, or
**Leave it running** and go on with setup while it works.

The task runs with the Standard launch flags and never bypasses the agent's
approvals. When the agent asks to run something, setup shows the request, the
exact command included, with **Allow** and **Don't allow**; neither is chosen
for you, so Enter alone allows nothing. The phone shows the same request.

The built-in skills tell the agent to ask before installing any program, to
never use `sudo` or an administrator prompt, never sign in for you and never
ask for a password, token or key in the chat: it gives you the command to run
in your own terminal instead. Anything on the phone's own screen is yours to
do.

| Skill | What it does |
| --- | --- |
| `setup-tailscale` | Reach this computer from anywhere with Tailscale |
| `setup-phone` | Install the app over Wireless debugging and pair it |
| `setup-projects` | Find likely project folders and add the ones chosen |
| `setup-desktop` | Let the phone watch and control this computer's screen |
| `setup-voice` | Download the speech model |
| `setup-telegram`, `setup-discord` | Connect a bot of your own |
| `setup-accounts` | Add other accounts, such as GitHub or Google |
| `setup-fix` | Fix what the readiness check found |
| `setup-everything` | Go through the parts chosen, one at a time |

Library's **Skills** lists them too, marked **Built into Øphio**, to start from
the phone. A task reads its skill from `setup-skills` in Øphio's data folder,
where Øphio writes it when the task starts.

## What stays with you

Some things only the person at the computer may do. When Øphio runs as part of
an agent's task, it refuses to:

- run `ophio setup`;
- show a pairing code (`ophio pair`) or confirm a pairing (`ophio pair confirm`);
- pair with a phone's Wireless debugging (`ophio phone pair`);
- change what a device may do (`ophio devices allow` and `deny`) or end its
  access (`ophio devices revoke`);
- replace this computer's certificate, or delete Øphio's data;
- answer an approval or a chat's request to link, start or instruct another
  task, let queued tasks start without asking, or give tasks an account, a
  login or a project's committed settings;
- delete a one-off's folder;
- reveal an account's key or a login's password, or quit Øphio.

Everything else the setup skills use, such as `ophio status`, `ophio doctor`,
`ophio voice install` or `ophio projects add`, works from a task. A refused
command says why and asks for it to be run in your own terminal. When an
agent brings a phone to pair, setup shows its code for you to compare and asks
you what it may do. Signing in, entering keys and tokens, and accepting
licences are also yours. This guard stops mistakes, and a page talking an
agent into pairing a stranger; it does not stop an agent that sets out to run
as you.
