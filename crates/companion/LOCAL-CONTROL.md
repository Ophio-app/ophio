# Stop control from the computer

While the phone or an agent holds input, the companion shows a local Stop
indicator naming the holder. On Linux, click the notification or its
**Stop all computer control** button (servers without buttons, such as mako,
use the click); dismissing it also requests Stop. On Windows, click the
notification-area Stop icon.

Run `ophio stop` from the same operating-system account to release every
phone input session and request Stop for every computer-use task on this
computer. Bind that command to a keyboard shortcut in your desktop's shortcut
settings. The command uses the companion's existing owner-only local socket;
no network access, pairing secret, or administrator permission is needed.
It leaves unrelated coding tasks running. Input release and native task exit
are separate confirmations; check the task's intervention receipt for native
interrupt and child-process status. Earlier effects cannot be undone.

Linux requires a desktop notification service advertising actions (mako,
dunst, GNOME Shell and Plasma all do). Input and computer use stay unavailable
until the indicator can be shown; the companion keeps retrying, so starting or
restarting the notification service is enough. On Windows
the companion must run in the interactive user's session for the tray icon to
be available. Windows runtime qualification is still required.
