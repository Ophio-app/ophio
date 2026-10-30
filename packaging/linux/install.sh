#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Installs Øphio from this extracted archive for the current user, or updates
# an earlier install. `ophio install` does the work: the programs go in
# ~/.local/opt/ophio/bin and the ophio command links to them from ~/.local/bin.
# A running Øphio is restarted on the new version once no task is running, and
# goes back to the earlier one if it doesn't come back working. Only that one
# earlier version is kept; nothing else the install or an update made is left
# behind. Settings and data are left alone.
set -eu

archive=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
if [ ! -f "$archive/ophio" ]; then
  echo "ophio is missing beside install.sh. Run install.sh from the extracted archive." >&2
  exit 1
fi
exec "$archive/ophio" install "$@"
