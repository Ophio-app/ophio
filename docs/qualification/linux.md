<!-- SPDX-License-Identifier: Apache-2.0 -->
# Linux desktop qualification

These are real compositor tests in disposable KVM guests, with the Android debug
app connected through its native pinned TLS transport. They are not hardware GPU
or general distribution certification. A route is qualified only for the checks
marked **pass**; **unrun** means no qualifying evidence was obtained.
The final follow-up below records the current qualification status; earlier
failures are retained as historical evidence.
The original run below found a release-blocking Plasma lock-screen injection
and failures in initial control and geometry propagation. The follow-up section
records fixes, new measurements and remaining qualification limits separately.

Original run dates: 2026-09-27–28 UTC. That run used baseline `74229a8`.
A proposed PipeWire change was tested and reverted after tracing the actual
source removal (`462608d`, reverted by `6d4b197`). Lab instructions are in
[tools/lab/README.md](../../tools/lab/README.md).

All qualification guests used QEMU's restricted user networking, 8 vCPUs,
6 GiB RAM, a virtio GPU and software rendering. QEMU had `-display none`, no VNC
or SPICE, and only loopback SSH 47222 and Øphio 47112 forwards. The Android
AVD used fresh lab-local userdata, API 35, software rendering, no window/audio,
isolated ADB, and the debug APK built from this checkout. Harnesses were authored
fixtures; no inference, external accounts or network service was used.

Raw evidence is excluded from source control under `target/lab-evidence/`.
Paths below are relative to that directory. Image checksums and pinned publisher
signature verification are retained separately in each guest's
`target/lab/<target>/image.json`. Enrollment offers and keys are excluded from
both the report and evidence.

## Hyprland

| Component | Tested value |
| --- | --- |
| Distribution/image | Arch Linux rolling, official cloud image 20260915.594445; upgraded guest packages |
| Kernel | 7.2.7-arch1-1 |
| Compositor | Hyprland 0.56.2-3; Aquamarine 0.15.1 |
| Portal | xdg-desktop-portal 1.22.1-2; backend hyprland 1.4.1-2 |
| Media | PipeWire 1:1.6.9-1; WirePlumber 0.5.17-2 |
| Renderer | Mesa 26.2.3-arch1.1; llvmpipe, LLVM 22.1.8, 256 bits |
| Output | Virtio DRM Virtual-1, 1280 × 720, 60 Hz, scale 1 |
| Øphio | Ordinary graphical-session user; source-built debug binary; backend `wlroots` |

| Check | Result | Evidence / limit |
| --- | --- | --- |
| Doctor, backend selection, desktop enable | pass | `hyprland-doctor.json`, `hyprland-enable.json`, `hyprland-systeminfo.txt` |
| Restart, disable and re-enable | pass | `hyprland/disable.json`, `hyprland/reenable.json`; direct protocols have no portal consent dialog |
| Android pairing and watch | pass | `hyprland/watch.json`, `hyprland/phone-canvas.png`, `hyprland/guest.png` |
| Initial control from the UI | fail | Empty initial control projection disables Enable control: `hyprland/initial-control.json` |
| Pointer, click, ASCII, Unicode, chord, scrolling | pass | After the explicit initialization workaround below; `hyprland/input.json`, `hyprland/input-state.json`, `hyprland/input-events.jsonl` |
| Handback; disconnect while dragging and holding Shift | pass | `hyprland/held-before-disconnect.json`, `hyprland/released-after-disconnect.json` |
| Live resize/rescale and correct coordinates | fail | 1600 × 1200 at 1.25× reaches native query/media revision 2, but the phone retains revision 1 and disables input: `hyprland/resize.json` |
| Lock during control | unrun | Partial evidence: hyprlock locked the output; stale-picture expiry revoked the lease and refused later text. Immediate lock fencing was not established: `hyprland/lock.json`, `hyprland/locked.png` |
| Scripted MCP observation/action; phone Stop | pass | `hyprland/computer-stop-rerun.json`, `hyprland/computer-actions.jsonl`, `hyprland/computer-observation.png` |
| Disconnect/reconnect truthfulness | pass | Offline/stale/input-off state, followed by explicit recovery; `hyprland/disconnected.json`, `hyprland/reconnected.json` |

First frame: **2146.5 ms**. Steady sample: **180 acknowledged frames in
31.010 s (5.805 fps)**. All 182 frames across startup and sampling had a canvas
draw before acknowledgement. The independent `grim` screenshot and phone canvas
show the same guest witness layout and changing clock.

The held-input witness recorded both Shift and button 1, then neither, with
2.931 seconds between those observations. This is an upper bound measured from
the last held-key event, not transport outage latency. Hyprlock did not set
logind's `LockedHint` in this setup: control stopped through the existing stale
picture deadline. This does not establish affirmative lock-state detection.

## GNOME

| Component | Tested value |
| --- | --- |
| Distribution | Ubuntu 24.04.5 LTS; official Noble cloud image 20260926 |
| Kernel | 6.8.0-142-generic |
| Compositor | GNOME Shell 46.0-0ubuntu6~24.04.15; Mutter 46.2-1ubuntu0.24.04.16 |
| Portal | xdg-desktop-portal 1.18.4-1ubuntu2.24.04.3; GNOME backend 46.2-0ubuntu1; RemoteDesktop interface v2 |
| Media | PipeWire 1.0.5-1ubuntu3.3; WirePlumber 0.4.17-1ubuntu4.1 |
| Renderer | Mesa 25.2.8-0ubuntu0.24.04.2; llvmpipe, LLVM 20.1.2, 256 bits |
| Output | Headless virtual Meta-0, 1280 × 720 at 60 Hz, initial scale 1 |
| Øphio | Ordinary graphical-session user; debug binary built with Rust 1.98.0 inside Ubuntu; backend `portal` |

| Check | Result | Evidence / limit |
| --- | --- | --- |
| Doctor and portal backend selection | pass | `gnome-doctor-final.json`, `gnome/companion-status.json`, `gnome/tuple.txt` |
| Consent dialog, restart restore, disable/re-enable | pass | `gnome-consent-confirmed.jsonl`, `gnome-restore-dialog.jsonl`, `gnome-restore-doctor.json`, `gnome-disable.json`, `gnome-reenable.json` |
| Android pairing and watch | pass | `gnome/watch.json`, `gnome/phone-canvas.png`, `gnome/guest-final.png` |
| Initial control from the UI | fail | Same missing initial control projection as Hyprland; diagnostic initialization required |
| Pointer, click, ASCII, chord, scrolling | pass | `gnome/input-state-final.json` and `gnome/input-events.jsonl`; the witness received `Desktop ASCII 123` |
| Non-ASCII text | fail | `gnome/input-state-final.json`, `gnome-session.log`: unsupported keysyms are dropped |
| Handback and held input on disconnect | pass | `gnome/held-before-disconnect.json`, `gnome/released-after-disconnect.json`: held Shift/button 1 both released |
| Live rescale | fail | `gnome/resize.json`, `gnome-scale-pipewire.txt`, `gnome-display-final.txt`: scale 1 → 1.25 removes Mutter's source node; no continued frames or usable coordinates |
| Lock while controlling | unrun | Standalone headless Shell has no `org.gnome.Shell.ScreenShield`; lock call fails and `LockedHint=no`: `gnome-lock-attempt.txt` |
| Scripted MCP observation/action; phone Stop | pass | `gnome/computer-stop.json`, `gnome/computer-actions.jsonl`, `gnome/computer-observation.png`; five deliberate late actions refused |
| Phone disconnect/reconnect | pass | `gnome/disconnected.json`; input was not automatically restored, and vanished targets were shown as unavailable |

The final watch sample, after restoring a portal session at 1.25×, received
**250 frames in 31.0057 s (8.063 fps)**; first frame **283.4 ms**. The guest
screen remained 1280 × 720 physical pixels. `gnome/guest-final.png` was obtained
independently using `gnome-screenshot`. The held-input observations are
3.772 seconds apart; this is an upper bound from the last held-key event.
At 1.25× the first physical frame advances native geometry to revision 2 while
the phone retains revision 1. Frames continue to arrive, but the phone remains
in Display changed with input disabled; this is a second geometry failure.

Rescaling removes the original Mutter screen-cast stream rather than merely
renegotiating its format. The phone reports paused/stale capture and blocks
input. Disable/enable obtains a new session using saved consent, but this is
manual recovery and does not satisfy the live geometry requirement. The lab
retains both the original failure and the unsuccessful transient-pause experiment.

The host-built binary cannot start here: it requires `GLIBC_2.43`, while this
guest provides glibc 2.39 (`gnome-artifact-abi.log`). Building the same source in
the guest permits route testing; this does **not** qualify a distributable Linux
archive. `gnome-build.log` records the guest build; the final baseline build is
in `gnome-final-build.log`.

The real Remote Desktop dialog was inspected with AT-SPI, its preselected monitor
was retained, Allow Remote Interaction was selected, and Share was activated.
`gnome-consent-confirmed.jsonl` records those selected controls;
`gnome-portal-selected-trace.jsonl` records a successful response containing
devices, streams and a restore-token field, without recording token values.

## KDE Plasma

| Component | Tested value |
| --- | --- |
| Distribution/image | Arch Linux rolling, official cloud image 20260915.594445; upgraded guest packages |
| Kernel | 7.2.7-arch1-1 |
| Compositor | KWin 6.7.5-1; plasma-workspace 6.7.5-1 |
| Portal | xdg-desktop-portal 1.22.1-2; KDE backend 6.7.5-1 |
| Media | PipeWire 1:1.6.9-1; WirePlumber 0.5.17-2 |
| Renderer | Mesa 1:26.2.3-1; llvmpipe, LLVM 22.1.8, 256 bits |
| Output | Virtio DRM Virtual-1, 1280 × 800, 74.99 Hz, scale 1 |
| Øphio | Ordinary graphical-session user; source-built debug binary; backend `portal` |

| Check | Result | Evidence / limit |
| --- | --- | --- |
| Doctor and backend selection | pass | `plasma/doctor.json`, `plasma/companion-status.json`, `plasma/tuple.txt` |
| Consent dialog, restart restore, disable/re-enable | pass | `plasma-consent-accessible.jsonl`, `plasma-enable-accessible.json`, `plasma-restore-dialog.jsonl`, `plasma-disable.json`, `plasma-reenable.json` |
| Android pairing and watch | pass | `plasma/pair.json`, `plasma/watch-initial.json`, `plasma/phone-canvas.png`, `plasma/guest.png` |
| Initial control from the UI | fail | Empty initial control projection; `plasma/initial-control.json` |
| Pointer, click, ASCII, Unicode, chord, scrolling | pass | After initialization workaround; `plasma/input.json`, `plasma/input-events.jsonl`, `plasma/input-guest.png` |
| Handback and held input on disconnect | pass | `plasma/held-before-disconnect.json`, `plasma/released-after-disconnect.json`, `plasma/disconnect-result.json` |
| Live resize/rescale and correct coordinates | fail | Capture resumes, but the phone retains old geometry and native scale is wrong: `plasma/resize.json`, `plasma-scale-pipewire.txt` |
| Lock while controlling | fail | Active lock screen receives synthetic text; phone remains Live with active human control: `plasma/lock-timed.json`, `plasma/locked-probe.png` |
| Scripted MCP observation/action; phone Stop | pass | `plasma/computer-stop.json`, `plasma/computer-actions.jsonl`, `plasma/computer-observation.png`; five deliberate late actions refused |
| Phone disconnect/reconnect | pass | `plasma/disconnected.json`, `plasma/reconnected.json`; offline then online, with no automatic input ownership |

