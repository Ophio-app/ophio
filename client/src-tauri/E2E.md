# The Android app against Øphio on the computer

Verification date: **2026-10-01**. The final pass used a clean installation of
the packaged x86_64 debug APK, a fresh computer fixture and a fresh, confirmed
pairing, then ran every step below in order without restarting anything. The
steps ran from `696923b2`, and `ophio`, `computer-mcp` and the APK were built
there. The Rust code of `ophio` and `computer-mcp` is that of `e504fba6`, so
Cargo found nothing to rebuild and kept the two built there. The release
candidate adds only this document. All agent processes were authored
fixtures.

## Environment and isolation

- Android 15 / API 35, x86_64 AVD `buildphone`, Android System WebView
  124.0.6367.219. Default 1080 × 2400, density 420; packaged WebView width 412
  CSS pixels.
- Isolated ADB server `ADB_SERVER_SOCKET=tcp:5038`. Every ADB command named the
  emulator (`emulator-5566` here, through `ANDROID_SERIAL`); the tools refuse any
  serial that is not an emulator. No other ADB server or device was queried.
- Headless, read-only emulator: `-read-only -no-window -no-audio -no-boot-anim
  -no-snapshot -gpu swiftshader_indirect`, on its own console port and netsim
  instance, so another emulator can run beside it without sharing state.
- Øphio on the computer bound only to `127.0.0.1` on the port in `OPHIO_PORT`
  (47116 here), through `tools/emulator/isolated-companion.sh`. The emulator
  reached it at `10.0.2.2` with the original certificate pin and one-use pairing
  proof. DevTools for the packaged WebView were forwarded to `OPHIO_DEVTOOLS_PORT`
  (9230 here).
- Rust 1.98.1, Node 24.19.0, JDK 21, build-scoped Android SDK/NDK. App 0.1.0,
  build 1000, minimum SDK 29, target SDK 37; Øphio on the computer 0.1.0;
  negotiated protocol 1.0.
- No visible browser or desktop windows, desktop capture/control, real agent
  accounts, installed agent CLIs or model inference. Tailscale discovery was
  stubbed; no account configuration or credentials were accessed.

The wrapper clears the inherited environment, uses private fixture HOME and XDG
config/data/state/cache directories, disables desktop access, and requires fake
Claude, Codex and Tailscale executables before running. Its `command -v` checks
resolved only fixture executables (`fake-paths.log`). Fixture copies omit
argument, MCP configuration and stdin logging. `start` detaches the isolated
server from a short-lived shell and records its exact PID for fault injection
and cleanup.

The app was uninstalled before the rebuilt APK was installed, so no earlier app
data survived. The fixture was new. The AVD runs read-only, so its disk is not
changed by a pass beyond what the pass itself writes while it runs.

## Reproduction

`LANE` means this checkout's absolute path. `BUILD_ENV` means the build-policy
`env.sh` supplied for the build. Source it in every new shell with the variables
below. Use only the designated build AVD and isolated server; never substitute
another device or an Øphio already running.

```sh
cd "$LANE"
source "$BUILD_ENV"
export ADB_SERVER_SOCKET=tcp:5038 ANDROID_SERIAL=emulator-5566
export OPHIO_PORT=47116 OPHIO_DEVTOOLS_PORT=9230
export CARGO_BUILD_JOBS=8 CARGO_TARGET_DIR="$LANE/target"
mkdir -p target/e2e-evidence
cargo build -p companion -p computer-mcp
(cd client && CARGO_TARGET_DIR="$LANE/client/src-tauri/target" \
  npm run tauri android build -- --debug --apk --target x86_64)
emulator -avd buildphone -read-only -port 5566 -no-window -no-audio \
  -no-boot-anim -gpu swiftshader_indirect -no-snapshot -no-metrics \
  > target/e2e-evidence/emulator.log 2>&1 &
```

After the emulator has booted, set up the fixture, clean-install and pair:

