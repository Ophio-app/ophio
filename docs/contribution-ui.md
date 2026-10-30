# Opening a game from Clankerhub

The phone's screens for a contribution handoff (OI-01-U, issue #10). They sit
on the [contribution receiver](contribution-import.md) and follow Clankerhub's
CH-016 design and copy, adopted in its owner decisions of 2026-10-10, in
Øphio's own design system. Source: `client/src/screens/contribute/`.

## What the person sees

| Screen | When | What it does |
| --- | --- | --- |
| **From Clankerhub** row under Tasks | A handoff is on the phone | Waits until it's opened, dismissed or expires (D-7). Expired handoffs that never started a download are removed with "Expired handoffs are removed. Nothing was downloaded." The section isn't there while none waits. |
| **Scan a Clankerhub code** in Tasks → New | The phone has a camera | Scans the code a computer's browser shows, for the same review as a link. |
| **Open a game from Clankerhub?** | Opening one | Computer, folder and rules, then the game, source, revision and task as data. Checking a handoff is inert, so it runs as soon as there is a computer and a folder. |
| **Review the game's rules** | Review rules | The rule files by role, what trusting them means, and the person's own check (below), including the handoff fingerprint. **Trust rules and get source** is the one confirmation (D-4). |
| **Getting the source** | After the confirmation | Check, download, check rules, add project, each as text. Cancel. The person may leave. |
| **{project} is ready** | Prepared or reopened | Nothing has run yet, queue paused. **Plan something** opens Review in Plan with a starter request; nothing starts until Start there. |
| Blockers | Any other receiver code | Heading, what happened and what didn't, a next action. Unknown codes stop as "Something unexpected" with the code for reporting. |

Every message is in `codes.ts`. Values from the handoff are shown as data and
never written into a sentence. Once Øphio has started getting the source, no
message says "nothing was downloaded": the receiver keeps every interrupted
download.

## Read before trust

On a computer advertising `policy-preview`, each rule has **Read**. Opening it
fetches source data, verifies the pinned commit and policy digests, and displays
literal text with its size in bytes. It creates no project, grants no trust and
executes nothing. The same shared screen works on Android and iPhone. Older
computers show an update explanation instead of receiving an unknown query.
Reading can fail or time out; **Read** retries. Closing the screen discards the
preview, and another operation, computer or policy digest gets a new reader.
Reads on the same connection are sent one at a time, so a second file's timeout
does not run while the first downloads. Leaving Rules discards unsent reads.

Matching bytes are not publisher authentication. The manual check below remains
until the [publisher contract dependency](contribution-import.md#remaining-authentication-dependency-147)
is resolved. Remembered trust and **See changes** are still outstanding in #147.

## The person's check

This receiver profile can't confirm with Clankerhub that a handoff is genuine
and still listed. The rules screen says so and asks the person to compare the
game, release, source, full revision and the handoff fingerprint (its
SHA-256, in groups of four) with the game's Clankerhub page, then tick "I
checked the game's Clankerhub page just now, and all of it matches". The
fingerprint is what binds the rule files: the other four can stay the same
while a handoff names different rule files. If the page doesn't show it, the
screen says not to trust the handoff, so Clankerhub's listing has to show it
for this check to be possible. That check is what the phone sends as the
receiver's approval, naming the publisher `clankerhub`:

- `checked_at` is when the box was ticked, recorded 30 seconds earlier so a
  phone clock slightly ahead of the computer's isn't refused. It only shortens
  how long the check counts.
- `valid_until` is at most five minutes later and never after the packet
  expires. A check over four minutes old is asked for again before sending.
- Each later download, resume or reopen needs the check again.

This is a manual step. It does not make the returning path one press, and it
isn't automatic publisher or revocation checking (#147). The packet itself,
issued per press, has nothing on the listing to compare: its expiry and
binding stay bounded by the receiver's own checks until publisher
authentication exists. After download the Ready screen says the rules match
"the handoff you checked", which is what the receiver verified.

A request to download is kept on the phone before it's sent. If its answer is
lost, the phone asks the computer what became of it once connected (also
after a restart), keeps the handoff past its expiry meanwhile, and never says
nothing was downloaded. Replies to a request since replaced, such as a check
for a folder the person changed, or progress overtaken by Cancel, are
ignored.

If phone storage is full or unavailable, a download is not sent until its
command ID and recovery details have been saved successfully. Tasks and both
handoff screens show **Recovery details aren’t saved**, with **Try saving
again** after freeing storage. That button only saves; starting a download
still requires the person's confirmation. Unsaved handoffs stay visible in
the running app instead of expiring. Keep the app open until saving succeeds:
changes held only in memory cannot survive a restart. After a sent request,
the saved command remains available for recovery if saving its answer fails;
the answer and removal of the pending marker are saved together. This uses
the shared web client on Android and iPhone, with no native plugin changes.

Returning from Rules while offline with an unresolved download shows **No
answer yet**: the computer may have started getting the source. This takes
priority over the ordinary offline notice until the computer answers.

## Which computers are asked

Companions with the receiver advertise `contribution_import_v1`. The phone
sends contribution requests only to one that does; an older companion can't
parse them and would drop the connection. The receiver's own capabilities
query then says whether that computer's system can hold contribution
workspaces. The folder list offers Library folders Øphio may write as a whole
that aren't a project's own or an agent's, and remembers the choice per
computer on the phone. Choosing another computer switches the app to it and
reopens the same handoff there. Ready shows Next up as it is: a reopened
project keeps its queue, so a running one is said to be running.

## Delivery

[Contribution delivery](contribution-delivery.md) now brings a user-opened
Android app link or desktop QR scan into `handoffs().receive(...)`, then opens
the existing review. The paired computer retrieves the exact referenced record;
opening grants no trust or permission to run work. Clankerhub's sender wiring
and the real combined pilot remain in CH-062/CH-065.

## Not built yet

- **Workspace-conflict detail**
  (changed files, branch, the existing project) and its "use my workspace as
  it is" choice, space needed and available, missing capabilities, the game's
  display name and download progress: the receiver doesn't report them yet.
- **Task terms and the required model** (OI-02): a task reference is shown as
  not attached, and Start isn't offered.

## Trying it

`npm run dev:sim` in `client/`, then open `/?handoff`: one fixture handoff
waits under Tasks. `window.__sim.host.contribution` drives the simulated
receiver (`next.review`, `next.prepare`, `next.outcome`, `hold`, `release()`,
`supported`); it downloads nothing and checks no digests. The screenshot plan
is `client/scripts/plans/contribution.json`:

```sh
node scripts/shots.mjs http://localhost:5173 <out-dir> scripts/plans/contribution.json
```

Run the screen tests from `client/` with
`sh scripts/test.sh src/screens/contribute/contribution.test.tsx`.
They wait for the receiver's result before using Review rules: lazy imports
and real asynchronous SHA-256 work can outlast a fixed fake-timer advance.
The queue-reopening case runs with both normal hashing and a delayed digest
result to cover that race. The lapsed-check case moves the clock forward six
minutes with `vi.setSystemTime`, then exercises reopening; it does not run
every intervening UI and heartbeat timer. The default test timeout remains
unchanged.
The lost-receipt/offline regression returns from Rules with an unresolved
download, checks the notice, then reconnects and verifies that the original
request is recovered without sending a second download.
