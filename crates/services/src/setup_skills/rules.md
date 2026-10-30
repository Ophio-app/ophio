
## How every Øphio setup skill works

- Use Øphio's own command, `ophio`: on Windows, `ophio.exe` in the folder Øphio was unpacked to. If `ophio` isn't found, try `~/.local/bin/ophio`, then `~/.local/opt/ophio/bin/ophio`, and otherwise ask the person where Øphio is. Read `ophio <command> --help` before using a command for the first time, and use its commands rather than editing Øphio's files or driving `adb` yourself.
- This task runs inside Øphio. Never stop, restart or uninstall Øphio or its login service from here; when something needs a restart, tell the person and leave it to them.
- Start by looking: `ophio status` and `ophio doctor` say what is already done. Don't redo it.
- Ask before installing any program. Say what it is, why it's needed and the exact command, and wait for the person to approve it.
- Never use `sudo`, an administrator password or a Windows UAC prompt. When something needs one, give the person the exact command to run in their own terminal, and wait for them.
- Never sign in anywhere for the person, and never ask for a password, token or key in this chat. When one is needed, give them the command that asks for it in their own terminal, such as `ophio channels token set telegram`, and wait until they say it's done.
- Some Øphio commands refuse to run from a task, because only the person at the computer may do them: showing or confirming a pairing, pairing a phone's Wireless debugging, changing what a device may do, and `ophio setup` itself. Explain what they'll see, ask them to do it in `ophio setup` or their own terminal, and wait. Every other command these skills use works from here; if one is refused anyway, say what it said and ask the person to run it in their own terminal.
- Anything on the phone's own screen, accepting a licence and anything that costs money are the person's to do. Say exactly what to tap or read, then wait for them.
- Change nothing this setup doesn't need. If something fails twice, stop, say what you saw, and ask.
- When you're done, say in a few lines what is set up now and what, if anything, is left for the person.
