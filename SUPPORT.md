# Platform support and qualification

The 0.3.0 beta targets Android ARM64 phones and Linux x86_64 computers. The
release files were checked in a Linux virtual machine with an Android emulator;
physical-phone and real-provider checks were not run. Windows and Mac have
separate, limited checks described below. The
[v0.3.0 release](https://github.com/ophio-app/ophio/releases/tag/v0.3.0)
carries the official files: the Linux archive
`ophio-0.3.0-x86_64-linux.tar.gz`, the source `ophio-0.3.0-source.tar.gz`,
the Android app `ophio-0.3.0-arm64.apk` and `SHA256SUMS`. The 0.3.0 release
also includes an experimental, unsigned Windows 11 x86_64 ZIP, not a beta
support promise;
paid signing is optional and is not required for that download. Before
installing Øphio on Windows, read
[Windows warnings and how to continue](packaging/windows/INSTALL.md#windows-warnings-and-how-to-continue)
for the **Windows protected your PC** and Smart App Control choices. 0.3.0 also
has an experimental Mac archive for Apple silicon,
`ophio-0.3.0-aarch64-macos.tar.gz`, which is not a beta support promise either;
its limits are in the table. iPhone is not a supported release target and has
no download; experimental
implementations do not establish support.

The table distinguishes checks of the final 0.3.0 files from earlier results.
Earlier passes do not qualify a later file. No complete physical phone-to-host
combination is certified by compilation, synthetic tests or archive checks
alone. Physical-phone checks remain unqualified for these release files.
Start with the [beta walkthrough](docs/beta.md); Linux route
details and limits are in `docs/qualification/linux.md`. Current Windows and
Mac feature differences and the work needed to close them are described in
[Windows and macOS qualification](docs/qualification/windows-macos.md).

| Platform | Implemented scope | Qualification status |
| --- | --- | --- |
| Android ARM64 phone | Tauri native shell, pinned transport, device keys and Kotlin integration; configured minimum API 29, target/compile API 37 | The final signed 0.3.0 app was checked on Android 15 emulators with ARM translation: installation, pairing, release-key update from 0.2.0, core Chat and Task use with both mixed versions, attachments, permissions and interrupted replies. Agents were synthetic programs, not real providers. Foldable checks covered independent panes, drafts, restart, closing either side, keyboard/Back and large text; the tablet run stopped after emulator crashes. Full focus/selection, reply-sound, locked-screen tap and dismissal/reconnection checks remain incomplete. No physical-phone result is claimed for this file. Earlier builds were hand-tested on an Android 14 tablet; that is not a 0.3.0 pass. API 29 is a build minimum, not a tested support floor. |
| Linux x86_64, Wayland native protocols | wlroots screencopy or ext-image-copy-capture, SHM frames, virtual pointer/keyboard | Earlier builds: Hyprland 0.56 qualified in a disposable KVM guest with software rendering: capture, input, geometry, lock fencing, disconnect/reconnect and phone Stop during computer use. Sway, other wlroots compositors and physical GPUs remain unqualified. |
| Linux x86_64, desktop portals | ScreenCast + RemoteDesktop with PipeWire and explicit session consent | Earlier builds: GNOME 46 (Ubuntu 24.04) and KDE Plasma 6.7 qualified independently in KVM guests: consent and restore, lock fencing, input, disconnect/reconnect and phone Stop. GNOME 46 ends capture when the display is rescaled; sharing must be reopened explicitly and may ask for consent again. Characters outside the portal keymap are refused as a whole; explicit paste works. |
| Linux x86_64, X11 | GetImage, RandR, XFixes cursor composition and XTest input | Implemented fallback; native interoperability remains unqualified. MIT-SHM acceleration is not implemented. |
| Windows 11 x86_64 | Native capture and input, ConPTY terminals, per-user protected storage and IPC, logon startup, Job Object process containment, preview discovery, system readings, clipboard paste and voice app launch; per-user idle updates, rollback and interrupted-update recovery through a stable launcher. Limited chats and tasks are refused with a Windows-specific explanation; resource-limited containers are refused until their enforcement is qualified. | Experimental unsigned download. The final ZIP passed archive and checksum checks, local-fixture installer acceptance/refusal, phone-free setup, service operation, diagnostics and removal retaining data in a Windows 11 24H2 virtual machine. Update, retained-version rollback and re-update passed from a synthetic managed older installation, not an old flat ZIP install. A script that starts the service and waits for output can hang; typing commands yourself and the one-line installer are unaffected. An earlier 0.3.0 archive passed connected emulator workflows and packaged locked-file recovery; those were not repeated on the final ZIP. Physical PCs, hardware graphics, Windows 10, real providers and matching-phone workflows remain unqualified. See [Windows and macOS qualification](docs/qualification/windows-macos.md). |
| iPhone | Tauri native shell with a Swift device plugin, iOS 16 or later | In development, not released, and no download exists in this release. Builds and starts in the iPhone simulator. Never run on a real iPhone, so Face ID, camera, microphone and the local network prompt are untested. Alerts arrive only while the app is open. For split view on a foldable iPhone, the app reads only whether it is folded, not where the crease is; this has run only in the simulator. |
| Mac, Apple silicon | System sandbox, launchd login agent, per-user idle updates and rollback, ScreenCaptureKit capture, CoreGraphics input, menu bar Stop, window actions, named-control voice accessibility, voice app launch and explicit clipboard paste | Experimental download. The final archive passed checksum, architecture, strict signature and clean version checks on macOS 27.2, plus a synthetic clear-before-extraction quarantine test. Its programs are signed ad hoc, not signed with an Apple Developer ID or notarized. Gatekeeper rejected clean and synthetically marked copies as expected; read [the Mac install guide](packaging/macos/INSTALL.md). Installation, background service, updates, rollback, uninstall and full workflows were not tested on this final archive. Screen capture, input, voice and clipboard control, real-browser downloads, physical phones and other macOS versions remain unqualified. The programs declare a macOS 11.0 minimum; this is not a runtime test or a promise of support. Intel Macs are not supported. Strong process containment and hard disk budgets for chats are unsupported, and a limited chat can read other programs' command lines. |

Use an ordinary logged-in desktop session with explicit capture/input permission.
Input targets the session's focus and does not guarantee isolation to one window.
No auto-login, secure-desktop control, unrestricted elevated helper, or root-wide
input daemon is provided. Lock/logout, sleep, permission loss and disconnected
sessions must surface unavailable or unknown state. They do not imply successful
completion of an interrupted task.

Linux x86_64 release archives require glibc **2.39 or later**; Ubuntu 24.04 is
the baseline userspace. Packaging reads the glibc symbol versions both shipped
executables need and rejects anything above 2.39, so a release is built on
Ubuntu 24.04 or an equivalent older sysroot. PipeWire and libxkbcommon come from
the installed system and their versions are not checked. Compositor, portal,
PipeWire and input support still depend on the qualified route listed in
`docs/qualification/linux.md`. ARM Linux/Windows, alternate architectures and
other compositor combinations are not currently qualified distribution targets.

Claude Code and Codex integration capabilities are reported independently by
Øphio on the computer, using the installed native tool and the owner's account.
No test claim implies arbitrary harness-version compatibility or successful paid
inference. Øphio supports Codex 0.153.2 and later. Readiness and `ophio doctor`
say when the Codex found is older, or incomplete, such as a copy of the
executable without the `codex-code-mode-host` helper that code-mode models run
their commands through, and point to a newer installation when there is one. Terminal access and other native integrations do not establish
managed-task or GUI-control parity. Optional external chat services have their
own privacy and availability boundaries; Android background attention delivery
also requires physical-device qualification.

A project's checks, which Øphio runs after each task, run on Linux inside the
same kind of bubblewrap sandbox as limited chats, with no more access than the
agent had. On Windows, where that sandbox does not exist, checks are recorded
as having run without one. Windows is experimental, not a beta support
promise. Supporting limited chats/tasks would require a
real filesystem, network and credential boundary; supporting resource-limited
containers would also require qualified native resource enforcement. Process
cleanup alone does not provide these guarantees.

When requesting help, include the artifact version and hash, OS/backend,
relevant permission state, and a minimal synthetic reproduction. Share only
redacted diagnostics. Use [SECURITY.md](SECURITY.md) for suspected vulnerabilities;
installation/build instructions are in [BUILDING.md](BUILDING.md).