```sh
W=client/src-tauri/tools/emulator/isolated-companion.sh
"$W" setup
"$W" /bin/sh -c 'command -v claude; command -v codex; command -v tailscale' \
  > target/e2e-evidence/fake-paths.log
"$W" "$LANE/target/debug/ophio" roots add \
  "$LANE/target/phone-e2e/notes" --label Notes --writable
"$W" start   # after roots add: both use the data directory
adb -s "$ANDROID_SERIAL" uninstall app.ophio   # do not keep its data
adb -s "$ANDROID_SERIAL" install \
  client/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
adb -s "$ANDROID_SERIAL" shell am start -n app.ophio/.MainActivity
phone_pid=$(adb -s "$ANDROID_SERIAL" shell pidof app.ophio | tr -d '\r')
adb -s "$ANDROID_SERIAL" forward "tcp:$OPHIO_DEVTOOLS_PORT" \
  "localabstract:webview_devtools_remote_$phone_pid"
umask 077
"$W" "$LANE/target/debug/ophio" pair \
  > target/phone-e2e/pair.txt 2> target/phone-e2e/pair-status.txt </dev/null &
(cd client && node src-tauri/tools/emulator/real-companion.mjs pair \
  > ../target/e2e-evidence/pair.log 2>&1)
```

`pair` waits for enrollment. Its offer stays in protected, ignored fixture
storage; never print it or copy it into evidence. The `pair` step waits while the
computer holds the phone's request and writes the code the phone shows to
`target/phone-e2e/pair-code.txt`. When `pair.txt` ends with the same code,
confirm from a second shell with the same variables, through the same wrapper:
`"$W" "$LANE/target/debug/ophio" pair confirm <code>`.

Then run every step in order, stopping at the first failure:

```sh
cd "$LANE/client"
for step in work stream data library-ui share permission background question \
  closed-lifecycle image-share notification stop-continue device-grants library-uncertain \
  terminal-ui readiness library-retained transfer-quota upload-resume \
  download-cancel large-text lock-links revoke; do
  if [ "$step" = lock-links ]; then
    adb -s "$ANDROID_SERIAL" shell locksettings set-pin 842615
    node src-tauri/tools/emulator/lock-links.mjs > "$LANE/target/e2e-evidence/$step.log" 2>&1
    rc=$?
    adb -s "$ANDROID_SERIAL" shell locksettings clear --old 842615
  else
    node src-tauri/tools/emulator/real-companion.mjs "$step" \
      > "$LANE/target/e2e-evidence/$step.log" 2>&1
    rc=$?
  fi
  [ "$rc" = 0 ] || break
done
```

The PIN is a disposable emulator credential that `lock-links.mjs` also names; it
is set only for that step and cleared after it. `permission` requires a fresh app
permission state, so `pair` answers the first-run alerts question with Not now
and `background` turns alerts on from Settings. `question` asks two questions
through the real computer-mcp bridge while the app is closed and answers both
from the notification: the first with a choice's button, the second by typing
in the notification's reply box. It leaves alerts and battery use as it found
them. `closed-lifecycle` restarts the
emulator and reinstalls the APK it was given; a comma-separated list after it
runs some of its cases. `image-share` uses Android Files, Downloads and the native
share chooser to supply a real content URI grant.

`work` starts the authored Codex fixture, waits for its approval and taps the
phone's Stop button. `stop-continue` inspects that task and its receipt and uses
the real composer to continue a different, finished task. `device-grants` removes
and restores this device's terminal grant through the phone, then removes its
management grant and verifies the refusal; only the isolated owner CLI restores
management afterward.

`library-uncertain` validates the recorded PID of the isolated Øphio against its
executable path, pauses only that process, uses Review → Save on computer, then
drops the native control connection before acknowledgement. A `finally` block
resumes that process. `library-retained` saves from the phone while a writer on
the computer holds the same note open and changes it, so the computer keeps the
displaced version as a recovery copy; the phone must list it under **Needs
checking**, open a draft from it and discard it.

`transfer-quota` declares two unfinished 2 GiB uploads from this device and
checks that a third file is refused with the held total, leaves no record and
does not reach the Inbox, then sends after the held uploads are cancelled.
`upload-resume` drops the upload's connection after 8 MiB of a 64 MiB file and
requires the resumed upload to start from what the computer already staged, to
match its SHA-256 and to leave no staging behind. `download-cancel` cancels a
96 MiB download part way and checks that the source is unchanged and nothing is
saved on or offered to the phone.

`large-text` raises Android's font scale to 2.0, answers a fresh approval with a
real touch at that size, and audits Talk, Work, Control, Library, a note,
Transfers, Connections and the app's Settings. It fails on sideways overflow, a
covered control or one that leaves less than 44 px to press, measured as what a
press actually reaches. Clipped text is recorded, not failed. Before raising the
scale it proves the measure on three 48 px buttons of its own: one clipped by
its container and one partly covered must fail, and a whole one must pass. It
restores the default scale in a `finally` block. `lock-links` turns on the app
lock with the emulator's screen lock and opens a pending approval's link while
the app is locked, once warm and once after a force stop. Each time the lock
shows first with no item open, the link's preview appears only after unlock and
opens the item only on an explicit Open item, and the approval stays pending.
Once the approval is answered, the same link says so. A Recents capture is kept
for inspection.

