# Agent updates

Settings › Agent apps shows the installed and newest known versions of Claude Code and Codex on the connected computer. An unknown version or failed check is not shown as “up to date.” Pinned programs and unsupported systems explain how to update on the computer instead of offering an install button.

## Choose how updates happen

One setting controls both agents:

- **Update automatically** checks every six hours and installs supported updates into Ophio’s own copies. It does not replace the computer’s other installations or change running tasks, accounts, permissions or selected models.
- **Tell me when there’s an update** checks every six hours without installing. This is the default. Plain version updates appear in the card without a notification every time.
- **Don’t check** stops scheduled checks and updates. Existing copies remain in use. **Check now** still allows an explicit check; a known newer version can still be installed with **Update now**.

**Update now** fetches and verifies one agent’s newest release using the same checksum, safe unpacking and startup checks as automatic updates. The previous copy is retained for recovery. It does not change the update setting. A failed update leaves the existing working copy in place and shows a problem rather than claiming success.

Computers whose old switch was on stay automatic. An old off setting or missing setting becomes “Tell me.” Older phones can still send the old command: on selects automatic updates, and off selects “Don’t check.” An older computer still shows its original two-choice control on a newer phone.

## Going back to 0.2.0 and upgrading again

Version 0.2.0 has only an on/off switch: on keeps and automatically updates Ophio’s own agent copies; off removes those copies and uses separately installed agents instead. It cannot keep copies without automatic updates.

All three current choices keep existing copies, so they are saved with the old switch **on**. Going back to 0.2.0 therefore keeps the copies and resumes automatic updates, even if you chose **Tell me** or **Don’t check**. To stop that on 0.2.0, turn its switch off, knowing that it removes Ophio’s copies. No current choice asks for that removal.

The full three-way choice is saved separately. Upgrading again restores it if the stored record still contains it. A 0.2.0 periodic check or setting change rewrites that record and drops the newer choice. 0.2.0 runs its first check as soon as it starts, so in practice the newer choice is lost once 0.2.0 has run, and upgrading again gives **Update automatically** unless you turned the switch off on 0.2.0. After such a rewrite, upgrading again follows the surviving old switch: on becomes **Update automatically**, and off becomes **Tell me**, as on an ordinary upgrade from 0.2.0. A lost choice cannot be recovered; select **Tell me** or **Don’t check** again if needed.

Setup progress is saved with a 0.2.0-readable boolean and a separate full choice too, so completed or skipped setup stays finished after going back. Upgrading again keeps the full setup choice unless 0.2.0 rewrote that file; then an answered on becomes automatic and an answered off becomes **Don’t check**. This setup answer records progress; the running update policy comes from the agent settings described above.

These storage round trips are covered by tests using the 0.2.0 readers. An installed-build rollback has not been verified. See [Upgrading](upgrading.md) for the general rollback procedure.

## New models without daily reminders

Before an update, Ophio looks for explicit model additions in official Claude Code changelogs and Codex GitHub release notes newer than the observed installed version. It only accepts narrowly validated model identifiers and creates the names with its own naming rules. Release prose is never used as notification text or instructions. Missing, unreadable or ambiguous notes produce no model claim.

A known model addition appears in the update card and beside the model choices, with **Update now** when this computer can install it. An announcement is not a promise that a particular account has access.

After an update, Ophio compares real model lists from before and after it. A newly listed model earns one quiet “now available” notice. The comparison waits for the ordinary model refresh, normally within five minutes, rather than starting extra background agent processes. Without a trustworthy before-and-after observation, it makes no availability claim.

Plain newer versions produce no notification until this computer has been behind for fourteen days. There is at most one such quiet reminder per computer every thirty days, unless an agent updates in between. Repeated patch releases do not repeat the same model announcement. Swiping a notice dismisses it across paired devices; an installed update clears obsolete update notices. New-model availability news remains until handled or superseded.

These notices have their own **Agent updates and new models** type in Settings › Notifications, recommended **Silent**. Tapping opens Agent apps. See [Notifications](notifications.md) for phone background behavior. Ophio does not infer that tasks are blocked from generic error text: no reliable maker-provided minimum-version signal is currently used.

## Privacy and limits

Checks request public npm version documents. When a newer version is found during a check, Ophio may also request the maker’s public release notes. These requests contain no account details, task text, project names or model choices. Normal network information, including the computer’s Internet address, is visible to those services. Checking does not download an agent program or launch one to ask its version; it uses versions already observed by Ophio.

Downloads and parsing are bounded. Claude’s changelog is limited to two mebibytes. Codex history is limited to six pages of five releases, two mebibytes per page, and thirty seconds overall. Older announcements outside that window can be missed; exact post-update model comparison does not depend on release notes. Program downloads retain the existing size, checksum and archive safety limits. Claude’s verification uses the existing serialized, graceful background-check discipline.

The policy, checking and phone interface are shared across supported Linux, Windows and Mac computers and both phone interfaces. Platform-specific availability still decides whether a computer can receive a managed copy.
