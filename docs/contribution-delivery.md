# Clankerhub delivery to the phone

Issue #146 adds a one-way, user-opened handoff transport. Android handles
`ophio-contribution://open/v1`; a computer browser can display that same link
as a QR code, scanned in Ophio with Tasks → New → Scan a Clankerhub code. The paired
computer fetches the referenced handoff through the authenticated control
connection. No local browser listener, service discovery, admin-token sharing,
hosted relay or new public endpoint is involved.

This implements the desktop **scan fallback** in CH-016 J10. Automatic delivery
from a desktop browser to its paired computer, with a phone notification, is
not implemented. This transport does not reply to Clankerhub. Clankerhub must
use the one-way “continue in Ophio / check your phone” state; it must not invent
`ReceiverTransport.probe` capabilities or return `prepared` from a URL launch.

## Producer contract for CH-062

The source boundary was checked against Clankerhub's `src/core/handoff/index.ts`
and `src/core/catalog/types.ts`: `buildImport` returns the packet object and the
exact validated handoff string; `Release.handoff` supplies its published path
and SHA-256. The producer must resolve that path against its independently
configured catalog asset base, not a base supplied by a game frame.

1. On the person's Open action, load a current catalog session and the exact
   published handoff bytes. Call `buildImport` with those bytes. A new Open or
   Resume creates a **fresh** packet; only a retry within its lifetime reuses one.
2. Serialize the returned packet **once** with `JSON.stringify`. These UTF-8
   bytes become `packetJson`. Do not subsequently pretty-print or normalize it.
3. Form the link below. On the same phone, navigate to it as an explicit user
   action. On a computer, display it as a QR code for Ophio's scanner.
4. Retain the existing unsupported/install path when the OS cannot open Ophio.
   The OS opening a scheme is not evidence that the receiver supports the game.

```text
ophio-contribution://open/v1?packet=<base64url-UTF8-packet-without-padding>&handoff=<percent-encoded-HTTPS-URL>
```

Exactly these two query parameters are accepted, once each; no fragment.
`client/src/platform/contributionLinks.ts` contains the producer codec and parser.
The URL identifies the **exact published handoff file**, not a page returning
HTML or an endpoint that rebuilds/reformats the record. The receiver compares
its raw SHA-256 to the packet's `handoffSha256` before returning it to the phone.
Clankerhub's web action/QR presentation remains CH-062's work in its own repo.

The URL must be canonical ASCII HTTPS on port 443, with a DNS hostname and a
nonempty path made of letters, digits, `.`, `_`, `-` and `/` separators. No
credentials, query, fragment, escapes, empty/dot segments or redirects. The
computer rejects private/special DNS answers, pins all permitted answers for
the TLS connection, sends no credentials and ignores ambient proxies. This
uses the existing contribution fetcher's restrictive network profile.

## Bounds, authentication and lifecycle

- Each record is limited to 65,536 UTF-8 bytes. The launch URL is limited to
  98,304 ASCII characters. Android retains at most eight pending native links;
  the web client retains only the latest unresolved contribution reference.
- A handoff can occupy the full 64 KiB without enlarging the QR: only its URL is
  carried. Use CH-060's compact packet serialization for QR codes. A whitespace-
  padded 64 KiB packet is accepted by the codec but does **not** fit a single QR;
  do not offer that as a desktop scan. Maximum OS/browser URI lengths still need
  physical-device qualification; parser limits are not a platform guarantee.
- Packets must have valid closed CH-015 fields and a lifetime no longer than
  ten minutes. Both the phone and computer reject expired delivery. The computer
  rechecks freshness and Manage authority during and after fetching; the fetch
  has an 18-second deadline and shares the receiver's two concurrency slots.
- New hosts advertise `contribution_delivery_v1`. Only an authenticated paired
  connection with Manage permission sends `Query::ContributionDelivery`:
  `{kind:"contribution_delivery", packet_json, handoff_url}`. Its result is
  `{kind:"contribution_delivery", handoff_json}`. Older hosts receive no new query.
- A launch survives first pairing, offline recovery and app restart. Locking
  suspends delivery and hides its presentation. Repeated opens and unlocks reuse
  an in-flight read for the same exact packet and URL. Different reads on one
  session wait for the current read to settle before starting their timeout;
  superseded, dismissed or locked requests are not sent from that wait.
  Dismissal, host switching, replacement by another launch or locking prevents
  a late reply from opening review. The delivery dialog lets the person switch
  to another paired computer without losing the pending reference. Expiry never
  renews the packet; the person must return to Clankerhub for a fresh one.
- Success calls `handoffs().receive(packetJson, handoffJson)` without modifying
  either string, then navigates to the existing contribution review. Duplicate
  records reopen the same phone-local handoff. No setup, source acquisition,
  project registration, trust approval or model run occurs during delivery.

Custom-scheme links are **untrusted input**, not proof of website origin or
publisher identity. They contain no credentials or local authority. HTTPS and
the digest bind bytes; the paired connection authenticates the receiving host
and device. Independent catalog/publisher authentication and remembered trust
remain #147. The existing review/prepare boundary still applies to every link.
No game-frame message may trigger this privileged application action.

## Platform and verification limits

Android's intent filter and `AppLinks` queue accept the new scheme on cold and
warm launches. The generic camera scanner already returns arbitrary QR text;
the shared UI routes contribution codes through the same intake.

Shared web, protocol and companion code works for iPhone. Its scan path reuses
the existing `scan` plugin command. Direct iPhone URL entry is **owed**: the Swift
plugin exists only on `feat/iphone`, outside this task's main-based branch. It
must add this prefix and the 96 KiB limit in `AppLinks.swift`, and register
`ophio-contribution` in the generated app's `CFBundleURLTypes`. This is recorded
in the private iPhone ledger. No Mac build or physical iPhone result is claimed.

Automated tests cover URL parsing/bounds, exact-byte preservation, duplicate
opens, camera entry, old hosts, denied grants, lock/unlock, late replies,
expiry, digest mismatch, full-size handoffs and absence of workspace/journal
side effects. Browser/client mocks do not qualify Android intent dispatch,
camera hardware, an installed phone, or Clankerhub's real end-to-end pilot.
CH-065 still owns that combined real round trip.
