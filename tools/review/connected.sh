#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# The connected review environment: this checkout's companion, running the
# review agent as `claude`, paired with the packaged app on a headless
# emulator. Source the toolchain environment first. See README.md.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
fixture="$repo/target/review-connected"
companion="$repo/target/debug/ophio"
serial=emulator-5562
package=app.ophio
apk="$repo/client/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk"
devtools=tcp:9226
# Only fixture state: no real home, config, keys or agents reach the companion.
isolated=(env -i HOME="$fixture/home" XDG_CONFIG_HOME="$fixture/config"
  XDG_DATA_HOME="$fixture/data" XDG_STATE_HOME="$fixture/state"
  XDG_CACHE_HOME="$fixture/cache" XDG_RUNTIME_DIR="$fixture/run"
  PATH="$fixture/bin:/usr/bin:/bin" SHELL=/bin/sh LANG=C.UTF-8)

usage() {
  echo "Usage: $0 up [--rebuild] | status | down | companion pause|resume|restart | mcp-config [SHOTS_DIR]" >&2
  exit 2
}

require_environment() {
  if [[ ${ADB_SERVER_SOCKET:-} != tcp:5038 ]] || ! command -v emulator adb node npm >/dev/null; then
    echo "Source the toolchain environment first; it selects the isolated ADB server." >&2
    exit 1
  fi
}

adb_() { adb -s "$serial" "$@"; }

# Start a command in its own session and record its PID and start time, which
# together name that one process even after the PID is reused.
spawn() {
  local name=$1 out=$2 err=$3
  shift 3
  python3 - "$fixture/$name.pid" "$out" "$err" "$@" <<'PY'
import os, subprocess, sys
pidfile, out, err, *command = sys.argv[1:]
os.umask(0o077)
with open(out, "ab") as stdout, open(err, "ab") as stderr:
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, start_new_session=True)
with open(f"/proc/{child.pid}/stat") as stat:
    started = stat.read().rsplit(")", 1)[1].split()[19]
with open(pidfile, "w") as file:
    file.write(f"{child.pid} {started}\n")
PY
}

# Fields of /proc/PID/stat after the command name: 0 is the state, 19 the start time.
stat_field() {
  local stat
  stat=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
  read -ra stat <<<"${stat##*) }"
  echo "${stat[$2]}"
}

# Print the recorded PID while that same process is still alive.
running() {
  local pidfile="$fixture/$1.pid" pid started
  [[ -f $pidfile ]] || return 1
  read -r pid started <"$pidfile"
  [[ $(stat_field "$pid" 19) == "$started" && $(stat_field "$pid" 0) != Z ]] || return 1
  echo "$pid"
}

gone() { [[ ! -e /proc/$1 || $(stat_field "$1" 0) == Z ]]; }

# Wait up to $2 seconds for a condition command to succeed.
wait_for() {
  local what=$1 seconds=$2
  shift 2
  for ((n = 0; n < seconds * 2; n++)); do
    "$@" >/dev/null 2>&1 && return
    sleep 0.5
  done
  echo "Timed out waiting for $what." >&2
  return 1
}

# End every process still in the session a recorded process led.
end_session() {
  local session=$1 left pid
  left=$(ps -o pid= -s "$session" | tr -d ' ' || true)
  [[ -n $left ]] || return 0
  kill -TERM $left 2>/dev/null || true
  for pid in $left; do
    wait_for "PID $pid to exit" 10 gone "$pid" || { kill -KILL "$pid" 2>/dev/null || true; }
  done
  echo "Stopped $(wc -w <<<"$left") leftover process(es) from session $session."
}

# Stop one recorded process, verify its exit, then clear its session.
stop() {
  local name=$1 pid
  if ! pid=$(running "$name"); then
    rm -f "$fixture/$name.pid"
    return
  fi
  if [[ $name == emulator ]]; then
    adb_ emu kill >/dev/null 2>&1 || true
    wait_for "the emulator to exit" 60 gone "$pid" || true
  fi
  kill -CONT "$pid" 2>/dev/null || true # a failure drill may have paused it
  kill -TERM "$pid" 2>/dev/null || true
  if ! wait_for "$name to exit" 15 gone "$pid"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_for "$name to exit" 5 gone "$pid"
  fi
  end_session "$pid"
  rm -f "$fixture/$name.pid"
  echo "Stopped $name (PID $pid)."
}

