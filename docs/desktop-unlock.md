# Unlocking the computer

When a Linux computer's screen is locked, the person can unlock it from the
app with the computer account's password, wherever the lock is in the way.
The computer checks the password against the account's and, when it is
right, lets go of the lock. Agents can't unlock a computer; no tool takes or
returns its password.

## Who can

Unlocking takes both **Control** and **Manage** on the paired device. The
password opens the whole computer, so Control alone isn't enough. A device
with Control but not Manage sees why it can't unlock rather than the button.

The button only appears once the computer has said that it is locked, and only
on a computer that can check a password (see below). It is offered wherever
the lock stops something, right where the app says so, and those places say
to unlock it "here":

- the top of the Control screen, under a notice that the computer is locked;
- each display's own screen, and a computer-use task's screen;
- the screens a new review or a new computer-use task can work on, under the
  list, so the screen can be picked as soon as it is unlocked;
- Chat and Review, in place of Open Control, when a locked screen stops a
  computer-use task from continuing;
- handing input back to an agent.

That includes a computer that was already locked when Ophio started sharing
its desktop: its displays are listed as taking no input until it is unlocked.
Where the device can't unlock it, these places say to unlock it at the
computer instead, and Chat and Review keep Open Control.

One unlock is checked at a time. While it is, another screen shows it being
checked rather than asking for the password again, and then whether it
worked. If the connection drops while it is checked, the app asks the
computer how that unlock ended once it reconnects, without sending the
password again, and takes no other password until it knows. Why an unlock
didn't work is shown only while the computer stays as it was: once it is seen
to unlock or lock again, at the computer or anywhere else, that reason is
dropped, including one that arrives later.

## What happens to the password

The phone sends the password once, inside the unlock command, and clears the
field as it sends. It isn't kept on the phone: the copy of the command the
app keeps while it waits for the result has an empty password. A password
typed but not sent is dropped when its screen is left for another, when the
app goes into the background, or when the device may no longer unlock the
computer.

The computer uses it only for the PAM check. PAM's copy of it is wiped as
the check ends, and Ophio's own copies as it lets go of them, which for the
command itself is once the unlock is settled, a few seconds later; the network
buffers it arrived through are freed in the usual way. The
command the computer records in its history, like every command with a secret
in it, has the password removed, so no log, transcript, receipt or history
file holds it.

## How the computer checks it

Ophio loads `libpam.so.0` when it starts and checks the password through PAM
for the account Ophio runs as. It doesn't use the computer's `login` stack: a
login stack can ask for a one-time code, a security key's PIN or a
fingerprint where a password is expected, or let a module that turned the
password away be outvoted, in more ways than reading it beforehand can rule
out. Ophio checks against a stack of its own instead, written for each check
into a new directory only the account can reach (under `XDG_RUNTIME_DIR`) and
removed after it, and read from there with `pam_start_confdir`:

```
-auth [success=ok ignore=ignore module_unknown=ignore default=die] pam_faillock.so preauth
auth [success=ok default=die] pam_unix.so
```

The first line asks `pam_faillock`, where the computer has it, whether the
account is locked after wrong passwords, and stops there if it is. It only
looks: it never counts a try nor clears one. Given no settings of its own, it
uses the computer's, from `/etc/security/faillock.conf`: the same ones the
computer's login uses, as long as none of the computer's PAM stacks gives
`pam_faillock` settings of its own. Ophio reads them all when it starts and
again before each check, and where one does, it offers no unlock (see
below). The second asks once for the
password the computer keeps for the account, and passes only when it is
right. The password is given to that one prompt and to nothing else; a check
that asks for anything more, or never asks for the password, unlocks nothing.

A second factor the computer's own login asks for, such as a code or a key,
is not asked for when unlocking from Ophio. The paired device's Control and
Manage stand in for it, together with the password.

