# Accounts

Accounts holds the sign-ins and keys that tasks on a computer use. Each
computer has its own, in the app under Settings › Accounts. Every active
account goes to every task the computer starts, on Claude Code, Codex or any
other agent. Keys and tokens stay on the computer: the phone sees each
account's name, who it is signed in as and its state, never a secret.

## What ships

| Service | Kind | Signs in with | Tasks receive |
|---|---|---|---|
| Claude Code | Agent login | `claude auth login` | The agent uses its own login |
| Codex | Agent login | `codex login` | The agent uses its own login |
| GitHub | CLI login | `gh auth login --web` | `GH_CONFIG_DIR`, a private folder for this account |
| OpenRouter | API key | The key, pasted on the phone | `OPENROUTER_API_KEY` |
| Google (Gmail) | OAuth app | Google's page, with your own OAuth client | `GOOGLE_OAUTH_ACCESS_TOKEN` and `GOOGLE_APPLICATION_CREDENTIALS` |

**Add a service** adds any other in one of three shapes:

- **API key:** a name, the environment variable tasks receive it in, an
  optional https address that answers 200 when given the key, and an optional
  security page.
- **OAuth app:** the sign-in and token addresses, the scopes and the variable
  for the access token. Then give it your OAuth client at the computer (below).
- **CLI login:** the sign-in, status and optional sign-out commands, and the
  variable that names the tool's config folder, such as `CLOUDSDK_CONFIG`.

Variables the computer or Ophio itself needs, such as `PATH`, `HOME` or
`GH_TOKEN`, cannot be set by a service.

## Signing in

Sign-ins happen on the computer, in its normal browser, controlled from the
phone:

1. On the phone, open Accounts, pick the service and tap **Sign in**. The phone
   asks for its own unlock first.
2. The computer starts the service's official sign-in, which opens its browser
   at the provider's page. The phone switches to Desktop control with a strip
   along the top: "Signing in to GitHub", and Cancel.
3. Sign in there. The password manager in that browser fills passwords and
   passkeys; the phone keyboard types anything else. When the provider shows a
   code, as GitHub does, the strip shows it with **Type it**.
4. The computer waits for the official tool to finish, then checks who is
   signed in with the tool's own status command. The strip says who, and goes
   back to Accounts.

A sign-in that fails, is cancelled or takes longer than 10 minutes is said so
plainly, and nothing is saved.

For an API key, tap **Set key** (or **Replace key**) and paste it, or let the
phone's password manager fill the one field. The key crosses the paired,
encrypted link once and is never sent back. A key must be at least 8
letters, digits or punctuation, without spaces or quotes (`" ' \` < > &`), so
Ophio can find and hide it wherever it shows; one that isn't is refused. The
same goes for an OAuth client's secret and for the tokens a provider sends: one
Ophio couldn't hide is refused, and the sign-in or refresh says so.

Google needs your own OAuth client, a Desktop app created in Google Cloud
Console. Download its client JSON, then open Google in Accounts and tap
**Choose the client file**, or at the computer:

```sh
ophio accounts set-client google client.json
```

Either way the client goes into the computer's keyring, after the phone's
unlock when it comes from the phone; the file can be deleted after.

**Password manager on this computer**, a card shown until you tap **It's set
up**, names the computer's default browser and opens your password manager's
extension page there, so it is ready before the first sign-in.

## Each account

Tapping an account shows who it is signed in as, when it was last checked and
the tasks using it now, with:

- **Sign in** or **Sign in again**, and **Check now**;
- **Pause** or **Resume**. Pause stops the account being given to tasks that
  start after it, and `ophio token` from renewing it. Tasks already running
  keep what they were given when they started. A token being fetched when
  the pause comes is given to no one. Programs you run yourself aren't
  affected. A paused agent login starts no new tasks on that agent;
- **Sign out**, which first cancels a sign-in still going for the account,
  then runs the service's own sign-out, revokes Google's refresh token, or
  forgets the key. If the service's own sign-out fails, or its folder can't be
  removed, Accounts says so and leaves the account as it was, to try again.
  Until a sign-out ends, the account can't be signed in or given a key;
- **Delete permanently**, which also forgets the account's keys, removes a
  service you added, and opens the provider's security page on the computer
  where a session or key has to be ended by hand. A delete whose sign-out
  fails keeps the service, and opens nothing.

Ophio's GitHub sign-in asks for the `workflow` permission too, so an agent
can add or change the automated checks in `.github/workflows`. An older
sign-in without it keeps working for everything else; its row says **missing a
permission**, and **Sign in again** asks for it. Agents are told to point you
there rather than to a terminal command.

Sign in, Set key, Choose the client file, Add a service, Sign out and Delete
each ask for the phone's own unlock, every time. A phone with no secure screen
lock can't be asked, so those actions are refused there until one is set;
Pause and Pause all still work.

