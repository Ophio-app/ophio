// SPDX-License-Identifier: Apache-2.0
import { useId, useRef } from "react";
import { PRODUCT_NAME } from "../../brand";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import { useHandledOnScreen } from "../../platform/handled";
import { useRoute } from "../../shell/Screen";
import { useReleaseCheck } from "./useReleaseCheck";

export function NewVersion() {
  const { session, platform, profile } = useApp();
  const { active } = useRoute();
  const link = useLink();
  const admin = useHost((s) => s.grants?.admin ?? false);
  const { view, error, supported, reload } = useReleaseCheck();
  const openIds = useHost((s) => Object.values(s.attention).filter((a) => a.new_version === view?.newer?.version && !a.task_id && a.resolved_at === null).map((a) => a.attention_id).sort().join(" "));
  useHandledOnScreen(platform, profile.profileId, openIds ? openIds.split(" ") : [], active && supported && !!view?.newer && !!view.how_to_update, session);
  const saved = useRef(0);
  const enabled = useCommandControl({
    kind: "instant", value: view?.enabled ?? false,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_release_check", enabled: next }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  const hint = useId();
  if (!supported) return null;
  const online = link.state === "online";
  return <article className="settings-card settings-stack">
    {view?.newer && <>
      <p>{PRODUCT_NAME} {view.newer.version} is available for {profile.name}.</p>
      <Button onClick={() => void platform.openExternal(view.newer!.url).catch(() => toast("The release page couldn’t be opened. Try again.", { tone: "stop" }))}>What’s new</Button>
      <p className="settings-meta">{view.how_to_update}</p>
    </>}
    {view?.checking && <p className="settings-meta" role="status">Checking for new versions…</p>}
    {view?.enabled && view.checked_at != null && !view.newer && !view.checking &&
      <p>{profile.name} has the newest {PRODUCT_NAME}. Checked <Age at={view.checked_at} suffix=" ago" /></p>}
    <label className="settings-check"><input type="checkbox" checked={enabled.value} aria-describedby={hint}
      disabled={!view || !admin || !online}
      onChange={(e) => enabled.set(e.target.checked, { label: "Check for new versions", what: "whether the computer checks for new versions", said: e.target.checked ? "The computer checks for new versions once a day." : "The computer no longer checks for new versions." })} />Check for new versions of {PRODUCT_NAME}</label>
    <p id={hint} className="settings-meta">Once a day, {profile.name} asks GitHub whether a newer {PRODUCT_NAME} is out, and tells your phones once about each new version. It doesn’t sign in, sends nothing about you, and downloads nothing.</p>
    {!admin && <p className="settings-meta">This device needs the Manage access permission to change this.</p>}
    {!online && <Notice tone="unknown">Offline. Connect to change this.</Notice>}
    {error && <Notice tone="unknown">{error}</Notice>}
  </article>;
}
