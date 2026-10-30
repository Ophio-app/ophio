# Contribution verification: Ophio #147 and Clankerhub

Status: implementation handoff, 2026-10-14. This is a proposed shared contract,
not an implemented verification service or a configured trust authority.

## Verified starting point

- Ophio issue #147 remains open. PR #193, merged as
  `91edb23a8161af4642167103277c53b488b54700`, implements policy text and sizes
  before trust. Preserve that implementation and its cancellation/queue fixes.
- `ContributionRules.tsx` still requires the manual Clankerhub checkbox.
  `useContribution.ts` turns it into `ContributionApproval`.
  `contribution/validate.rs::approval` refuses it at five minutes, and
  `review_contribution` deliberately returns `awaiting_local_approval` for a
  previously prepared operation. There is no remembered rule-trust record.
- `npm test --prefix client -- src/screens/contribute/contribution.test.tsx`
  with `TMPDIR` set to the task's scratch directory
  passed all 22 tests on the branch after merging `origin/main` (`dd02b051`).
  These include the disabled preparation button before the manual check,
  approval expiry, and reading rules without creating a project. This is
  simulator evidence, not a live publisher or physical-phone test.
- Clankerhub's `src/core/handoff/index.ts::buildImport` creates an unsigned
  browser packet. `src/core/catalog/loading.ts::CatalogAuthority` is separate
  trusted input; it is not a network authentication mechanism.
  `schemas/handoff/README.md` requires independent approval of both exact
  packet bytes and current catalog handoff bytes. It supplies no operational
  verification endpoint, signing key or packet issuer.

## Shared engineering proposal

Use a publisher-operated HTTPS issuer and verifier. This avoids distributing
signing keys in the browser and lets every reopen check current revocation.
Agree this contract in both repositories before claiming interoperability.
No deployment address is selected here.

The receiver must obtain the publisher ID and verification URL from trusted
host configuration, outside the packet, handoff, repository and imported rules.
Initially that configuration can name one Clankerhub authority. Do not derive
a trusted origin from a game URL, use a packet-supplied key, or silently enable
a preview authority in release builds. Establishing or changing this trust
anchor is distinct from the person's trust in a game's rules.

### Clankerhub ownership

1. Introduce issuance against the independently approved current catalog.
   The server, not browser input, selects the approved handoff bytes. Validate
   game/release, repository/commit and every policy reference against that
   catalog. Use server-generated packet IDs and a bounded lifetime. Return
   the exact packet and handoff JSON strings for the existing transport.
2. Persist the issued packet ID, exact packet SHA-256, handoff SHA-256, issue
   and expiry times and publisher identity. A packet ID cannot acquire new
   bytes. Do not authorize any arbitrary client-created envelope merely
   because its handoff hash is currently approved.
3. Add a verifier accepting a receiver-generated random challenge, packet ID,
   exact packet SHA-256 and handoff SHA-256. It must independently load current
   catalog/revocation state and the issuance record. Reject unknown, altered,
   expired or revoked packets, changed or withdrawn listings, and stale or
   unavailable authority state.
4. A successful response binds the challenge, contract version, publisher ID,
   both exact byte digests, packet ID/expiry, game/release, repository/commit,
   and check time/valid-until. Bound its lifetime to at most 60 seconds and
   never beyond packet or catalog expiry. Responses are not cacheable.
   The HTTPS response authenticates the publisher; no client assertion does.
5. Preserve browser isolation. A game iframe cannot issue a contribution,
   reach the companion, obtain pairing/admin credentials or authorize work.
   Keep the verifier independent of creator-hosted game/source traffic.

This introduces a small authority service and a durable issuance/revocation
store. Clankerhub currently has no such production service. The implementation
must include its operator configuration and local integration harness; a
hard-coded success response or test-only authority does not fulfill this gate.
Public deployment and selection of the real trust anchor remain explicit.

### Ophio ownership

1. Add an optional capability for receiver-owned publisher verification.
   Legacy companions retain the honest manual flow. A configured authority's
   failure must not silently downgrade to manual or remembered authentication.
