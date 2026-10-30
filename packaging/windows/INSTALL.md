# Test Øphio on Windows

This x86_64 archive is cross-compiled on Linux with LLVM/MinGW. It has been
run on Windows 11 24H2 (build 26100) in a virtual machine, by an ordinary user
and by an administrator, but not yet on Windows 10 or on a physical PC. It is
unsigned, and has no installer or automatic updater. Treat it as a preview.

A downloaded archive carries Windows' mark of the web. Unblock it before
extracting, so that the programs inside are not blocked one at a time: in
File Explorer, open the archive's Properties and tick **Unblock**, or in
PowerShell run `Unblock-File .\ophio-0.1.0-x86_64-windows.zip`.

Extract to a folder you own, such as one under your user folder, and keep
`ophio.exe` and `computer-mcp.exe` together, with `ophio.apk`, the phone app,
when the archive carries it. Retain LICENSE, NOTICE, THIRD-PARTY-NOTICES.txt
and sbom.cdx.json; the phone app carries its own notices, under Settings ›
Versions. Øphio runs in the background with no window of its own; opening
`ophio.exe` from File Explorer explains how to set it up and changes nothing.
In an ordinary, non-administrator PowerShell in that folder, run:

```powershell
.\ophio.exe setup
```

Setup goes through the rest one step at a time: starting Øphio at every
login, how your phone reaches this computer, Claude Code and Codex,
installing the phone app and pairing it, extras such as desktop access and
voice, and a check that everything works. Any step can be left for later, and
`.\ophio.exe setup` carries on where it was left. Once an agent is connected,
it can do most steps for you as a task you watch and approve; pairing, what a
device may do and signing in stay with you. By hand, the essentials are:

```powershell
.\ophio.exe service install
.\ophio.exe pair
```

`service install` starts Øphio now and whenever you log in. It is a per-user
startup entry, not a Session-0 Windows service, and it shows in
**Settings › Apps › Startup**, where it can be turned off. Closing the
terminal does not stop Øphio. Run from an administrator terminal, it only
registers the entry, because Øphio and the agents it starts would run as
administrator too; it then starts at your next login. `.\ophio.exe service
status` says whether it is registered and running. `.\ophio.exe run` runs
Øphio in the terminal instead, until the terminal is closed.

Unless this computer is on Tailscale, Øphio accepts connections only from this
computer itself. Run Tailscale on this computer and on your phone, or, on a
network you trust, add this computer's local address (from `ipconfig`) to
`%APPDATA%\ophio\config\config.toml`:

```toml
allow_lan = true
listen = ["192.168.1.20"]
```

The first time Øphio listens there, Windows Defender Firewall asks whether to
allow it. Allow private networks only; on a standard account that needs an
administrator. Windows treats a new network as public until told otherwise, so
set yours to **Private network** under **Settings › Network & internet**, in
that network's properties. If the question was declined or closed, Windows adds
inbound rules that block `ophio.exe`, and a blocking rule wins over any rule that
allows it: an administrator first deletes the `ophio.exe` rules marked with a
block sign under **Windows Defender Firewall with Advanced Security › Inbound
Rules**, then allows `ophio.exe` under **Windows Security › Firewall & network
protection › Allow an app through firewall**. Restart Øphio after any of these
changes: `.\ophio.exe service uninstall`, then `.\ophio.exe service install`.
If a pairing code offers only this computer's own address, `.\ophio.exe pair`
says so.

When the phone asks to pair, answer `y` only if it shows the same code as the
terminal; without a terminal, confirm with `.\ophio.exe pair confirm <code>`.
`.\ophio.exe doctor` checks the setup. Watching and controlling the desktop is
off until `.\ophio.exe desktop enable`. WGC monitor capture with DXGI fallback
and SendInput need a logged-in interactive session. Elevated apps, the UAC
secure desktop and locked sessions are outside managed input access.

If the earlier `companion.exe` is on this computer, stop it before running any
`ophio.exe` command: close its window, or end it in Task Manager. Øphio's
commands cannot reach the earlier program while it runs. Once it has stopped,
Øphio moves its settings, identity, paired devices and history to Øphio's own
folders the first time it starts.

To update, run `.\ophio.exe service uninstall`, which stops Øphio and waits
until it has, replace both executables, then run `.\ophio.exe service install`
from the new copy. Removing the folder does not delete projects or application
data. Review `.\ophio.exe uninstall-data --help` before choosing to remove
application data.