online() { adb_ get-state >/dev/null 2>&1; }
booted() { [[ $(adb_ shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]]; }
app_pid() { adb_ shell pidof "$package" 2>/dev/null | tr -d '\r'; }
forwarded() { adb forward --list 2>/dev/null | awk -v s="$serial" -v d="$devtools" '$1 == s && $2 == d {print $3}'; }
webview_ready() { curl -sf http://127.0.0.1:9226/json/list | grep -q '"url": "http://tauri.localhost'; }

# A fresh fixture: the review agent as `claude`, a git demo project and notes.
# The record of an emulator this environment booted survives the reset.
make_fixture() {
  local project="$fixture/projects/tip-splitter" notes="$fixture/notes" socket
  install -d -m 700 "$fixture"
  find "$fixture" -mindepth 1 -maxdepth 1 ! -name 'emulator.*' -exec rm -rf {} +
  (umask 077 && mkdir -p "$fixture"/{bin,home,config/ophio,data,state,cache,run,evidence,projects})
  socket="$fixture/run/ophio/run/owner.sock"
  if ((${#socket} >= 108)); then
    echo "This checkout's path is too long for the companion's socket." >&2
    exit 1
  fi
  install -m 700 "$repo/tools/review/review_claude.py" "$fixture/bin/claude"
  printf '#!/bin/sh\nexit 1\n' >"$fixture/bin/tailscale"
  chmod 700 "$fixture/bin/tailscale"
  # The emulator reaches this loopback address as 10.0.2.2.
  cat >"$fixture/config/ophio/config.toml" <<'TOML'
port = 47112
listen = ["127.0.0.1"]
allow_lan = false
desktop_enabled = false
TOML
  cp -r "$repo/tools/review/demo" "$project"
  find "$project" -name __pycache__ -prune -exec rm -rf {} +
  git -C "$project" init --quiet --initial-branch=main
  git -C "$project" add --all
  git -C "$project" -c user.name="Review fixture" -c user.email=review@example.invalid \
    commit --quiet --message "Start the tip splitter"
  mkdir -p "$notes/Trips"
  printf '# Groceries\n\n- Oat milk\n- Lemons\n- Coffee beans\n- Basil\n' >"$notes/Groceries.md"
  printf '# Reading list\n\n1. The Overstory\n2. Piranesi\n3. A Psalm for the Wild-Built\n' >"$notes/Reading list.md"
  printf '# Lisbon in May\n\n- Alfama and the castle\n- Belém\n- Sintra by train\n\n## To book\n\n- [ ] Tram 28, early\n- [x] Flights\n' >"$notes/Trips/Lisbon.md"
}

# Start the companion on the fixture's state and wait for its owner socket.
run_companion() {
  local socket="$fixture/run/ophio/run/owner.sock"
  rm -f "$socket" # left behind only by a companion that had to be killed
  spawn companion "$fixture/companion.log" "$fixture/companion.log" "${isolated[@]}" "$companion" run
  wait_for "the companion to start" 30 test -S "$socket"
}

start_companion() {
  run_companion
  "${isolated[@]}" "$companion" roots add "$fixture/notes" --label Notes --writable >/dev/null
  "${isolated[@]}" "$companion" projects add "$fixture/projects/tip-splitter" --name "Tip splitter" >/dev/null
}

start_emulator() {
  if online; then
    echo "Using $serial, which was already running; down will leave it running."
  else
    echo "Booting $serial headless…"
    # Boot cold: a saved quick-boot state can bring the phone up with its Wi-Fi
    # off and refusing to turn on, so it never reaches the companion. Read-only,
    # so it can run beside other checks' emulators of the same AVD; `up`
    # installs the app fresh each time, so nothing needs to persist.
    spawn emulator "$fixture/emulator.log" "$fixture/emulator.log" "$(command -v emulator)" \
      -avd buildphone -read-only -no-snapshot -no-window -no-audio -no-boot-anim \
      -gpu swiftshader_indirect -port 5562
  fi
  if ! wait_for "$serial to boot" 300 booted; then
    echo "See target/review-connected/emulator.log." >&2
    exit 1
  fi
  wait_for "the package manager" 60 adb_ shell pm path android
}

apk_stale() {
  [[ -f $apk ]] || return 0
  [[ -n $(find "$repo/client/src" "$repo/client/index.html" "$repo/client/package.json" \
    "$repo/client/src-tauri/src" "$repo/client/src-tauri/plugins" \
    "$repo/client/src-tauri/Cargo.toml" "$repo/client/src-tauri/tauri.conf.json" \
    "$repo/client/src-tauri/capabilities" "$repo/client/src-tauri/gen/android/app/src" \
    "$repo/crates/protocol/src" "$repo/crates/ports/src" -type f -newer "$apk" -print -quit) ]]
}

install_app() {
  if ((rebuild)) || apk_stale; then
    echo "Building the debug APK for x86_64; this takes a few minutes…"
    if ! (cd "$repo/client" && CARGO_TARGET_DIR="$repo/client/src-tauri/target" \
      npm run tauri android build -- --debug --apk --target x86_64) >"$fixture/apk-build.log" 2>&1; then
      echo "The APK build failed; see target/review-connected/apk-build.log." >&2
      exit 1
    fi
  fi
  echo "Installing the app fresh…"
  adb_ uninstall "$package" >/dev/null 2>&1 || true
  adb_ install "$apk" >/dev/null
}

# Start the app, or bring it back to the front if a reviewer left it.
launch_app() {
  adb_ shell am start -n "$package/.MainActivity" >/dev/null
  wait_for "the app to start" 30 app_pid
}

forward_devtools() {
  adb_ forward "$devtools" "localabstract:webview_devtools_remote_$(app_pid)" >/dev/null
  wait_for "the app's WebView" 60 webview_ready
}

# Pair through the real Welcome screen. The offer stays in fixture storage.
# `ophio pair` runs without a terminal, so the owner's confirmation is
# `ophio pair confirm` with the code the phone shows, once the computer has
# printed the same code.
pair() {
  local pid ui code
  (umask 077 && : >"$fixture/pair.txt")
  rm -f "$fixture/pair-code.txt"
  spawn pair "$fixture/pair.txt" "$fixture/pair-status.txt" "${isolated[@]}" "$companion" pair
  wait_for "a pairing offer" 15 grep -q 'ophio://pair?d=' "$fixture/pair.txt"
  echo "Pairing through the app's Welcome screen…"
  OPHIO_FIXTURE="$fixture" OPHIO_EVIDENCE="$fixture/evidence" \
    node "$repo/client/src-tauri/tools/emulator/real-companion.mjs" pair >"$fixture/pair-ui.log" 2>&1 &
  ui=$!
  if ! wait_for "the phone to show its code" 30 test -s "$fixture/pair-code.txt"; then
    kill "$ui" 2>/dev/null || true
    exit 1
  fi
  code=$(<"$fixture/pair-code.txt")
  if ! wait_for "the computer to show the same code" 15 grep -q "pair confirm $code\$" "$fixture/pair.txt" ||
    ! "${isolated[@]}" "$companion" pair confirm "$code" >>"$fixture/pair-status.txt" 2>&1 ||
    ! wait "$ui"; then
    kill "$ui" 2>/dev/null || true
    echo "Pairing through the app failed; see target/review-connected/pair-ui.log." >&2
    exit 1
  fi
  pid=$(running pair) && wait_for "the companion to confirm pairing" 20 gone "$pid"
  rm -f "$fixture/pair.txt" "$fixture/pair.pid" "$fixture/pair-code.txt"
  touch "$fixture/paired"
}

status() {
  local pid app target state
  if pid=$(running companion); then
    [[ $(stat_field "$pid" 0) == T ]] && state=paused || state=running
    echo "Øphio      $state, PID $pid, 127.0.0.1:47112"
  else
    echo "Øphio      stopped"
  fi
  if ! online; then
    echo "Emulator   $serial not running"
  elif pid=$(running emulator); then
    echo "Emulator   $serial, booted here, PID $pid"
  else
    echo "Emulator   $serial, started elsewhere"
  fi
  if online && app=$(app_pid) && [[ -n $app ]]; then
    echo "App        $package, PID $app$([[ -f $fixture/paired ]] && echo ", paired")"
  else
    echo "App        not running"
  fi
  if target=$(forwarded) && [[ -n $target ]]; then
    echo "DevTools   http://127.0.0.1:9226 → $target"
  else
    echo "DevTools   not forwarded"
  fi
}

# Undo what the reviewer tools may have changed on the emulator.
restore_device() {
  adb_ shell settings put system font_scale 1.0
  adb_ shell cmd uimode night no >/dev/null
  adb_ shell settings put system user_rotation 0
  adb_ shell settings put system accelerometer_rotation 1
  adb_ shell cmd connectivity airplane-mode disable
}

up() {
  local pid
  echo "Building the companion and its MCP server if they are stale…"
  (cd "$repo" && CARGO_TARGET_DIR="$repo/target" cargo build --quiet -p companion -p computer-mcp)
  if pid=$(running companion); then
    kill -CONT "$pid" # a failure drill may have paused it
  else
    stop pair
    make_fixture
    start_companion
  fi
  if ! booted; then
    start_emulator
    rm -f "$fixture/paired"
  fi
  if [[ ! -f $fixture/paired ]] || ((rebuild)); then
    stop pair
    rm -f "$fixture/paired"
    install_app
    launch_app
    forward_devtools
    pair
  else
    launch_app
    forward_devtools
  fi
  status
}

down() {
  if [[ -n $(forwarded) ]]; then
    adb_ forward --remove "$devtools"
    echo "Removed the DevTools forward."
  fi
  stop pair
  stop companion
  if online && booted; then
    restore_device
  fi
  if running emulator >/dev/null; then
    stop emulator
  elif online; then
    echo "Left $serial running; this environment did not boot it."
  fi
  rm -f "$fixture/paired"
  status
}

# Failure drills for the reviewer tools, on the recorded companion only.
drill() {
  local pid
  if ! pid=$(running companion); then
    echo "The companion is not running; run up first." >&2
    exit 1
  fi
  case $1 in
    pause)
      kill -STOP "$pid"
      echo "Paused the companion (PID $pid)."
      ;;
    resume)
      kill -CONT "$pid"
      echo "Resumed the companion (PID $pid)."
      ;;
    restart)
      stop companion >/dev/null
      run_companion
      echo "Restarted the companion: PID $pid stopped, PID $(running companion) started."
      ;;
  esac
}

# The reviewer's MCP servers, resolved for this checkout and environment.
mcp_config() {
  python3 - "$repo/tools/review/mcp-servers.json" "$repo" "$(realpath -m -- "${1:-$fixture/shots}")" \
    "$(command -v node)" "$(command -v npx)" "$PATH" <<'PY'
import json, sys
template, repo, shots, node, npx, path = sys.argv[1:]
values = {"{repo}": repo, "{shots}": shots, "{node}": node, "{npx}": npx, "{path}": path}
def resolve(value):
    if isinstance(value, dict):
        return {key: resolve(item) for key, item in value.items()}
    if isinstance(value, list):
        return [resolve(item) for item in value]
    for placeholder, replacement in values.items():
        value = value.replace(placeholder, replacement)
    return value
with open(template) as file:
    print(json.dumps(resolve(json.load(file)), indent=2))
PY
}

rebuild=0
case "${1:-}:${2:-}" in
  up:--rebuild) rebuild=1 ;;
  up: | status: | down: | companion:pause | companion:resume | companion:restart | mcp-config:*) ;;
  *) usage ;;
esac
require_environment
case $1 in
  up) up ;;
  status) status ;;
  down) down ;;
  companion) drill "$2" ;;
  mcp-config) mcp_config "${2:-}" ;;
esac
