# Agent logins

Agent logins are usernames and passwords that agents may use on websites, in
Ophio's own browser on the computer. Accounts are what Ophio's tools sign in
as; agent logins are for the times an agent has to use a website and an API
doesn't fit. Each computer has its own, in the app under Settings › Agent
logins.

Ophio types a password into the page itself. The agent asks for a login by
name and hears only whether it was filled, so the password never enters the
model's context, the provider's logs, a transcript or the phone's task view.

## Turning it on

Agent logins is off on each computer until someone accepts its notice there.
The first time Agent logins is opened, the notice fills the screen. **Turn on
Agent logins** stays disabled until the box is ticked, and the phone asks for
its unlock first. The computer records who accepted, when, and which version
of the notice. If the notice's wording changes, it asks again, and until then
agents can't use any login.

At the computer, `ophio agent-logins enable` shows the same notice and turns
it on when you type `yes`; anything else leaves it off, and the command ends
with an error. `ophio setup` offers it under Extras.

The notice, version 1:

> **Before you give agents logins**
>
> Agents can use the logins you add here to sign in to websites and act as you
> there. That includes posting, sending messages, buying, and changing or
> deleting things.
>
> Øphio is built to keep these passwords away from the AI:
>
> - It doesn't send your passwords to the AI model or its provider. Øphio
>   types them into the page itself, and only on the sites you set.
> - It stores them encrypted on this computer, so the model can't read them.
>
> This is how Øphio is designed, not a guarantee. We've taken every reasonable
> measure to make it secure, but no software is perfectly secure. A website, a
> bug, or an agent working around Øphio could still expose a password. We also
> can't control what an agent does once it's signed in.
>
> Agent logins is provided as is, without any guarantee. You use it at your own
> risk. Øphio and its contributors aren't responsible for what agents do with
> your logins, or for any loss that results.
>
> Only add logins you're willing to let an agent use, and keep bank and payment
> accounts out.
>
> ☐ I understand that agents act with my logins and that I use this at my own
> risk.

This sits on top of the Apache-2.0 license's own "AS IS" disclaimer. A short
reminder stays at the top of Agent logins and on the Add a login form: "Agents
can act as you on these sites. You use Agent logins at your own risk."

While Agent logins is off, logins already added can still be paused, signed
out and deleted, and nothing else.

## Adding a login

**Add a login** asks for:

- **Name,** such as "Facebook": 1 to 60 characters, different from the others.
- **Sites,** one per line, up to 10. Each is reduced to its host name, without
  `www.`, and covers its subdomains too. IP addresses and shared hosting
  addresses are refused.
- **Username.**
- **Password:** typed, pasted, filled by the phone's own password manager, or
  **Generate**, which makes a 20-character password on the phone and shows it
  so you can save it in your own manager too.
- **2FA setup key,** optional: the authenticator app's setup key or its
  `otpauth://` link. Ophio then makes the codes, so it holds both factors.
  Leave it out for your important accounts and the agent will ask you instead.
- **Notes for the agent,** which aren't secret, such as "Use the business page;
  never post without asking".
- **Who can use it:** every project or chosen projects, and any task or only
  tasks started from chosen agents.
- **How:** **Use whenever needed**, or **Ask me first** (below).
- **Tell me each time it's used.**

Adding, editing, replacing or removing a password or key, showing a password,
signing the agent browser out and deleting each ask for the phone's unlock
first, so the phone needs a secure screen lock.
A password or key crosses the paired, encrypted link once and the phone keeps
no copy, not even in its queue of unsent commands.

## Each login

Tapping a login shows its sites, who may use it, its history and:

- **Show password,** after the phone's unlock. It hides again after 30 seconds,
  and the history records it.
- **Edit, Replace password,** and **Add a 2FA key** or **Replace or remove the
  2FA key**. Taking a site off a login signs the agent browser out of it
  first; if that fails, the edit isn't saved and says why.
- **Pause** or **Resume.** A paused login isn't filled and its sites are closed
  to agents: a tab already open on one is emptied, as is a page from one that
  moved to an address of its own or a tab it opened, and a request waiting
  for your answer opens nothing. Tasks keep running.
- **Sign the agent browser out,** which clears the browser's cookies and site
  data across the whole of each of the login's sites: every subdomain, and
  every address and port the browser has visited there. The agent browser
  restarts for it first, so every task's tabs close, on any site, and a
  request a page already sent is stopped; no page is left to put the session
  back. The next task to use it signs in again.
- **Delete permanently,** which removes the password and key from the keyring
  and signs the agent browser out of its sites. If the sign-out fails, the
  login is kept and the delete says why.

If Ophio has no agent browser when you sign out, delete or take a site off a
login (the browser was uninstalled, say), the sign-out waits: the next time a
task browses, the browser is signed out first, and nothing browses until it
is.

**Pause all**, at the top, stops every agent filling any login and opening any
login's sites until **Resume all**. `ophio agent-logins pause-all` does the
same at the computer, and `ophio agent-logins` lists the logins and their
state.

The history keeps the latest 500 entries on the computer and shows the latest
50: every fill, code, approval, refusal, reveal and change, with the task, who
did it, the site and the time.

## What agents get

A coding task doing Work gets Ophio's agent browser unless its **Browser**
switch is turned off when the task is made. Ask and Plan tasks never get it.
Its runs have these tools, on Claude Code and Codex alike:

- `browser_open`, `browser_back`, `browser_tabs`, `browser_switch_tab` and
  `browser_close_tab`;
