# Øphio

Supervise and direct the coding agents on your own computers from your Android
phone.

- **Chat** things through with Claude Code or Codex on your computer, in
  long-running conversations where you can switch model between messages.
- **Tasks**: hand them tasks and follow their real approvals, questions and
  progress, or have them find and fix issues on GitHub. After each task,
  Øphio runs the project's own checks, which you approve, so you can see
  whether the work actually works.
- **Control** the desktop: watch it live, take over input, pause the agent,
  or hand a GUI task to a computer-use agent and step in privately when needed.
- **Point and ask**: capture the screen, draw on it, and send the agent
  exactly what you mean.
- **Library**: read and edit the Markdown that shapes your agents — skills,
  agent definitions, instructions and notes — with conflict-safe saves.
- **To-Do**: keep the steps only you can take alongside the agent work waiting
  on them, with a project picker and an all-projects view.

The Øphio app on your phone pairs directly with Øphio on each computer
(Linux, with experimental Windows and Mac downloads in 0.3.0; see
[SUPPORT.md](SUPPORT.md)). There is no Øphio-operated server between your phone
and computer. Agents use your own signed-in accounts and send their requests
to the provider you choose. Optional integrations and problem reports have
separate destinations; [problem reports](docs/reports.md) are reviewed before
sending and require a configured receiving service.

You pair a phone by running `ophio pair` on the computer and scanning its code.
The phone can connect only after you confirm, at the computer, that it shows
the same six-digit code. A phone can pair with several computers: the menu at
the top of the app switches between them, and **All computers** shows the
tasks, Needs you and To-Do of every one together. On a tablet held sideways or
an unfolded foldable, **Split view** shows two copies of the app side by side.

## Get started

You need two things: Øphio on a computer, and the Øphio app on your phone.
Setup on the computer walks you through the rest and ends with a code for
your phone to scan.

### What it runs on

| | In 0.3.0 |
|---|---|
| **Linux** on an x86_64 PC, with glibc 2.39 or later (Ubuntu 24.04 or newer) | The supported computer for the beta. |
| **Mac** with Apple silicon (M1 or later), macOS 13 or later | Experimental. Tested on one Mac. Not notarized by Apple. |
| **Windows 11** on an x64 PC | Experimental. The download isn't signed yet, so Windows may warn about it. |
| **Android** phone (64-bit ARM) | The supported phone. Installed from the release, not from Google Play. |
| **iPhone** | [Build it yourself on a Mac](packaging/ios/INSTALL.md), for developers only. No App Store or TestFlight download yet; Apple's developer-account approval is pending. |

[SUPPORT.md](SUPPORT.md) has the details, including which Linux desktops have
been tested.

### 1. Install Øphio on your computer

Install it as your ordinary user, signed in to the desktop, not as root or
administrator. Each command below downloads the newest release for your
computer, checks it against the release's published `SHA256SUMS` before
running anything from it, and installs it for you alone. Nothing is installed
if the check fails.

