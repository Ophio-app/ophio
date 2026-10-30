# Install Øphio on Linux

This archive targets x86_64 GNU/Linux with glibc 2.39 or later (Ubuntu 24.04
baseline). Packaging rejects executables requiring newer glibc symbols. PipeWire and the applicable
Wayland or X11 runtime libraries must be installed by your distribution.
If `ophio` reports that `libpipewire-0.3.so.0` or `libxkbcommon.so.0` cannot
be opened, install your distribution's PipeWire client library and
libxkbcommon (on Ubuntu, `libpipewire-0.3-0t64` and `libxkbcommon0`; on Arch,
`libpipewire` and `libxkbcommon`).

Archives built from earlier revisions of this version were installed, used
and uninstalled by a new user on GNOME 46 on Ubuntu 24.04, and on KDE
Plasma 6.7 and Hyprland 0.56 on Arch Linux, all in x86_64 virtual machines
with software rendering. Later changes are covered by automated tests, not by
another such install. Other distributions and desktops have not been tested.

Extract the archive and, from its directory, run the installer as your
ordinary user:

```sh
sh install.sh
```

`install.sh` runs `./ophio install`, which you can also run yourself. It puts
`ophio`, `computer-mcp` and `ophio-sandbox-shim` in `~/.local/opt/ophio/bin` and links the `ophio` command to
them from `~/.local/bin`. A download that carries the phone app, `ophio.apk`,
puts it in `~/.local/opt/ophio/app`, where `ophio setup` installs it from and
paired phones update themselves from. Øphio's icon goes in
`~/.local/share/icons/hicolor`, for its notifications. It changes nothing
else. Keep LICENSE, NOTICE, THIRD-PARTY-NOTICES.txt and sbom.cdx.json with
your copy; the phone app carries its own notices, under Settings › Versions.
Once it is installed, you can delete the archive and the folder you extracted.

If the installer says `~/.local/bin` is not on your PATH, `ophio` is not yet a
command you can type. Ubuntu adds that folder when you next log in; Arch does
not. Add this line to your shell's startup file (for example `~/.bashrc`) and
open a new terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Until then, run `~/.local/opt/ophio/bin/ophio` in place of `ophio`.

## Chat access controls

Chat can keep a conversation to one project and off the internet. Øphio does
that by running the conversation inside bubblewrap, a small sandbox program
that comes from your distribution. It needs:

- bubblewrap 0.9 or newer at `/usr/bin/bwrap` (the `bubblewrap` package; a
  copy elsewhere on `PATH` is not used);
- `ophio-sandbox-shim` beside `ophio`, which the download already includes;
- permission for bubblewrap to start a sandbox as your ordinary user.

Check the last one as the user who runs Øphio:

```sh
/usr/bin/bwrap --unshare-user --unshare-pid --unshare-net --ro-bind / / /usr/bin/true
```

It should finish without printing anything. Until it does, the phone hides
Chat’s access controls and no limited chat can run; tasks and ordinary chats
still work. Øphio never runs a limited chat without its sandbox.

**Ubuntu 24.04 and later** can stop bubblewrap with AppArmor, with an error
such as `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`.

If `/etc/apparmor.d/bwrap` already exists, do not replace it: add the line
`userns,` inside the existing profile, keep its other rules, then reload it
with `sudo apparmor_parser -r /etc/apparmor.d/bwrap`.
If no profile for `/usr/bin/bwrap` exists, these commands create one without
switching AppArmor off. The new profile lets bubblewrap, and only bubblewrap,
create the user namespaces that Ubuntu's setting otherwise restricts.
The commands refuse to overwrite an existing file:

```sh
sudo sh -c 'set -C; cat > /etc/apparmor.d/bwrap && apparmor_parser -r /etc/apparmor.d/bwrap' <<'EOF'
abi <abi/4.0>,
include <tunables/global>

profile bwrap /usr/bin/bwrap flags=(unconfined) {
  userns,

  include if exists <local/bwrap>
}
EOF
```

Run the bubblewrap check above again after these commands. If it still fails,
another restriction may be responsible.

On any system, a namespace error can also mean user namespaces are switched
off or the service that runs Øphio forbids them; ask whoever manages the
computer. A check that passes in a terminal does not prove a more restricted
service can do the same.

Øphio checks bubblewrap once when it starts, so restart it after any fix.
If you installed its service with `ophio service install`, run:

```sh
systemctl --user restart ophio
```

If you started `ophio run` in a terminal, stop it with Ctrl+C and run it again.
If your desktop starts it through its own autostart instead of the service,
log out and back in. Reconnect the phone after Øphio restarts.

