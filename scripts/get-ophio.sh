#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Installs Øphio on a Linux computer or a Mac with one command:
#
#   curl -fsSL https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.sh | sh
#
# It downloads the newest release for this computer from Øphio's GitHub
# releases, checks it against the release's published SHA256SUMS, and runs the
# installer inside the download, the same `install.sh` the install guides use.
# On a first install on Linux, that installer then starts `ophio setup`, which
# starts Øphio, connects your phone and checks what works; on a Mac it ends
# with the Mac guide's steps to start Øphio and pair. Nothing is installed if the
# checksum does not match. It installs for you alone, in your home folder, and
# never uses sudo.
#
# Options, after `sh -s --` when piped:
#   --version 0.3.0   install that release instead of the newest
#   --no-setup        don't start setup after a first install
#
# OPHIO_RELEASES changes where releases are downloaded from (for mirrors and
# tests); it defaults to https://github.com/ophio-app/ophio/releases.
#
# Everything is inside main(), which runs on the last line, so a download that
# breaks off part way runs nothing.

set -eu

say() { printf '%s\n' "$*"; }
fail() {
  printf 'Øphio was not installed: %s\n' "$*" >&2
  exit 1
}

# The guides, for messages that send people to them.
GUIDES=https://github.com/ophio-app/ophio/blob/main

usage() {
  say "Usage: get-ophio.sh [--version VERSION] [--no-setup]"
  say "Downloads Øphio for this computer, checks it against the release's SHA256SUMS and installs it."
}

# Which download this computer needs, as it is named in a release.
target() {
  os=$(uname -s)
  machine=$(uname -m)
  case "$os" in
    Linux)
      case "$machine" in
        x86_64 | amd64) say x86_64-linux ;;
        *) fail "there is no Øphio download for Linux on $machine yet, only for x86_64 (Intel and AMD) computers. BUILDING.md in the source says how to build it yourself." ;;
      esac
      ;;
    Darwin)
      # A terminal running under Rosetta reports x86_64 on Apple silicon.
      if [ "$machine" = arm64 ] || [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
        say aarch64-macos
      else
        fail "Øphio's Mac download needs a Mac with Apple silicon (M1 or later). Intel Macs aren't supported."
      fi
      ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
      fail "on Windows, run this in PowerShell instead: irm https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.ps1 | iex"
      ;;
    *) fail "there is no Øphio download for $os. Øphio runs on Linux, Macs and Windows." ;;
  esac
}

# fetch URL FILE [progress]: downloads URL to FILE, failing on any HTTP error.
fetch() {
  if [ -n "$downloader_curl" ]; then
    if [ "${3:-}" = progress ] && [ -t 2 ]; then
      # shellcheck disable=SC2086 # $https_only is deliberately split into words.
      curl -fL --retry 3 $https_only --progress-bar -o "$2" "$1"
    else
      # shellcheck disable=SC2086
      curl -fsSL --retry 3 $https_only -o "$2" "$1"
    fi
  else
    # shellcheck disable=SC2086
    wget -q $https_only -O "$2" "$1"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

# Why a program from the download didn't run here, in words someone can act on.
explain_not_running() {
  problem=$1
  case "$problem" in
    *GLIBC_* | *glibc*)
      have=$(getconf GNU_LIBC_VERSION 2>/dev/null || say "an unknown C library")
      say "Øphio for Linux needs glibc 2.39 or later, as in Ubuntu 24.04 or newer. This computer has $have."
      ;;
    *libpipewire* | *libxkbcommon*)
      say "This computer is missing a library Øphio needs. Install your distribution's PipeWire client library and libxkbcommon (on Ubuntu, libpipewire-0.3-0t64 and libxkbcommon0; on Fedora, pipewire-libs and libxkbcommon; on Arch, libpipewire and libxkbcommon), then run this again."
      ;;
    *) say "The installer's own check of the program said: $problem" ;;
  esac
}

