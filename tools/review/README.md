<!-- SPDX-License-Identifier: Apache-2.0 -->
# Connected review environment

A connected environment for using the real app with a real companion, without a
model and without a shell:

- the packaged Android app on a headless emulator (`emulator-5562`);
- paired over the real pinned transport with this checkout's companion
  (`target/debug/ophio`), listening on `127.0.0.1:47112`;
- running tasks the companion really launches, with `review_claude.py` standing
  in for `claude` and working on a small demo project, the tip splitter;
- driven through two MCP servers: `app` (Playwright, inside the app's WebView)
  and `phone` (the Android device itself).

## Up and down

Source the toolchain environment from `BUILDING.md` first. It must provide the
Android SDK, an AVD named `buildphone` and the isolated ADB server (`tcp:5038`).

```sh
bash tools/review/connected.sh up          # or: up --rebuild
bash tools/review/connected.sh status
bash tools/review/connected.sh down
```

`up` builds the companion, starts it on a fresh fixture, boots the emulator if
it is not running (cold and read-only, so it can share the AVD with other
checks' emulators), and builds the APK when its sources are newer than the
last build. It then installs and launches the app, pairs it through the
Welcome screen, confirming at the computer with `ophio pair confirm` and the
code the app shows, adds the tip splitter and forwards DevTools to
`127.0.0.1:9226`. The first run takes several minutes. Running `up` again is
safe: it reuses whatever is still live, thaws a paused companion and brings
the app back to the front.

`down` removes the forward, stops the companion and puts back the device
settings the tools change. It stops the emulator only if `up` booted it, and
it confirms that each process has exited.

## Driving it from an MCP client

```sh
bash tools/review/connected.sh mcp-config /tmp/review-shots >/tmp/review-mcp.json
claude --mcp-config /tmp/review-mcp.json --strict-mcp-config \
  --tools "" --allowedTools mcp__app,mcp__phone
```

`--tools ""` removes the shell, file and edit tools, so the session only has
`app` and `phone`. Playwright saves its screenshots and snapshots in the shots
directory.

## Prompt words

The agent picks what to do from the words of the task, in this order:

| Words in the task                        | What the agent does                                        |
| ---------------------------------------- | ---------------------------------------------------------- |
| "give up"                                | Looks around, then fails with an error                     |
| "question", "ask me"                     | Asks a multiple-choice question about rounding             |
| "fetch", "url", "look up"                | Asks to fetch a Python docs page (scripted, no request)    |
| "changelog", "new file"                  | Asks to write a new `CHANGELOG.md`                         |
| "slow", "take your time", "every file"   | Reads every file, a few seconds each: use it to try Stop   |
| anything else                            | Fixes the failing test, then makes one improvement per task |

In the default case the agent asks before each edit and each test run. The
receipt lists the changed files and the check, which fails and then passes.
If you deny a step, the agent stops there and says why. In Plan mode the
agent reads the project and proposes numbered steps. In Ask mode it only reads
and answers.

## What the tools can and cannot do

`app` is `@playwright/mcp` attached to the app's WebView over DevTools.

- It can: take accessible snapshots, click and type by ref, press keys, run
  JavaScript, take page screenshots and go back in the page's history.
- It cannot: see or touch anything outside the page (the status bar, the
  keyboard, Android dialogs), or change the device.
- Refs are numbered afresh in each session, so take a snapshot first.
- `browser_wait_for` gives up after 5 seconds whatever `time` it is given. To
  wait longer, take snapshots again.

`phone` is `phone-mcp.mjs`, which works on the device through ADB.

- `screenshot`: the whole screen, in device pixels (1080×2400 in portrait).
- `tap` and `swipe`: real touches, in those device pixels.
- `back`: Android Back. If the keyboard is up, it closes the keyboard; at the
  app's first screen, it leaves the app. Run `up` to bring the app back.
- `keyboard`: shows the keyboard by tapping the focused field, or hides it.
- `rotate`, `font_scale` (1.0, 1.3 or 2.0) and `dark_theme`.
- `companion`: pauses, resumes or restarts this environment's companion.
- `network`: cuts every network with airplane mode, or restores it. ADB and
  DevTools keep working.
- It cannot: read the page's structure. Use `app` for that.

## Isolation

- The companion runs with an empty environment. Its home, XDG directories,
  logs and state are all under `target/review-connected/`, so no real
  configuration, keys or agents reach it.
- `claude` on its `PATH` is the scripted agent, and `tailscale` always fails.
  The companion listens on loopback only.
- The pairing offer goes to a private fixture file that is deleted after
  pairing. It is never printed.
- Every process the script starts is recorded by PID and start time. `down`
  and the drills act only on those processes.
- Only the isolated ADB server and `emulator-5562` are used. `phone` refuses
  to start without that server.
