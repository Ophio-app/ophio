# Platform support and qualification

The 0.1.0 beta targets Android ARM64 phones and Linux x86_64 computers.
The [v0.1.0 release](https://github.com/ophio-app/ophio/releases/tag/v0.1.0)
identifies the official files and their exact qualification once published;
this source document alone is neither release approval nor proof that a
particular download passed. A Windows build exists and is partly qualified
in a virtual machine, but Windows is not an initial beta support promise.
macOS and iPhone are not supported release targets; experimental
implementations do not establish support.

The table below records prior qualification, not a passing result for every
later release. No complete physical phone-to-host combination is certified by
compilation, synthetic tests or archive checks alone. Each beta candidate
needs fresh installation and physical-phone checks on its advertised
combination. Start with the [beta walkthrough](docs/beta.md); Linux route
details and limits are in `docs/qualification/linux.md`.

| Platform | Implemented scope | Qualification status |
| --- | --- | --- |
| Android ARM64 phone | Tauri native shell, pinned transport, device keys and Kotlin integration; configured minimum API 29, target/compile API 37 | Hand-tested on one physical Android 14 tablet against Øphio on a Linux computer: pairing, conversation, tasks and receipts, library, Stop and background task alerts. Desktop watching from a physical device, other devices, IME breadth, microphone and long background lifecycle remain release qualification work. API 29 is a build minimum, not a tested support floor; Android 13+ is the proposed physical test baseline. |
| Linux x86_64, Wayland native protocols | wlroots screencopy or ext-image-copy-capture, SHM frames, virtual pointer/keyboard | Hyprland 0.56 qualified in a disposable KVM guest with software rendering: capture, input, geometry, lock fencing, disconnect/reconnect and phone Stop during computer use. Sway, other wlroots compositors and physical GPUs remain unqualified. |
| Linux x86_64, desktop portals | ScreenCast + RemoteDesktop with PipeWire and explicit session consent | GNOME 46 (Ubuntu 24.04) and KDE Plasma 6.7 qualified independently in KVM guests: consent and restore, lock fencing, input, disconnect/reconnect and phone Stop. GNOME 46 ends capture when the display is rescaled; sharing must be reopened explicitly and may ask for consent again. Characters outside the portal keymap are refused as a whole; explicit paste works. |
| Linux x86_64, X11 | GetImage, RandR, XFixes cursor composition and XTest input | Implemented fallback; native interoperability remains unqualified. MIT-SHM acceleration is not implemented. |
| Windows 11 x86_64 | WGC monitor capture with DXGI fallback, SendInput, ConPTY, per-user protected storage/IPC and logon startup; every agent run Øphio launches starts suspended in a Job Object that allows no breakaway; private handoff for restricted Claude runs | Partly qualified in a KVM guest running Windows 11 Enterprise LTSC 2024 (24H2) with one 1280×800 display on the Microsoft Basic Display Adapter. Binaries are cross-compiled on Linux. The native tests of twelve crates, `containment` and `harness-claude` included, passed in the recorded standard-user desktop session; four were also exercised on a GitHub-hosted Windows Server 2025 runner. Those results do not establish that Windows checks run for the current candidate. Run in the guest: cross-account DPAPI refusal; owner-pipe refusal of another account and of SYSTEM; the packaged archive's install, login start, stop and update; console Ctrl+C, Ctrl+Break and close; and, with an Android emulator, pairing, the PowerShell terminal, file transfer, desktop watching, Take over, typing on US and German layouts and the lock state. In a classic console window, characters needing a dead-key composition or missing from the keyboard layout are dropped; Windows Terminal receives them. A physical PC, hardware GPU, more than one monitor, DPI or rotation changes, DXGI fallback, an elevated or UAC desktop, pointer input from the phone, a real Claude or Codex run, private handoff and Windows 10 are untested. `crates/platform-windows/README.md` lists what remains. |

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

When requesting help, include the artifact version and hash, OS/backend,
relevant permission state, and a minimal synthetic reproduction. Share only
redacted diagnostics. Use [SECURITY.md](SECURITY.md) for suspected vulnerabilities;
installation/build instructions are in [BUILDING.md](BUILDING.md).
