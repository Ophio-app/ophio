# Private preview verification

This procedure checks private previews end to end with the packaged Android
debug app, Øphio running in an isolated Linux fixture, and Vite 8. It uses an
Android 15 / API 35 x86_64 emulator without a window, audio or snapshots, with
private data/cache images and software rendering. No desktop window, personal
device, signed-in agent or external account is involved. The only task it
creates is queued with `ask_before_start`, so it stays `accepted`.

## What it checks

| Check | Expected result |
| --- | --- |
| Paired native preview side connection | A fresh fixture device key and pinned TLS; the registered preview opens from its card |
| Forwarded Vite HTTP page and modules | Served through the Android ephemeral loopback listener |
| Vite WebSocket hot reload | Editing the fixture module changes the open page from “Before hot reload” to “After hot reload” without navigation |
| Preview isolation | The separate WebView has no `__TAURI_INTERNALS__`; the gate cookie is absent from `document.cookie` |
| Native Take picture | Captures only the preview WebView and returns its PNG to the card |
| Restart recovery | The saved picture renders after the app process restarts |
| Attach to this task | Goes through the real resumable upload; `task_artifacts` shows a `source` link to the queued task |
| Emulator control loss | Disconnecting the owning control link dismisses the preview and the card reports the lost connection |
| Unit-level closure and authorization | Control loss, idle/background expiry, unauthorized/cross-origin requests, no replay and exact cookie checks |
| Computer route and IPv6 | Paired-device/control ownership, grant enforcement, control-loss closure, no replay and IPv4→IPv6 fallback |

The helpers write `hot-reload.png`, `picture.png` and `result.json` under the
ignored `target/preview-results/`. Pairing offers and keys are not results;
delete them with the isolated fixture after the run.

Keep Hyper keep-alive enabled on the preview listener: disabling it rewrites
`Connection: upgrade` to `Connection: close` and breaks Chromium WebSocket
handshakes. The native transport test checks the upgrade response header and
bidirectional upgraded bytes.

## Reproduction

Use the repository root as `$REPO`, the toolchain environment file described in
[BUILDING.md](../../BUILDING.md) as `$BUILD_ENV_FILE`, and an unused isolated
AVD. Follow the isolation requirements in [E2E.md](E2E.md). Do not use a
physical phone or an emulator another process is using. Every ADB command below
names the isolated emulator explicitly. Verify its listening process belongs to
this run before connecting.

```sh
cd "$REPO"
source "$BUILD_ENV_FILE"
export CARGO_TARGET_DIR="$REPO/target"
cargo build -p companion -p computer-mcp
client/src-tauri/tools/emulator/isolated-companion.sh setup
# Replace the fixture's configured port with an ephemeral loopback port.
python3 - <<'PY'
from pathlib import Path
p = Path('target/phone-e2e/config/ophio/config.toml')
p.write_text(p.read_text().replace('port = 47112', 'port = 0'))
PY
node client/src-tauri/tools/emulator/preview-fixture.mjs
```

Start only this isolated Øphio in a tracked background process using
`tools/emulator/isolated-companion.sh "$REPO/target/debug/ophio" run`.
Set `PREVIEW_OPHIO_PORT` to its actual loopback listening port. Launch Vite
from `client/` with `PREVIEW_TEST_HOST=127.0.0.2 node
src-tauri/tools/emulator/preview-vite.mjs` in another tracked process; leave
`PREVIEW_TEST_HOST` unset to use `127.0.0.1` when nothing else listens on port
5890 there. The helper also serves fonts from a symlinked `node_modules` and the
fixture directory, without changing the shared Vite configuration.

Create private emulator data/cache paths, then launch an x86_64 API 35 AVD with
`-no-window -no-audio -no-boot-anim -no-snapshot -read-only
-gpu swiftshader_indirect -port 5562 -data <private-data.img>
-cache <private-cache.img>`. Never reuse the AVD while another run owns that port.

