# Install Øphio on a Mac

The Mac download is experimental. It is not a beta support promise: Øphio's
beta is for Android phones and Linux computers. It is for Macs with Apple
silicon (M1 or later), and needs macOS 13 or later, but it has been tested
on one Mac, running macOS 27.2, only.

The programs are not signed with an Apple Developer ID and are not
notarized by Apple. They carry only an ad hoc signature, which needs no
Apple account, so macOS can't tell who made them. The steps below work
without notarization, but follow them in order: macOS blocks a program
from a browser download in a way that is hard to undo.

## One command

This command does the next two sections for you: it downloads the newest
release with `curl`, checks it against the release's `SHA256SUMS`, unpacks
it and runs the installer. Run it in Terminal as your ordinary user:

```sh
curl -fsSL https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.sh | sh
```

It is [scripts/get-ophio.sh](../../scripts/get-ophio.sh) in the source. It
ends by printing `ophio service install` and `ophio pair`, the last steps of
[Install](#install) below.

## Download and check

To do it by hand, in Terminal, download the archive and `SHA256SUMS` from the release page,
then check the archive against the published hash:

```sh
curl -LO https://github.com/ophio-app/ophio/releases/download/v0.3.0/ophio-0.3.0-aarch64-macos.tar.gz
curl -LO https://github.com/ophio-app/ophio/releases/download/v0.3.0/SHA256SUMS
grep aarch64-macos SHA256SUMS | shasum -a 256 -c
```

The last command must print `OK`. A download made with `curl` is not marked
as coming from the internet, so macOS runs the programs in it.

**If you downloaded the archive with a browser**, don't double-click it.
Remove the browser's mark from the archive first, and only then unpack it:

```sh
xattr -d com.apple.quarantine ophio-0.3.0-aarch64-macos.tar.gz
```

If a program from a marked archive has already been started, macOS shows
"“ophio” Not Opened" and leaves that folder blocked: later starts from it
wait without a message, even after the mark is removed. Click **Done** in
that message, delete the folder, and unpack again into a new folder from the
cleared archive.

## Install

Unpack the archive, enter the folder it makes, and run the installer as your
ordinary user:

```sh
tar -xzf ophio-0.3.0-aarch64-macos.tar.gz
cd ophio-0.3.0-aarch64-macos
sh install.sh
```

`install.sh` runs `./ophio install`, which puts `ophio` and `computer-mcp` in
`~/.local/opt/ophio/bin` and links the `ophio` command to them from
`~/.local/bin`. It changes nothing else. If the installer says `~/.local/bin`
is not on your PATH, add this line to `~/.zshrc` and open a new terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Run `ophio service install` to start Øphio now and whenever you log in. It
writes a launchd agent to `~/Library/LaunchAgents/app.ophio.plist`. Then run
`ophio pair` and scan the code with the Øphio app on your phone.

## Updating

Download and check the new archive as above, and give it to `ophio update`
without unpacking it, using the name of the file you downloaded:

```sh
ophio update ophio-0.3.1-aarch64-macos.tar.gz
```

This works for a browser download too: what `ophio update` installs carries
no browser mark. Øphio waits until it has been idle for a minute, restarts on
the new version, checks that it works, and goes back to the earlier version
if it doesn't. It keeps one earlier version; `ophio update --rollback` goes
back to it.

## Watching and controlling the screen

Turn on desktop sharing with `ophio desktop enable`. macOS then asks you, at the
Mac itself, to allow Øphio under System Settings › Privacy & Security in two
places: Screen & System Audio Recording, to see the screen, and Accessibility,
to control it. Restart Øphio after giving them.

Because the programs are signed ad hoc, each update is a new program to
macOS: it forgets both permissions, and you give them again after every
update. Screen watching and control have not yet been tested with this
download.

While a phone is in control, an Øphio item in the menu bar has a Stop computer
control command. Control stays off whenever that item can't be shown. Øphio can
lock this Mac from the phone but can't unlock it: only the login screen can.

## How chats are limited on a Mac

A limited chat runs inside macOS's own sandbox. Its agent can read and change
only the folders you gave it, and reach only the internet you allowed. Four
things differ from Linux:

- A Mac can't change what a running program may open. Changing a chat's folders
  ends the agent's current turn, and the next turn starts with the new access.
  Turning the internet on or off applies at once.
- A Mac has no separate network for each program. An agent held to its folders
  can start a local server, and the phone can open it as a preview, but the agent
  can't connect to it or to anything else on this Mac unless you allow "your own
  network" for that chat.
- A file's name and size can be seen from inside by anyone who already knows its
  exact path. Its contents and the contents of folders can't.
- A limited agent can see the command lines of other programs you run, so
  never put a password or key in a command's arguments.

## What isn't on a Mac yet

- Pausing an agent you started outside Øphio to hand it over.
- Hard limits on a chat's disk use, and containment of a program that leaves
  its process group on purpose. Stopping a chat ends the processes Øphio
  started for it.

## Uninstalling

Run `ophio service uninstall`, then delete `~/.local/opt/ophio` and the `ophio`
link in `~/.local/bin`. `ophio uninstall-data` shows what it would delete;
`ophio uninstall-data --yes` deletes Øphio's settings, paired devices and data
in `~/Library/Application Support/ophio`. Files you received from your phone
stay in `~/Downloads/ophio`, and agents' own sign-ins are kept.