First frame: **733.9 ms**. Steady sample: **233 frames in 31.020 s
(7.511 fps)**, with all 236 startup/sample frames drawn before acknowledgement.
A separate sample after restarting Øphio received **189 frames in 31.0078 s
(6.095 fps)**, first frame **318.9 ms** (`plasma/watch-after-restart.json`).
Independent Spectacle and QMP screenshots show the guest desktop and input
witness seen by the phone. Held-input observations were 3.749 seconds apart;
reconnection was observed 6.592 seconds after restoring emulator networking.
These are observation intervals, including polling and command overhead.

The real KDE Remote Control dialog showed screen viewing and input control,
with restoration allowed. Guest AT-SPI activated **Approve**. Qt accessibility
must be enabled in the portal's activation environment. The initial KWin virtual
backend could not allocate DRM buffers in this guest; the tested configuration
uses `kwin_wayland --drm` on the guest's virtio GPU.

Changing the output to 1920 × 1080 at 1.25× gives KWin a logical size of
1536 × 864. Øphio instead reports scale **1.5**, using the new physical
width against the old logical width of 1280. Native geometry advances to revision
2 while the phone retains 1280 × 800, scale 1, revision 1. PipeWire pauses and
resumes successfully; frames continue, but correct coordinate mapping cannot
be qualified. Fixing only the paused-stream handling would not resolve this.

For the lock failure, the guest confirmed `org.freedesktop.ScreenSaver.GetActive`
was true before the probe. Logind also reported `LockedHint=yes`. The phone's
normal Desktop input callback then accepted `LOCK_TEST`. Its input receipt said
`injected` by the 502 ms sample; the active lease and Live UI persisted through
the 8007 ms sample. The guest QMP image shows characters in the lock-screen
password field. No password or other credential was supplied. This establishes
actual input reaching the locked session, not merely a misleading receipt.

The portal session remaining open is insufficient evidence that the desktop is
unlocked. Øphio needs authoritative lock observation, input fencing,
and a corresponding phone state. The current lock-request operation alone does
not supply that observation. This requires coordinated implementation and a
regression run; no speculative product fix is included here.

## Cross-route findings and reproduction notes

The initial-control failure can be isolated by sending an authenticated
`pause_input` command once through the phone session. This creates the missing
control projection, after which the normal UI can request a lease. The lab's
`initialize-control` step records the failure before applying this workaround.
Input passes following that step do not turn the initial UI failure into a pass.
The owning services/client code needs to publish initial control views and update
the phone's target projection after geometry changes.

GNOME's legacy portal keysym path silently drops characters absent from its active
keyboard group. The guest logs report missing keycodes for the Unicode fixture;
the witness receives `Caf` and spaces instead of the accented, Greek and Japanese
text, despite successful D-Bus input acknowledgements. No application-specific
Unicode-entry shortcut or clipboard substitution was used to disguise this.

The scripted harness launches the real `computer-mcp` executable with the
per-run configuration from Øphio. It observes the screen, clicks the witness,
types synthetic `x` characters, acknowledges native interrupt, and deliberately
attempts five additional actions. A Stop pass requires all five to fail and no
`AFTER_STOP` text in the guest witness. Run completion alone is insufficient.

Frame timing observes the real app media link and canvas; no frame or transport
is replaced. The sample starts after the first acknowledged frame and runs for
at least 31 seconds. Only one instrumentation wrapper is installed, and duplicate
sequence numbers invalidate a sample. First-frame time is media-open to first
draw acknowledgement. These software-renderer measurements are not a throughput
promise for physical computers. Fixture text and changing timestamps are the
only desktop content used in these runs.

To reproduce, follow the lab README to provision each target, restart with
restricted networking, install and pair the Android debug build, then run
`watch`, `initialize-control`, `input`, the disconnect helper, and `computer`.
Record initial UI failures before applying the diagnostic workaround. Accept
portal consent through guest AT-SPI, restart Øphio to test restoration,
and run desktop disable/enable independently. Retain intermediate evidence
before reusing a helper's output filename.

For Hyprland geometry, use `hyprctl keyword monitor
'Virtual-1,1600x1200@60,auto,1.25'` through `in-session.sh`. For GNOME, enable
`scale-monitor-framebuffer` and apply 1.25 scale using Mutter's DisplayConfig
interface, preserving the current serial and physical mode. For Plasma, use
`kscreen-doctor output.Virtual-1.mode.1920x1080@60 output.Virtual-1.scale.1.25`.
Capture phone state and a fresh native snapshot while the existing watch stays
open. Do not treat starting a new stream as recovery of that stream.

For the Plasma lock reproduction, restore the original output, open a live
watch and run `phone.mjs plasma arm`. Request `org.freedesktop.ScreenSaver.Lock`
in the guest, confirm `GetActive=true`, then invoke the armed phone's normal
`labSend({action:'text',text:'LOCK_TEST'})` through its isolated WebView CDP.
Record input acknowledgements and UI at approximately 0.5, 1.5, 4 and 8 seconds,
and capture the guest with `lab.py screenshot plasma`. The earlier delayed
probe was refused after stale-picture expiry; only the timed probe distinguishes
that fallback from lock fencing.

At the end of the original run, all guests and the isolated emulator were stopped. No lab TCP
forward remained listening (`shutdown.json`). X11 and release archive
installation were not qualified. No dependency was added to the product.

## Repository verification

Commands used the build toolchain, a checkout-local `CARGO_TARGET_DIR`, eight
build jobs and cleared host desktop variables.