2. Verify independently on every prepare, resume and reopen. Use a new random
   challenge, exact byte binding and strict bounded responses. Validate TLS,
   pin DNS answers for the connection, reject redirects/private destinations,
   omit credentials/proxies, and apply a deadline within the control request's
   timeout. Reuse the existing contribution network safety rules.
3. Keep publisher evidence separate from rule consent. Never treat a saved
   five-minute `ContributionApproval` as remembered rule trust. Check fresh
   evidence again at the admission/registration boundary; failure retains
   existing files and projects without granting new authority.
4. Persist revocable rule trust scoped to authenticated principal, publisher
   and authority configuration, repository, full commit and sorted policy
   set, selected host, and root ID/path/device/inode. A changed binding needs
   new consent. An unchanged packet renewal must not prompt again. Keep the
   previous policy references for changed-role reporting; include additions,
   removals and digest changes. Root-ID reuse cannot revive old trust.
5. Expose explicit consent and revocation through the existing person-only
   Manage/Library authorization boundary. Imported content and running agents
   cannot create remembered trust. Revocation must fence in-flight work and
   preserve user files. A stored prepared result is not ongoing authentication
   or permission to start a task.
6. Reuse existing workspace reconciliation, policy verification, cancellation
   and command deduplication. Zero-confirmation reopening must still detect
   changed local policy files, changed roots and workspace conflicts. It must
   not change an existing queue or execute setup, code or a paid model run.

### Presentation ownership

Clankerhub's project rules assign this integration's presentation to Opus 5.5
and nonvisual engineering to Astra/xhigh. Give presentation the final typed
verification/trust result and capability before changing the flow:

- First use: verify the publisher automatically; retain Read and byte sizes;
  one explicit rule-trust confirmation prepares the source.
- Unchanged return: fresh automatic verification plus remembered consent opens
  the existing managed project without the manual checkbox.
- Changed rules: show affected roles, keep documents readable, and require
  new consent. Do not claim a textual diff unless verified previous text exists.
- Offline, revoked or unavailable verification: explain the actual blocker;
  never present cached success as current verification.
- Keep explicit rule-trust revocation accessible. Shared web/protocol/Rust
  behavior covers Android and iPhone; record any native work separately.

## Required conformance evidence

Use the same owned fixtures in both projects and test the production verifier
and receiver together, rather than copying approval assertions into a mock.

- First consent, unchanged return after more than five minutes, renewed packet,
  receiver restart, changed roles and explicit rule-trust revocation.
- Packet-byte substitution (including whitespace), same ID/different digest,
  unknown ID, expiry, altered handoff, wrong repository/commit/publisher,
  replayed response/challenge, stale cache and clock rollback.
- Packet revocation, listing withdrawal/change, unavailable current catalog,
  endpoint failure/timeout, redirects, DNS rebinding and oversized responses.
- Another principal/host/root, replaced root inode, narrowed grants, canceled
  or superseded operation, revocation during acquisition and duplicate commands.
- Changed local policies and dirty work; existing files, projects and queue
  state stay intact. Reads and every failed check create no task or execution
  permission. Older companions never receive unsupported commands.

## Coordination interruption

The owner explicitly requested work with Clankerhub. This run verified actual
Codex `gpt-6-astra` / `xhigh`, inspected both repositories and attempted a
read-only engineering coordination request through Ophio. Both `ophio_agents`
and `ask_agent` were refused with `This run doesn't plan the person's work`.
No helper started and no Clankerhub files were changed.

Ophio issue #203 records the
reproducer: `bridge.rs` rejects all Work calls from GitHub tasks before
`work_tools.rs` can apply its explicit Agents/Ask exceptions. Do not bypass
that gate using private app state or another agent launcher. Resume coordination
from an ordinary Ophio chat with helper access, or after that gate is fixed.

The implementation still needs the Clankerhub authority, receiver verification
and durable trust, presentation, interoperability/security checks and a reviewed
PR. This handoff does not close #147 and does not replace those changes.
