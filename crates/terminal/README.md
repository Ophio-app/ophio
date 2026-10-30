# Host terminals

`PtyHost::new(PtyConfig::default(), clock)` implements `ports::TerminalHost` using
portable-pty: Unix PTYs and Windows ConPTY. The caller authenticates and authorizes
terminal operations. The default allows 16 running sessions, 2 MiB of retained
bytes per session, and 32 retained exited sessions (evicted on later opens).
Titles come from the launch specification; OSC title parsing is not enabled.

Unix defaults to `$SHELL`, then the passwd shell, then `/bin/sh`, with login mode.
Windows searches PATH for `pwsh.exe`, `powershell.exe`, then `cmd.exe`. Explicit
programs receive separate argument strings, a working directory, environment
additions and `TERM=xterm-256color`; no command-line interpolation is added.

portable-pty creates a pseudo-console that inherits its cursor, so Windows first
writes a cursor position request (`ESC [ 6 n`) and shows nothing more until it
is answered. The host answers it at once with the top-left corner and keeps it
out of the output, so a shell starts with no viewer attached and a viewer that
attaches later does not answer it again into the shell.

Each terminal owns a native reader, a bounded output/coalescing worker and a
process reaper. Output is coalesced up to 16 KiB / 8 ms. Input writes have a
bounded, ordered queue on Unix. A task writes the nonblocking master, sleeping
on readiness when input is full. Accepted writes finish even if their caller
disconnects. Close, host drop and process exit cancel active and queued input;
the reaper never waits for a writer lock. The native reader also waits on
readiness because duplicated Unix master descriptors share nonblocking mode.
Windows serializes native input separately from process control and closes
ConPTY without waiting for the input lock.
Buffers and live broadcast queues are bounded. Native diagnostics are sanitized
before crossing the adapter boundary.

Attach snapshots and subscribes under the same stream lock used to advance byte
offsets. Deliver replay before receiving live events. Resume with the **next byte
not rendered**, not the beginning of the last frame. On `RecvError::Lagged`, discard
the old receiver and attach again at that offset. If older bytes are unavailable,
`Replay.truncated` is true and `Replay.from` names the oldest retained byte. An
offset ahead of produced output is rejected. Attaching after exit replays retained
bytes and immediately supplies the exit event. Sessions survive disconnected
attachments; a companion restart reports old terminal ids as absent.

Close terminates the process tree and waits for exit. Linux signals the PTY
session and its observed descendants, including job-control process groups.
Windows assigns each process to a kill-on-close Job Object and terminates that
job. Windows assignment happens immediately after portable-pty spawns the process;
portable-pty does not expose suspended spawning, so a hostile program could escape
before assignment. Linux descendant enumeration also cannot contain adversarial
fork/setsid races. These are cleanup mechanisms, not security sandboxes. Windows
job assignment fails closed if native policy rejects it.

The reaper drains output before publishing the final exit event. Reader failure
or a drain timeout yields an unknown exit code. The host kills owned processes
when dropped. The service manager must still clean up a forcibly crashed Linux
companion's descendants; there is no persistent worker/reattach protocol in this
adapter. Windows job handles provide crash cleanup. Tests use isolated shell
scripts only. The full-input Close regression uses a noncanonical, non-echoing
PTY, acknowledges one native input byte, and checks that the rest of the write
stays pending before Close. A separate FIFO releases the fixture on test failure;
the test runtime does not wait for stuck native writes during shutdown, so a
Close regression reports a failure instead of hanging the test binary.
The input tests also exercise a single-worker blocking pool, another terminal
during blocked input, cancellation on natural exit, and complete ordered input
after a caller disconnects.
`tests/conpty.rs` starts PowerShell and Command Prompt with no
input and expects their output; it runs natively on Windows and has passed on
Windows 11 24H2.