CDP connects only to the packaged WebView forwarded through this ADB server.
Pairing, Stop, Continue, run selection, Library editing/recovery, share intake,
permissions, readiness, terminal controls and the large-text approval use actual
UI handlers or touches. Task creation, snapshots, terminal attach and bulk
transfer checks also use the app's actual HostSession/Platform through React
context. They exercise native pinned TLS and protocol handling; this is not a
claim that every action was a manual tap.

Afterwards remove only this forward, stop the emulator, and stop only the PID
recorded in `target/phone-e2e/companion.pid` after checking that its command line
is this checkout's `target/debug/ophio run`. Do not use a broad process-name
kill.

## Results and evidence

Local, ignored evidence is under `target/e2e-evidence/`. Every retained PNG was
opened for visual inspection. JSON files record observed assertions. Logs exclude
pairing offers and keys.

| Check | Result | Evidence |
| --- | --- | --- |
| Clean APK, fresh pairing confirmed on the computer, caught-up snapshot | Pass | `phone-paired.png`, `pair.log`, `work.json` |
| Task start, live delta and durable completed reply | Pass | `phone-stream.png`, `stream.json` |
| Approval Allow and Deny through the real computer-mcp bridge | Pass; the agent received each decision | `phone-approval-allow.png`, `phone-approval-deny.png`, `work.json` |
| Stop while awaiting approval, receipts, Continue, run picker | Pass | `phone-stop-*.png`, `phone-continued.png`, `phone-run-picker.png`, `stop-continue.json` |
| Terminal attach, input, output and close | Pass | `phone-terminal-text.png`, `data.json` |
| Terminal arrow keys and key row | Pass; PTY received all four arrows; 412 px wide and 528 px high with no horizontal overflow, every key 48 px | `phone-terminal-keys.png`, `phone-terminal-focused.png`, `terminal-ui.json` |
| 3 MiB upload/download | Pass; 3,145,728 bytes each way, matching SHA-256, Keystore-wrapped key protection | `phone-transfers.png`, `data.json` |
| Library list/open/edit/review/save and concurrent edit | Pass; conflict keeps both versions | `phone-library-ui-*.png`, `library-ui.json` |
| Drop connection during Library save, reopen and recover | Pass; **Outcome unknown** becomes **Saved on computer** after Check again | `phone-library-uncertain.png`, `phone-library-recovered.png`, `library-uncertain.json` |
| Library retained copy, **Needs checking** | Pass; displaced computer write listed with its reason, draft from it, copy discarded, nothing left beside the note | `phone-retained-*.png`, `library-retained.json` |
| Android text and image share → Inbox → Talk draft | Pass; no task started | `phone-share-*.png`, `phone-image-draft.png`, `share.json`, `image-share.json` |
| Alert offer, notification permission, hidden notification and tap | Pass; approval category and project only | `phone-notification-*.png`, `phone-task-notification.png`, `permission.json`, `notification.json` |
| Alerts when the app is closed: swipe away, open from the alert, quiet on screen, answered at the computer, off | Pass; raised by the watcher after Android restarted it in a new process, opened the exact approval, nothing raised while on screen, cancelled when stopped at the computer; no notice of Øphio's own in the status bar while on, and off stops the watcher | `phone-alerts-setup.png`, `phone-closed-*.png`, `background.json` |
| A question answered from its notification while the app is closed | Pass; asked as a question from Claude Code with its words, the project and a button for each of two choices; the button's choice and a typed answer each reached the agent as its answer, the alert cleared, both questions were recorded as answered, nothing crashed | `phone-question-asked.png`, `phone-question-reply.png`, `question.json` |
| Closed app across restart, update, Doze and airplane mode, and with battery use limited | Pass; an alert asked after a restart or an update was raised within a second, and one showing before either was raised again; raised within a second in forced Doze with battery use Unrestricted; one asked in airplane mode was raised as soon as Wi-Fi had settled after the network came back; each was taken away within a second of being answered at the computer. With battery use limited, Android stopped the watcher once the closed app went idle, nothing was raised, and on opening the app the watcher started again and the approval was there waiting | `closed-lifecycle.json` |
| Device grants and refusals | Pass; terminal and management refusals named | `phone-device-permissions.png`, `phone-*-refused.png`, `device-grants.json` |
| Readiness Check again and connection sheet | Pass | `phone-readiness-*.png`, `readiness.json` |
| Transfer allowance | Pass; refused at 4.0 GB held, no record, not in Inbox; sent once freed | `phone-transfer-quota*.png`, `transfer-quota.json` |
| Upload resumed after a dropped connection | Pass; dropped at 8,650,752 bytes, resumed from there, SHA-256 matched, staging removed | `phone-upload-*.png`, `upload-resume.json` |
| Download cancelled part way | Pass; cancelled after 19,398,656 of 100,663,296 bytes, computer copy unchanged, nothing saved or offered on the phone | `phone-download-*.png`, `download-cancel.json` |
| Font scale 2.0 | Pass; the measure first failed a clipped and a covered 48 px control at 20 px and passed a whole one; 30 px body text on eight screens, none clipped, scrolling sideways, covered or under 44 px to press; approval answered by tap | `large-*.png`, `large-text.json` |
| App lock and links | Pass; warm and cold links while locked, preview only after unlock, approval still pending, answered link says so | `lock-links/*.png`, `lock-links/results.json` |
| Revoke last and attempt reconnect | Pass; revoked, refuses to reconnect | `phone-revoked.png`, `revoke.json` |
| Real desktop watch/control | Skipped; desktop disabled for this run | No desktop captured or controlled |

