# Trying the Android-and-Linux beta

This guide covers the 0.1.0 Android-and-Linux beta. Once published, the
[v0.1.0 release](https://github.com/ophio-app/ophio/releases/tag/v0.1.0)
identifies the official matching Android app and Linux archive and states
what was actually checked. This guide alone is not approval of a release or
proof that an arbitrary build passed: use only the files identified in an
approved release, not a build from an automated check. Physical-device and
real-agent checks remain separate from the virtual-device lab results.

The first beta is for Android phones and the Linux desktop combinations listed
in [SUPPORT.md](../SUPPORT.md). Windows is experimental; macOS and iPhone are
not supported beta targets. Start with a spare project containing no private
material. A beta can have defects, and stopping an agent does not undo the
files it has already changed.

## Before you start

You need:

- an ARM64 Android phone and a Linux x86_64 computer on a listed desktop;
- a normal graphical login on the computer, not a root login;
- a way for the phone to reach it: preferably both devices signed into your
  own Tailscale network, or an explicitly configured trusted local network;
- your own supported coding-agent installation and sign-in on the computer;
- the dependencies in the [Linux installation guide](../packaging/linux/INSTALL.md),
  including the requirements for enforcing Chat's access controls.

A minimum Android version in a build file is not a tested support promise.
Check the release's exact phone and Linux test results. On Linux, installing
a package named `bubblewrap` is not enough by itself: its version, executable
location and permission to create namespaces must pass the documented checks.
Do not disable system security globally to make a check pass.

## 1. Install and check the computer

Extract the approved Linux archive. From the extracted directory, run as your
ordinary user:

```sh
sh install.sh
```

The installer normally opens setup on a new installation. If it does not, run:

```sh
ophio setup
```

Follow its steps for automatic startup, the phone connection and agent sign-in.
Use the approved Android app from the same release when setup asks you to
install the phone app. Do not grant optional desktop, microphone or background
permissions just to get past setup; enable them when you want the feature.

Then run:

```sh
ophio doctor
```

**Expected:** the features you intend to use are ready, or each unavailable
feature has a reason and a remedy. A successful install is not proof that
Chat's sandbox or desktop capture works. If the `ophio` command is not found,
follow the installer's PATH instructions rather than reinstalling as root.

[Setup details](setup.md) · [Linux requirements and remedies](../packaging/linux/INSTALL.md)

## 2. Pair the phone

Run this on the computer:

```sh
ophio pair
```

Open Øphio on the phone, scan the offered code, and compare the six-digit code
on both screens. Confirm on the computer only if they match. Give the phone
only the permissions you need for this test.

**Expected:** the phone connects to the intended computer. If the code offers
only a loopback address, the phone cannot reach it. Set up Tailscale on both
devices, or follow the trusted-local-network instructions in the
[Linux guide](../packaging/linux/INSTALL.md#reaching-this-computer-from-your-phone),
then restart Øphio and generate a new pairing code. Do not forward the port on
your internet router as a workaround.

Never post the pairing code, its full link, or a screenshot containing it in a
public issue. A changed computer certificate needs deliberate verification,
not automatic acceptance.

## 3. Complete one useful session

1. In Chat, choose a small test project. Check which folders the agent may
   read or change and whether internet access is enabled before sending.
2. Ask it to describe that project without changing anything. Confirm that
   the answer belongs to the selected project and chosen agent.
3. In Tasks, start a small reversible task in the test project. Follow its
   progress, answer any real question, and inspect the result. A finished
   turn is not the same as verified work: check its recorded test results.
4. Try Stop on a separate harmless task while it is running. Confirm the
   task stops and does not immediately restart itself. Inspect any changes
   it made before stopping.
5. Switch away from the phone app briefly and return. The current task state
   should appear without replaying old states as though they were happening
   now.
6. Open Library and To-Do. Confirm that the selected project remains clear
   and that anything needing you explains the action to take.

Agent requests use your own provider account and its allowance or billing.
Only send material you are comfortable sharing with that provider. Øphio does
not make an untrusted agent or downloaded project safe merely by running it.

Desktop control is a separate optional test. Enable it deliberately, keep
private windows out of view, and follow the release's watch/takeover/Stop
check. Do not interpret passing the text-chat steps above as proof that live
desktop control is qualified on your machine.

## 4. Check updates and recovery

When an approved newer Linux archive is available, give that archive to the
installed `ophio update` command. It normally waits for active work to finish;
`ophio update` without arguments shows its progress. Do not use a forced update
while important work is running.

After the computer updates, use **Settings → Versions → Update now** on the
phone when the matching Android update is offered.

**Expected:** the computer starts again, the phone reconnects, the paired
identity and granted permissions remain correct, and both versions match the
release. A refused signature or version mismatch is a failed check, not a
reason to bypass verification or uninstall immediately.

The computer keeps one previous program version. `ophio update --rollback`
can restore it, but a database migration can also require restoring the
pre-migration backup and losing later records. Rollback does not undo work an
agent made in your projects. Read [upgrading and recovery](upgrading.md)
before attempting it, and record recovery as untested if you did not actually
exercise it.

## 5. Send feedback without sharing private data

Use **Report a problem** for the in-app reporting flow, and inspect every
included image and diagnostic field before sending. **A locally saved or
queued report is not proof that the maintainers received it.** This source
has no receiving address by default; computer delivery needs a configured
intake, and direct phone delivery needs the address built into the phone app.
The release must state which route is available and demonstrate receipt from
outside the maintainer's network before promising in-app delivery.

If the official public repository offers Issues, a manually written issue is
an interim route for an ordinary non-security problem. Include the release
version, phone and Linux desktop versions, short steps to reproduce, and what
you expected versus what happened. Use made-up project content. Do not upload
an entire diagnostic bundle, chat transcript or project just because someone
asks for it.

For a suspected security problem, use the verified private route in
[SECURITY.md](../SECURITY.md), **not a public issue**. Never include passwords,
provider keys, device keys or pairing offers. See [problem-report privacy and
delivery](reports.md) for the in-app route's details.

## Recording a beta acceptance check

For each candidate, record:

- the exact source revision, artifact names and checksums;
- phone model and Android version, Linux distribution and desktop version;
- the steps above as **passed**, **failed**, or **not tried**, with what was
  actually observed;
- agent and tool versions where relevant, without account secrets;
- whether tests used a physical phone, emulator, virtual machine or browser
  simulator;
- any update, background-alert, keyboard, accessibility or desktop-control
  checks that remain untested.

Screenshots and passing unit tests help explain a result; neither replaces a
fresh installation with a physical phone. Do not reuse an older candidate's
passing receipt as the result for a new artifact.
