# Android device bridge

Internal Tauri plugin for Android Keystore, microphone capture, share intake,
Downloads, local notifications, an optional foreground service, and shell insets.
Kotlin commands are callable from Rust only. The packaged WebView receives the
narrow root commands defined in the app's capability file; it cannot create,
unwrap, or sign with keys directly.

The QR scanner uses Camera2 and the Apache-2.0 ZXing decoder locally. Camera
frames and microphone PCM remain in memory. Neither uses a cloud service.

Run `KeyChecks` from the library's Android test APK to exercise the real Keystore.
It checks non-exportability, wrapping, tamper rejection, and deletion without
exposing key bytes or using a signed-in account. `-e downloads true` runs only
the Downloads recovery checks instead: fixture files in the test app's own
Downloads rows, checking that a restart keeps finished downloads and removes only
unfinished ones. `-e shares true` runs only the share inbox recovery checks:
fixture files in the test app's own share directory, as a process killed while
copying, publishing or acknowledging a share leaves them, checking that a
restart publishes whole records, frees the rest of the quota, and leaves
committed shares and imports still running alone. Two concurrent-instance checks
pause recovery after its record snapshot, then let a new or already active import
continue, checking that its published bytes survive cleanup and another restart.
Recovery and record publication share a process-wide lock; copying bytes from
the sharing app does not hold that lock.
Recovery stops before sweeping bytes if a metadata scan or staged-record rename
fails. Fault-injection checks verify that complete shares survive these failures
and that a later restart publishes staged shares and reclaims orphaned bytes.

`previewShow` keeps its dialog open after each capture and resolves on dismissal
with `{ pictures: [{ id, size, width, height, capturedAt }, ...] }`. Rust forwards
this JSON value unchanged; the TypeScript bridge also accepts the older single
descriptor or null result. `capturedAt` is the capture's Unix time in milliseconds,
saved alongside its original computer, preview and task binding. Recovery accepts
older metadata without this timestamp and labels the capture time as unrecorded.
PNG and metadata files remain pending until the app has durably saved each draft
and calls `previewPictureAck`; closing the dialog never acknowledges a picture.

## Problem report diagnostics

The web entry point is `client/src/platform/reportNative.ts`. Its four functions
take pending crashes, watch screenshots, capture the app's view, and ask whether
screenshot detection is available. Browser, simulator, and desktop builds return
empty results. These operations do not send anything to a computer or an intake.

The Rust shell exposes `take_native_crashes`, `screenshot_supported`,
`start_screenshot_watch`, `stop_screenshot_watch`, and `capture_own_view`.
They call the Kotlin commands `takeNativeCrashes`, `screenshotSupported`,
`startScreenshotWatch`, `stopScreenshotWatch`, and `captureOwnView`. Screenshot
events use a separate channel with `{ type: "screenshot" }`; they do not replace
the app's lifecycle channel. Multiple web subscribers share one watch.

Android screenshot detection needs API 34 and the normal `DETECT_SCREEN_CAPTURE`
permission. The callback follows the current activity, unregisters on pause and
destroy, and resumes only while requested and unlocked. Exit reasons need API 30;
normal exits, package changes, permission changes, and nonfatal signals are left
out. Low memory, freezer, dependency and excessive resource exits count only at
foreground or visible process importance. Foreground-service and unknown importance
do not establish visibility. Android can close backgrounded apps for these reasons,
and reopening the app later must not offer to report a crash. `-e exits true`
runs only these exit checks from `KeyChecks`. A committed preference timestamp
prevents returning old failures again.
Android supplies no historical app version, so exit records use the installed
version at collection. Descriptions are scrubbed and never include a trace.

Own-view capture works at the app's minimum API 29. It copies the activity's
window, falling back to drawing its own root view when the protected surface
refuses a copy. It never reads the display or another app. JPEG quality and
dimensions decrease until the image fits 512 KiB before standard Base64 encoding.
A background or locked app returns no picture. Pictures stay in memory.

The shared Rust panic hook saves at most 16 pending records synchronously in the
app's private data directory, chains the previous hook, and clears records when
read. Messages use the protocol scrubber. Frames contain only repository-relative
Rust source locations, at most 40; builds without line information may have only
the panic location. The hook is installed on mobile targets, including iOS.
Taking crashes consumes them: the report composer should retain the returned
records in its draft even if the person dismisses the sheet.

### Kotlin compile without packaging

An isolated Gradle root can include only the two library modules. This avoids
building Rust, the web bundle, and the application. Prepare it from the repo root:

```bash
python3 - <<'PY'
import os, shutil
from pathlib import Path
root = Path.cwd()
out = Path(os.environ['CARGO_TARGET_DIR']) / 'report-kotlin-check'
out.mkdir(parents=True, exist_ok=True)
tauri = next((Path.home() / '.cargo/registry/src').glob('*/tauri-2.12.0/mobile/android'))
for name, source in [('device', root / 'client/src-tauri/plugins/device/android'), ('tauri-android', tauri)]:
    shutil.copytree(source, out / name, dirs_exist_ok=True, ignore=shutil.ignore_patterns('build', '.gradle'))
(out / 'settings.gradle').write_text('rootProject.name = "native-report-check"\ninclude ":tauri-plugin-device", ":tauri-android"\nproject(":tauri-plugin-device").projectDir = file("device")\n')
for name in ['build.gradle.kts', 'gradle.properties']:
    shutil.copyfile(root / 'client/src-tauri/gen/android' / name, out / name)
PY
# Source your build-scoped Android SDK/JDK environment, then:
client/src-tauri/gen/android/gradlew -p "$CARGO_TARGET_DIR/report-kotlin-check" \
  --offline --no-daemon --no-configuration-cache --max-workers=4 \
  :tauri-plugin-device:assembleDebugAndroidTest
```

This compiles the plugin and its instrumentation checks; it does not run them or
install anything. It creates only a local debug test APK, without release signing.