Transfer SHA-256:
`a1feacf0d812ba4d0b0e463ed45bbd583cea1de55c54693116754b30b5794745`.

## Checked beside the pass

On 2026-10-04 the watcher for alerts while the app is closed stopped running
as an Android foreground service, so it no longer keeps a notice in the status
bar. Android then keeps it running only while Øphio's battery use is
Unrestricted. The same day showed that the app had never held Android's
permission to see network changes, so the watcher could not tell the network
had come back and waited out its retry instead: 40 s after airplane mode on the
emulator, and up to 15 minutes after a long time offline. A build of the commit
before Øphio stopped using a foreground service did the same, so the delay was
not new. With the permission, `background` and every `closed-lifecycle` case
were run again on the emulator after a fresh pairing, from a build whose Android
and Rust code is that of `35747d17`, all seven cases in one run, and both steps
left battery use as they found it. The two rows above give those results; the
airplane case now fails if the alert comes more than 15 s after the network.
The cases where an alert shows before a restart or an update now fail unless
Android shows it anew afterwards: an update clears the app's alerts a moment
after the install returns, and one run found the old alert still there while
the app's process was between the old and the new. One run of the restart
cases failed when the emulator came back from its restart with Wi-Fi connected
but no route, so nothing on it could reach the computer; turning Wi-Fi off and
on restored it, the app reconnected at once, and both restart cases then
passed. One earlier `background` run failed when a swipe on its third closing
left the card in Recents, after its first two alerts had been raised and
cleared as they should. The rest of the pass is that of 2026-10-01.

On 2026-10-08 an agent's question became answerable from its notification, and
`question` was added. It was run on API 35 after a fresh pairing, with `work`,
`permission` and `background`, from a build of `5229b989` with the step as
committed; all four passed. Its first run failed after the first answer had
reached the agent, because the step created its second task through the app it
had just swiped away; the step now opens the app again before each question.
Android versions before 12 offer the choices only in the reply box, and that
path has not been run.

Review of an earlier pass led to two product changes. Both are in the checkout
above, so this pass ran with them, but it did not provoke what either guards
against; each was checked as stated.

- **A device's terminals are contained where the computer allows it.** Where
  Øphio can make a cgroup (cgroup v2, in a control group Øphio may divide), a
  terminal a device opens runs its program in a cgroup of its own. Øphio on the
  computer starts the program itself and joins it to the group before the
  program is loaded, so nothing the program loads runs outside the group; if
  it cannot join the group it made, the program never runs. The program is
  found where the pty library found it. What such a program leaves running can
  no longer pass for the owner's own terminal, even after Øphio on the
  computer has been restarted. Where no group can be made, the program runs
  without one and is recognised only by its ancestry, so only while it is
  still attached to Øphio; `SECURITY.md` states this limit and that running
  Øphio as its installed service covers such programs. Each group is removed
  once what it held has ended, together with any empty groups the program made
  inside it, and groups left by an Øphio no longer running are removed before
  another is made. Checked by the containment, terminal and companion tests.
  The companion test preloads a library into a device terminal's program that
  detaches a helper as the program is loaded, and requires the owner's channel
  to refuse that helper, and to refuse it again once another Øphio has taken
  over. The cgroup tests skip themselves without a writable cgroup v2
  delegation, so they were run in a disposable Ubuntu 24.04 VM inside a
  delegated systemd user scope. There they passed, and the companion test
  failed as it should in three controls: with the terminal's group taken out,
  with only the running Øphio's own groups recognised, and with the program
  joining through `/bin/sh` as it did before. In each, the helper was given a
  pairing offer.