| Command | Result |
| --- | --- |
| `cargo fmt --all` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS SHELL=/bin/sh cargo test --workspace` | pass: 487 passed, 2 ignored, 0 failed across 52 suites |
| `bash crates/test-support/check-parallel.sh` | pass: 25 normal and 10 CPU-loaded runs; each 487 passed, 2 ignored, 0 failed |
| `python3 -m unittest discover -s tools/lab -p 'test_*.py' -v` | pass: 7 isolation/verification tests |
| `ruff format --check tools/lab` and `ruff check tools/lab` | pass, Ruff 0.15.0 |
| `prettier --check tools/lab/phone.mjs` | pass, Prettier 3.6.2 |
| `node --check tools/lab/phone.mjs` | pass |
| `for file in tools/lab/guest/*.sh; do bash -n "$file" || exit; done` | pass |
| `git diff --check` | pass |

The two ignored tests were already ignored at the source baseline: transport
finding K-1 and continuation finding DL-2. Logs are `final-fmt.log`,
`final-clippy.log`, `final-tests.log`, `final-parallel.log` and
`final-lab-tests.log`. Individual repeated-suite logs and counts are under
`target/parallel-evidence/acceptance/`.
The Android debug build completed using
`npm run tauri android build -- --debug --apk --target x86_64` in the staged
Android project (`android-build.log`). No shared Rust change remains, so a
Windows cross-check was not required. Ruff and Prettier were local test tools
(both MIT), not new runtime or distributed dependencies.


## Follow-up with control and portability fixes

Run date: 2026-09-28 UTC. These results supplement, rather than overwrite, the
original evidence above. Raw paths in this section are relative to
`target/lab-evidence/q2/`. The release binaries contain source `0d3e333`; the
subsequent `b585512` commit adds only recorded-geometry tests. The Android debug
APK contains client source `baf539b`; later client changes affect only the
simulator. Guests retain the exact compositor, portal, kernel and media package
versions in the original tables; no packages were installed or upgraded for
this follow-up. The Hyprland tuple identifies Mesa as `1:26.2.3-1`.

All runs used one restricted-network guest at a time and the real Android app,
with the original frame and held-input measurement method. The isolated phone
uses emulator 5582, ADB 5042 and WebView forwarding 9232. A fresh guest data
directory and Welcome enrollment establish the first owner without diagnostic
control commands. `initialize-control` was not used. Harness calls remain
fixture-only.

### Portable archive

The release build runs offline inside the existing Ubuntu 24.04.5 guest with
Rust 1.98.0, its installed native libraries and vendored locked dependencies.
This chooses an already available glibc 2.39 baseline instead of downloading a
new toolchain or distribution. It uses six Cargo jobs. Both binaries were
assembled into the actual Linux archive and then installed from that archive
in Ubuntu and the Arch guests.

- Archive: `dist/linux/ophio-0.1.0-x86_64-linux.tar.gz`, 17,384,772 bytes.
- SHA-256: `65678c2f51a044130501a1e61d34c296d781419fd7c132f3fb1adb9bbb440c01`.
- Highest required symbol versions: ophio `GLIBC_2.39`, computer-mcp
  `GLIBC_2.34`; the packaging check rejects either binary above 2.39.
- Ubuntu archive install, version/help and dynamic loading: **pass**;
  `gnome/archive-installed-final.log`. The native transport and desktop checks
  below run that installed archive. The build log is retained separately at
  `target/lab-evidence/q2-release-build-source.log`.

### Hyprland follow-up

A minimal session had no notification daemon, so the new local-Stop requirement
correctly refused the first lease. The lab-only GTK notification witness now
shows working Stop buttons and emits real notification action signals. An early
center click hit that window and revoked control; those input attempts are not
passes. The final witness is positioned away from the center input probe.
This setup qualifies the local action interface with a lab witness, not an
uninstalled production notification daemon.

| Check | Follow-up result | Evidence / limit |
| --- | --- | --- |
| Initial control from UI | pass | `hyprland/initial-control.json`, `hyprland/arm.json`; a Ready view exists before any control command, and normal Enable control admits a lease once local Stop is available |
| Android watch | pass | `hyprland/watch-run.log`, `hyprland/phone-canvas.png`; 356 frames in 31.0109 s (11.4798 fps), first frame 310.8 ms |
| Pointer, click, ASCII, Unicode, chord, scroll | pass | `hyprland/input.json`, `hyprland/input-state-final.json`; all seven acknowledgements injected, complete accented/Greek/Japanese text in the witness |
| Live resize/rescale and coordinates | pass | `hyprland/resize-final.json`; same target/watch advances 1280×720 revision 1 to 1600×1200, scale 1.25, revision 2; center click is accepted and compositor pointer is logical 639,479 |
| Held input and disconnect/reconnect | pass | `hyprland/disconnect-result.json`; Shift/button observed then released, 2970.294 ms observation interval; offline then online after 8676.098 ms from network restoration |
| Lock during control | pass | `hyprland/lock-timed.json`, `hyprland/lock-native-final.txt`, `hyprland/qmp.png`, `hyprland/snapshot.png`; revoked lease, Locked UI, hidden picture and no injected input at 501.4, 1504.8, 4009.6 and 8021.5 ms |
| MCP observation/action and phone Stop | unrun | `hyprland/computer-run.log`; native computer-use delegation is not configured in the older authored harness fixture |

Hyprlock 0.9.6 is run with immediate rendering; the compositor confirmed its
lock at 454.0 ms. The normal phone input callback was already removed, so the
probe records refusal rather than an injected acknowledgement. Logind still
reported `LockedHint=no`. Hyprland's `j/locked` reply supplies authoritative
state. Its documented IPC event socket has no lock-specific event; the adapter
rechecks on socket activity, every 250 ms and immediately before each native
injection. This does not establish a lock-specific push signal. Earlier
resource-loading and probe-tool failures are retained separately.

The original frame summary survives in `watch-run.log`, but a later failed
short open overwrote its per-frame JSON. The helper now stores open failures
under their own step name. No per-frame draw assertion is claimed for this
retained Hyprland sample.

The notification window's Stop action was observed to revoke a lease during the
early probe. This is separate from the blocked agent/MCP Stop qualification.

### GNOME follow-up

| Check | Follow-up result | Evidence / limit |
| --- | --- | --- |
| Initial control from UI | pass | `gnome/initial-control.json`; normal Enable control needs no pause command |
| Android watch | pass | `gnome/watch.json`, `gnome/guest-q2-final.png`; 360 frames in 31.0061 s (11.6106 fps), first frame 69.1 ms |
| Whole-text refusal | pass | `gnome/input.json`, `gnome/input-state-final.json`; unavailable characters are named and no prefix is typed |
| Explicit Paste instead | fail | `gnome/paste-final.json`; the user choice sets clipboard and sends the chord, but the guest witness did not confirm pasted text after rescaling; an injected acknowledgement alone is insufficient |
| Scale geometry and source recovery | partial | `gnome/resize-final.json`, `gnome/reopen-final.json`; phone receives scale 1.25 and revision 3, names source loss, and one Reopen sharing action opens a new stream with saved consent; correct input coordinates remain unqualified |
| Lock while controlling | unrun | `gnome/lock-timed.json`; standalone Shell lacks `org.gnome.Shell.ScreenShield`, so no real lock could be produced |
| Held input and disconnect/reconnect | unrun | `gnome/disconnect-final-run.log`; stale-picture state prevented admission before the outage probe |
| MCP observation/action and phone Stop | unrun | `gnome/computer-run.log`; authored fixture lacks native child delegation readiness |

Mutter's headless seat advertises no keyboard (`gnome/wayland-current.txt`).
Requesting a keyboard on that seat crashed the compositor in an intermediate
build; the final build checks the seat capability first. With no reachable
keymap, text is conservatively refused in full, including ASCII. The final
refusal leaves the compositor running. Clipboard support does not by itself
prove that a paste chord reached the intended application.

The old screen-cast node is removed on rescale. Registry removal, including a
node already absent during initial enumeration, now ends that stream explicitly.
A mere PipeWire Paused transition does not. Reopen sharing creates a new portal
session; it is not continued capture from the old stream. Saved consent avoided
a dialog in this guest. The portal API provides no guarantee that restoration
will be dialog-free, so recovery remains an explicit user action and explains
that permission may be requested.

### Plasma follow-up

| Check | Follow-up result | Evidence / limit |
| --- | --- | --- |
| Initial control from UI | pass | `plasma/initial-control.json`, `plasma/input.json`; normal Enable control admits a lease without a diagnostic command |
| Android watch | pass | `plasma/watch.json`, `plasma/phone-canvas.png`; 362 frames in 31.0078 s (11.6745 fps), first frame 183.4 ms; all 364 startup/sample frames drawn before acknowledgement |
| Pointer, click, ASCII, chord, scroll | pass | `plasma/input.json`, `plasma/input-state-final.json`; witness receives complete ASCII and records click/scroll |
| Unicode refusal and explicit paste | pass | `plasma/input.json`, `plasma/paste-final.json`, `plasma/paste-witness.json`; text is refused in full, then the labelled Paste instead choice delivers complete accented, Greek and Japanese text |
| Live resize/rescale and coordinates | pass | `plasma/resize-final.json`; same target/watch advances to revision 2, 1920×1080 at 1.25; KWin independently reports 1536×864 logical size; center click reaches the witness |
| Held input and disconnect/reconnect | pass | `plasma/disconnect-result.json`; held Shift/button are released, 3743.513 ms observation interval; offline then online after 6560.406 ms from network restoration |
| Lock during control, original output | pass | `plasma/original-lock-before.json`, `plasma/lock-timed.json`, `plasma/lock-native.txt`, `plasma/qmp.png`, `plasma/snapshot.png`; original 1280×800 at scale 1 is verified before locking; lease revoked and no injected input at 687.3, 1504.3, 4011.6 and 8021.1 ms |
| MCP observation/action and phone Stop | unrun | `plasma/computer-run.log`; authored harness lacks native computer-use delegation readiness |

Both `GetActive=true` and `LockedHint=yes` are recorded. The lock request and
confirmation took 683.0 ms, so the first sample is later than the requested
500 ms; this is reported as an observation bound, not sub-500 ms latency. The
normal Desktop callback is unavailable because the locked UI removes its
picture/input controls. There is no injected acknowledgement, and the independent
guest image shows the lock screen without inserted text. The Locked UI, hidden
picture and revoked lease persist through eight seconds. The same result was
also observed at scale 1.25 (`plasma/scaled-lock-timed.json`).

KScreen rejects a mode string containing the fractional refresh rate while
returning success. The final restore uses the recorded numeric mode ID 1 and
checks the phone's actual 1280×800 geometry before the lock request. The first
scaled lock run is retained, not relabelled as the original-size run.

### Limits recorded at the end of the earlier follow-up

The initial-control defect and the measured Hyprland/Plasma geometry failures
are fixed in these runs. The Plasma lock-screen input failure is no longer
reproduced. Missing or broken lock observation is reported as unknown and fences
control; a portal session remaining open is not treated as proof of unlock.
The service revokes leases and withdraws agent observations on lock, while the
native input path rechecks immediately before injection. Fake D-Bus/IPC tests
cover signals and a lock arriving between admission and injection.

**Full route qualification is still not complete.** GNOME lock, correct input
after rescale and pasted-text delivery need qualifying evidence in a session
with a working screen shield and keyboard. All three MCP/agent Stop reruns are
blocked by the authored fixture's missing delegation capability; readiness was
not bypassed and no live inference was run. Desktop disable/re-enable was not
repeated as a separate follow-up qualification. Saved-consent restoration was
exercised by the GNOME reopen and the Plasma restart.

The portal keymap preflight is conservative: it requires characters to exist
in every reachable keyboard group because a passive Wayland client cannot
observe another application's active group. A missing keymap refuses the whole
text. This also changes Plasma's former direct Unicode behavior into an explicit
paste choice; that choice passed in Plasma. No clipboard substitution occurs
without the person's action.

The simulator plan is `client/scripts/plans/locked-desktop-text.json`. Two complete review
iterations, a final 15-case run, and focused source-reopen iterations cover light,
dark, 360-pixel/200% text, wide/landscape, offline, locked, refused text and
explicit paste. Final captures are under `q2/simulator/` and
`q2/simulator-reopen-final/` relative to `target/lab-evidence/`. Each final step
reported `ok`, with no console or page-overflow errors. Review corrected stale
frame stretching during geometry changes, a refusal message surviving successful
paste, and redundant locked controls. The shared safety strip is still tall at
200% text; earlier removed targets can retain an Input needs checking warning
after Øphio restarts. These are not silently cleared as confirmed releases.

The final workspace test run passed 619 tests with two ignored tests. The
parallel stress gate remains **failed** in terminal integration tests: an earlier
attempt failed `real_shell_exit_code_and_replay_offsets`; a repeat passed all
25 normal iterations and the first loaded iteration, then failed
`repeated_attach_during_heavy_output_has_no_gaps_or_duplicates` in loaded
iteration 2. The run was bounded to six CPUs and six Cargo jobs to respect the
shared machine. Logs are `target/q2-parallel-final.log` and
`target/q2-parallel-retry.log`. These terminal files were not changed here.

All guests, the Q2 emulator, its ADB server and the simulator were stopped at
the end. `q2/shutdown.json` (relative to `target/lab-evidence/`) records closed
loopback endpoints 47222, 47112, 5891, 5582, 5583, 5042 and 9232. Lab disks,
archive, build outputs and evidence remain excluded from source control for
review; no release was published.

## Final follow-up

Run date: 2026-09-28 UTC. Evidence in this section is relative to
`target/lab-evidence/q3/`. Earlier tables remain historical observations.
Øphio and the bridge were built offline in the existing Ubuntu guest with
Rust 1.98.0 and eight build jobs, from product source `f35b1ac`. The Android APK
contains client source `985ff49`; the final portal-recovery copy was additionally
verified in the simulator. Earlier lock/disconnect/enable samples on Hyprland
and Plasma used `f044ffe`; their final Stop/watch checks use the rebuilt binary.
The only intervening Rust change is the capture-cleanup repair described below. The executable SHA-256 values are:

- Øphio: `fac6e4a9fc3d3ddc307875149fbdec9a031c0103193a10abfe43f2f16f112728`.
- Bridge: `a6efa6cf3a0ee30ea69f7b850f55c8fd683b212e0e10473e3f769ece55851977`.

The required GLIBC versions remain 2.39 and 2.34 respectively. These are the
executables installed in all three guests; the earlier archive is not relabelled
as containing these fixes. Images and disks were reused through canonical lab
paths. Only the missing GDM/session packages were installed, in provisioning
mode; every qualification run used restricted networking. The Android emulator
had fresh lab-local userdata, no window, ADB 5042, serial 5582 and CDP 9232.

The current authored Claude help contains every flag required by the adapter,
including `--tools` and `--system-prompt-snapshot`. The stale installed probe was
replaced. A regression test compares the fixture help against the adapter’s
actual required flag list. The scripted specialist validates the child-only MCP
configuration and uses the real per-run bridge. Neither readiness nor child
isolation was bypassed. `computer-readiness.json` records availability on the
phone for each route; the owner explicitly granted control and computer use.

Stop measurements use one run per final route result. The phone records
`performance.now()` immediately before the task’s Stop button click and at the
first projection with both a revoked lease and withdrawn observation. Receipt
arrival is measured separately. Guest witness `realtime_ns` and Øphio’s
receipt timestamp use the same guest clock; no phone/guest clock subtraction is
used. The witness must record observation-driven movement, a click and text.
Five calls deliberately started after native interrupt must fail, and no witness
input may follow the receipt. A call already in flight is classified by its start,
not by its reply time. The helper now also requires active agent input at the tap.

The helper now leaves any earlier Desktop view before creating a new run,
matching the app’s review flow. Previously its navigation could legitimately
pause the newly granted agent while retiring the old supervisor.

Whole-machine CPU stress in other checkouts overlapped some early samples.
The GNOME and Plasma `loaded-*` Stop attempts had already paused/expired input
before the tap; they are **fail**, not mid-action Stop passes. GNOME’s
`text-timeout.json` also preserves an interrupted batch with an honest unknown
outcome. No timeout or native lock check was relaxed to get a qualification pass.

### Hyprland — Final follow-up

The package tuple is unchanged: Arch image 20260915.594445, kernel
7.2.7-arch1-1, Hyprland 0.56.2-3, Aquamarine 0.15.1, hyprlock 0.9.6,
xdg-desktop-portal 1.22.1-2, hyprland portal 1.4.1-2, PipeWire 1:1.6.9-1,
WirePlumber 0.5.17-2 and Mesa 1:26.2.3-1. Output: virtio Virtual-1,
1280×720 at scale 1. `hyprland/tuple.txt` records the exact native tuple.
The lab notification witness continued to provide visible, actionable local Stop.

| Check | Final result | Evidence / measurement |
| --- | --- | --- |
| Initial control and watch | pass | `hyprland/initial-control.json`, `watch.json`; 238 frames in 31.0654 s (7.6613 fps), first frame 767.0 ms; every recorded frame had a draw before acknowledgement |
| MCP observation/action; phone Stop | pass | `hyprland/computer-stop.json`, `stop-result.json`, `stop-actions.jsonl`, `stop-input-events.jsonl`; active agent at tap, 13 witness text changes and one click, five deliberate late calls refused, zero input after receipt; revocation 453.8 ms, receipt arrival 946.6 ms |
| Lock during control after signal watcher changes | pass | `hyprland/lock-timed.json`, `lock-native.txt`, `qmp.png`; lock confirmed at 884.237 ms; revoked/locked with no input acknowledgement at 888.871, 1508.598, 4002.486 and 8021.171 ms |
| Held input and disconnect/reconnect | pass | `hyprland/disconnect-result.json`; Shift/button observed then released, 2356.976 ms observation interval; offline then online after 5601.219 ms from network restoration |
| Desktop disable → re-enable | pass | `hyprland/disable.json`, `disabled-phone.json`, `reenable.json`, `open.json`; removed target clears its safety state; new target opens live |

The earlier lock confirmation was 454.0 ms, with samples at 501.4, 1504.8,
4009.6 and 8021.5 ms. The earlier held-input/reconnect intervals were
2970.294/8676.098 ms. These are one sample per version, not latency percentiles
or a claim that the signal change improved scheduling under shared host load.
Hyprland still has no lock-specific IPC event: socket activity plus the 250 ms
fallback and the native check before insertion remain necessary. A stopped
locker left the compositor locked, so the disposable guest was rebooted before
subsequent checks; no credential was entered.

### GNOME — Final follow-up

GNOME now runs in a normal GDM Wayland session on seat0/tty2 and the guest’s
virtio output, with real virtual keyboard and pointer devices. GDM is
46.2-1ubuntu1~24.04.9 and gnome-session-bin is 46.0-1ubuntu4. The unchanged
base tuple is Ubuntu 24.04.5, kernel 6.8.0-142-generic, Shell
46.0-0ubuntu6~24.04.15, Mutter 46.2-1ubuntu0.24.04.16, portal
1.18.4-1ubuntu2.24.04.3, GNOME backend 46.2-0ubuntu1, PipeWire
1.0.5-1ubuntu3.3, WirePlumber 0.4.17-1ubuntu4.1 and Mesa
25.2.8-0ubuntu0.24.04.2. See `gnome/tuple.txt`, `gdm-versions.txt` and
`session-type.txt`. Initial output is 1280×800 at scale 1; rescale is 1.25.

The normal seat exposed a repeated-keymap-read defect: Mutter can send file
descriptors sharing a file position. Positional bounded reads now preserve the
position, so subsequent ASCII preflight sees the complete keymap. Tests cover
shared cursors, truncation, invalid bytes and size limits. Whole-text refusal
still occurs before any prefix is inserted; clipboard substitution remains an
explicit Paste instead choice.

GNOME’s legacy absolute pointer method consumes stream pixels; the KDE route
consumes logical coordinates. The selected desktop now chooses that mapping,
with tests for scale, monitor origin and out-of-bounds input. After rescaling,
the witness center is (512,293) in a 1024×586 text view. The observed point was
(511.602,292.602), within one logical pixel, in
`gnome/final-rescale-input-witness.json` and `final-rescale-input.json`.
The same run delivered the full ASCII batch after the coordinate change.

Catalog refresh now continues independently of lock signals, so source removal
and geometry revisions reach the phone even when the lock state stays unchanged.
An acknowledged portal closure is reusable during explicit reopening, including
a portal that has removed its old session object. A failed Close reply still
leaves the outcome unknown. `gnome/closed-session-reopen.json` records actual
recovery after lock and rescale; the new unit test covers both acknowledged and
unacknowledged closure.

| Check | Final result | Evidence / measurement |
| --- | --- | --- |
| Normal GDM seat and initial control | pass | `gnome/session-type.txt`, `gdm-versions.txt`, `initial-control.json`; no initialization workaround |
| Watch | pass | `gnome/watch.json`, `phone-canvas.png`; 355 frames in 31.006 s (11.4494 fps), first frame 72.4 ms; every recorded frame drawn before acknowledgement |
| ASCII, click, chord and scroll | pass | `gnome/final-ascii-witness.json`, `final-rescale-input.json`; complete `Desktop ASCII 123` batch before and after rescale |
| Whole-text refusal and explicit Paste instead | pass | `gnome/paste-refusal.json`, `paste.json`, `final-paste-witness.json`; no partial prefix from the refused batch, complete accented/Greek/Japanese text after explicit Paste |
| Rescale and correct input afterwards | pass with explicit reopen | `gnome/final-rescale-source-loss.json`, `final-rescale-input.json`, `final-rescale-input-witness.json`; new stream at 1.25, click within one logical pixel of the center |
| Continuous old stream across rescale | fail — native source removed | Mutter 46 destroys its old monitor stream; explicit reopening obtains a new stream. Øphio cannot retain a node the compositor deleted |
| Lock while controlling | pass | `gnome/lock-timed.json`, `lock-native-final.txt`, `input-events.jsonl`; confirmation 629.583 ms; locked/paused/revoked and no input acknowledgements at 633.223, 1505.374, 4011.677 and 8020.634 ms |
| Held input and disconnect/reconnect | pass | `gnome/disconnect-result.json`; held Shift/button released, 3734.547 ms observation interval; offline then online after 8874.716 ms from restoration |
| Desktop disable → re-enable | pass | `gnome/final-disable.json`, `final-reenable.json`, `final-reenable-dialog.jsonl`, `open.json`; fresh explicit portal consent, subsequent live picture |
| Saved consent after restarting Øphio | pass | `gnome/final-restore-dialog.jsonl`, `final-restore-open.json`; no new dialog, first frame 73.5 ms |
| MCP observation/action; phone Stop | pass | `gnome/computer-stop.json`, `stop-result.json`, `stop-actions.jsonl`, `stop-input-events.jsonl`; active agent at tap, 17 text changes and one click, five deliberate late calls refused, zero input after receipt; revocation 74.1 ms, receipt arrival 345.8 ms |
| request_human private handoff | pass | `gnome/computer-private.json`, `computer-private.png`; ready true, input and screenshot acknowledgements true, no uncovered endpoints, prior run paused |

These are one qualifying run per row (four timed observations within the lock
run), except the five capture reopen cycles below. The earlier standalone Shell
had no native lock/keyboard seat to compare. `lock-pre-final-stale-phone.json`
is retained as an invalid observation: its stale phone acknowledgements and
transient connection did not establish the required locked projection. The clean
repeat above started online with an active human lease and an empty input-ack
list. The lab validator now rejects contaminated, incomplete or unlocked samples.

The native ScreenSaver and logind sources both confirmed the final lock.
The normal input callback refused the probe, and no lock-test text reached the
witness. QMP's final image shows the powered-down virtio output; it is not claimed
as a visible lock-screen capture. A prior probe displayed the GNOME shield after
a guest-tablet wake; no key, click or credential was used to wake it. Portal
sharing can end at lock; input remains off, and explicit desktop disable/enable
on the guest restored sharing. The phone now distinguishes this condition from
revoked device pairing instead of recommending pairing again.

### Plasma — Final follow-up

The native tuple remains Arch image 20260915.594445, kernel 7.2.7-arch1-1,
KWin/plasma-workspace 6.7.5-1, portal 1.22.1-2, KDE backend 6.7.5-1,
PipeWire 1:1.6.9-1, WirePlumber 0.5.17-2 and Mesa 1:26.2.3-1.
The virtio output is 1280×800 at scale 1. The real Remote Control consent
was accepted through guest AT-SPI; `plasma/consent-dialog.jsonl` and
`consent-accepted.json` record it without storing restore-token values.


| Check | Final result | Evidence / measurement |
| --- | --- | --- |
| Final rebuilt watch | pass | `plasma/watch.json`, `phone-canvas.png`; 361 frames in 31.0074 s (11.6424 fps), first frame 55.8 ms; every recorded frame drawn before acknowledgement |
| Lock during control after signal watcher changes | pass | `plasma/lock-timed.json`, `lock-native.txt`, `qmp.png`, `snapshot.png`; confirmation 2217.030 ms; locked/revoked with no acknowledgement at 2235.918, 2262.754, 4009.853 and 8009.335 ms |
| Desktop disable → re-enable | pass | `plasma/disable.json`, `disabled-phone.json`, `reenable.json`, `reenable-consent.json`; explicit portal consent and a subsequent live open |
| MCP observation/action; phone Stop | pass | `plasma/computer-stop.json`, `stop-result.json`, `stop-actions.jsonl`, `stop-input-events.jsonl`; active agent at tap, 17 witness text changes and one click, five deliberate late calls refused, zero input events after receipt; revocation 57.0 ms, receipt arrival 220.4 ms |
| Held input and disconnect/reconnect | pass | `plasma/disconnect-result.json`; Shift/button observed then released, 4775.387 ms observation interval; offline then online after 9835.474 ms from restoration |

The earlier held-input/reconnect intervals were 3743.513/6560.406 ms.
The earlier original-size lock confirmation was 683.0 ms, with samples at
687.3, 1504.3, 4011.6 and 8021.1 ms. Both versions are single observations
including request/confirmation overhead. A preliminary attempt encountered an
automatic idle lock before admission and was not counted; guest idle locking was
then disabled without disabling the explicit lock operation.

### Final Stop timestamp comparison

Each row is one active-input run using the rebuilt Øphio. Raw event lists
also record pointer movement, button press/release and individual text changes.
Timestamps below share the guest real-time clock; phone latency uses its separate
monotonic clock as described above.

| Route | Last witness input (ns) | Øphio receipt (ms) | Phone tap → revoked input (ms) | Phone tap → receipt (ms) |
| --- | ---: | ---: | ---: | ---: |
| Hyprland | 1790583690392502527 | 1790583690895 | 453.8 | 946.6 |
| Gnome | 1790582560416227594 | 1790582560582 | 74.1 | 345.8 |
| Plasma | 1790583936463182280 | 1790583936777 | 57.0 | 220.4 |

### Capture cleanup regression

A repeated GNOME watch/private-step sequence exposed a native capture cleanup
crash in source `f044ffe`: a callback re-entered while the pending-result
`RefCell` was still borrowed. `gnome/capture-cleanup-crash.log` retains the
backtrace. Source `f35b1ac` replaces that handoff with a move-only cell, so no
runtime borrow survives a callback or native destructor. Two regression tests
exercise re-entry during successful/error cleanup, terminal-error preservation
and full-frame recovery when pending frames are replaced. No fencing or input
deadline was relaxed. Five subsequent GNOME watch-open cycles and the final
31-second watch completed with the same Øphio process; see
`gnome/capture-cycle-1.txt` through `capture-cycle-5.txt` and `watch.json`.

### Phone safety state and visual review

Removed targets now remove their control/safety projection on catalog updates
and authoritative snapshots. A later control event for an absent target cannot
resurrect it. A boot change by itself retains honest uncertainty until the new
inventory arrives. Four projection tests cover these distinctions, and historical
intervention receipts remain available.

The shared rail keeps locked and paused state visible in a compact full-width
status row. Stop and Leave control remain reachable; long target names truncate
only in the collapsed row. The expanded rail retains the locked state. At 360 px
and 200% text the measured collapsed heights are 97 px while active and 133.8 px
while locked/paused. No action or state label is hidden to obtain that size.

`client/scripts/plans/compact-safety-status.json` contains 17 scenarios covering light/dark,
360 px/200% text, wide/landscape, offline, empty, removed target plus a late event,
Stop, expanded locked state, revoked access, ended portal sharing and unknown input. Every final step
reported `ok`, with no console errors or horizontal page overflow. Every image
was inspected in multiple complete iterations; final images are in
`q3/ui-final-portal/`. Review corrected the status hierarchy, missing locked
label in the expanded rail and unnecessarily long sharing-recovery copy. Collapsed names still use an ellipsis; expansion
shows the full context. No new dependency was added.

## Release archive, new user

Run dates: 2026-09-29 UTC for all three routes and 2026-09-30 UTC for a GNOME
recheck. Evidence in this section is relative to `target/lab-evidence/`.
The earlier sections used a lab account and executables the lab installed. This
pass started from the release archive, in a fresh account (`newuser`) that had
never run Øphio, and followed only the archive's `INSTALL.md`, through to the
documented uninstall. Provisioning created the account (`u50/*-newuser.log`), and
every run afterwards used restricted networking. The phone was the Android app
(debug universal APK `e6259ff822ec047516f1ef63a6070190d07c5a0a1563df4a44116fb66e949157`,
`u50/apk.txt`) on a lab emulator.

Both archives were built offline in the Ubuntu builder guest, with Rust 1.98.0,
by the release build script:

- The first pass used source `e56be0d`: `ophio-0.1.0-x86_64-linux.tar.gz`,
  19,828,751 bytes, SHA-256
  `c623531ae7b5205a1069b560d65c24fe9383a7aae85a1c1257d082f3629983fb`
  (`archives/e56be0db1e42-build.log`).
- The GNOME recheck used source `a9e5df5`: 19,858,411 bytes, SHA-256
  `345faae0bb4ac4167340b67d14b8177af889276e4d0912adc75d8178a8776807`
  (`u50/archives/a9e5df510b68-build.log`).

Both builds report 434 license components, no failures, no exceptions and one
review item: `option-ext` 0.2.0, MPL-2.0, which is shipped. Øphio needs GLIBC
2.39 and the bridge needs 2.34. The Arch and Ubuntu base images matched their
published checksums, with valid signatures (`u50/image-verification.txt`).

Hygiene of the first archive is recorded in `u50/hygiene/`:

- **Contents.** The archive holds nine files, all owned by uid/gid 0 with one
  fixed timestamp.
- **Scan.** The source-candidate rules found nothing. The only embedded build
  paths are remapped `cargo`/`src` roots.
- **Licensing.** `NOTICE` is present and `LICENSE` is Apache-2.0.
- **SBOM.** It is CycloneDX 1.5 with 433 components. Every one of the 154
  vendored crates referenced by `ophio`, and the 11 referenced by
  `computer-mcp`, is present in it.

Neither archive is the final release artifact. Changes committed after
`a9e5df5` are covered by tests, not by this lab pass. The release archive is
rebuilt from the release commit.

### Hyprland — new user

Arch Linux, kernel 7.2.7-arch1-1, Hyprland 0.56.2-3, Aquamarine 0.15.1-1,
xdg-desktop-portal 1.22.1-2, Hyprland portal 1.4.1-2, PipeWire 1:1.6.9-1,
WirePlumber 0.5.17-2, libxkbcommon 1.13.2-1, glibc 2.44+r50, Mesa
1:26.2.3-1; mako 1.11.0-1 was added during the run (`u50/hyprland/tuple.txt`).

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Install from the archive | pass with remedy | `u50/hyprland/01-install.txt`, `02-relogin-path.txt`, `03-path-remedy.txt`; installer exit 0 and named the missing PATH entry. `ophio` was still not on PATH after logging in again, because the default Arch shell startup adds nothing. The PATH line the guide gives fixed it |
| `ophio doctor` before the first run | fail, fixed | `01-install.txt`: raw "I/O failure: No such file or directory". It now says Øphio is not running and how to start it (0924e29a; `u50/gnome-recheck/01-install-doctor.txt`) |
| Run by hand | pass | `04-run-by-hand.txt`; status names each remaining setup step and its command |
| Start at login | pass with remedy | `05-service-install.txt` … `08-autostart-remedy.txt`. Hyprland starts no `graphical-session.target`, so `service install` fell back to `default.target` and said desktop access would be unavailable, and Desktop view then reported no graphical endpoints. After uninstalling the service, `exec-once = ophio run` started Øphio with Capture and Lock session ready |
| Input and computer use | pass with remedy | `04-run-by-hand.txt`, `15-install-mako.txt`; Input stayed not ready and named an action-capable notification service. With mako running, Input and Computer use were ready |
| Library folder and project | pass | `09-root-and-project.txt` |
| Pair | pass | `10-pair.txt`, `paired.png`; both screens showed 309655 |
| Agent task | pass | `task.json`, `task-approval.png`, `task-conversation.png`, `task-receipt.png`; completed through the per-run permission bridge ("Permission allow") |
| Watch | pass | `watch.json`, `phone-canvas.png`; 336 frames in 31.0475 s (10.82 fps), first frame 693.4 ms, 1280×720 |
| Revoke and re-pair | pass | `13-revoke.txt`, `revoked.json`, `14-repair.txt`; the revoked phone failed device authentication, and re-pairing restored the project and task history |
| Uninstall | fail, fixed | `16-uninstall.txt`: `service uninstall` with no managed unit failed with "I/O failure: entity not found" (6e68f763). `uninstall-data` correctly refused while Øphio ran, then succeeded after the autostart line was removed and the account logged in again. It named a received-files folder that did not exist (27d952c0) |
| Residue | fail, fixed | `17-residue.txt`; an empty `~/.config/ophio`, and `~/.local/share/ophio` holding only `instance.lock`, remained (7bd6f30a) |

The lab step that removed the autostart line reported failure only because
its own count of remaining matches was zero. The line was gone, and Øphio did
not start at the next login.

### Plasma — new user

Arch Linux, kernel 7.2.7-arch1-1, KWin, plasma-workspace and KDE portal
6.7.5-1, xdg-desktop-portal 1.22.1-2, PipeWire 1:1.6.9-1, WirePlumber
0.5.17-2, libxkbcommon 1.13.2-1, glibc 2.44+r50, Mesa 1:26.2.3-1
(`u50/plasma/tuple.txt`). It was a Wayland session with `graphical-session.target`
active (`00-session.txt`).

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Install from the archive | pass with remedy | `u50/plasma/01-install.txt`, `02-path-remedy.txt`; same PATH finding and remedy as Hyprland |
| `ophio doctor` before the first run | fail, fixed | `01-install.txt`; same raw I/O failure (0924e29a) |
| Run by hand, then start with the session | pass | `03-run-by-hand-then-service.txt`, `04-starts-with-session.txt`; the unit is wanted by `graphical-session.target` and was active after logging in again |
| Consent refused | pass | `05-consent-dialog.jsonl`, `05-consent-dialog.png`, `06-consent-refused.txt`; Deny left Capture needing setup, with the portal dialog named as the remedy |
| Consent approved | pass | `07-consent-approved.txt`; Capture, Input, Lock session and Computer use ready |
| Library folder, project and pair | pass | `08-root-and-project.txt`, `09-pair.txt`, `paired.png`; both screens showed 462786 |
| Agent task | pass | `task.json`, `task-*.png`; completed through the permission bridge |
| Watch | pass | `watch.json`, `phone-canvas.png`, `11-guest-during-watch.png`; 329 frames in 31.0094 s (10.61 fps), first frame 139.7 ms, 1280×800 |
| Revoke and re-pair | pass | `12-revoke.txt`, `revoked.json`, `13-repair.txt`; project and task history restored |
| Uninstall | pass, message fixed | `14-uninstall.txt`; both commands exit 0, and the absent received-files folder was named (27d952c0) |
| Residue | fail, fixed | `15-residue.txt`; same empty Øphio folders and lock file (7bd6f30a) |

`~/.local/share/flatpak/db/remote-desktop` is the desktop portal's own record of
the consent. It is not Øphio's, and uninstalling does not remove it.

### GNOME — new user

Ubuntu 24.04.5 LTS, kernel 6.8.0-142-generic, GDM 46.2-1ubuntu1~24.04.9,
Shell 46.0-0ubuntu6~24.04.15, Mutter 46.2-1ubuntu0.24.04.16, portal
1.18.4-1ubuntu2.24.04.3, GNOME backend 46.2-0ubuntu1, PipeWire
1.0.5-1ubuntu3.3, WirePlumber 0.4.17-1ubuntu4.1, libxkbcommon 1.6.0-1build1,
libc6 2.39-0ubuntu8.9, Mesa 25.2.8-0ubuntu0.24.04.2 (`u50/gnome/tuple.txt`).

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Install, version, help | pass | `u50/gnome/01-install.txt`, `02-version-help-doctor.txt`, `03-relogin-path.txt`; Ubuntu's profile put `~/.local/bin` on PATH after logging in again |
| Run by hand and stop | pass | `04-run-by-hand.txt`, `06-stop-by-hand.txt` |
| Pair with default settings | pass with remedy | `05-pair-loopback-only.txt`. Without Tailscale, Øphio listens only on loopback, so the code offered `127.0.0.1:47100` and a phone could not reach it. `ophio pair` now says so and names both remedies, and the install guide explains them (8bc4af83). Later steps set `allow_lan` and `listen` |
| Start with the session | pass | `07-service-install.txt`, `08-starts-with-session.txt` |
| Reads while the consent dialog is open | fail, fixed | `10a-pending-consent-blocks-cli.txt`; `ophio doctor` and `status` timed out ("connection deadline elapsed") until the dialog was answered. Settings reads no longer wait behind a change being applied (da40178c, with a regression test) |
| Consent refused and approved | pass | `09-consent-dialog.jsonl`, `09-consent-dialog.png`, `10-consent-refused.txt`, `11-consent-approved.txt`; Cancel named the portal remedy. Share with remote interaction made Capture, Input and Computer use ready |
| Lock session as a user service | fail, fixed | `11-consent-approved.txt`, `20-after-host-restart-doctor.txt`: "No session locker configured". GNOME does not name the login session to user services, and Øphio now asks logind (a9e5df51; recheck below) |
| Library folder, project and pair | pass | `12-root-and-project.txt`, `14-pair-confirm.txt`, `paired.png` |
| Agent task | pass | `16-task.log`, `task.json`, `task-*.png`; completed through the permission bridge |
| Open turn and Stop | pass | `15-task-stuck.png`, `15-open-turn-stopped.txt`. The first task used the fixture's open-turn script, which never finishes and sends an event type Øphio does not know. The conversation labelled the unreadable update, Stop stayed reachable, and stopping ended the run as interrupted. It is not counted as a completion |
| Watch | pass | `watch.json`, `phone-canvas.png`, `17-guest-during-watch.png`; 282 frames in 31.0172 s (9.09 fps), first frame 1137.9 ms, 1280×800 |
| Revoke and re-pair | pass | `18-revoke.txt` … `23-repair.txt`. The first re-pair attempt (`19-*`) was lost to a lab helper error before any code was shown, and is not counted. After the lab host restarted, a freshly installed phone app paired, was revoked, and re-paired with the project and both task records |
| Missing PipeWire library | pass with remedy | `24-missing-library.txt`; with `libpipewire-0.3-0t64` removed, the system loader stops `ophio` with exit 127, naming the library, before any Øphio code runs. The install guide names the package to install |
| Service installed without a graphical session | pass with remedy | `25-no-graphical-session.txt` … `27-remedy-service-install.txt`. From a login with `graphical-session.target` inactive, the service fell back to `default.target` with a note. `desktop enable` alone could not help; running `service install` again from the graphical login did |
| Uninstall | pass, message fixed | `28-uninstall.txt`, `28b-after-uninstall-data.txt`; `uninstall-data` without `--yes` names the folders and changes nothing. With `--yes` it succeeded, but named an absent received-files folder (27d952c0) |
| Residue | fail, fixed | `29-residue.txt`; same empty Øphio folders and lock file (7bd6f30a) |

### GNOME recheck

The same account then installed the `a9e5df5` archive over the earlier
uninstall (`u50/gnome-recheck/00-before.txt`). It ran with `XDG_SESSION_ID`
unset, as GNOME starts user services.

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Doctor and status before the first run | pass | `01-install-doctor.txt`; archive hash matched, "Ophio is not running", naming `ophio run` and `ophio service install` |
| Service install, twice | pass | `02-service-install-doctor.txt`; the second install replaced the first cleanly |
| Desktop enable from the service | pass | `03-consent-dialog.jsonl`, `03-desktop-enable.txt`, `04-doctor-after-enable.txt`; Capture, Input, Lock session and Computer use ready |
| Lock from the service | pass | `05-lock-from-service.txt`; `LockedHint` went from no to yes, and status showed control not ready because the computer is locked |
| Sharing after unlock | pass, message fixed | `06-unlock.txt`; GNOME ended sharing at lock, and doctor said only "Enable the feature again on the computer". Status and doctor now name `ophio desktop enable` (405085c8) |
| Re-enable | pass | `07-reenable.txt`; Desktop view ready |
| Uninstall | pass | `08-uninstall.txt`; a second `service uninstall` says there is nothing to remove and exits 0. `uninstall-data --yes` no longer names an absent folder |
| Residue | pass | `09-residue.txt`, `home-after-uninstall.txt`; no Øphio path remains in the home directory. The empty `owner.lock` in the per-login runtime folder stays until logout, when the system clears that folder |

### Changes made from these runs

Each code change has a regression test; 26676dc7 changes only the install guide:

- 0924e29a: commands that need a running Øphio say it is not running.
- 6e68f763: `service uninstall` succeeds when no unit is managed.
- 7bd6f30a: `uninstall-data` removes the lock it holds and empty folders.
- 27d952c0: the received-files folder is named only if it exists.
- 26676dc7: the install guide covers PATH, libraries, the notification
  service, autostart and uninstall, as found here.
- 8bc4af83: pairing explains loopback-only reach.
- a9e5df51: GNOME lock works when Øphio runs as a user service.
- 405085c8: status names the command that restores ended sharing.
- da40178c: settings reads answer while a desktop change waits for consent.
- 0c0a9d57: stopping Øphio, or interrupting `ophio desktop enable`, abandons
  an unanswered consent dialog instead of waiting for it, and leaves sharing
  off.
- 2c92ca3a: a failed or interrupted change that closes sharing records it as
  off, so neither status nor the next start says it is on.
- 634621e1: an interrupted enable cannot save consent after a later
  `ophio desktop disable` forgot it.
- de27f34c: without a session named by the desktop, Øphio locks the user's
  graphical session only when there is exactly one of the given type, and
  otherwise reports locking unavailable.
- ccf4773f: an enable given up at any point, even after the person approved
  it, closes the desktop session it opened.
- a8f833e8: `ophio desktop disable` withdraws an enable still waiting for the
  person instead of waiting behind its dialog.
- f2087469: sharing is saved as on only once the person confirms it, and a
  failed change reports sharing off even when its settings cannot be saved.
- 477c5fb3: an enable asked for after `ophio desktop disable` waits for that
  turn-off to finish, even when it reaches the settings first.
- 414ef61e: a refused sharing dialog, or a desktop that cannot be shared,
  leaves sharing off and says why instead of recording it on for the next
  start.
- 0586e24b: `ophio desktop disable` closes sharing and forgets consent even
  when its setting cannot be saved, and says the setting is unsaved.
- a33f0be1: `ophio desktop disable` never waits on another setting that has
  become invalid, such as a removed Library folder.
- e389995e: sharing closes, and a turn-off forgets consent, even when the
  change cannot be recorded for the phone.
- aebd5bc6: with Øphio stopped, `ophio desktop disable` forgets consent even
  when its setting cannot be saved.
- bcc78cce: when sharing ends, input is released on every screen even when
  none of it can be recorded, and input that may not have been released is
  reported as such.
- a35091de: with Øphio stopped, `ophio desktop disable` forgets consent even
  when the settings cannot be read, and leaves them as they are.
- dbb2347e: when sharing ends, voice input is let go even when that cannot be
  recorded.
- 4fcfe7ea: with Øphio stopped, `ophio desktop disable` forgets consent even
  when the settings folder, or another Øphio folder it does not need, is open
  to other people, and leaves those settings as they are.
- 3fbeb1c2: with Øphio stopped, `ophio desktop disable` forgets consent even
  when the per-login runtime folder cannot be read, and leaves a running Øphio
  it cannot reach alone.
- d52de11a: stopping, pausing or revoking control releases every screen it
  covers even when one cannot confirm it let go, and reports input that may
  still be held ahead of anything that went unrecorded.
- 7ceb499e: a voice stop that cannot be recorded is kept and recorded later,
  so the phone is told why voice stopped rather than of a restart.
- 33bc7854: stopping a run releases its screens even when its access cannot
  be withdrawn.
- 6bfff0a4: a locked desktop fences every screen even when one cannot confirm
  it let go or nothing can be recorded, and reports input that may still be
  held first.
- 79234fc8: a task Stop the agent confirms still reports a screen it could
  not release.
- 30f596d2: a Library, Inbox or retention change that fails says first when
  undoing it left input unreleased, and says when sharing could not close.
- d951571c: a private step stops the agent even when none of it can be
  recorded, and reports input that may still be held first.
- 91f5d835: a forced stop the computer cannot confirm keeps the run, its
  project and its Stop until the process is seen to end.
- 574d07c9: a screen whose state could not be saved is saved whole once
  saving works again, so a phone never keeps stale input or access.
- eaed22e4: turning sharing off while input may still be held reports the
  outcome as unknown, not as a refused setting.
- 04af20f6, with 14ed1e63: a forced stop still ends helpers the agent left
  running after it exited, and a stop that could not end them can be tried
  again. Watching for the exit wakes Øphio only when the agent itself exits.
- 37724b20: a Stop reaches only the run it was accepted for, never one the
  task was resumed into before it took effect.
- 547cf2d8: a Stop the agent acknowledged is not reported as done while
  what it started may still be running on the computer.
- 18546aea, with 58da3640: where a run keeps only its process group, ending
  it never signals a group another program may since have been given the same
  number, and an agent that has exited no longer keeps its run from ending.
- cc3234d7: a process whose name is not text no longer keeps its run from
  ending.
- 75be5035: where `/proc` hides some processes from Øphio, a run that keeps
  only its process group is not reported as ended, since a hidden process may
  still be in it.
- 170a2ba4: a run is reported as ended only once the agent itself has exited,
  even if it left the group or cgroup it was started in.
- c01e6400: where `/proc` hides the processes Øphio may not trace, membership
  of its exempt group no longer counts, since Linux hides them from that
  group too.
- 4f904d8a: a mount whose path is not text no longer keeps a run from ending.
- b20626f1: where `/proc` was made for another PID namespace than Øphio's, a
  run that keeps only its process group is not reported as ended, since
  `/proc` gives its processes other numbers.
- 8ec4614e: only the mount on top of `/proc` says whether it shows every
  process, and one that is not proc shows none.
- 79118f94: a mount over a process's own `/proc` entry, or over `/proc` with
  only part of a proc, counts as hiding processes.
- b61bbc6a: membership of `/proc`'s exempt group counts only where Øphio runs
  in the initial user namespace, since another may number groups otherwise.
- f6dab6d9: `/proc` is opened once for each check and every process's entry
  is read through it without crossing a mount, so a mount made during the
  check leaves the run unconfirmed instead of hiding a member.
- 20bf9554: `/proc`'s options are read from the proc that is scanned, which
  replaces the mount-position rules of 8ec4614e and 79118f94; a mount inside
  an older, covered `/proc` no longer keeps runs from ending.
- 5e84dccd: Øphio's user namespace is read through the opened `/proc` too,
  and the exempt group counts only where that link names the initial one.
- 3975e468: a check that finds none left reads `/proc`'s options again, and
  a remount that changed them meanwhile leaves the run unconfirmed.

The last forty-seven postdate every lab binary. The lab user had one graphical
session, which de27f34c still names.

## Repeated Stop samples, release archive

Run date: 2026-09-30 UTC, Hyprland only. Evidence is relative to
`target/lab-evidence/`. This pass repeated the phone Stop check twenty-one
times, to replace the single Hyprland observation above with a distribution.

- **Øphio.** The release archive built from source `9b1b86e5` in the builder
  guest (`archives/9b1b86e56683-build.log`), 20,318,136 bytes, SHA-256
  `d2f9beb286ba0b6bbde6f560caa5be13eb3da5e47728552ac1151323aca29075`. Its
  executables were installed for the lab account (`ophio` SHA-256
  `acb2fec1132bdea4364f1ce5de21f8a62ad865c32f797c1bb6dfeecd6f95f076`,
  `computer-mcp`
  `91c7c2e788bd083c95680a74822126cdf9c0c0d582cf7bc320e448fb8384cddd`).
  Øphio ran in a user scope with `Delegate=yes`, as under the user service
  `ophio service install` writes, so each agent run had a cgroup of its own.
- **Phone.** The Android app, debug universal APK
  `94a3aa1c56997c32cbff5349f796b5425e0c1059b0a2d7b29e59a0350984c15d` built from
  source `f6e458ac`, on a lab emulator, paired through the Welcome screen
  (`t8-pair/hyprland/pair.json`).
- **Guest.** The package tuple of "Hyprland — Final follow-up" is unchanged
  (`t8d-watch/hyprland/tuple.txt`).

Each sample is one run judged on its own, with the method and pass conditions
described under "Final follow-up": active agent input at the tap, witness text
and a click, five deliberately late calls refused, no input after the receipt,
and every receipt fact confirmed. `t8d-summary/stop-samples.json` summarises them
with nearest-rank percentiles; a failed sample would count, not be dropped.

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Phone Stop, 21 samples | pass, 21 of 21 | `t8d-stop-01` … `t8d-stop-21/hyprland/stop-result.json`, `t8d-summary/stop-samples.json`. Tap to revoked input: min 30.7, median 41.3, 90th percentile 134.7, max 638.9 ms. Tap to receipt: min 163.5, median 230.0, 90th percentile 277.8, max 980.8 ms |
| Watch | pass | `t8d-watch/hyprland/watch.json`; 363 frames in 31.0201 s (11.70 fps), first frame 94.1 ms, 1280×720; every frame was drawn before acknowledgement |

The slowest sample, `t8d-stop-13` (638.9/980.8 ms), passed every condition; the
next slowest took 138.0 ms to revoke input. Two earlier attempts are kept and
are not part of the distribution:

- `t8r-stop-01`, on the same archive, **failed**. The lab had started Øphio
  directly rather than in a delegated scope, so the run had no cgroup of its own
  and its receipt reported "Everything the agent started" as unknown. Its
  timings were 60.2/642.2 ms. The lab now starts Øphio delegated, as its user
  service is.
- `t8-stop-01`, on a source build, stopped before the tap: the Stop button's
  name now ends with the display the task works on, and the phone helper
  matched the old name exactly, so it found no Stop. The helper now matches a
  task's Stop with or without the display.

These are samples of one virtual machine on a shared host, not a latency bound.

## Public beta candidate, new user (2026-10-26)

Run date: 2026-10-26 UTC. Evidence in this section is relative to
`target/lab-evidence/beta-010/gnome/` in the `beta/candidate` worktree. This
pass tested the exact public beta candidate: the Linux download built in
public mode with the phone app signed on the release key, installed by a
new account (`newuser`) that had never run Øphio, following only the
download's `INSTALL.md`. The phone was that signed app on a lab emulator.

Guest: Ubuntu 24.04.5 LTS, kernel 6.8.0-142-generic, GNOME Shell 46.0, Mutter
46.2, xdg-desktop-portal 1.18.4 with the GNOME portal 46.2, PipeWire 1.0.5,
WirePlumber 0.4.17, bubblewrap 0.9.0, glibc 2.39, Wayland session
(`tuple.txt`). Restricted networking throughout; a lab-only route let setup see
a home network without outside access (`02-lab-route.txt`).

Phone: Android 15 x86_64 emulator (Google APIs, ARM translation), fresh data,
running the download's own `ophio.apk`: the signed release app, SHA-256
`8b8a0730e5c3d1a02f34ba0cbbbeadb5046305dc75cc08da74566945ae1fb291`, signer
CN=Ophio Android Release 2026 (`2df838f5…9516`).

The agent was the lab's stand-in for Claude Code (`tools/lab/guest/person.sh`
installs it for all users as `/usr/local/bin/claude`). Since 9c8e760f it saves
its conversation where real Claude Code does, so limited chats carry it from
one turn to the next as they would for a person.

Downloads, all built in the Ubuntu builder guest in public mode with that app:

- First install, source 8af77074: `ophio-0.1.0-x86_64-linux.tar.gz`, SHA-256
  `02950f0902148a0ef0a3156e52c7fd14ae425ffedc6cc1e921bbca6fdb78b3d6`,
  build d77d6ddd53e9 (`01-receive.txt`).
- Update and roll-back test only, branch `lab/update-test-0.1.1` (ba802de6,
  version number changed and nothing else): `ophio-0.1.1-x86_64-linux.tar.gz`,
  SHA-256 `74186f358697b0d0ed859363e74a362a78730c30d796f4fb9d618eb5d8a5d7c1`,
  build ca215a2e1970. Not for release.
- Limited-chat fix, source ed6ab267: SHA-256
  `a40391b1c1e8b3d54dcd2b263241b477c74381ae35dda6913c06a5043cb776d3`,
  build 5d8f1e235068, installed over the first with `ophio update`
  (`43-fix-update.txt`).
- Recheck, source 64c8e18d (the limited-chat fixes and the reworded Ubuntu
  advice): SHA-256
  `ef4880aaccc875d1fb3b25ff1142d2da8000cba85c4859ee9a864f423351ae7b`,
  build 1cfb219a0dca, installed over the previous one with `ophio update`
  (`53-recheck-update.txt`).
- Final candidate, source b5e5a5de (the uninstall fixes):
  `ophio-0.1.0-x86_64-linux.tar.gz`, SHA-256
  `0813d24d499cc65071e23315d120565f342d2e5e6534e119d76ccf2c65eb2860`,
  44,391,999 bytes, build e41a474475b4. The bundled `ophio.apk` is the signed
  app above, all three programs need at most GLIBC 2.39, and the licence
  report lists 467 components with no failures or exceptions and one review
  item (option-ext 0.2.0, MPL-2.0, shipped). Installed fresh after the first
  uninstall (`71-before-reinstall.txt`, `72-reinstall.txt`).

### Setup, pairing, a task and Stop

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Receive and unpack | pass | `01-receive.txt`; the checksum matched, and the download holds the program, the bridge, the sandbox helper, the phone app, `INSTALL.md`, licences, notices, the license report and the SBOM |
| Install | pass with remedy | `06-setup-transcript.txt`; `sh install.sh` installed to `~/.local/opt/ophio` and said `~/.local/bin` was not on PATH and what to do. Ubuntu's own shell start-up adds it at the next login |
| Setup | pass | `06-setup-transcript.txt`, `07-status-doctor.txt`, `08-doctor.json`; all six steps, with the service started at login, the home network found through the lab route, Claude Code found and signed in, and the phone paired |
| Screen sharing consent | pass | `04-consent-dialog.txt`, `05-consent-actions.txt`; GNOME's Remote Desktop dialog, with remote interaction allowed and the choice remembered. Capture, Input, Lock session and Computer use were then ready |
| Pair | pass | `03-phone-pair.log`, `pair.json`, `paired.png`; the same six-digit code on both screens, pinned connection and device authentication |
| Agent task with an approval | pass | `task.json`, `task-approval.png`, `task-conversation.png`, `task-receipt.png`; the agent asked to run a command, the phone allowed it, and the task completed with a receipt |
| Alerts | pass | `09-alerts-after-continue.png` … `13-after-battery.png`; the phone asked for notification permission and for running in the background, and both took |
| Stop | pass | `14-stop-before.png`, `15-stop-pressed.png`, `16-stop-after.png`; a busy task was interrupted about a second after Stop, and nothing was left running on the computer |
| Network connections | pass | `35-connections.txt`; Øphio's only connections were the phone's two, to its own port, under restricted networking |

### Update and roll back

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Update to 0.1.1 | pass | `20-update-receive.txt` … `23-after-update.txt`; `ophio update` waited for a minute of idle, restarted and reported 0.1.1 running |
| Phone after the update | pass | `24-phone-after-update.json`, `25-phone-after-update.png`; still paired and online, with the earlier tasks kept |
| Versions screen on the phone | pass, lab error | `26-versions.png`, `28-versions.png`, `29-after-reload.png`; the error screen in `26-versions-error.png` came from the lab script opening a screen that does not exist (see findings) |
| Roll back to 0.1.0 | pass with wording finding | `30-rollback.txt`, `31-after-rollback.txt`, `32-versions-after-rollback.png`, `33-work-after-rollback.png`; back on build d77d6ddd53e9, still paired, work intact. It printed "Updating Øphio 0.1.1 … to Øphio 0.1.0" while going back |
| Updates to the fixed builds | pass | `43-fix-update.txt`, `53-recheck-update.txt`; the second took about 77 seconds, most of it the idle wait |

### Limited chats

Stock Ubuntu 24.04 stops bubblewrap from creating its network namespace
(`36-apparmor-fix.txt`: "Failed RTM_NEWADDR: Operation not permitted"), so
chats ran only in their read-only, offline form and the phone hid the options
to let a chat change files or use the internet. Loading the AppArmor profile
for bubblewrap that the reworded `INSTALL.md` describes fixed it
(`36-apparmor-fix.txt`, `37-doctor-after-fix.json`).

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Chat limited to a project, first build | fail, fixed | `38-*`, `39-*`, `40-*`; "The agent didn't start". The sandbox's connection file path was one character over Linux's limit for a seven-letter user name (ed6ab267). It passes for six-letter names, which is why earlier passes missed it |
| Missing `~/.claude` | finding | `45-claude-folder.txt`; with no `~/.claude` folder, a limited chat showed a raw error. Creating the empty folder let it start |
| Chat limited to a project, ed6ab267 | pass | `44-*`, `46-*`, `47-*`; started and replied |
| Stop, then a second message | fail, fixed | `48-*`, `50-*`; the next message after Stop failed, with a false "could not start agent" warning, because the agent had ended its turn without saving its conversation (81b71d96, 3577401a) |
| Whole-computer chat, Stop and send | pass | `49-*`; not affected |
| Recheck on 64c8e18d | pass | `54-*` … `65-recheck-summary.txt`; Stop, a second message after Stop, a third message after a normal turn, a held message withdrawn by Stop, and Carry on all worked, with no false warning. The agent's saved conversation grew by one entry per start and came back out of the sandbox after each turn (`58-recheck-computer.txt`) |

### Revoke and uninstall

| Check | Result | Evidence / measurement |
| --- | --- | --- |
| Revoke the phone | pass | `66-before-uninstall.txt`, `67-revoke.txt`, `68-phone-after-revoke.png`; the phone could no longer connect, and the computer said how to pair again |
| Uninstall | fail, fixed | `69-uninstall.txt`; both commands worked, but the preview without `--yes` began "configuration error:" and used shorthand ("stop/uninstall"), and the locked keyring message was unclear (115afa44) |
| Residue | fail, fixed | `70-after-uninstall.txt`; `report-trail.json` and `report-crashes.json` were left in `~/.local/share/ophio`. Uninstall now removes the problem-report records, their signing key and Øphio's own agent copies (115afa44). Review then made it remove files left by interrupted writes and a runtime folder kept under the data folder, without following linked folders (64c08c93), and made a start that races an uninstall safe while the lock file and both folders are still removed (8bdc35b2, b5e5a5de) |
| Reinstall over the leftovers | fail, fixed | `71-before-reinstall.txt`, `72-reinstall.txt`; the two report files the first uninstall left made the data folder look used, so the installer did not start the welcome setup. Setup was run by hand (`73-setup-service.txt`) |
| Uninstall recheck, b5e5a5de | pass | `73-setup-service.txt`, `74-uninstall.txt`, `75-after-uninstall.txt`; with Øphio running as a login service and both folders populated, the guide's five commands worked. The preview without `--yes` now begins "Nothing was deleted" and says in full what to stop first. Afterwards nothing named Øphio was left outside Downloads and Projects: no folders, settings, user services, processes or listening ports. The person's Downloads, Projects and `~/.claude` were kept. Uninstall also said it couldn't open the keyring to check it; the lab account has no password and logs in automatically, so GNOME's keyring is never unlocked |
| Reinstall after a clean uninstall | pass | `76-reinstall-after-uninstall.txt`; the installer started the welcome setup by itself again. Removed again afterwards (`77-final-uninstall.txt`) |

### Findings from this pass

These were seen in this pass. None blocked a person from installing, pairing,
running tasks or uninstalling. All but the last are fixed on the release line
after this candidate, and the next candidate's lab pass checks them again.

- **Phone hid the chat access options without saying why.** When the computer
  can't limit chats (stock Ubuntu before the AppArmor step), the options
  disappeared with no explanation. Fixed: the new-chat screen now says chats
  can only read here, and either that this computer can't limit chats yet or
  that `ophio doctor` shows the missing setup step.
- **A saved screen that doesn't exist trapped the app.** If the app reopened on
  a saved screen it didn't know, it showed "Something went wrong" and Reload
  returned to the same screen (`26-versions-error.png`). Only a lab script
  caused this here. Fixed: unknown saved screens are dropped on restart.
- **Colour codes with `TERM=dumb`.** `ophio status` still coloured lines such
  as "Desktop view ready" in a terminal that asks for none. Fixed.
- **Doctor said nothing needed attention while agent limits needed setup.**
  Fixed: the summary now reads "Nothing is broken." followed by what is still
  to set up.
- **Repeated text.** The phone showed its own explanation before Android's
  notification prompt twice; the second, a native dialog, is removed, since
  every way in already explains first. The computer's offer of the phone app
  listed the release signer once per signing scheme; it now names it once.
- **Install guide and installer.** The guide's link to the chat access section
  was broken inside the download (fixed in ed6ab267). The installer printed
  "Next, run: ophio setup" and then started setup itself; it now says
  "Starting setup now" when it does. "A checklist's files" now reads "Files
  from your checklists".
- **Jargon a new person won't know.** "Intake" on the phone's Your reports
  screen (`34-your-reports.png`) is gone from what people read; the Versions
  screen now says the device key is kept in Android's secure storage; the
  roll-back line now says it is going back to the earlier version.
- **"Send reports" is ticked by default** on the phone, with nowhere set up to
  send them, so reports wait on the computer. The owner decided to keep this
  default; the screen now says reports wait on the computer.
- **Missing `~/.claude` gave a raw error** in a limited chat (above). Fixed:
  the sandbox creates the missing agent settings folder, private to the user.
- **"Steer now" is not offered for Claude Code**, so messages sent during a
  turn wait until it ends. This is by design for Claude Code; Stop withdraws
  them. Not changed.

Lab-only artefacts, not product defects: the stand-in agent's help-call log
was not writable for the new user; "Claude Code sent an update this
version does not read (future_native_type)" comes from the stand-in agent, and
the stand-in waits on its first turn because the app wraps a conversation's
opening message.

### Not covered by this pass

- A physical phone or tablet, and TalkBack.
- A real model: the agent was the stand-in, so a first chat and a limited chat
  with real Claude Code (Stop and a second message) remain owner checks.
- The phone app updating itself to a newer app signed on the release key; that
  needs a second signed app. Android enforces the key match, and the update
  path is the one the personal phone uses for every update.
- Other desktops. Earlier sections cover Hyprland and Plasma on older builds;
  the changes since are covered by tests, not by a lab pass there.

## Remaining limits and release status

Hyprland and Plasma pass the route checks covered by the original and follow-up
tables: initial control, capture, safe text delivery, geometry and input,
lock fencing, held-input disconnect/reconnect, explicit desktop disable/re-enable,
and authored MCP computer use with phone Stop. The final GNOME GDM session now
also passes keyboard delivery, explicit Unicode paste, native lock fencing,
post-rescale coordinates, restart restore, disconnect/reconnect, computer use
and private handoff. Historical failed/unrun rows above are not rewritten as if
they originally passed.

GNOME 46 continuous capture across a live logical-size change remains **fail**:
the compositor removes the stream. The implemented explicit Reopen sharing
path passes, including correct subsequent input. This is a native lifecycle
limit, not missing geometry propagation in Øphio. Mutter's
[monitor stream implementation](https://raw.githubusercontent.com/GNOME/mutter/gnome-46/src/backends/meta-screen-cast-monitor-stream.c)
closes the stream when its logical monitor rectangle changes. A new portal
session can use saved consent, but Øphio cannot guarantee the portal
will never show another permission dialog. Owner approval remains explicit.

Direct typing of characters absent from the reachable portal keymaps remains
**unsupported** on GNOME/Plasma, with whole-text refusal and explicit clipboard
paste verified. It is not reported as successful partial typing. No remaining
requested lab check is silently omitted. Real model inference, physical GPU
coverage, arbitrary distributions and X11 remain **unrun**, outside this isolated
fixture-only qualification and its allowed hardware/access scope. These results
do not certify those environments.

The release archive was installed, used and uninstalled from a new account on
all three routes, following only its install guide. Only GNOME was rechecked
on the second archive; the Hyprland and Plasma fixes from that pass, and the
forty-seven changes made after it, are covered by tests rather than a lab run. The
release archive is rebuilt from the release commit, so its hash differs from
both archives recorded here. The public beta candidate (b5e5a5de, in the
section before this one) was tested on GNOME only. The per-login runtime folder keeps an empty
`owner.lock` after uninstall until the session ends.

Apart from the repeated Hyprland Stop samples, all timings are single
observations, not percentiles or hard latency bounds.
The lock probe retains the original 500/1500/4000/8000 ms schedule and records
actual elapsed time including native request/confirmation overhead. The held-input
interval is the gap between guest witness observations, not the instant the
transport first became unreachable. No timeout, lease fence or unknown-outcome
rule was weakened. Each final Stop receipt confirms desktop access withdrawn,
agent Stop acknowledgement and the authored child ending; all five deliberately
late actions are refused on each route.

### Final verification

Commands ran with the toolchain from `BUILDING.md` and a dedicated Cargo target
directory.

| Gate | Result |
| --- | --- |
| `cargo fmt --all` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS SHELL=/bin/sh cargo test --workspace` | pass: 730 tests, 2 existing ignored, 0 failed |
| `taskset -c 0-7 bash crates/test-support/check-parallel.sh` | pass: 25 normal + 10 loaded repetitions, each 730 passed / 2 ignored / 0 failed; `target/q3-parallel-final-complete.log` |
| `cargo check -p companion -p computer-mcp --target x86_64-pc-windows-gnullvm` | pass |
| `cd client && node scripts/protocol-barrel.mjs && PATH=$HOME/.local/bin:$PATH npx tsc --noEmit -p . && npx vitest run && npm run build` | pass: 241 generated types, 62 test files / 445 tests; existing large-chunk warning only |
| `python3 -m unittest discover -s scripts/tests -t .` | pass: 31 tests |
| `python3 -m unittest discover -s tools/lab -p 'test_*.py'` | pass: 23 tests |
| `target/lab/toolenv/bin/ruff check tools/lab` | pass; changed Python files formatted |
| `node --check tools/lab/phone.mjs` and `node --check tools/lab/lock_probe.mjs` | pass |
| `bash -n tools/lab/guest/*.sh` (each file separately) | pass |
| `node scripts/shots.mjs http://127.0.0.1:5892 ../target/lab-evidence/q3/ui-final-portal scripts/plans/compact-safety-status.json` (from `client`) | pass: 17/17, reviewed in two full iterations |

No Tauri source was changed. Earlier interrupted stress runs and failed attempts
remain distinguishable from the completed gate. The final `bash scripts/source-candidate.sh` audit runs against the committed
report; its result is retained in `target/q3-source-candidate-final.log`.

All three guests, the isolated emulator, its ADB server and the simulator were
stopped. `q3/shutdown.json` records closed loopback endpoints 47222, 47112, 5892,
5582, 5583, 5042 and 9232. Lab disks, binaries and raw evidence remain in place,
excluded from source control. No artifact was published.
