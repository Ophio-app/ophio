// SPDX-License-Identifier: Apache-2.0
//
// A warning says what an action will do before it goes ahead. Each one can
// be turned off by itself with “Don’t show me again”, or every one at once in
// Settings; from then on its action goes ahead as though the person had
// confirmed it. Approvals the computer asks for, and checks of who is using
// the phone, are not warnings: they always ask.

import { useCallback, useState } from "react";
import { useStore } from "../state/store";
import { prefs, type Prefs } from "./prefs";

/** Every warning that can be turned off, named for Settings. */
export const WARNINGS = {
  "issues.stop": "Stop Run all",
  "github.merge": "Merge once checks pass",
  "task.force_stop": "Force stop",
  "task.archive": "Archive a task",
  "tasks.archive_all": "Archive several tasks",
  "next_up.remove": "Remove from Next up",
  "project.remove": "Remove a project",
  "project.delete": "Delete a one-off",
  "inbox.discard": "Discard a shared item",
  "computer.leave": "Leave a computer you control",
  "control.dismiss": "Dismiss unresolved control",
  "terminal.open": "Open a new terminal",
  "terminal.close": "Close a terminal",
  "terminal.paste": "Paste several lines",
  "window.close": "Close a window",
  "picture.remove": "Remove a picture",
  "mark.delete": "Delete a mark",
  "voice.mark": "Speak while you mark",
  "file.delete": "Delete a file",
  "draft.discard": "Discard a draft",
  "recovery.discard": "Discard a recovery copy",
  "drafts.clear": "Clear sent drafts",
  "account.delete": "Delete an account",
  "login.delete": "Delete an agent login",
  "device.revoke": "Revoke a device",
  "chat.revoke": "Revoke a chat",
  "preview.remove": "Remove a preview",
  "access.whole_computer": "Let a conversation reach the whole computer",
} as const;

export type WarningId = keyof typeof WARNINGS;

/** Whether `id` still asks before its action goes ahead. */
export function warns(id: WarningId): boolean {
  return asks(prefs.get(), id);
}

const asks = (p: Prefs, id: WarningId) => !p.warningsOff && !(p.hiddenWarnings ?? []).includes(id);

export function hideWarning(id: WarningId): void {
  prefs.set((p) => ((p.hiddenWarnings ?? []).includes(id) ? p : { ...p, hiddenWarnings: [...(p.hiddenWarnings ?? []), id] }));
}

export function showWarning(id: string): void {
  prefs.set((p) => ({ ...p, hiddenWarnings: (p.hiddenWarnings ?? []).filter((x) => x !== id) }));
}

/**
 * One warning's gate. `ask(open, go)` opens the warning, or goes ahead at
 * once when it is turned off. `check` is its “Don’t show me again” box for
 * the warning's body, and `agreed()`, called as the person goes ahead, keeps
 * that choice: closing or cancelling never turns a warning off. `more` is
 * the “…” a button's name ends with while it still opens the warning.
 */
export function useWarning(id: WarningId) {
  const [skip, setSkip] = useState(false);
  const on = useStore(prefs, (p) => asks(p, id));
  const ask = useCallback((open: () => void, go: () => void) => {
    if (!warns(id)) { go(); return; }
    setSkip(false);
    open();
  }, [id]);
  const agreed = useCallback(() => { if (skip) hideWarning(id); }, [id, skip]);
  return { ask, agreed, more: on ? "…" : "", check: <DontShowAgain checked={skip} onChange={setSkip} /> };
}

export function DontShowAgain({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <label className="warning-skip">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      Don’t show me again
    </label>
  );
}
