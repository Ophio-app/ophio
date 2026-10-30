# Local lock and object-link checks

These checks run on an isolated Android 15 / API 35 emulator with a debug APK.
Follow [E2E.md](E2E.md) for the headless emulator, isolated ADB server, authored
Claude/Codex fixtures, starting Øphio on the computer, and pairing. Use only
that fixture; this test starts an authored permission request and denies it
during cleanup. It performs no model inference or external account access.

After pairing, with app lock off and the toolchain environment from
[BUILDING.md](../../BUILDING.md) sourced, run from `client/`:

```sh
adb -s emulator-5562 shell locksettings set-pin 842615
node src-tauri/tools/emulator/lock-links.mjs
adb -s emulator-5562 shell locksettings clear --old 842615
```

The PIN above is a disposable emulator fixture. Never substitute a real phone or
personal credential. The script requires `ADB_SERVER_SOCKET=tcp:5038`, targets
only the emulator named by `ANDROID_SERIAL` (default `emulator-5562`), checks the
paired address is the fixture's `10.0.2.2` port, and connects only to that
emulator's forwarded debug WebView. Set up forwarding as in E2E.md; the script
refreshes it after cold starts.

The script checks that:

- Settings enables local lock through Android's device-credential prompt.
- Leaving the app protects its recents thumbnail. Returning shows only the lock
  screen; a pending VIEW intent stays hidden.
- Warm and cold `am start -a android.intent.action.VIEW -d <object-reference>`
  launches authenticate locally, check access with the paired computer and show
  a preview before navigation.
- The authored approval remains pending after unlocking, after preview and after
  Open item. Only the test's explicit denial changes it.
- Reopening the denied request reports that it has already been answered.
- The sideloaded build reports no store installer. No update is installed.

Ignored evidence is written to `target/e2e-evidence/lock-links/`: `results.json`, warm/cold
lock, preview and approval PNGs, a settled preview and `recents-protected.png`.
The recents image uses Android screencap; app images use the isolated debug
WebView because `FLAG_SECURE` intentionally blocks ordinary app screenshots.
The recents card must contain no app content.

Before a PIN is set, Settings reports lock unavailable. An unpaired object
reference is refused after unlocking, without an Open item action. The script
does not cover strong-biometric enrollment or Android 10's credential fallback;
it uses Android 15's system PIN prompt. The 1/5/15 minute policies use Android's
monotonic elapsed clock; the script covers the immediate policy and cold-start
lock, not a timed soak of every interval.

The script disables app lock on success. If it fails, unlock with the fixture PIN
and disable it in Settings before clearing that PIN. Stop the isolated Øphio
and emulator you started, remove port forwarding, and delete the fixture pairing
and authentication state when finished. Screenshots and synthetic result records
must remain outside git.
