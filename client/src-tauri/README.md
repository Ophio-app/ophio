# Android native shell

This is a separate Cargo workspace for the Android app. Its only production
host dependency is the shared protocol/clock contract. The WebView loads
packaged assets; Rust owns pinned TLS, Ed25519 authentication and bounded
WebSocket queues. A private Kotlin plugin owns Android device APIs.

Build with the configured JDK 21, Android SDK/NDK and Rust Android toolchains.
From client/, use a private target directory and bounded build concurrency:

```sh
export CARGO_TARGET_DIR="$PWD/src-tauri/target" CARGO_BUILD_JOBS=8
npm ci --no-audit --no-fund
npm run tauri android build -- --debug --apk --target x86_64
npm run tauri android build -- --debug --apk --target aarch64
```

Each build writes gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk.
Copy it before building another ABI. Generated Android project sources and the
workspace lockfile are committed; caches, APKs, native libraries and Gradle’s
machine-specific Tauri wiring are ignored. `tools/sync-native.mjs` synchronizes
native identity and theme resources before builds. Run `tauri android init`
only when regenerating the project intentionally; preserve MainActivity’s Back
handling, the checked-in manifest/capabilities, the wrapper’s
`distributionSha256Sum` and gradle/verification-metadata.xml. Its templates
carry neither checksum, and `tools/sync-native.mjs` stops the build without them.

From src-tauri/:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

From client/:

```sh
npx tsc --noEmit -p .
npm test
```

`npm test` includes the native IPC bridge tests in `src-tauri/tests/`, so CI's
client stage checks shared-item acknowledgement after durable storage and
cleanup of incomplete downloads. To run just those tests with the same
configuration, use `npm test -- src-tauri/tests/native.test.ts`.

The protocol’s actual pairing/authentication framing is exercised against the
real server of Øphio on the computer in tools/dev-host/tests/transport.rs. To
run it manually:

```sh
# src-tauri/, isolated development data only
umask 077
cargo run -p dev-host -- ./target/dev-host-data 47101 > ./target/dev-pair-link.txt
```

The development host binds loopback only and advertises the emulator alias
10.0.2.2. It creates its own identity, persists it across restarts, and uses
synthetic desktop, terminal, speech and agent ports behind real services and
transport. No signed-in agent is launched. The one-use pairing link is sensitive;
do not commit or log its value. When the phone asks to pair, type the code it
shows into the development host's terminal and press Enter. metrics.json contains only fixture ids, journal
head and a PCM sample count. Keep the same data directory for reconnect tests.
Use a fresh fixture directory for a new first-owner pairing: later phones get
the computer’s restricted grants. Forgetting on the phone does not silently
revoke or change grants on the computer.

Run the emulator without a desktop window and use the isolated ADB server:

```sh
emulator -avd <avd> -port 5562 -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect -no-snapshot -no-metrics
adb -s emulator-5562 install -r ./target/ophio-x86_64-debug.apk
adb -s emulator-5562 shell am start -n app.ophio/.MainActivity
```

The tools/emulator helpers require ADB_SERVER_SOCKET=tcp:5038. They use
emulator-5562, Øphio on port 47112 and DevTools port 9226 unless ANDROID_SERIAL,
OPHIO_PORT and OPHIO_DEVTOOLS_PORT name others, so parallel runs can
each keep their own. Forward the emulator's webview_devtools_remote_<pid> socket
to the DevTools port, then use the CDP helper against that WebView. `node src-tauri/tools/emulator/transport.mjs` from
client/ verifies native control, 3 MiB paced transfer/hash/Downloads, terminal
and media. It temporarily replaces the normal screen’s control link to observe
its events; restart the app afterward. It uses only fixture data. Stop the
emulator and development host when finished. Production APKs do not expose
WebView debugging.

The private plugin’s Android test APK runs real Keystore checks:

```sh
# gen/android/
./gradlew :tauri-plugin-device:assembleDebugAndroidTest :tauri-plugin-device:lintDebug
# src-tauri/
adb -s emulator-5562 install -r plugins/device/android/build/outputs/apk/androidTest/debug/tauri-plugin-device-debug-androidTest.apk
adb -s emulator-5562 shell am instrument -w app.ophio.device.test/app.ophio.device.KeyChecks
```

`-e clips true` runs only the clipboard checks; `-e scan true` runs only the
QR scanner's, which decode a pairing code drawn dark on light and light on
dark, as a terminal on a dark theme shows it, and an empty frame.

Platform extensions are optional. Consumers should subscribe to onPlatformError,
onShared and onNotificationOpen once. Shared item ids support durable inbox
deduplication across a process crash; errors accompany the affected item.
Notification routes are delivered as data and never approve or start a task.
`notify` requires the attention consumer to call it; there is no hidden task
subscription. `stayConnected(true)` presents a battery explanation and requires
an authenticated control link. It is off by default and stops on disconnect,
forget, explicit stop, or Android’s foreground-service time limit.

The viewport is inset natively for system bars, cutouts and IME. Its CSS safe-area
variables are therefore zero (the viewport itself has no obscured pixels),
avoiding double padding and preventing scrolling beneath the bars. Native colors
are generated from the shared tokens. Android font scale maps to WebView text
zoom; AndroidX Back traverses the existing web history guard before leaving
the root. Live-view consumers explicitly request keepAwake and it is cleared
when the app leaves the foreground.

See DEPENDENCIES.md for licenses/identifiers. Emulator checks do not cover
native Keystore Ed25519 support, camera focus/orientation and optical QR decoding,
microphone interruption by a phone call, OEM lifecycle behavior or long-running
foreground-service limits; those need a physical device (see SUPPORT.md).
