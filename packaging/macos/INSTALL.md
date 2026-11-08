# Install Øphio on a Mac

Øphio on a Mac is in development and not released. This release has no Mac
download, and Macs are not supported. This guide is for an archive built from
the source, for Macs with Apple silicon on macOS 13 or later. It has been
tested on one Apple silicon Mac only.

Unpack the archive, enter the folder it makes, and run the installer as your
ordinary user:

```sh
tar -xzf ophio-*-aarch64-macos.tar.gz
cd ophio-*-aarch64-macos
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
writes a launchd agent to `~/Library/LaunchAgents/app.ophio.plist`.
`ophio service uninstall` removes it again.

## Watching and controlling the screen

Turn on desktop sharing with `ophio desktop enable`. macOS then asks you, at the
Mac itself, to allow Øphio under System Settings › Privacy & Security in two
places: Screen & System Audio Recording, to see the screen, and Accessibility,
to control it. Restart Øphio after giving them. Until this build is signed with a
Developer ID, macOS may ask again after each update.

While a phone is in control, an Øphio item in the menu bar has a Stop computer
control command. Control stays off whenever that item can't be shown. Øphio can
lock this Mac from the phone but can't unlock it: only the login screen can.

## How chats are limited on a Mac

A limited chat runs inside macOS's own sandbox. Its agent can read and change
only the folders you gave it, and reach only the internet you allowed. Three
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

## What isn't on a Mac yet

- Voice control, which reads the screen through accessibility.
- Listing and acting on other apps' windows.
- Pausing an agent you started outside Øphio to hand it over.

Uninstall with `ophio service uninstall`, then delete `~/.local/opt/ophio` and
the `ophio` link in `~/.local/bin`. `ophio uninstall-data` deletes settings,
paired devices and data.