If the shim is missing or not executable, reinstall from the complete
download. For a source build, run
`cargo build --locked --release -p companion -p computer-mcp -p sandbox`
and keep all three executables together in `target/release` (or your
`CARGO_TARGET_DIR`’s `release` directory). Debug-profile workspace tests do not
supply a missing release shim. The source tree’s `crates/sandbox/README.md`
describes what the sandbox isolates and how it is verified.

## Setting up

On a computer that has not run Øphio before, the installer, run in a
terminal, ends by starting `ophio setup`. Run it yourself from a terminal in
your graphical login session otherwise. It goes through the rest one step at a
time: starting Øphio at every login, how your phone reaches this computer,
Claude Code and Codex, installing the phone app and pairing it, extras such as
desktop access and voice, and a check that everything works. Any step can be
left for later, and `ophio setup` carries on where it was left. Once an agent
is connected, it can do most steps for you as a task you watch and approve;
pairing, what a device may do and signing in stay with you. `ophio --help`
lists every command.

The sections below do the same by hand.

## Reaching this computer from your phone

Unless this computer is on Tailscale, Øphio accepts connections only from this
computer itself. Your phone reaches it in one of two ways:

- **Tailscale.** Run Tailscale on this computer and on your phone, signed in to
  the same tailnet. Øphio finds this computer's Tailscale address when it
  starts, and the pairing code offers it.
- **Your local network.** On a network you trust, add this computer's local
  address to `~/.config/ophio/config.toml` (create the file if it is missing).
  For example:

  ```toml
  allow_lan = true
  listen = ["192.168.1.20"]
  ```

  `ip -brief address` shows this computer's addresses. Use the address itself,
  not `0.0.0.0`: a pairing code can only offer an address the phone can dial.
  Anyone on that network can then reach Øphio's port, though pairing still
  needs the code both screens show.

Øphio looks up its addresses when it starts: after setting up either one, or
after this computer's address changes, restart Øphio. If a pairing code offers
only this computer's own address, `ophio pair` says so.

## Pairing and desktop access

Start Øphio with `ophio run` as your ordinary user in your graphical login
session. Desktop sharing starts disabled. In another terminal, use `ophio pair` to
pair the phone and `ophio desktop enable` to opt into capture and input.
When the phone asks to pair, `ophio pair` shows the phone's name and a code.
Answer `y` only if the phone shows the same code; without a terminal, confirm
with `ophio pair confirm <code>`.
Use `ophio doctor` to inspect the actual capabilities and remedies.

While your phone or an agent controls the computer, Øphio shows a desktop
notification: clicking or dismissing it stops all computer control. GNOME and
Plasma have a notification service built in. On Hyprland and other
compositors without one, run a notification service that supports actions,
such as mako or dunst; until then `ophio doctor` reports Input and Computer
use as not ready.

For optional automatic startup, stop the Øphio you started by hand, then run
from a terminal inside your desktop session (not over SSH or from a text
console):

```sh
ophio service install
ophio service status
```

The installer writes one managed systemd user unit, `ophio.service`, for that
executable and starts it. When the user manager's `graphical-session.target`
is active, the unit starts with the desktop session and stops with it. Your
desktop must supply its display environment to the user service manager.

If that target is inactive or cannot be checked, the installer uses
`default.target` and prints a note: desktop viewing and control will be
unavailable when it starts this way. If you ran it outside your desktop
session, run `ophio service install` again from a terminal inside it.
Otherwise your desktop does not start `graphical-session.target`: remove this
automatic startup with `ophio service uninstall` and add `ophio run` to the
desktop's own autostart, or start it manually in a graphical terminal. On
Hyprland, for example, add this line to `~/.config/hypr/hyprland.conf`. The
full path works even when your PATH is only set for terminals:

```
exec-once = ~/.local/bin/ophio run
```

Re-running `service install` replaces a managed unit, removes its old startup
links, and restarts Øphio. It refuses to replace an unmanaged unit.
Terminals and agents retain the ordinary user's session privileges; `sudo`
still uses the system's normal authentication. No root service or desktop
permission bypass is included. The packaging script never installs or starts
the unit. `ophio desktop disable` ends sharing and forgets saved portal
consent, so enabling portal sharing again asks for consent on the desktop.

## Updating

Download the new archive and give it to the installed Øphio:

```sh
ophio update ~/Downloads/ophio-1.2.4-x86_64-linux.tar.gz
```

