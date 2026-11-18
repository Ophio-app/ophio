# Windows and macOS: current differences

Windows 11 x86_64 has an experimental unsigned ZIP in the 0.3.0 release,
not a beta support promise. Paid signing is optional and is not required for
that download. Read
[Windows warnings and how to continue](../../packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
before installing. 0.3.0 also adds an experimental Mac archive for Apple
silicon, described in [The Mac download](#the-mac-download). The public beta
targets Android phones and Linux computers only. See
[SUPPORT.md](../../SUPPORT.md). The checks below do not establish a complete
phone-to-computer experience, real agent inference, or desktop permission behavior.

The [final Windows ZIP checks](#final-windows-030-zip-checks) cover installation
and a limited update cycle in a Windows 11 virtual machine, with a known
service-start output issue. The [final Mac archive checks](#the-mac-download)
cover archive integrity, signatures and command-line version output on macOS
27.2, not installation or full workflows. Earlier-build results below do not
qualify these final files.

Chat, Tasks, approvals, pairing, file transfers, Library and most preview
handling use shared services. Native adapters still determine whether agent
processes, terminals, storage and desktop actions work on each system.

## Feature work and remaining gaps

| Area | Windows | macOS | Remaining qualification |
| --- | --- | --- | --- |
| Computer updates | Per-user installation, idle scheduling, journaled replacement, rollback and interrupted-update recovery through a stable launcher are implemented. Recovery checks the earlier version when interruption happened before replacement. | Per-user idle updates, a health check after restarting, automatic return to the earlier version when that fails, and `ophio update --rollback` are implemented. | The final Windows ZIP passed update, retained-version rollback and re-update from a synthetic managed baseline. Locked-file recovery passed with an earlier 0.3.0 archive, not the final ZIP. Normal upgrade from an inherited-permission flat ZIP install, direct older-download downgrade and power-loss recovery remain unqualified. Earlier Mac archive update and rollback checks do not qualify the final Mac archive; its lifecycle operations were not tested. |
| Automatic preview discovery | A native adapter verifies the user, process lifetime, listener and working-folder identity before admitting a project server. | Local project servers can be discovered automatically. | Windows code passes cross-compilation checks; the connected native workflow checks manually registered previews, not automatic discovery end to end. |
| Apps and windows | Native listing, focus, close and launch adapters exist. | Native listing, focus, close and launch are wired. | Real app activation, lock changes and permission changes still need desktop checks. |
| Voice commands for named controls | Windows accessibility is wired. | A native accessibility adapter retains control identities and checks authority before acting. | Real controls, permission revocation and Stop during native activation remain unqualified. |
| Launching apps by voice | Start-menu shortcuts and validated packaged applications are wired. | Validated application bundles and system defaults are wired. | Neither launcher is qualified by opening real apps. Browser and Email defaults on Windows cover exact packaged-app associations only; unmatched desktop defaults, Terminal and File Manager remain absent. |
| Clipboard paste through desktop control | Explicit clipboard replacement and paste are implemented. | Explicit clipboard replacement and paste are implemented. | Mac private-pasteboard fixtures pass without touching the general clipboard. End-to-end paste into real applications remains unqualified. |
| Project checks after each task | Approved checks run without a sandbox, and the run is recorded as unsandboxed. | Approved checks run inside the Mac system sandbox (`sandbox-exec`). | Neither is qualified with real projects. On Linux, checks run in bubblewrap inside their own system unit; Windows has no equivalent yet. |
| System readings | Native network, graphics, processor, pagefile, battery and free-memory readers are implemented. | A native system monitor exists. | Hardware qualification remains incomplete. Unsupported readings remain absent, not zero; free memory is distinct from reclaimable cache. |

Windows and Mac now share desktop-session code in `crates/platform-desktop`:
monitor geometry, capture lifetime, input sequencing and release of held keys
and buttons. Each system still supplies its own native capture and input driver.
Linux uses its separate desktop backend. Tests of the shared code do not prove
that either native driver works on a particular desktop.

Explicit Paste replaces the computer's clipboard, never reads or restores its
previous contents, and is never silently substituted for ordinary text input.
A generation check detects intervening copies, but clipboard replacement and key
delivery are not atomic. A partial failure is reported as uncertain, and
operating-system acceptance does not prove that the application consumed the text.

Packaged Windows applications are revalidated against their registered identity
and version. The operating system cannot pin that version atomically between the
last check and activation, so package replacement can still race that interval.

## Isolation and stopping work

**Limited chats and tasks are refused on Windows.** Its sandbox provider reports
that limited chats are unavailable and explains that it cannot keep a chat to
one project or off the internet. A request with those limits never falls back
to unrestricted access. Unrestricted agent runs use a separate path; this does
not qualify real provider conversations on Windows. Supporting limited
chats/tasks requires a real filesystem, network and credential boundary, not
just process cleanup.

Windows uses Job Objects for the processes Ophio starts, assigning a new process
before letting it run. A Job does not restrict files, networks, registry or
credentials. Resource-limited containers are also refused until native CPU and
process-limit enforcement is qualified; the test-only implementation is not an
available guarantee. Cleanup waits for each process to exit, not just for the
Job's process count to reach zero. A lost Job notification can leave cleanup
reported as not confirmed rather than falsely reporting success.

Mac uses the system sandbox for limited-access conversations. Process cleanup
uses process groups, which are weaker than Linux cgroups: a program that starts
a detached session can leave the original group. Mac also lacks the Linux
aggregate resource-limit implementation and refuses hard storage budgets.
Cleanup receipts must not imply stronger guarantees than the native backend
can establish.

Mac model-only relay support passes native tests using dummy credentials and a
local fake provider. Each run has a dedicated loopback port and a random
credential checked before requests affect inspection, audit, budgets or run
signals. The agent receives fresh provider state instead of the person's
sign-in files. This relay mode does not grant unrestricted internet or local
network access. However, another sandboxed run belonging to the same user can
read the relay credential from command-line arguments. Do not treat it as a
boundary between mutually hostile runs under the same operating-system user.

Mac sandbox rules cannot change during a running process, so changed folder or
loopback access requires stopping and resuming the agent. Outside model-only
relay mode, a local-network grant reaches other servers on the same Mac because
the loopback network is shared.

Other programs' command-line arguments can still be read by a limited agent on
Mac. Do not pass secrets as command arguments. Network connection listings and
some hardware identifiers are also visible. Stronger separation requires a
different operating-system user or equivalent isolation, not another sandbox
rule. A shared process-start lock protects guarded private sockets and pipes,
but does not yet cover the relay's upstream HTTP connections or the OAuth
loopback listener. The [conversation sandbox guide](../../crates/sandbox/README.md#on-a-mac)
records the sandbox and sign-in renewal boundaries in detail.

## Library and file watching

Windows native tests cover Library edits, conflicts, moves and filesystem
identity checks, including FAT32 and exFAT volumes. FAT/exFAT Library roots are
read-only; ReFS is untested. A separately pinned nested Library root prevents
moving its parent, and limited roots cannot be moved. File notifications can
overflow without a warning from the watcher. A permanently stuck watch
cancellation can retain old handles. These are remaining limits, not guarantees
of complete change detection on every filesystem.

Mac Library registration batches folder watches into one event-stream restart.
Native measurements show this reduces repeated registration work, but starting
the event stream can still take several seconds. An attempted reuse of old
watches was rejected after native tests found a missed folder move; fresh
batched registration is retained. Generation-based rescan acknowledgement
prevents an earlier refresh from clearing a later rescan request.

## Operating-system permission boundaries

Mac screen watching requires Screen Recording permission, and input requires
Accessibility permission. A locked or sleeping desktop is unavailable. Unlocking
through the system login screen, Windows secure-desktop control, and bypassing
native permission prompts are not missing features to solve by elevating Ophio.
The person must retain control of these boundaries. Mac screen capture and input
remain unqualified. The Mac download is signed ad hoc and not notarized; see
[The Mac download](#the-mac-download) for what that means for Gatekeeper and for
these permissions.

## The Mac download

The experimental 0.3.0 Apple-silicon archive is 30,087,616 bytes and has SHA-256:

```text
06aa1e81740a338a247a403fad886cc0b23ab67e6bfef4b51f5a446b1576a926
```

This identifies the final file checked, not an Apple trust signature. It holds
`ophio`, `computer-mcp`, the installer and licence files, but no phone app.
Both programs are signed ad hoc with the hardened runtime. They are not signed
with an Apple Developer ID or notarized by Apple.

The final archive checks on macOS 27.2 passed:

- Archive contents, internal checksums, Apple-silicon program architecture,
  strict signature verification, licences and runtime inventory were checked.
- The clean command-line version check returned `ophio 0.3.0`.
- A synthetic quarantine test confirmed the clear-before-extraction sequence:
  verify the archive checksum, run
  `xattr -d com.apple.quarantine ophio-0.3.0-aarch64-macos.tar.gz`
  on the archive before unpacking or starting it, then unpack a fresh copy.
  The fresh copy's version check passed. This does not disable Gatekeeper
  globally.

Gatekeeper rejected both clean and synthetically quarantined copies, as
expected for this signature. A real browser download, quarantine propagation,
launching a marked program, graphical security prompts and recovery after a
blocked launch were not tested.

Installation, background-service installation, start, stop and restart,
updates, rollback, update cancellation and uninstall were not tested with this
final archive. Neither were phone workflows, real provider conversations,
screen capture, input, voice control or clipboard paste. The version checks on
macOS 27.2 do not establish macOS 13 compatibility or full application behavior.
Both programs declare a macOS 11.0 load-command minimum; that is not proof of
runtime compatibility. This download does not support Intel Macs.

An ad-hoc signature changes with the build. Screen Recording and Accessibility
permissions therefore need to be granted again after updates; the final archive
checks did not exercise these permissions.

`scripts/sign_macos.py` can sign with a Developer ID Application certificate
and submit a signed build for notarization. Neither has been tested with
Apple's service. By Apple's documentation, notarization permits Gatekeeper
acceptance after a browser download, and using the same Developer ID preserves
permissions across updates. Those outcomes remain untested here.

## What the checks establish

Automatic Linux-hosted checks compile and lint the Windows code and check and
lint the Apple silicon Mac Rust code. The Mac check uses check-only native
objects, without an Apple SDK: it does not link a usable Mac application or run
Mac tests. The native Windows CI job is optional and was disabled for the
2026-11-12 baseline. Neither cross-check replaces native runtime tests.

Native Mac development checks through 2026-11-12 include:

- Mac connected workflows using authored test agents, Library grant and conflict
  checks, saved work after restart, native platform and terminal tests,
  process-containment tests, selected account-safe sandbox checks, and
  model-relay tests with a fake provider. These do not open the desktop.
- Mac metadata, ownership and private clipboard fixtures, plus keyboard-mapping
  and voice-driver regressions. They do not activate real apps, deliver input
  to them, request Accessibility permission or read the person's clipboard.

Earlier Mac archive checks on 2026-11-17 reported installation, login-service
operation, emulator pairing, terminal use, transfers, Library edits, update,
rollback and removal. A release-source native suite also passed 3,966 tests
in 27 crates. These are earlier-build results, not qualification of the final
archive above or of real provider conversations, physical-phone use or desktop
permissions. Later fixes addressed removal of a finished update job and a
duplicate data-folder entry in the uninstall preview.

Native runs still have limits. Mac platform unit tests have intermittently
crashed in parallel while passing serially. Four launcher tests can time out
under whole-suite load while passing alone. Process-start lock tests establish
the guarded sites, not coverage of every program start in the application.

## Final Windows 0.3.0 ZIP checks

The experimental unsigned x64 ZIP is 32,704,929 bytes and has SHA-256:

```text
3740598abac436f83fcf1276491971038d683c9c181a70eddd68675c1b611a47
```

This is the final file checked in a Windows 11 24H2 virtual machine. The hash
identifies the file; it is not a trusted Windows signature.

- Archive contents, internal checksums, x64 programs, version, public phone-app
  trust pin, private-content scan, licences and runtime inventory passed.
- The unchanged one-line installer script installed this exact ZIP using local
  download fixtures. Incorrect checksums and ambiguous or invalid manifest
  entries were refused. This was not a published-download installer test.
- A synthetic internet-download mark was removed before fresh extraction.
  Archive and program hashes were unchanged. No marked program was launched
  and no Windows security setting was changed.
- Phone-free setup and resumption, service startup, stopping and status,
  diagnostics, and service removal retaining application data passed.
- Update from a synthetic managed 0.2.0 baseline, rollback to the retained
  version and re-update to the exact 0.3.0 ZIP passed. Configuration, identity
  and a synthetic project were retained. The baseline used earlier program
  bytes and assigned installation metadata; it was not a published 0.2.0
  Windows download or a normal flat ZIP installation.

**Known issue:** starting the service leaves redirected output streams open.
A script that starts it and waits for all output can hang even though the
command exits successfully and the service runs. Typing commands normally and
the one-line installer are unaffected. Windows remains experimental with this
issue disclosed.

An attempted direct update to an older 0.2.0 archive did not finish. There is
no published Windows 0.2.0 download. Direct older-download downgrade is not
qualified; rollback to a retained version is a different operation and passed.

Locked-file recovery was not tested with this final ZIP. It passed with an
earlier 0.3.0 packaged candidate, as recorded below. Normal migration from an
older inherited-permission flat ZIP installation remains unqualified.

The published one-line installer, real-browser downloads, SmartScreen prompts,
Smart App Control acceptance, physical PCs and graphics hardware, Windows 10,
Windows ARM, matching-phone workflows, real provider conversations, update
cancellation, power-loss recovery and destructive data removal were not
qualified by this run.

## Earlier Windows 0.3.0 candidate checks

The host-only ZIP checked in Windows 11 KVM has SHA-256:

```text
4dd68dbad24ac3302b6acd4e1e649630ab607b66b1c39caeca7b9654602d807a
```

This identifies the initial tested candidate, not the final release artifact
or a trusted Windows signature. Rebuilding or signing changes the artifact
and needs a new hash and verification. The additional source-build checks
below do not mean this initial ZIP contains those fixes. The paired Android
app was a signed 0.2.0 build in an emulator, not a matching 0.3.0 app or a
physical phone.

- The extracted archive and inner checksums were verified. Fresh installation
  as an ordinary user produced a managed installation and stable launcher.
- Pairing, stand-in chat and task completion, and an approval-gated file write
  passed. The witness file was absent before approval and present afterward.
  Both real agents were signed out: these are not real-provider turns.
- The connected terminal ran a real PowerShell command through ConPTY. Transfers
  in both directions completed with verified bytes. Desktop capture, pointer
  and keyboard input were exercised in the virtual desktop.
- Local Stop ended input, leaving read-only watching. Input remained off after
  background/resume. A connection-related delivery error was visible on resume;
  this is not qualification of uninterrupted background connectivity.
- All 1,503 ordinary service test cases passed at least once across bounded
  groups. Whole-suite attempts exceeded their aggregate time limits; there is
  no passing single aggregate run. The separately selected mapped-drive
  regression also passed; the large snapshot measurement was not run.
- Native platform, account, process-containment, ConPTY, Library and connected
  workflow suites passed, including the exclusive per-user owner pipe. Account
  and ports tests initially failed in the test staging location and passed on
  rerun in ordinary filesystem paths. Interactive repeated-capture, local Stop
  and held-key tests passed in the logged-in virtual desktop.
- The native locked-image recovery regression passed on isolated rerun after a
  timeout. The packaged application also recovered from a real executable
  sharing error through its stable launcher, retaining its identity,
  configuration and project. This was not a power interruption.
- An older test 0.2.0 flat ZIP installation with inherited permissions was
  intentionally refused by the owner-only security check. This was not a
  published Windows 0.2.0 download.
  A lab-only permissions workaround allowed migration, rollback and re-upgrade,
  retaining identity, configuration and a registered project. That workaround
  does **not** qualify normal upgrades and is not an installation instruction.
- Removal stopped the background program and login startup while retaining
  identity, configuration and project data.

Follow-up source-build checks established these narrower results:

- The improved inherited-permission refusal left the old program files and
  permissions unchanged. All 18 Windows updater unit tests passed.
- The documented clean-install path stopped Øphio, renamed only the old program
  folder to a unique backup and installed without a permissions workaround.
  Identity, configuration, a registered project, its file bytes and a revoked
  Android pairing record, including its grants, were retained. The backup's
  bytes and permissions were unchanged. This does not qualify migration of
  live paired conversation or task history.
- Corrected stable-launcher startup passed actual background start, stop and
  start again, with successful command exits and independent owner-pipe and
  project checks. Both new launcher-handoff regressions and all five native
  install unit tests passed. A clean intermediate launcher exit is no longer
  reported as failure while the background companion is starting.
- All 29 optional-signing mock tests passed on native Windows, including
  final-archive verification orchestration, no-overwrite publication, exact
  destination file-permission probing and unsafe archive-name rejection.
  These mocks did not authenticate or make a real signing request.
- `Unblock-File` removed an explicitly synthetic internet download mark from
  a disposable copy of the exact ZIP without changing its hash. Expanded
  executable bytes matched and had no download mark. This did not launch an
  executable, qualify a real browser download or establish SmartScreen or
  Smart App Control acceptance. No security setting was changed.

Setup-screen snapshot regeneration remains deliberately refused on Windows to
preserve the Unix baselines. Native terminal fixtures use isolated data and
PowerShell without profiles; the connected terminal can run the person's shell
profile. Neither path establishes compatibility with every profile.

These results are bounded candidate evidence, not a passing result for later
artifacts. A matching 0.3.0 Android pairing, real-provider sign-in and turns,
physical PC/GPU/phone use, Windows 10 and power-loss recovery remain unverified.
Normal upgrade from the old inherited-permission installation remains
unqualified. Windows Smart App Control can reject executables that do not meet
its trusted-signature or reputation policy; removing an archive's download mark
does not override that policy. The
[Windows installation guide](../../packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
explains SmartScreen's per-app option separately from the system-wide Smart
App Control choice and its security trade-off. Real Authenticode signing,
trusted timestamp verification and Windows reputation remain unverified.

Mac still lacks complete physical-phone qualification. Native adapter tests
and connected workflows with test agents do not establish live sign-in renewal,
reboot recovery, paid inference or real Mac desktop control.

Tests must not silently use installed agents, the person's keychain, a login
shell's startup files or live accounts. The Mac sandbox suite contains explicit
real-agent and keychain cases; an ordinary full run is not account-isolated.
Keep those cases out of unattended qualification unless their separate
prerequisites and authorization are satisfied.