**Linux or a Mac**, in a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.sh | sh
```

On Linux, setup starts as soon as the install finishes. On a Mac, the command
ends by printing the two commands that start Øphio and pair your phone, as in
the [Mac guide](packaging/macos/INSTALL.md).

**Windows**, in PowerShell, not as administrator:

```powershell
irm https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.ps1 | iex
```

It waits for the install to finish (about a minute), then starts setup. A
download made this way isn't marked as coming from the internet, so in our
tests the **Windows protected your PC** prompt did not appear. Smart App
Control is different: when it is on, it blocks the unsigned download however
it arrives, and the command stops before downloading anything. [Windows
warnings and how to continue](packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
explains your choices.

You can read the scripts before you run them:
[get-ophio.sh](scripts/get-ophio.sh) and [get-ophio.ps1](scripts/get-ophio.ps1).
Running one again when that version is already installed changes nothing.

**To install by hand instead**, download your computer's file and
`SHA256SUMS` from the [latest
release](https://github.com/ophio-app/ophio/releases/latest), check one
against the other, and follow the guide for your computer:
[Linux](packaging/linux/INSTALL.md), [Mac](packaging/macos/INSTALL.md) or
[Windows](packaging/windows/INSTALL.md).
For the experimental, unsigned Windows 11 x86_64 ZIP in 0.3.0, read
[Windows warnings and how to continue](packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
for the **Windows protected your PC** and Smart App Control steps before installing.

### 2. Let your phone reach the computer

Your phone talks to the computer directly; there is no Øphio server in
between. Setup's **Reach** step offers two ways:

- **Tailscale** (recommended), if it runs on both the computer and the phone,
  signed in to the same account. It works from anywhere.
- **This Wi‑Fi only**, if the phone and the computer are on the same network
  and you trust everyone on it.

If you skip this step, the pairing code can offer only the computer's own
address, which the phone can't dial: setup's pairing step then says the
computer can't be reached. Run `ophio setup` again to choose one.

### 3. Install the app on your phone

**iPhone:** There is no App Store or TestFlight download yet. Developers with
a Mac can follow the [iPhone build guide](packaging/ios/INSTALL.md); only
simulator runs have been tested, not installation on a real iPhone.

**Android:** Setup's **Phone** step shows a code to scan with your phone's camera, which
opens the download. Or download `ophio-0.3.0-arm64.apk` from the [latest
release](https://github.com/ophio-app/ophio/releases/latest) on the phone.
Because the app doesn't come from Google Play, Android checks with you twice:

1. Chrome says the **file might be harmful**. Choose **Download anyway**.
2. When you open it, Android says Chrome **isn't allowed to install unknown
   apps**. Tap **Settings**, turn on **Allow from this source**, go back, and
   tap **Install**.

The app is signed with Øphio's release key; [its
fingerprint](packaging/android/SIGNING.md) lets you check that it is the
real one.

### 4. Pair

In the app, choose **Pair a computer** and scan the code that setup shows
(or that `ophio pair` shows later). The computer then asks whether to let the
phone in: say yes only if it shows the same six-digit code as the phone.

The [beta walkthrough](docs/beta.md) goes further, and [setup](docs/setup.md)
describes every step of `ophio setup`.

### Already using Øphio?

0.3.0 has a new phone app. If you already use Øphio, update the computer
first with `ophio update ophio-0.3.0-x86_64-linux.tar.gz`; a paired phone is
then offered the new app by the computer, under Settings › Versions ›
**Update now**. You can also install `ophio-0.3.0-arm64.apk` from the release
by hand. Most new features need both ends updated.
[Upgrading](docs/upgrading.md#updating-to-030) has the details, including
going back to 0.2.0.

## Layout

| Path | What |
|---|---|
| `crates/` | Øphio on the computer (Rust) |
| `client/` | Android app (Tauri 2 + React/TypeScript) |
| `docs/` | Architecture and user documentation |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[adding project folders from your phone](docs/projects.md),
[native launch flags and permission defaults](docs/launch-flags.md),
[telling the agent which branch to work on](docs/branches.md),
[starting from an agent, a skill or a scratch pad](docs/launchers.md),
[the checklist of what you do yourself](docs/checklist.md),
[every agent file in Library](docs/agent-files.md),
[attaching files in Chat](docs/chat-files.md),
[what a run's result records](docs/run-results.md), and
[logins agents can use on websites](docs/agent-logins.md).

## Building Øphio for the computer

```sh
export CARGO_TARGET_DIR="$PWD/target" CARGO_BUILD_JOBS=8
cargo build --locked --release -p companion -p computer-mcp -p sandbox
env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS cargo test --workspace
```

See [BUILDING.md](BUILDING.md) for prerequisites, offline caches, client and
cross-platform builds, and verification commands.

## Local installation candidates

With the documented toolchains and caches prepared, run
`scripts/package-linux.sh`, `scripts/package-windows.sh`, or
`scripts/package-android.sh`. Artifacts and their SHA-256 manifest go under
`dist/`. Host archives include the companion programs and installation instructions;
Android produces unsigned release artifacts plus a separate locally test-signed
APK. Packaging neither installs nor publishes anything.

Read [SUPPORT.md](SUPPORT.md) before testing a candidate. Windows has run only in
a virtual machine, and physical Android/platform combinations remain unqualified.

## License

Product-owned code is Apache-2.0. See [LICENSE](LICENSE), [NOTICE](NOTICE) and
[CONTRIBUTING.md](CONTRIBUTING.md). Dependencies retain their own licences;
each artifact carries full third-party notices and a CycloneDX 1.5 SBOM.
`python3 scripts/licenses.py --check` validates the offline inventory and
`scripts/source-candidate.sh` audits the committed source tree.
