# Øphio

Supervise and direct the coding agents on your own computers from your Android
phone.

- **Chat** things through with Claude Code or Codex on your computer, in
  long-running conversations where you can switch model between messages.
- **Tasks**: hand them tasks and follow their real approvals, questions and
  progress, or have them find and fix issues on GitHub.
- **Control** the desktop: watch it live, take over input, pause the agent,
  or hand a GUI task to a computer-use agent and step in privately when needed.
- **Point and ask**: capture the screen, draw on it, and send the agent
  exactly what you mean.
- **Library**: read and edit the Markdown that shapes your agents — skills,
  agent definitions, instructions and notes — with conflict-safe saves.
- **To-Do**: keep the steps only you can take alongside the agent work waiting
  on them, with a project picker and an all-projects view.

The Øphio app on your phone pairs directly with Øphio on each computer
(Linux; Windows and Mac code is in the source, but has no download and isn't
supported, see [SUPPORT.md](SUPPORT.md)). There is no Øphio-operated server between your phone
and computer. Agents use your own signed-in accounts and send their requests
to the provider you choose. Optional integrations and problem reports have
separate destinations; [problem reports](docs/reports.md) are reviewed before
sending and require a configured receiving service.

You pair a phone by running `ophio pair` on the computer and scanning its code.
The phone can connect only after you confirm, at the computer, that it shows
the same six-digit code. A phone can pair with several computers: the menu at
the top of the app switches between them, and **All computers** shows the
tasks, Needs you and To-Do of every one together.

## Install

The 0.2.0 beta is for Android and Linux. Download the Linux archive,
`ophio-0.2.0-x86_64-linux.tar.gz`, and the Android app,
`ophio-0.2.0-arm64.apk`, from the [v0.2.0
release](https://github.com/ophio-app/ophio/releases/tag/v0.2.0) (the [latest
release](https://github.com/ophio-app/ophio/releases/latest) page always shows
the newest), and check them against the release's `SHA256SUMS` file. Then
follow the [Linux installation guide](packaging/linux/INSTALL.md) and the
[beta walkthrough](docs/beta.md); [setup](docs/setup.md) describes what `ophio
setup` does. [Platform limits](SUPPORT.md) lists what is supported.

0.2.0 has a new phone app. If you already use Øphio, update the computer
first with `ophio update ophio-0.2.0-x86_64-linux.tar.gz`; a paired phone is
then offered the new app by the computer, under Settings › Versions ›
**Update now**. You can also install `ophio-0.2.0-arm64.apk` from the release
by hand. [Upgrading](docs/upgrading.md) has the details.

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
