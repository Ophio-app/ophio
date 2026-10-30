<!-- SPDX-License-Identifier: Apache-2.0 -->
# Headless Linux desktop lab

This lab boots official cloud images under KVM. All disks, generated SSH keys,
firmware variables and Android state belong under `target/lab/`; raw evidence
belongs under `target/lab-evidence/`. Neither directory is shipped. Scripts clear
host display/session variables. Compositors and portal services run only in the
disposable guest, as an ordinary user. Guest administration requires a marker
file and checks that the machine is virtualized.

Host prerequisites: Python 3.11+, QEMU/KVM, `qemu-img`, OVMF, `xorriso`, OpenSSH,
GnuPG and curl. Use the repository's build environment for Rust and Android.
No script installs host packages or modifies host service configuration.

```sh
export CARGO_TARGET_DIR="$PWD/target" CARGO_BUILD_JOBS=8
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
cargo build -p companion -p computer-mcp
python3 tools/lab/lab.py fetch arch
python3 tools/lab/lab.py create hyprland
python3 tools/lab/lab.py start hyprland --provision
python3 tools/lab/lab.py wait hyprland
python3 tools/lab/lab.py provision hyprland
python3 tools/lab/lab.py artifacts hyprland
python3 tools/lab/lab.py stop hyprland
python3 tools/lab/lab.py start hyprland
python3 tools/lab/lab.py wait hyprland
python3 tools/lab/lab.py ssh hyprland bash lab/in-session.sh ophio desktop enable
```

Repeat with `fetch ubuntu` / `create gnome`, then `create plasma` using Arch.
On Ubuntu, use `artifacts gnome --guest-build` while the VM is in provisioning
mode. It archives the committed workspace source and compiles it inside Ubuntu
with Rust 1.98.0 and the guest's libc. Commit intended product changes first.
The host-built binary may require a newer glibc and must not be substituted as
evidence that an Ubuntu installation works.
Qualify each route independently. At most one lab VM runs at a time: 8 vCPUs,
6 GiB RAM, a 24 GiB overlay and two loopback TCP forwards (SSH 47222 and Øphio
47112 by default; see "Release archive qualification" to change them).
Provisioning has outbound networking. Default startup adds QEMU
`restrict=on`; restart into that mode before recording qualification results.
QMP uses a private Unix socket. There are no VNC, SPICE or host display endpoints.