**Pause all**, at the top, pauses every account at once, for a lost phone or a
worrying moment. Tasks already running keep what they were given, so stop any
that must end too. At the computer:

```sh
ophio accounts pause-all
```

Accounts are checked every 15 minutes with each service's own status command,
and once at start. The history of sign-ins, pauses, sign-outs and deletions,
with the device and time, is kept on the computer; the phone shows the latest
50. It holds no keys.

## Which phone can

Account actions need the **accounts** permission, which only the computer can
give, device by device:

```sh
ophio devices allow "Galaxy" accounts control
```

`control` lets that phone use Desktop control to sign in. Revoking a phone's
pairing takes both with it.

## Where secrets live

- Keys, refresh tokens and OAuth clients go in the computer's keyring on
  Linux (Secret Service), each item named "Ophio: <account> <item>". If no
  unlocked keyring can be opened, they go in a file only your user can read,
  and Accounts says so. On Windows they go in such a file, encrypted with
  Windows data protection.
- Once Ophio has kept anything in the keyring, a later start with the keyring
  locked or not running keeps new values in the file, and signing out or
  deleting an account is refused with "The keyring is locked or not running":
  what is in the keyring can't be removed until it is unlocked. When the
  keyring is back, a value left in the file is moved into it. If a copy of
  an item is in a locked part of the keyring, signing out or deleting removes
  the copies it can reach and then reports that the rest needs the keyring
  unlocked, so the account is kept until you try again. A removal the keyring
  wants confirmed on the desktop counts as not done, and is reported the same
  way.
- Setting a value removes the copies kept in other parts of the keyring, for
  instance while another part was the default, so the new one is what is
  read. A copy in a locked part is reported, and the value read is the one
  just set.
- When a renewed Google sign-in can't be saved, because the keyring locked
  meanwhile, Ophio keeps using it until it stops and the account shows as
  needing attention. Unlock the keyring, then tap **Check now** to save it.
  A renewal that finishes after you sign in again is discarded, so the newer
  sign-in stays.
- CLI logins keep their token in the tool's own store, in a private folder per
  account under Ophio's data.
- Agent logins stay in the agent's own store, as when you sign in yourself.
- Ophio writes nothing secret to the database, logs, events, transcripts or
  notifications. What a task prints, and what a tool shows when it asks for
  approval, is masked: every key, token and client secret Ophio holds is
  replaced before the output is stored or sent to the phone, from the moment
  Ophio first reads it, including a value split across pieces of streamed
  output and one replaced while the task runs. What Ophio held and has since
  replaced or signed out stays masked too, the latest 1,024 such values. Where
  a preview is cut to fit, what is left of a cut value is hidden too, unless
  only its first 1 to 3 characters are left; those show. A value a task re-encodes (URL-escaped, base64)
  is not recognised. Ophio never reads agent logins or the tokens an added
  CLI login keeps in its own store, so those aren't masked; GitHub's token,
  which Ophio reads with `gh auth token`, is.

## For tasks

A task receives each active account's variable when it starts. A task that
runs long enough for its access token to expire can ask for a fresh one:

```sh
ophio token google
```

It prints the account's key or a fresh access token. Inside a task, it works
for the accounts that task was given, and only while the task runs; the task
receives a credential for this, masked like the rest. At the computer, outside
a task, it works for any account. Either way Ophio must be running and the
account active.

A Chat conversation that is held to what it may read gets its accounts too.
Where an account's variable names a file or folder, such as GitHub's sign-in
folder or Google's credentials file, the conversation may read it, whatever
else it may read. GitHub, like any internet service, still needs the
conversation's **Internet** on.

`ophio accounts` lists each account and its state; `--json` gives the full
result.

## What agents are told

**Settings › Agent apps › Agents can ask me questions** is on by
default: an agent asks when something is unclear, on the phone and in a
notification, and waits. Off, every run is told to make the best choice it
can and say what it chose and why, so you can change it, and to stop and ask
only when a choice can't be undone or only you can know the answer. The
setting is kept on the computer and needs the Manage access permission to
change.

Every run you work with is also told that when it is blocked it must say which
system is blocking it (an Ophio setting, GitHub, Claude Code, Codex, the
computer, the network or the project's own tools), how you can fix it, and the
pros and cons of each fix, pointing to the place in Ophio where there is one.

## Removing Ophio

`ophio uninstall-data` signs out what it can first: it revokes Google's
refresh token, runs each CLI login's own sign-out and removes every Ophio item
from the keyring. Agent logins are yours and are kept. It then lists any
account whose sign-in or key may still work, with the page where you end it.
`gh auth logout` does not revoke the token at GitHub, so a signed-in GitHub
is always listed.