- `browser_observe`, which returns a screenshot, the page's real address and a
  list of the page's controls;
- `browser_click`, `browser_type`, `browser_press` and `browser_scroll`;
- `list_logins`, which returns the name, sites, username, notes and whether a
  2FA key is stored, for the logins this task may use, never a password;
- `fill_login(login, submit?)`, which fills the username and password on the
  current page and answers "filled" or a plain refusal;
- `fill_code(login)`, which fills the current authenticator code;
- `ask_owner_for_code(login, why)`, which asks you for a code the site sent.

There is no tool that returns a password, and none that runs a script in the
page or reads a field's value. A task browses in its own tabs, which close
when its run ends. Tasks can observe and use their tabs while other tasks
browse, without switching tabs first. Downloads land in the project's
`downloads` folder.
They appear under their final names only after the whole file has been copied,
without overwriting an existing file. If delivery fails, `browser_observe`
reports the error and the path of the complete file kept in Ophio's private
`agent-browser/downloading` folder. After fixing the destination's permissions
or freeing space, that file can be copied or moved to the project without
downloading it again. Completed files kept for recovery survive browser
restarts; unfinished `.crdownload` files are cleared on startup.

A task may only open the sites of logins it may use, or of no login at all,
and that goes for the whole site: a task kept from `app.example.com` is kept
from every part of `example.com`. The browser keeps one signed-in profile, so
this keeps a task from riding a session that another task signed in.

## Checks before every fill

1. The task may use the login, and it isn't paused; for **Ask me first**, you
   allowed it for this run.
2. The page, and the frame holding the field, are HTTPS on one of the login's
   sites or their subdomains, as the browser itself reports them. Lookalike
   addresses, plain HTTP and other sites' frames are refused.
3. The field is a real, visible input of the right kind: a password field for
   the password, a text, email or phone field for the username, and a code
   field for a code. Hidden decoy fields are refused.
4. The check and the typing happen on the same live page, and the field is
   checked again once it has focus. If the page changes in between, or the
   field changes kind when focused, nothing is typed.

What the agent sees afterwards passes through Ophio: the control list leaves
out the values of password and filled fields, and the exact value of every
stored password and setup key, and of every code Ophio makes or you give, is
masked from tool results, run output, events and transcripts. A page Ophio
typed a password or code into gets no screenshot, since it could show what was
typed somewhere else on the page; nor does any other tab of the task while
that page is open, or one opened from it. Screenshots come back when the tab
moves to another page. A site that shows a password again on a later page
would still be masked in text, but not in the picture. A file downloaded from
a page Ophio typed into isn't kept, since the page could have written what was
typed into it; the agent is told.

Masking finds a password or code as it is written. A site's own scripts see
everything typed into it, and a site that showed the password back split up
or encoded would get it past masking. That is the site misusing your
password, which Ophio can't prevent; give agents logins only for sites you
trust with them.

## Ask me first

The first time a task needs an **Ask me first** login, its sites stay closed
to it and the phone shows a request naming the task, the login and the site.
Allowing it needs the phone's unlock, and lasts for that run of the task:
when the turn ends, the next one asks again. If the login is edited, paused
or given a new password before you answer, the request opens nothing; the
task asks again. A login can't be allowed from a linked chat; open the phone.

## Codes the site sends

When a site texts or emails you a code, the agent asks for it and the task
shows a code prompt: "Facebook: …", with a field for the code. Type it and tap
**Fill code**; Ophio fills it into the page and the agent never sees it. The
code goes only into the tab and page the agent asked from, and only if the
task may still use the login; if either has changed, it is refused and the
history says so. A code is 3 to 12 letters or digits; spaces and dashes are
ignored. **No code…**
tells the agent you won't give one.

## Watching and taking over

The agent browser shows in Desktop control, where you can watch it and take it
over. Taking over pauses the agent's browser tools for that tab: anything
the agent was already typing finishes before your input goes in, and nothing
of the agent's goes in after. Use it for
the first sign-in, passkeys, CAPTCHAs and "is this you?" checks; the profile
keeps the session, so the agent can carry on after. Agents can't use passkeys
or security keys themselves.

Large sites often challenge new or automated sign-ins. Signing in the first
time yourself, then letting the agent reuse the session, avoids most of this.
Whether a site's terms allow automated use is your call.

## Being told

With **Tell me each time it's used** on, the phone says so while the app is
open: "Facebook was used on facebook.com for …", with **Open task**. A use that
happened while the app was closed is in the history, not announced later.

## Where things are kept

- Passwords and setup keys are in the computer's keyring, with Accounts' keys.
  The database keeps only names, sites, usernames, notes, rules and history.
- The agent browser is a Chromium-family browser found on the computer, run
  headless with no network debugging port. Its profile is in Ophio's data
  folder under `agent-browser`, private to your user and apart from your
  everyday browser. Alongside its cookies it keeps a list of the site
  addresses it has loaded (the latest 20,000), so signing out can clear what
  each of them stored. `ophio uninstall-data` removes it.

An agent running as you could deliberately dig the keyring or the browser
profile out of your account, as with Accounts. The design keeps the normal
path from ever showing an agent a password, and keeps pages from steering one
to the wrong site. It can't stop an agent from posting or buying once it is
signed in: **Ask me first**, the notes and keeping payment accounts out are the
controls for that.

There is no sync, import or export. If the computer is lost, its logins go
with it.