main() {
  version=${OPHIO_VERSION:-}
  setup=
  while [ $# -gt 0 ]; do
    case "$1" in
      --version)
        [ $# -ge 2 ] || fail "--version needs a version number, such as 0.3.0."
        version=$2
        shift 2
        ;;
      --version=*) version=${1#--version=}; shift ;;
      --no-setup) setup=--no-setup; shift ;;
      -h | --help) usage; exit 0 ;;
      *) usage >&2; fail "unknown option $1." ;;
    esac
  done
  version=${version#v}
  case "$version" in
    *[!0-9A-Za-z.+-]*) fail "$version isn't a version number, such as 0.3.0." ;;
  esac

  if [ "$(id -u)" = 0 ]; then
    fail "run this as your ordinary user, not as root or with sudo. Øphio installs for one person, in their home folder, and the agents it starts run as that person."
  fi

  want=$(target)

  if command -v curl >/dev/null 2>&1; then
    downloader_curl=1
  elif command -v wget >/dev/null 2>&1; then
    downloader_curl=
  else
    fail "this needs curl or wget to download Øphio. On Ubuntu: sudo apt install curl"
  fi
  if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "this needs sha256sum or shasum to check the download."
  fi
  command -v tar >/dev/null 2>&1 || fail "this needs tar to unpack the download."

  releases=${OPHIO_RELEASES:-https://github.com/ophio-app/ophio/releases}
  releases=${releases%/}
  https_only=
  case "$releases" in
    https://*)
      if [ -n "$downloader_curl" ]; then
        https_only="--proto =https --proto-redir =https"
      else
        https_only=--https-only
      fi
      ;;
  esac

  work=$(mktemp -d "${TMPDIR:-/tmp}/ophio-get.XXXXXX") || fail "couldn't make a temporary folder."
  # shellcheck disable=SC2064 # $work is fixed now, and must be removed even if it changes.
  trap "rm -rf '$work'" EXIT
  trap 'exit 130' INT TERM HUP

  if [ -n "$version" ]; then
    where="$releases/download/v$version"
    which_release="release $version"
  else
    where="$releases/latest/download"
    which_release="newest release"
  fi
  say "Finding Øphio's $which_release…"
  fetch "$where/SHA256SUMS" "$work/SHA256SUMS" ||
    fail "couldn't download the $which_release's SHA256SUMS from $where. Check this computer's internet connection, or that the release exists."

  # The SHA256SUMS line for this computer's download names the version too.
  pattern="^[0-9a-f]{64}  ophio-[0-9][0-9A-Za-z.+-]*-$want\.tar\.gz\$"
  count=$(grep -cE "$pattern" "$work/SHA256SUMS" || true)
  if [ "$count" = 0 ]; then
    found=$(sed -n 's/^[0-9a-f]\{64\}  ophio-\([0-9][0-9A-Za-z.+-]*\)-source\.tar\.gz$/\1/p' "$work/SHA256SUMS" | head -n 1)
    case "$want" in
      *macos) kind="a Mac with Apple silicon" ;;
      *) kind="Linux on x86_64" ;;
    esac
    fail "Øphio's ${found:+$found, the }$which_release, has no download for $kind. The release page lists what each release has: $releases"
  fi
  if [ "$count" != 1 ]; then
    fail "the release's SHA256SUMS lists more than one download for this computer."
  fi
  line=$(grep -E "$pattern" "$work/SHA256SUMS")
  expected=${line%%  *}
  name=${line#*  }
  folder=${name%.tar.gz}
  found=${folder#ophio-}
  found=${found%-"$want"}

  if [ -x "$HOME/.local/bin/ophio" ] &&
    [ "$("$HOME/.local/bin/ophio" --version 2>/dev/null || true)" = "ophio $found" ]; then
    say "Øphio $found is already installed, so there is nothing to download."
    say "To run setup again, run: ~/.local/bin/ophio setup"
    exit 0
  fi

  say "Downloading $name…"
  fetch "$releases/download/v$found/$name" "$work/$name" progress ||
    fail "couldn't download $name from $releases/download/v$found."

  actual=$(sha256 "$work/$name")
  if [ "$actual" != "$expected" ]; then
    fail "$name doesn't match the release's SHA256SUMS (expected $expected, got $actual). It may have been damaged or changed on the way. Nothing was installed. Try again later, or download it from the release page and check it by hand."
  fi
  say "Checked $name against the release's SHA256SUMS."

  mkdir "$work/unpacked"
  tar -xzf "$work/$name" -C "$work/unpacked" || fail "couldn't unpack $name."
  unpacked="$work/unpacked/$folder"
  if [ ! -f "$unpacked/install.sh" ] || [ ! -x "$unpacked/ophio" ]; then
    fail "$name doesn't contain the installer it should."
  fi
  # The installer checks the program too, but this says why in plain words.
  if ! problem=$("$unpacked/ophio" --version 2>&1); then
    explain_not_running "$problem" >&2
    fail "Øphio $found can't run on this computer."
  fi

  # The Mac guide starts Øphio with `ophio service install`; setup's walk has
  # not been tried on a Mac, so the Mac install ends with the guide's steps.
  case "$want" in
    *macos) setup=--no-setup ;;
  esac

  say "Installing Øphio $found for $(id -un)…"
  # Piped into sh, standard input is the script, not the keyboard. Setup needs
  # the keyboard, so give the installer the terminal when there is one.
  if [ ! -t 0 ] && (: </dev/tty) 2>/dev/null; then
    sh "$unpacked/install.sh" $setup </dev/tty
  else
    sh "$unpacked/install.sh" $setup
  fi

  case ":${PATH:-}:" in
    *":$HOME/.local/bin:"*) ;;
    *)
      case "${SHELL:-}" in
        */zsh) rc="$HOME/.zshrc" ;;
        */bash) rc="$HOME/.bashrc" ;;
        *) rc= ;;
      esac
      say ""
      say "To type ophio as a command, add ~/.local/bin to your PATH."
      if [ -n "$rc" ]; then
        say "Run this once, then open a new terminal:"
        say ""
        say "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc"
      else
        say "Add this line to your shell's startup file, then open a new terminal:"
        say ""
        say "  export PATH=\"\$HOME/.local/bin:\$PATH\""
      fi
      say ""
      say "Until then, run it as ~/.local/bin/ophio."
      ;;
  esac
  case "$want" in
    *macos)
      say ""
      say "Next, start Øphio now and whenever you log in, then pair your phone:"
      say ""
      say "  ~/.local/bin/ophio service install"
      say "  ~/.local/bin/ophio pair"
      say ""
      say "The Mac guide, for what's different on a Mac: $GUIDES/packaging/macos/INSTALL.md"
      ;;
    *) say "The Linux guide, for anything setup reports: $GUIDES/packaging/linux/INSTALL.md" ;;
  esac
}

main "$@"
