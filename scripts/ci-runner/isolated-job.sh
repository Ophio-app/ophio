#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Host-side supervisor. Configuration is supplied by the user unit's environment.
set -euo pipefail
set +x
umask 077

: "${ISOLATED_REPOSITORY:?set OWNER/REPO}"
: "${ISOLATED_TOKEN_FILE:?set the host token file}"
: "${ISOLATED_RUNNER_INSTALL:?set the read-only, unregistered install}"
: "${ISOLATED_ROOTFS:?set the provisioned Ubuntu root}"
: "${ISOLATED_STATE_DIR:?set the private host state folder}"
ISOLATED_API_URL=${ISOLATED_API_URL:-https://api.github.com}
ISOLATED_RUNNER_GROUP_ID=${ISOLATED_RUNNER_GROUP_ID:-1}
ISOLATED_TIMEOUT_SECONDS=${ISOLATED_TIMEOUT_SECONDS:-7200}
ISOLATED_BWRAP=${ISOLATED_BWRAP:-/usr/bin/bwrap}
export ISOLATED_API_URL ISOLATED_RUNNER_GROUP_ID

fail() { printf 'isolated-job: %s\n' "$*" >&2; exit 1; }
[[ "$ISOLATED_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]] || fail 'invalid timeout'
[[ "$ISOLATED_RUNNER_GROUP_ID" =~ ^[1-9][0-9]*$ ]] || fail 'invalid runner group'
[[ "$ISOLATED_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail 'invalid repository'
for path in "$ISOLATED_RUNNER_INSTALL" "$ISOLATED_ROOTFS" "$ISOLATED_STATE_DIR" "$ISOLATED_TOKEN_FILE"; do
    [[ "$path" == /* ]] || fail 'configuration paths must be absolute'
done
ISOLATED_RUNNER_INSTALL=$(realpath -e -- "$ISOLATED_RUNNER_INSTALL")
ISOLATED_ROOTFS=$(realpath -e -- "$ISOLATED_ROOTFS")
ISOLATED_STATE_DIR=$(realpath -m -- "$ISOLATED_STATE_DIR")
ISOLATED_TOKEN_FILE=$(realpath -e -- "$ISOLATED_TOKEN_FILE")
export ISOLATED_TOKEN_FILE
paths=("$ISOLATED_ROOTFS" "$ISOLATED_RUNNER_INSTALL" "$ISOLATED_STATE_DIR")
for outer in "${paths[@]}"; do
    [[ "$ISOLATED_TOKEN_FILE/" != "$outer/"* ]] || fail 'token must be outside all runner folders'
    for inner in "${paths[@]}"; do
        [[ "$inner" == "$outer" || "$inner/" != "$outer/"* ]] || fail 'runner folders must not overlap'
    done
done
[[ "$ISOLATED_ROOTFS" != "$ISOLATED_RUNNER_INSTALL" &&
   "$ISOLATED_ROOTFS" != "$ISOLATED_STATE_DIR" &&
   "$ISOLATED_RUNNER_INSTALL" != "$ISOLATED_STATE_DIR" ]] || fail 'runner folders must differ'
[[ -x "$ISOLATED_RUNNER_INSTALL/run.sh" ]] || fail 'missing runner install'
[[ -x "$ISOLATED_ROOTFS/bin/sh" ]] || fail 'missing Ubuntu root'
[[ -z "$(find "$ISOLATED_RUNNER_INSTALL" ! -type l -perm /222 -print -quit)" ]] || fail 'runner install must be read-only'
for name in .runner .credentials .credentials_rsaparams .env .path _work home; do
    [[ ! -e "$ISOLATED_RUNNER_INSTALL/$name" && ! -L "$ISOLATED_RUNNER_INSTALL/$name" ]] || fail 'runner install must be unregistered and unused'
done

mkdir -p -- "$ISOLATED_STATE_DIR/jobs"
chmod 700 -- "$ISOLATED_STATE_DIR" "$ISOLATED_STATE_DIR/jobs"
attempt=$(mktemp -d "$ISOLATED_STATE_DIR/jobs/job.XXXXXXXXXXXX")
job_id=${attempt##*/}
job_pid=
started=$SECONDS
cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if [[ -n "$job_pid" ]]; then
        # timeout leads its process group; bwrap's PID namespace dies with it.
        kill -KILL -- "-$job_pid" 2>/dev/null || kill -KILL "$job_pid" 2>/dev/null || true
        wait "$job_pid" 2>/dev/null || true
    fi
    # A job can remove permissions from its own directories.
    chmod -R u+rwX -- "$attempt" || status=1
    rm -rf --one-file-system -- "$attempt" || status=1
    printf 'job_id=%s exit=%s duration_seconds=%s\n' "$job_id" "$status" "$((SECONDS - started))" >> "$ISOLATED_STATE_DIR/jobs.log"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -- "$attempt/runner"
cp -a --no-preserve=ownership --reflink=auto -- "$ISOLATED_RUNNER_INSTALL/." "$attempt/runner/"
chmod -R u+w -- "$attempt/runner"
mkdir -- "$attempt/runner/home"

# Only this host process reads the API token. Neither errors nor responses are logged.
python3 - "$attempt/jitconfig" "$job_id" <<'PY'
import json
import os
from pathlib import Path
import stat
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


try:
    token_path = Path(os.environ["ISOLATED_TOKEN_FILE"])
    info = token_path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077 or info.st_uid != os.getuid():
        raise ValueError("token permissions")
    token = token_path.read_text(encoding="utf-8").strip()
    if not token or any(c.isspace() for c in token):
        raise ValueError("token format")
    base = os.environ["ISOLATED_API_URL"].rstrip("/")
    url = urllib.parse.urlsplit(base)
    if (url.scheme != "https" and not (
        url.scheme == "http" and url.hostname in {"127.0.0.1", "::1", "localhost"}
    )) or url.username or url.password or url.query or url.fragment:
        raise ValueError("API URL")
    body = json.dumps({
        "name": "ophio-isolated-" + sys.argv[2],
        "runner_group_id": int(os.environ["ISOLATED_RUNNER_GROUP_ID"]),
        "labels": ["self-hosted", "ophio-isolated"],
        "work_folder": "_work",
    }).encode()
    request = urllib.request.Request(
        base + "/repos/" + os.environ["ISOLATED_REPOSITORY"] + "/actions/runners/generate-jitconfig",
        data=body, method="POST", headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2026-03-10",
            "Content-Type": "application/json",
        },
    )
    # Ignore host proxy variables and refuse redirects carrying authorization.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(request, timeout=30) as response:
        if response.status != 201:
            raise ValueError("API status")
        data = json.loads(response.read(1024 * 1024))
    config = data["encoded_jit_config"]
    if not isinstance(config, str) or not config or any(c.isspace() for c in config):
        raise ValueError("JIT config")
    labels = {label["name"] for label in data["runner"]["labels"]}
    if labels != {"self-hosted", "ophio-isolated"}:
        raise ValueError("runner labels")
    Path(sys.argv[1]).write_text(config, encoding="utf-8")
except Exception:
    print("isolated-job: JIT configuration failed", file=sys.stderr)
    sys.exit(1)
PY

# The trusted root contains an empty /ci-shared mount point. Recreate its
# read-only top level without that path, so even the mount point is absent.
root_args=(--tmpfs /)
shopt -s dotglob nullglob
for entry in "$ISOLATED_ROOTFS"/*; do
    name=${entry##*/}
    [[ "$name" != ci-shared ]] || continue
    if [[ -L "$entry" ]]; then
        root_args+=(--symlink "$(readlink -- "$entry")" "/$name")
    else
        root_args+=(--ro-bind "$entry" "/$name")
    fi
done

timeout --kill-after=10s "${ISOLATED_TIMEOUT_SECONDS}s" "$ISOLATED_BWRAP" \
    "${root_args[@]}" \
    --ro-bind /etc/resolv.conf /etc/resolv.conf \
    --dev /dev --proc /proc \
    --tmpfs /tmp --tmpfs /run --tmpfs /var/tmp \
    --bind "$attempt/runner" /runner \
    --ro-bind "$attempt/jitconfig" /run/jitconfig \
    --clearenv \
    --setenv HOME /runner/home \
    --setenv USER runner \
    --setenv PATH /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin \
    --setenv LANG C.UTF-8 \
    --unshare-user --uid 1000 --gid 1000 \
    --unshare-pid --unshare-ipc --unshare-uts --hostname ophio-ci-isolated \
    --die-with-parent --new-session \
    --remount-ro / --chdir /runner \
    /bin/sh -c 'config=$(cat /run/jitconfig) && exec /runner/run.sh --jitconfig "$config"' &
job_pid=$!
status=0
wait "$job_pid" || status=$?
job_pid=
exit "$status"
