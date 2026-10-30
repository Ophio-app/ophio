---
name: Turn on desktop access
description: Let paired devices see and control this computer's desktop.
---
Help the person turn on desktop access, so a paired device can watch this computer's screen and, when they allow it, use its mouse and keyboard.

1. Run `ophio desktop status`. It says whether desktop access is on and whether this desktop supports it.
2. If something is missing, such as a screen-sharing service on Linux, it says what. Ask before installing anything, and give the person any command that needs their password to run themselves.
3. Run `ophio desktop enable`. On Linux, the desktop may ask the person to approve screen sharing; they answer it on the screen.
4. Each device also needs permission to view or control the desktop. Run `ophio devices` to show what each may do now. Changing it is the person's, with `ophio devices allow <device> view` or `ophio devices allow <device> control` in their own terminal.
5. Run `ophio desktop status` again and say what is ready.