Only one check runs at a time: an unlock asked for while one is being checked
is turned away rather than kept waiting. Once the check finishes and its
password copy is wiped, the next check is allowed before the result is
returned, so a caller can retry as soon as it receives the answer.
The device's Control and Manage are checked just before the password is
passed on, and again once it is found right, before the lock is released,
so a device that loses either, or is removed, while its password is checked
unlocks nothing. Nor does a password
whose computer stops or restarts sharing its desktop while it is checked;
sharing that ends while the lock is already being released ends once it has.
A check that takes longer than 20 seconds is given up, and no other starts
until it ends.

A right password then releases the lock, within five seconds in all:

- `loginctl unlock-session` for Ophio's login session, which GNOME's and KDE's
  lock screens obey;
- SIGUSR1 to the `hyprlock` and `swaylock` processes of the same account on
  the same Wayland display, which they take as "unlock". A display is told
  apart by its socket, its name within its runtime directory, so a locker in
  another session of the same account isn't released.

The unlock is reported done only when the computer then reports its screen
unlocked, within five seconds. If it doesn't, the app says the password was
right but the computer didn't confirm it, so the person can check its screen.

## Wrong passwords and lockouts

Tries from Ophio never count toward the computer's own lockout, nor clear it,
so unlocking from the app can't lock the account at the computer. While the
computer has the account locked after wrong passwords (`pam_faillock`, which
many distributions set to three in 15 minutes), Ophio doesn't check one at
all, and the app says to unlock it at the computer.

Ophio has a pause of its own, so guessing from a device gets nowhere: after
**three tries that didn't unlock it within 15 minutes**, it asks for the
computer to be unlocked at the computer until the first of them is 15
minutes old. Any try that didn't unlock the computer counts, not only a wrong
password, from when the computer answered it. The right password isn't
counted, but the wrong ones before it still are, so a right one never makes
room for more guesses. The pause outlasts a restart of Ophio: it keeps when
recent tries were made, never what was typed.

## Where it doesn't work

The capability reports unsupported, and the app shows no unlock, when:

- the computer isn't Linux (Windows can't be unlocked from Ophio);
- `libpam.so.0` can't be loaded, or is older than Linux-PAM 1.4 and so can't
  read a stack from Ophio's own directory, or Ophio can't find where it reads
  stacks and loads modules from. Those places are read from the loaded
  library that supplies `pam_start_confdir`, in its readable, non-writable
  segments, not from a pathname that could now name a different file;
- Ophio has neither a login session nor a Wayland display to release;
- Ophio runs with `no_new_privs` (a service with `NoNewPrivileges=yes`, some
  sandboxes) and can't read the computer's passwords, because `pam_unix`'s
  password helper then can't either, and every password would be called
  wrong;
- the account isn't in `/etc/passwd`, as with directory (LDAP, SSSD) and
  systemd-homed accounts, whose passwords `pam_unix` can't check;
- any of the computer's PAM stacks (in `/etc/pam.d`, the vendor's
  `/usr/lib/pam.d`, `/etc/pam.conf`, the places the loaded libpam was built
  to read, or one they include, from any directory) gives `pam_faillock` settings of its own, such as how many
  wrong passwords lock the account, for how long, where it keeps their count
  or another settings file, or keeps `pam_tally` or `pam_tally2`, or a stack
  or the module a line loads can't be read by Ophio. Ophio couldn't then
  tell when the computer has the account locked. Only the words that say
  which step a line is and how it reports (`preauth`, `authfail`,
  `authsucc`, `silent`, `audit`, `no_log_info`, `nodelay`) may follow
  `pam_faillock`; settings belong in `faillock.conf`. These modules are
  known by the file a line loads, so one loaded by another name, through a
  link or as a copy, counts too. Names are read byte for byte, as PAM reads
  them. A module named through `$ISA`, whose directory Ophio can't know, or
  one Ophio can't read, isn't followed whatever it is. A lockout some other
  module keeps is one Ophio can't recognize, and it isn't followed.

Lock screens other than those listed above, which neither obey logind nor
take SIGUSR1, are not released even by a right password; the app then
reports that the computer didn't confirm the unlock.