- **A link belongs to the page that asked for it.** After a reload, a link the
  earlier page asked for is refused instead of being handed to the new page,
  and Retry connection opens a refused link again. Checked by the app's unit
  tests and the native side's tests.

## Defects fixed

Earlier passes on this date, and the search for one fault described under
coverage limits, found these. Each fix is in the checkout above.

- **The companion refused the owner's own terminal** when it was started with
  `nohup` from a shell that exited at once. It took the process group it shares
  with the owner's terminals for one only it uses. A group now counts as the
  companion's alone only while no other program is in it or below it.
- **A reloaded page could not reconnect** ("The connection is busy"). The native
  side refuses a second link to the same computer and only the page that opened a
  link closed it. Links opened before a page load, and a pairing request waiting
  on the owner, now end when the new page starts.
- **The companion panicked when a connection ended on a failed write,** hit when
  an upload's connection was dropped. Every writer error now reads as a closed
  connection.
- **Touch targets did not grow with Android's text size.** Android zooms a
  WebView's text but not `rem` lengths, so at font scale 2.0 the transfer screen's
  File and Text choices left 42 px and Talk's After this turn 36 px. Targets are
  now at least a line of the root text plus padding, which is unchanged wherever
  `rem` already scales.
- **At font scale 2.0 the Work title broke mid-word beside Settings,** because
  its basis was a `rem` length that Android's text zoom does not grow. The title
  now takes the full width as its basis and Settings wraps below it.
- **At font scale 2.0 the top bar cut the project name short** ("Phone ap…").
  The name now takes up to two lines before it is shortened.
- **Settings stretched the app frame at font scale 2.0.** The words that tell a
  screen reader which agent a models list belongs to had no positioned
  ancestor inside the scrolling screen, so they were placed thousands of
  pixels below the frame and did not scroll with the list. Nothing visible was
  cut, but the audit recorded the frame as clipped. The list's summary now
  holds them.
- **Twelve more pieces of hidden text sat outside the screens that scroll
  them.** The search for the Settings fault described under coverage limits
  found it in the "to" between two stages in a project's stage history, the
  safety rail's caption beside its toggle in sheets, Annotate's file inputs,
  the words beside a hand-back in Talk at enlarged text, " · Details" after
  Offline in the composer on a short screen, the "Mode" heading of the review
  screen's mode list, the "Added:" and "Removed:" before each line of a diff
  in the change sheet, an approval card's single question and the words that
  say whether a proposed line is added or removed, the remote keyboard's
  heading, Annotate's title while a comment is typed with the keyboard
  open at enlarged text or in landscape, the "Capturing…" status in
  Annotate's "Add a source" sheet while a capture is pending, and the "Not
  allowed:" beside a permission the computer left out on the screen shown
  after pairing. The nearest wrapper inside each scrolling screen, or the
  screen itself, now holds them. Nothing visible changed.
- The checked-in Android colours were regenerated from the current design tokens.
  The build already wrote the right ones.

The harness changed with them. The terminal step reaches the keys as a narrow
phone shows them; `large-text` starts from the default scale and measures what a
press reaches; `lock-links` reuses the pass's folder and sets the screen lock as
a person would, in Settings, then returns to the app; native taps never reuse a
stale screen dump, and look again when Android ends the dump as it exits. The
emulator has a hardware keyboard, and the keyboard app could be left showing
only its small bar for one, which never resizes the page, so the terminal step
clears that app's data first to bring the on-screen keyboard back. Clearing
it restarts the keyboard app, and a tap made before Android has bound it again
was lost, so the step waits for that binding before it taps Show keyboard, then
for the keyboard to show. Leaving airplane mode brings cellular up before
Wi-Fi, and Android drops connections made over cellular 30 seconds after Wi-Fi
takes over. That drop once landed in the terminal step, which then tapped Show
keyboard while input was off; the app reconnected by itself five seconds later.
The two steps that leave coverage now wait for Wi-Fi to be the settled default
before they go on. The
image-share step finds the app in Android's chooser by the label the build gives
it, now Øphio. The pairing step waits for the computer's offer to be written,
since `pair` starts beside it, and the notification step waits up to ten seconds
for a tap to open its item instead of checking after a fixed second.
`large-text` counts only the reach it measures, never the box a control claims,
and proves that measure on its own fixture first. It also opens the app's
Settings; it had audited Connections under that name.