Downloads use HTTPS. Arch images must match the published SHA-256 and the
arch-boxes signature; Ubuntu images must match the signed SHA256SUMS. The
publisher fingerprints are pinned in `lab.py`; GnuPG uses a lab-local keyring.
Source references: [Arch's publisher repository](https://github.com/archlinux/arch-boxes)
and [Ubuntu's verification instructions](https://ubuntu.com/docs/public-images/public-images-how-to/verify-image-checksum/).
The exact URL, digest and signature status are retained in `verified.json` and
each guest's `image.json`. Never boot an image following verification failure.
Verified backing images are immutable: a later `fetch` checks the cached bytes
instead of replacing them beneath existing overlays. Use a fresh checkout/lab
directory to test a newer publisher image.

`artifacts` installs this checkout's debug companion and bridge plus authored
harness fixtures. It strips the fixtures' prompt, argument and MCP configuration
logging and installs an inert network-discovery executable. This is a source-build
installation check; it does not qualify release archive installation or ABI
compatibility with older distributions.

The GTK input witness records received events and resulting text inside the
guest. It updates a visible timestamp every 50 ms to exercise changing frames.
Only synthetic fixture text may be entered. `tuple.sh` records the OS, kernel,
package versions, Wayland globals, outputs and EGL renderer. `in-session.sh`
imports only saved guest graphical endpoints; it refuses a host invocation.
When the guest starts the companion itself (`ready.sh`, and `install-release.sh`
for an installed release), it runs it in a user scope that systemd delegates, as
the unit `ophio service install` writes has `Delegate=yes`. Each run then gets
a cgroup of its own. Started without delegation, Stop can end only the run's
process group and reports whether it ended everything the run started as unknown.
`lab.py wait` waits for SSH only. Before issuing companion commands, wait for
the guest's `$XDG_RUNTIME_DIR/ophio/run/owner.sock`; enabled portal startup
may first wait for consent. Use a bounded shell `timeout` for operations awaiting
consent, and automate the dialog in a second SSH command.

For GNOME consent, inspect `accessibility.py dump --app xdg-desktop-portal-gnome`
through `in-session.sh`, then use its `select` action on the `MetaVendor` toggle
button and `Allow Remote Interaction` check box, and `activate` on the `Share`
push button. `select` preserves already selected/pressed controls. This is the
actual portal UI, with no restore token seeded by the lab. For diagnostics,
`portal-trace.py` records only message metadata, response codes and result field
names; it does not record method arguments or token values.

For Plasma consent, inspect `accessibility.py dump --app xdg-desktop-portal-kde`,
then use `activate --name Approve --action-name Press`. The role name may
be `button` or `push button`; the helper requires exactly one visible match.
The guest exports `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` to D-Bus activation so
the portal dialog is accessible. Plasma runs on the guest's virtio DRM output;
its virtual backend could not allocate capture buffers in the tested guest.

The guest bootstrap includes a user graphical-session target and clears
inherited PAM capabilities before launching the compositor. These avoid lab
startup failures and allow the portal’s ordinary process inspection. The standalone
GNOME runner disables Xwayland, which crashes under the software renderer.
Normal-session qualification uses GDM instead; X11 remains outside this lab’s
scope. `gnome-screenshot` and `gjs` are installed explicitly because a minimal
package installation omits them.

Android uses an x86_64 AVD with **fresh per-run userdata**, the isolated ADB
server `tcp:5038`, serial `emulator-5562`, and `-no-window -no-audio`.
Build the debug APK from this checkout, install it on that emulator, and forward
its WebView CDP to loopback 9226 as described in `client/src-tauri/E2E.md`.
Do not copy a previous emulator's userdata or enrolled device keys.

Create a one-use offer with the guest's `ophio pair` command, redirecting its
output to `target/lab/<target>/pair.txt` with mode 0600. Never print it. While
the `pair` step waits, it writes the code the phone shows to
`target/lab/<target>/pair-code.txt`. Check that the guest's `ophio pair` output
shows the same code, then run `ophio pair confirm <code>` on the guest. Then:

```sh
node tools/lab/phone.mjs hyprland pair
node tools/lab/phone.mjs hyprland watch
node tools/lab/phone.mjs hyprland input
python3 tools/lab/disconnect.py hyprland
```

Pairing uses the actual Welcome UI and native certificate pinning. The helper
changes only the offer's address to the Android emulator's `10.0.2.2` route to
the guest's host forward (47112 by default),
and deletes the offer after enrollment. Watching opens Control → Desktop.
The measurement observes media callbacks and canvas drawing without replacing
transport or frames. It records every frame acknowledgement and how many canvas
draws preceded it; inspect errors and drawn frames before calling a watch pass.
First-frame time starts at the media open request. The steady sample runs for at
least 31 seconds after the first frame. This measures this VM/software-renderer
tuple, not physical GPU performance.

`input` requests control through the UI and invokes the Desktop component's
normal input callback (including its supervision checks) for deterministic
pointer/text/chord/scroll checks. Acknowledgements alone are insufficient:
compare `evidence/input-state.json` and guest screenshots. `hold` leaves a managed
button and Shift held for a subsequent disconnection test. Check guest releases,
phone connection state and journal recovery before marking the case passed.
`disconnect.py` automates that sequence: it checks the restricted VM and isolated
ADB, disables emulator Wi-Fi/data for five seconds, verifies released guest input,
records offline state, restores networking even on failure, and polls recovery.
Its timestamp difference is an observation interval, not exact outage latency.
`open` starts a watch with a short sample for subsequent interaction checks;
only `watch` supplies the required 31-second frame-rate sample. `arm` acquires
control without holding any keys or buttons, for a subsequent lock probe.
If initial control is unavailable, `initialize-control` records that failure and
sends a real authenticated pause command as a diagnostic workaround. Do not call
the initial UI flow a pass after using it. `computer` requires the guest's
`fake-computer.py` installed as `bin/claude` with the original sanitized fixture
saved as `bin/claude-probe`; set `LAB_PROJECT_PATH` to the guest's disposable
project directory. It starts a task through the authenticated phone session,
keeps the phone watching, and clicks the Stop button associated with that task.
Inspect the guest’s action log and witness for the five refused late actions.
The lease and agent observation must still be active immediately before Stop;
a task whose input already expired does not qualify as a mid-action Stop check.
Avoid simultaneous whole-machine CPU stress when recording route latency.
Install that scripted fixture once, after `artifacts`, using the guest SSH
wrapper (substitute the current target):

```sh
python3 tools/lab/lab.py ssh hyprland bash -c \
  'cp bin/claude bin/claude-probe; cp lab/fake-computer.py bin/claude; chmod 700 bin/claude'
LAB_PROJECT_PATH="$(python3 tools/lab/lab.py ssh hyprland realpath project)" \
  node tools/lab/phone.mjs hyprland computer
```

Use `lab.py collect <target>` to collect only the allowlisted evidence files and
doctor/status/tuple outputs. It never copies general configuration or credentials.
`lab.py screenshot <target>` captures the guest’s virtio framebuffer through
its private QMP socket, including lock screens. For GNOME, use the normal GDM
session described below; a standalone headless Shell has a separate output. QMP never connects to a host display server.
Preserve meaningful intermediate results before repeating a named check, since
its output file is replaced. Restart the Android app after guest replacement and
select the current display; old target identifiers are deliberately not reused.

Portal consent, persistence, output changes, session lock, and scripted computer
use require their own evidence. Do not infer those checks from a successful
capture or from another compositor. See `docs/qualification/linux.md` for the
actual results and remaining gaps.

Always stop the VM with `lab.py stop <target>` and stop the isolated emulator
with `adb -s emulator-5562 emu kill` when finished. Preserve evidence before
resetting a disposable guest. Never terminate processes based only on a stale PID.

Tool checks:

```sh
python3 -m unittest discover -s tools/lab -p 'test_*.py' -v
ruff format --check tools/lab
ruff check tools/lab
node --check tools/lab/phone.mjs
for file in tools/lab/guest/*.sh; do bash -n "$file" || exit; done
```

## Parallel reruns and portable releases

Set `LAB_EVIDENCE_RUN=<name>` to retain new results under
`target/lab-evidence/<name>/` without overwriting an earlier measurement. For a
second run beside another one, the phone and disconnect helpers accept alternate
isolated endpoints: `ADB_SERVER_SOCKET=tcp:5042`,
`LAB_ADB_SERIAL=emulator-5582`, and `LAB_CDP_PORT=9232`. Set
`ANDROID_ADB_SERVER_PORT=5042` for the emulator. Check process ownership and port
availability before installing or starting the app. Do not use another run's
emulator even if its serial matches the default. Use a fresh AVD directory under
`target/lab/`, copying only the device configuration, never enrolled userdata.
The guest forwards default to 47222 and 47112, with one guest at a time.

`guest/release.sh` builds both release executables offline in the existing
Ubuntu 24.04 guest with eight build jobs. Supply `lab/release-source.tar` from
`git archive HEAD Cargo.toml Cargo.lock .cargo crates LICENSE` and dependencies
under `lab/vendor` from `cargo vendor --locked --offline`. This path requires the
already provisioned toolchain/native libraries; it does not install packages.
Retrieve both binaries from
`lab/release-target/x86_64-unknown-linux-gnu/release/` into the checkout's matching
target directory. Run the licence gate, then `scripts/package-artifacts.py linux`
with a fixed `SOURCE_DATE_EPOCH`. That assembler checks GLIBC requirements against
2.39 before producing the archive. Install the **archive's** executables together
in the disposable guest and run the existing phone checks against those files.
`computer-mcp` requires a bound bridge session; `--version` is not its CLI.

Use `phone.mjs <target> initial-control` to assert that Enable control is enabled
without the diagnostic pause workaround.

### Local Stop in a minimal guest session

Current builds require an actionable on-computer notification before admitting
control. A minimal Hyprland guest may have no notification daemon. The optional
`guest/notifications.py` service uses the guest's existing GTK/PyGObject runtime,
shows the real notification text and action buttons, and emits the standard
notification action signals. It refuses root and machines without the disposable
lab marker, and does not replace an existing notification service. It is a lab
witness, not a product notification implementation.

```sh
python3 tools/lab/lab.py copy hyprland tools/lab/guest/notifications.py lab/notifications.py
python3 tools/lab/lab.py ssh hyprland bash lab/in-session.sh sh -c \
  'nohup python3 lab/notifications.py > evidence/notifications.log 2>&1 < /dev/null &'
python3 tools/lab/lab.py ssh hyprland bash lab/in-session.sh hyprctl keyword windowrule \
  'match:title ^Guest notification$, float on'
python3 tools/lab/lab.py ssh hyprland bash lab/in-session.sh hyprctl keyword windowrule \
  'match:title ^Guest notification$, move 880 0'
```

Start it before enabling desktop control. Keep its buttons visible and outside
the input witness's center probe. For an already mapped window, use guest-only
`hyprctl dispatch movewindowpixel 'exact 880 0,title:^Guest notification$'`.
Clicking Stop must revoke control; do not replace the notification service with
an endpoint that merely claims action support. Guest shutdown ends the service.

### Full GNOME session and final route checks

Existing lab directories may be symlinked into another checkout. The runner uses their
canonical paths and relocates a qcow2 backing pathname only after checking the
original image checksum. It neither replaces backing bytes nor resets userdata.

GNOME keyboard, lock and rescale checks require a normal GDM Wayland session on
the guest’s virtio GPU. A standalone headless Shell has no keyboard seat or screen
shield. While the guest is in provisioning mode, copy `guest/gdm-session.sh` and
run it as root **inside that marked guest**, with `--install` only if GDM is
missing. Stop the guest and restart with restricted networking before recording
results. The script configures guest-only autologin and the existing witness
through desktop autostart. QEMU remains `-display none`. Disable automatic idle
locking with guest `gsettings set org.gnome.desktop.session idle-delay 0` so a
build pause does not substitute for the deliberate lock test. Explicit lock
requests remain enabled. Do not change the host’s session or services.

Refresh both authored harness probes with `artifacts` or `install.sh` before
installing `fake-computer.py`. Always replace `claude-probe` with that newly
sanitized Claude fixture; keeping a previous probe can hide newly required flags.
The lab test checks its help against the adapter’s complete readiness flag set.
No readiness check in the adapter is bypassed. Grant the enrolled lab phone
`control` and `computer-use` using the guest owner’s `ophio devices allow`
command. This is separate from the portal’s screen-sharing consent.

`phone.mjs <target> paste` records whole-text refusal, presses the labelled
Paste instead button, and then leaves control. Compare its acknowledgements with
the complete witness text. `guest/rescale-gnome.py 1.25` changes the existing mode;
pass `1` to restore it. For a coordinate witness, run `input-app.py --fullscreen`
and compare received pointer coordinates with `view_size`. GDK’s Wayland
`x_root`/`y_root` are surface-relative and do not prove monitor coordinates.
The changing clock is at the bottom, clear of desktop notification banners.

`lock_probe.mjs <target>` arms control first,
request the real guest lock, confirm it natively, attempt normal phone input,
and observe at 500, 1500, 4000 and 8000 ms. It records actual sample times when
lock confirmation delays the first sample. `disconnect.py` uses a
five-second outage and guest event-timestamp comparison.
The lock probe requires a fresh, empty acknowledgement list and fails unless
all four observations show locked, paused input with revoked leases and no
acknowledgements. Preserve failed attempts separately before repeating a check.

For the specialist check, `phone.mjs <target> computer` records readiness before
starting, uses the child-scoped real MCP bridge, and taps that task’s Stop.
The fixture records guest monotonic and real-time timestamps without arguments,
bridge credentials or environment contents. Save the collected action and input
logs as `stop-actions.jsonl` and `stop-input-events.jsonl` in the route’s evidence
directory immediately after this check, then run:

```sh
python3 tools/lab/stop_result.py target/lab-evidence/<run>/gnome
```

The summary requires actual witness typing/clicks, five refused late calls, no
witness events after the companion’s Stop receipt timestamp, and confirmed Stop
facts. Phone timing is measured from `performance.now()` immediately before the
button click to first observed revoked input plus withdrawn observation; it
includes transport and projection delay. Receipt arrival is recorded separately.
The guest witness and companion timestamps use the same guest clock; they are
not subtracted from the phone clock. Keep sample counts with every result.
`computer-human` requests the fixture’s private handoff and explicitly presses
Start private step. Inspect `private_step.ready` and every acknowledgement, then
end the private step explicitly without resuming the agent.

For the shared safety-strip check, run `node tools/lab/sim.mjs` and the committed
`client/scripts/plans/compact-safety-status.json` through `client/scripts/shots.mjs`. The launcher
binds only loopback 5892 and allows the resolved package directory so local font
assets work when `node_modules` is a symlink. Stop it afterwards.

For GNOME lock recovery in this headless guest, `loginctl unlock-session` alone
does not wake the virtual output. After the timed probe, set the guest Mutter
`org.gnome.Mutter.DisplayConfig.PowerSaveMode` property to integer `0` to inspect
the still-locked screen. Confirm ScreenSaver and logind still report locked.
Then unlock the guest session and wake its output again. This is guest display
power control, not permission to deliver companion input. If the old capture
source was removed, press the phone’s explicit Reopen sharing action
(`phone.mjs gnome reopen`) and wait for a fresh picture before acquiring input.
If the portal instead ended sharing, enable desktop sharing again on the guest;
device pairing is still valid. Disabling and enabling sharing explicitly may
require another native permission dialog.
`ready.sh` obtains the missing GDM autostart session ID only from the same user’s
active, local Wayland session, so both ScreenSaver and logind can be observed.

After the timed GNOME lock samples, the guest may power down its virtio output.
Use `lab.py wake gnome`, wait one second, then `lab.py screenshot gnome` to
inspect the shield. This sends only tablet motion over the verified guest’s
private QMP socket, never keys or buttons. Reconfirm both native lock sources
after waking. Do this after the eight-second probe so its measurement is unchanged.
A direct PowerSaveMode change alone may leave the rendered output black.

## Release archive qualification

The `artifacts` route above installs a source build. This route instead builds
the Linux release archive with `scripts/package-linux.sh` and gives a fresh
guest's new ordinary user that archive and nothing else from the checkout.

The packaging script's glibc check needs the Ubuntu 24.04 baseline, so the
archive is built in a separate `builder` guest (Ubuntu, no desktop). `provision
builder` installs the native development packages and Rust 1.98.0 through
`guest/toolchain.sh`, which `artifacts --guest-build` also uses. `archive`
vendors the named commit's locked crates on the host with the build
environment's Cargo, copies that commit's `git archive` and the vendored crates
into the builder, runs `guest/package.sh` there offline with the commit's time
and hash, and retrieves the archive to `target/lab/release/<commit>/` after
checking its SHA-256. The copied source has no Git history, so the hash is how
the executables learn which commit they were built from. The build
log goes to the evidence directory's `archives/`. Vendored crates sit in the
builder's Cargo home, so the release build remaps their paths like the registry's.

```sh
python3 tools/lab/lab.py create builder
python3 tools/lab/lab.py start builder --provision
python3 tools/lab/lab.py wait builder
python3 tools/lab/lab.py provision builder
python3 tools/lab/lab.py archive builder HEAD
python3 tools/lab/lab.py stop builder
```

That archive carries no phone app. The public archive must carry the signed
one, because `ophio setup` installs it and paired phones update from it. Give
`archive` the signed APK and the Android build-tools directory that checks it:

```sh
python3 tools/lab/lab.py archive builder <commit> \
  --phone-app /path/to/signed.apk --build-tools ~/Android/Sdk/build-tools/37.0.0
```

This copies the APK and only the tools the check runs (`apksigner` with its jar,
`zipalign` with its C++ runtime) into the lab, then into the builder, and runs
`guest/package.sh <epoch> <commit> public`. That sets `OPHIO_PUBLIC_RELEASE=1`, so the
packaging requires the companion to pin the public release key and verifies the
app there with the builder's Java: a v3 signature by that key alone, and the
alignment. After retrieval the host checks the archive's `ophio.apk` is the APK
it was given. Retrieval uses a temporary file, so an interrupted transfer or
failed check leaves neither a releasable archive nor an earlier build record.
`build.json` then records
`"public": true`, the app's SHA-256 and the tools' SHA-256s. Every `archive`
run removes the builder's previous phone app and tools first. Java arrived with
`toolchain.sh`; a builder provisioned before it needs `provision builder` again.

For each desktop, provision as above, then run `newuser` while the guest is
still in provisioning mode. It creates `newuser`, an ordinary account without
sudo, and points the desktop's automatic login at it: GDM on Ubuntu, the tty
session running `Hyprland` or `startplasma-wayland` through a login shell on
Arch. The user gets no lab tools, no Øphio configuration and no extra groups.
The authored Claude fixture is installed as `/usr/local/bin/claude`, standing in
for an agent CLI installed for all users. A system autostart entry (or
`exec-once` in the user's Hyprland configuration) runs `guest/person-ready.sh`,
which saves only the session's desktop endpoints and PATH to the user's runtime
directory and opens the input witness. Restart into restricted networking, then
hand over the archive:

```sh
python3 tools/lab/lab.py newuser gnome
python3 tools/lab/lab.py stop gnome
python3 tools/lab/lab.py start gnome
python3 tools/lab/lab.py wait gnome
python3 tools/lab/lab.py release gnome target/lab/release/<commit>/ophio-<version>-x86_64-linux.tar.gz
python3 tools/lab/lab.py person gnome bash -c 'cd ~/Downloads && tar -xzf ophio-*.tar.gz'
```

`release` accepts only an archive the `archive` command made and puts it in the
user's `~/Downloads`. `person` runs a command as that user with the environment
a terminal in their desktop session inherits, including the session's PATH as
it was at login. `person --login` models a login outside the desktop, as over
SSH: the user's service manager runs, but no desktop endpoints are set. Follow
only the archive's `INSTALL.md` and the public documents from there. Run the
portal consent helper as the user too:
`person gnome python3 /usr/local/lib/desktop-lab/accessibility.py dump --app xdg-desktop-portal-gnome`.

The phone steps are the same as above with a freshly installed app. `pair`
also pairs a phone that was paired before, through its computers list and the
explicit replacement choice. `task` (with `LAB_PROJECT_PATH` set to the user's
project) starts the fixture's `bridge-permission` turn, which reaches the
archive's `computer-mcp`, allows the request from the phone and follows the run
to its receipt. After the owner runs `ophio devices revoke`, `revoked` checks that
the phone shows revoked access and cannot reconnect.

To run beside the connected review environment, give the lab its own forwards
and emulator. `start` records the forwards in the guest's `process.json`; later
commands and the phone helper use the recorded ones. A release guest's Øphio
listens on the product's default port, so its forward targets 47100:

```sh
export LAB_SSH_PORT=47224 LAB_OPHIO_PORT=47114 LAB_GUEST_OPHIO_PORT=47100
export LAB_ADB_SERIAL=emulator-5564 LAB_CDP_PORT=9234
```

Start that emulator with `-read-only -port 5564` from the build AVD. When
another emulator already holds the build AVD writable, `-read-only` cannot attach;
use a separate AVD directory holding only a copy of its `config.ini`, as above.
