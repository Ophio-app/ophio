---
name: Set up Tailscale
description: Install Tailscale so the phone reaches this computer away from home, and check Øphio uses it.
---
Help the person set up Tailscale, so their phone reaches this computer from anywhere, not just on the same Wi-Fi.

1. Run `tailscale status` to see whether Tailscale is installed and running on this computer.
2. If Tailscale isn't installed, ask before installing it. On Linux, Tailscale's own instructions are at https://tailscale.com/download/linux; their install script and most packages need the person's password, so give them the exact command to run in their own terminal. On Windows, they install it from https://tailscale.com/download/windows.
3. Starting it and signing in are the person's: on Linux, `sudo tailscale up` in their own terminal, which shows a link to sign in with; on Windows, from its icon in the corner of the taskbar. Wait for them.
4. On the phone, the person installs Tailscale and signs in to the same account. If the phone is connected over Wireless debugging, `ophio phone open tailscale` opens it, or its store page.
5. Check with `tailscale status` that Tailscale is running on this computer.
6. Øphio works out its addresses when it starts, so if it was running before Tailscale was up, it offers its Tailscale address only after a restart. Don't restart it from this task, which runs inside it: tell the person that pairing a device in `ophio setup` restarts it when needed, or that they can restart it themselves once this task has finished.