### Screen reader

A TalkBack pass on the same emulator walked the app's screens by swipe
navigation and answered an approval with a double tap. The focus ring in each
screenshot was matched to the page element under it through DevTools. It found
nine places where a screen reader stopped more than once on one piece of
information, or heard the wrong name, and each is fixed:

- an approval's expiry, a run divider, an agent's catalog line and a sentence
  containing a command are each read as one piece;
- a field whose label is hidden is named by the field, with no separate stop;
- an announcement clears ten seconds after it is read, instead of remaining as
  the first stop on every later screen;
- the notification choice is named by its label, each agent's models list says
  which agent it belongs to, and the Work screen's stage row says the stage.

The fixes were checked with TalkBack on a rebuilt APK; after them, Talk with a
pending approval read as 24 distinct, named stops. The captures are under
`screen-reader/`.

## Coverage limits and verification

On this emulator, TalkBack's touch exploration (moving a finger to hear what is
under it) did not reach web content at all, including a plain page in Chrome;
swipe navigation did. Touch exploration inside the app is therefore not
verified. The emulator has no audio, so TalkBack was followed through its focus
and the names the page gives, not heard. The TalkBack pass ran before the name
became Øphio and was not repeated on the final build, so how TalkBack's voice
says the name is not verified. The font-scale audit covers the eight screens in
`large-text.json` as each first opens (Talk with a pending approval); sheets,
dialogs and other states were not measured at that size on the emulator.

The fault found in Settings, hidden text placed against a container outside
the screen that scrolls it, was then searched for across the app. The search
ran in desktop Chromium's mobile emulation against the app's built-in
simulator, not in the packaged WebView. It visited every screen the simulator
can reach at 412 × 915 with the root text at 1, 1.3 and 2 times its size, at
915 × 412 at 1 and 1.5 times, and at 360 × 640, 800 × 1280 and 1280 × 800;
the same screens with the computer offline at 412 × 915 at 1 and 2 times and
at 915 × 412; and, at 412 × 915, the screens and sheets reached by tapping
every control on each of those screens, one level deep (about 7,700 taps). It
checks for that one fault only, and only where words are hidden at the time.
The app hides words for a screen reader in 21 places that are always hidden and
nine style rules that hide words only at some sizes or in some states: enlarged
text, a short or narrow screen, the keyboard open, the computer offline. So
every element either kind can hide was also put into its hidden placement on
each screen the simulator reaches, at 412 × 915 with the root text at 1 and 2
times (online and offline), at 915 × 412 (online and offline) and at
360 × 640, with Annotate in its typing state and a task in Plan mode started
for the purpose. Where the simulator has no such state (approval cards with a
question or a proposed edit, the remote keyboard while typing, the screen after
pairing when the computer leaves a permission out), hidden text was placed in
its container, and a capture was held pending in "Add a source" at eight sizes.
Together they found the twelve fixed above, the last two after a review named
a state the first searches never set, and none remain in what they visited.

This pass covers the default emulator size and light appearance, plus font scale
2.0. It does not renew earlier claims about dark mode, landscape, physical
hardware or other Android versions. No real desktop, real model/account,
private-network connection, voice or chat service was exercised. Alerts with
the app closed were checked across a swipe, a restart, an update, forced Doze
and airplane mode, each over a minute or two; a day of real use and its battery
cost are checked on the Galaxy and the tablet. Readiness reports fixture capabilities and the intentional
disabled-desktop state, not a working desktop setup.

| Command/check | Result |
| --- | --- |
| `cargo build -p companion -p computer-mcp` | Pass |
| `npm run tauri android build -- --debug --apk --target x86_64` | Pass |
| All 17 lane gates on the merged candidate: Rust format, Clippy and tests; Windows cross-check and Clippy; lockfiles; build; the Android shell's format, Clippy and tests; client type check and Vitest; script and lab tests; name and source-candidate audits | Pass |

Debug APKs, build caches, fixture state, pairing material, logs and screenshots
remain ignored and are not published with this document.