```sh
cd "$REPO/client"
source "$BUILD_ENV_FILE"
export CARGO_TARGET_DIR="$REPO/target"
npm run tauri android build -- --debug --apk --target x86_64
adb -s emulator-5562 install -r src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
adb -s emulator-5562 shell am start -n app.ophio/.MainActivity
phone_pid=$(adb -s emulator-5562 shell pidof app.ophio | tr -d '\r')
mkdir -p "$REPO/target/preview-e2e"
adb -s emulator-5562 forward tcp:0 "localabstract:webview_devtools_remote_$phone_pid" > "$REPO/target/preview-e2e/cdp-port"
```

Generate a fresh pairing offer by running `ophio pair` through the isolated
wrapper, redirecting it to `target/phone-e2e/pair.txt` under `umask 077`. Run
that command in a tracked background process: it waits for enrollment. Do not
print the offer. The `pair` step below waits while the computer holds the
request; it writes the code the phone was given to
`target/phone-e2e/pair-code.txt`. Check that `pair.txt` ends with the same
code, then confirm through the same wrapper with `ophio pair confirm <code>`. From the checkout root, with `PREVIEW_OPHIO_PORT` exported:

```sh
node client/src-tauri/tools/emulator/preview-e2e.mjs pair
node client/src-tauri/tools/emulator/preview-e2e.mjs open
node client/src-tauri/tools/emulator/preview-reload.mjs
node client/src-tauri/tools/emulator/preview-e2e.mjs capture
node client/src-tauri/tools/emulator/preview-e2e.mjs attach
node client/src-tauri/tools/emulator/preview-e2e.mjs open
node client/src-tauri/tools/emulator/preview-e2e.mjs closed
```

The helpers use CDP only against the forwarded emulator WebViews. Pairing invokes
the real native command with the disposable fixture offer. Project/preview
registration and queue-only task creation use the app's actual HostSession.
Open, picture and attachment checks use the rendered controls. The reload helper
connects to the separate unprivileged preview WebView. These are integration
checks, not a claim that registration or pairing was entered manually.

The `closed` check stops that session; restart the app before further checks.
After force-stopping/restarting this app, remove this run's old forwarding and
forward the new app PID to a fresh ephemeral port, updating `cdp-port`. Run
`preview-e2e.mjs recovery` to check the saved picture. Reset the fixture with
`preview-fixture.mjs` before another hot-reload check. Its canonical `/@fs/…`
module paths are deliberate: duplicate slashes cause mismatched Vite hot-accept
dependencies. After Kotlin or plugin manifest changes, rebuild without
recompiling the native library using
`./gradlew :app:assembleUniversalDebug -x :app:rustBuildUniversalDebug --offline`
and install the result again. The plugin manifest declares the biometric
permission as well as the app manifest, so standalone plugin lint can verify
the lock implementation.

On completion, remove only this run's ADB forwarding, stop its Øphio/Vite/
emulator processes, and delete its fixture pairing/authentication and emulator
data. Do not stop the shared ADB server or processes this run did not start.

## Screens and remaining limits

The [windows and previews plan](../scripts/plans/windows-and-previews.json) covers preview loading/open,
picture/attachment, computer removal, idle expiry, lost connection, revocation,
empty/error/offline states; and Windows list/search/open/focus/close confirmation,
GNOME unavailability, light/dark, 360 px at 200% text and 1280×800. The shared
shell's Stop strip still occupies substantial height at 200% text; content and
primary actions remain reachable.

Browser cookies have no port-scoping attribute. The implementation uses a fresh
random secret and unique cookie name per listener, exact authority/origin checks,
`HttpOnly; SameSite=Strict`, strips gate cookies from upstream requests, and
confines the unprivileged WebView to its exact loopback origin. This provides
application-level port isolation; it is not a claim that cookie storage itself
is port-scoped.

External navigation/resources are blocked by the WebView and CSP. A preview that
requires a CDN, cross-origin API or OAuth navigation will need a separately
reviewed policy. Vite HTTP/WebSocket reload is exercised; Next.js itself is
not. Idle/background deadlines use a fake clock in native tests, not a
five-minute emulator soak. Window actions are tested with fixtures and compile
checked for Windows; no real desktop windows are inspected or changed.
