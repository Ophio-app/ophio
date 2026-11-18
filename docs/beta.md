# Trying the Android-and-Linux beta

This guide covers the 0.3.0 Android-and-Linux beta. The official files are
attached to the [v0.3.0 release](https://github.com/ophio-app/ophio/releases/tag/v0.3.0): the Linux
archive `ophio-0.3.0-x86_64-linux.tar.gz`, the source
`ophio-0.3.0-source.tar.gz`, the Android app `ophio-0.3.0-arm64.apk`, and
`SHA256SUMS`, which lists the SHA-256 digest of each. Check what you downloaded against it, for
example with `sha256sum -c SHA256SUMS` in the folder that holds the files (it
reports the files you did not download as missing; `--ignore-missing` skips
them). The Android app is signed with Øphio's release key; its certificate
digest is in [packaging/android/SIGNING.md](../packaging/android/SIGNING.md).
Use only these files, not a build from some other source. Physical-device and
real-agent checks remain separate from the virtual-device lab results.

For these final files, an Ubuntu 24.04.5 virtual machine and Android 15 emulator
passed installation, pairing, release-key phone update, computer upgrade and
rollback, and core Chat/Task use with one end on 0.2.0. The agents were synthetic
programs, not real provider conversations. Foldable checks were partial; the
tablet run stopped after emulator crashes. Physical phones, audible alerts,
locked-screen notification taps and complete settings, dismissal/reconnection
and focus/selection checks were not qualified. See the
[release-file results](qualification/linux.md#030-release-file-checks).

The beta is for Android phones and the Linux desktop combinations listed
in [SUPPORT.md](../SUPPORT.md). Windows has an experimental download;
the Mac has an experimental download that is not part of the beta, and macOS and
iPhone are not supported beta targets. Start with a spare project containing no private
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

Download the Linux archive from the release and unpack it. The 0.3.0 archive
is `ophio-0.3.0-x86_64-linux.tar.gz`; use the name of the file you
downloaded. It unpacks to a folder of the same name without `.tar.gz`. From
the folder you downloaded it to, run as your ordinary user:

```sh
tar -xzf ophio-0.3.0-x86_64-linux.tar.gz
cd ophio-0.3.0-x86_64-linux
sh install.sh
```

On a fresh Ubuntu Desktop, a few things commonly get in the way when you set
up an agent; the [Linux guide](../packaging/linux/INSTALL.md#installing-claude-code-or-codex)
explains each. Ubuntu does not include `curl`, which Claude Code's installer
needs (`sudo apt install curl`). Codex installed with Ubuntu's own Node.js
fails with a permission error. Your terminal finds a newly installed agent in
`~/.local/bin` only after you log out and back in, or after you add the
`export PATH` line the installer shows to `~/.bashrc` and open a new
terminal. Chats kept to a project can start an agent only if it was installed
with its own installer (into `~/.local/bin`) or for everyone on the computer,
not one installed with npm inside your home folder.

The installer normally opens setup on a new installation. If it does not, run:

```sh
ophio setup
```

Follow its steps for automatic startup, the phone connection and agent sign-in.
Use the Android app from the same release when setup asks you to install the
phone app. If you download the APK in Chrome, it may say "File might be
harmful"; choose **Download anyway**, and allow installing from that source
when Android asks. Do not grant optional desktop, microphone or background
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

When a newer Linux archive is available on the releases page, give that
archive to the installed `ophio update` command. From 0.2.0, that is
`ophio update ophio-0.3.0-x86_64-linux.tar.gz`. It normally waits for active
work to finish; `ophio update` without arguments shows its progress. Do not
use a forced update while important work is running.

0.3.0 has a new phone app, and the Linux archive carries it. Once the computer
runs 0.3.0, a paired phone with the 0.2.0 app is offered it: a notification
says the update is ready, and **Settings → Versions → Update now** installs it
over the phone's connection to the computer. Installing
`ophio-0.3.0-arm64.apk` from the release by hand does the same. Either way
Android keeps your pairing and settings, because the new app is signed with
the same release key. Android may ask you to allow Øphio to install apps if
you have not already allowed it, and may ask you to confirm. If the first
try just after allowing installs fails, tap **Try again**;
[Upgrading](upgrading.md#updating-to-030) walks through it, including what
works while the phone and the computer run different versions.

**Expected:** the computer starts again, the phone reconnects, the paired
identity and granted permissions remain correct, and both versions match the
release. A refused signature or version mismatch is a failed check, not a
reason to bypass verification or uninstall immediately.

The computer keeps one previous program version. `ophio update --rollback`
can restore it, but a database migration can also require restoring the
pre-migration backup and losing later records. Rollback does not undo work an
agent made in your projects. Read [upgrading and recovery](upgrading.md),
and [going back to 0.2.0](upgrading.md#going-back-to-020) in particular,
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