Running the extracted archive's `install.sh` or `./ophio install` does the
same. Øphio:

1. Unpacks the download into `~/.local/opt/ophio/staging`, within size limits,
   and starts the new `ophio` once to check that it runs here.
2. Waits until nothing has run for a minute: no task working, agent running
   or approval waiting. Tasks in Next up don't hold it; they stay in place
   through the restart. It gives up after 48 hours. It waits in the
   background, so you can close the terminal; `ophio update` shows how it is
   going, and `ophio update --cancel` stops it. `--now` doesn't wait, and stops running
   tasks as a restart does.
3. Stops Øphio, swaps in the new programs and phone app, and starts it again.
   If it doesn't answer within 90 seconds, keep running for 15 more and
   listen where it listened before, the previous programs go back and start.
4. Removes what it unpacked and anything an interrupted update left behind.

Settings, paired devices and history stay as they are. Øphio keeps exactly one
earlier version, in `~/.local/opt/ophio/previous`, and `ophio update --rollback`
goes back to it; an install takes up about twice its programs' size and no
more, however often you update. `ophio update` on its own shows the installed
version, the one kept to go back to, how the last update went and how much
space the install uses. Each update's steps are in
`~/.local/opt/ophio/update.log`, which is kept short.

A version that changes Øphio's database backs it up first. The version
before it can't read the changed database, so going back past such an update
also needs that backup restored; `docs/upgrading.md` in Øphio's source says how.

### Updating the phone app

When the new version carries the phone app, paired Android phones offer it
themselves: a notification says the update is ready, and Settings leads with
an **Update now** card, also under Settings › Versions. The phone downloads it over its connection to this
computer, on the same network or over Tailscale from anywhere, with no cable
or wireless debugging. Android installs it only if it is signed like the
installed app, so a phone keeps the app it trusts. Android 12 and later
install it without asking; earlier versions ask you to confirm. The device
needs the File transfers permission. The first app with this updater has to be
installed by `ophio phone install` or by hand.

## Uninstalling

Stop Øphio and remove automatic startup, then remove its settings and data
and finally the programs:

```sh
ophio service uninstall
ophio uninstall-data
ophio uninstall-data --yes
rm -r ~/.local/opt/ophio ~/.local/bin/ophio
rm ~/.local/share/icons/hicolor/*/apps/ophio.*
```

`ophio service uninstall` stops Øphio and removes the login service; it says
so when there is none. If you start Øphio from your desktop's own autostart
instead, remove that entry and log out and in again first: `uninstall-data`
refuses while Øphio is running. Without `--yes`, `uninstall-data` only names
the folders it would delete from. It deletes the paired devices, this
computer's identity, history and settings. Removing only the programs keeps
them, so a later install carries on where this one left off.

These are kept:

- files received from your devices, in `~/Downloads/ophio` unless you chose
  another folder with `ophio inbox set`;
- your project and Library folders, and your agents' own accounts and settings;
- the desktop's own record of your sharing approval, kept by its portal in a
  permission store other apps share (`~/.local/share/flatpak/db/remote-desktop`
  on the tested desktops); Øphio does not edit it;
- the `~/.local/bin`, `~/.local/opt` and `~/.config/systemd/user` folders,
  which other programs may also use;
- the downloaded archive and the folder you extracted it to.

## Upgrading from the earlier companion program

Before the product was named, the program was called `companion`. Such an
install has the `companion` command, `~/.local/opt/companion`, the
`companion.service` login service, `~/.config/companion` and
`~/.local/share/companion`. To upgrade it:

1. Run the new archive's `install.sh`. It also removes the
   `~/.local/bin/companion` link when that link points into
   `~/.local/opt/companion`, and leaves any other `companion` file alone.
2. Run `ophio service install`. It stops and removes the `companion.service`
   unit the earlier program wrote, then starts Øphio in its place. A
   `companion.service` the earlier program did not write is left alone.
   Without automatic startup, stop the earlier program and run `ophio run`.
3. When it starts, Øphio renames `~/.config/companion` to `~/.config/ophio`
   and `~/.local/share/companion` to `~/.local/share/ophio`. The computer's
   identity, paired devices and history come with them, so paired devices keep
   working. If the earlier program is still running, or a folder cannot be
   renamed, Øphio says so and keeps using the old folder. It never merges into
   or replaces a folder that already exists under the new name.
4. Check `ophio status`, then delete `~/.local/opt/companion`.

Files received before the upgrade stay in `~/Downloads/companion`. New ones go
to `~/Downloads/ophio`, unless you chose another folder with `ophio inbox set`.
